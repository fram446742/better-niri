//! `better-shot`: screenshot capture for niri with optional clipboard and
//! annotation hooks.
//!
//! Modes: `ui` (niri's screenshot UI), `screen` (focused output), `window`
//! (focused or explicit window). Screenshots are saved to a generated path and
//! can be piped to `wl-copy` and/or opened in `satty` (both overridable).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use niri_ipc::{Action, Request, Response};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ui,
    Screen,
    Window,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    pub path: Option<PathBuf>,
    pub show_pointer: bool,
    pub window_id: Option<u64>,
    pub copy: bool,
    pub annotate: bool,
    pub dry_run: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Ui,
            path: None,
            show_pointer: true,
            window_id: None,
            copy: false,
            annotate: false,
            dry_run: false,
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub path: PathBuf,
    pub copied: bool,
    pub annotated: bool,
    pub action: Action,
}

/// Screenshots directory: `$XDG_PICTURES_DIR/Screenshots`, then
/// `$HOME/Pictures/Screenshots`, then the temp dir.
pub fn default_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_PICTURES_DIR") {
        return PathBuf::from(dir).join("Screenshots");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Pictures/Screenshots");
    }
    std::env::temp_dir()
}

/// Generates `better-shot-<epoch>.png` for the given time.
pub fn generated_path(unix_secs: u64) -> PathBuf {
    default_dir().join(format!("better-shot-{unix_secs}.png"))
}

pub fn build_action(options: &Options, path: &Path) -> Action {
    let path = path.to_string_lossy().into_owned();
    match options.mode {
        Mode::Ui => Action::Screenshot {
            show_pointer: options.show_pointer,
            stdout: false,
            path: Some(path),
        },
        Mode::Screen => Action::ScreenshotScreen {
            write_to_disk: true,
            show_pointer: options.show_pointer,
            stdout: false,
            path: Some(path),
        },
        Mode::Window => Action::ScreenshotWindow {
            id: options.window_id,
            write_to_disk: true,
            show_pointer: options.show_pointer,
            stdout: false,
            path: Some(path),
        },
    }
}

fn send(socket: &Path, request: Request) -> Result<Response, String> {
    use niri_ipc::socket::Socket;
    let mut socket =
        Socket::connect_to(socket).map_err(|err| format!("cannot connect to niri IPC: {err}"))?;
    let reply = socket
        .send(request)
        .map_err(|err| format!("niri IPC error: {err}"))?;
    reply.map_err(|err| format!("niri returned an error: {err}"))
}

fn run_program(program: &str, args: &[String]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|err| format!("cannot run {program}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

fn copy_to_clipboard(path: &Path) -> Result<(), String> {
    let program = std::env::var("BETTER_SHOT_CLIPBOARD").unwrap_or_else(|_| "wl-copy".to_owned());
    let mut child = Command::new(&program)
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| format!("cannot run {program}: {err}"))?;
    let bytes = std::fs::read(path).map_err(|err| format!("cannot read screenshot: {err}"))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| "clipboard stdin unavailable".to_owned())?
        .write_all(&bytes)
        .map_err(|err| format!("cannot write to {program}: {err}"))?;
    let status = child
        .wait()
        .map_err(|err| format!("cannot wait for {program}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

fn annotate(path: &Path) -> Result<(), String> {
    let program = std::env::var("BETTER_SHOT_ANNOTATE").unwrap_or_else(|_| "satty".to_owned());
    run_program(&program, &["-f".to_owned(), path.display().to_string()])
}

/// Captures a screenshot and runs the requested post-processing.
pub fn run(socket: &Path, options: &Options) -> Result<Outcome, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = options
        .path
        .clone()
        .unwrap_or_else(|| generated_path(timestamp));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let action = build_action(options, &path);

    if options.dry_run {
        return Ok(Outcome {
            path,
            copied: false,
            annotated: false,
            action,
        });
    }

    match send(socket, Request::Action(action.clone()))? {
        Response::Handled => (),
        other => return Err(format!("unexpected niri response: {other:?}")),
    }

    if options.copy {
        copy_to_clipboard(&path)?;
    }
    if options.annotate {
        annotate(&path)?;
    }

    Ok(Outcome {
        path,
        copied: options.copy,
        annotated: options.annotate,
        action,
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    use niri_ipc::Request;
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("better-shot-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_niri(path: &Path) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            let mut stream = stream;
            stream.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
            stream.flush().unwrap();
            vec![request]
        })
    }

    #[test]
    fn builds_actions_for_each_mode() {
        let path = Path::new("/tmp/shot.png");
        let ui = build_action(&Options::default(), path);
        assert_eq!(
            serde_json::to_value(ui).unwrap(),
            json!({"Screenshot": {"show_pointer": true, "stdout": false, "path": "/tmp/shot.png"}})
        );

        let screen = build_action(
            &Options {
                mode: Mode::Screen,
                ..Options::default()
            },
            path,
        );
        assert!(matches!(screen, Action::ScreenshotScreen { .. }));

        let window = build_action(
            &Options {
                mode: Mode::Window,
                window_id: Some(7),
                show_pointer: false,
                ..Options::default()
            },
            path,
        );
        assert_eq!(
            serde_json::to_value(window).unwrap(),
            json!({"ScreenshotWindow": {"id": 7, "write_to_disk": true, "show_pointer": false, "stdout": false, "path": "/tmp/shot.png"}})
        );
    }

    #[test]
    fn generated_path_is_png_in_screenshots_dir() {
        let path = generated_path(1234);
        assert_eq!(path.file_name().unwrap(), "better-shot-1234.png");
        assert!(path.to_string_lossy().contains("Screenshots"));
        assert_eq!(default_dir().file_name().unwrap(), "Screenshots");
    }

    #[test]
    fn sends_capture_to_niri() {
        let dir = temp_dir("niri");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket);
        let path = dir.join("shot.png");
        let options = Options {
            mode: Mode::Screen,
            path: Some(path.clone()),
            ..Options::default()
        };
        let outcome = run(&socket, &options).unwrap();
        assert_eq!(outcome.path, path);
        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::ScreenshotScreen { .. })
        ));
    }

    #[test]
    fn copy_and_annotate_use_configured_commands() {
        let dir = temp_dir("hooks");
        let image = dir.join("shot.png");
        std::fs::write(&image, b"png-bytes").unwrap();

        let copy_script = dir.join("copy");
        std::fs::write(
            &copy_script,
            format!("#!/bin/sh\ncat > {}\n", dir.join("copied").display()),
        )
        .unwrap();
        std::fs::set_permissions(&copy_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        unsafe { std::env::set_var("BETTER_SHOT_CLIPBOARD", &copy_script) };
        copy_to_clipboard(&image).unwrap();
        assert_eq!(std::fs::read(dir.join("copied")).unwrap(), b"png-bytes");

        let annotate_script = dir.join("annotate");
        std::fs::write(
            &annotate_script,
            format!(
                "#!/bin/sh\nprintf '%s' \"$2\" > {}\n",
                dir.join("annotated").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&annotate_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        unsafe { std::env::set_var("BETTER_SHOT_ANNOTATE", &annotate_script) };
        annotate(&image).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("annotated")).unwrap(),
            image.display().to_string()
        );

        unsafe { std::env::remove_var("BETTER_SHOT_CLIPBOARD") };
        unsafe { std::env::remove_var("BETTER_SHOT_ANNOTATE") };
    }
}
