//! `better-shot`: screenshot capture for niri with optional clipboard,
//! annotation, HDR handling, output formats and metadata.
//!
//! Modes: `ui` (niri's screenshot UI), `screen` (focused output), `window`
//! (focused or explicit window). Screenshots are saved to a generated path and
//! can be piped to `wl-copy`, annotated non-interactively, or opened in
//! `satty` (all overridable).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use image::RgbaImage;
use niri_ipc::{Action, Request, Response};

pub mod annotate;
pub mod cli;
pub mod hdr;
pub mod metadata;
pub mod output;
pub mod record;

use annotate::Annotation;
use hdr::{DEFAULT_REFERENCE_LUMINANCE, HdrDetection, HdrRequest};
use metadata::CaptureMetadata;
use output::OutputFormat;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ui,
    Screen,
    Window,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Screen => "screen",
            Self::Window => "window",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub mode: Mode,
    pub path: Option<PathBuf>,
    pub show_pointer: bool,
    pub window_id: Option<u64>,
    pub copy: bool,
    /// Open the final image in satty (interactive).
    pub annotate: bool,
    /// Built-in, non-interactive annotations.
    pub annotations: Vec<Annotation>,
    pub hdr: HdrRequest,
    pub format: Option<OutputFormat>,
    pub quality: u8,
    pub metadata: bool,
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
            annotations: Vec::new(),
            hdr: HdrRequest::Keep,
            format: None,
            quality: 90,
            metadata: false,
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
    pub format: OutputFormat,
    pub hdr: HdrDetection,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub source_color: Option<&'static str>,
    pub metadata_path: Option<PathBuf>,
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
    generated_path_with_format(unix_secs, OutputFormat::Png)
}

/// Generates `better-shot-<epoch>.<ext>` for the chosen format.
pub fn generated_path_with_format(unix_secs: u64, format: OutputFormat) -> PathBuf {
    default_dir().join(format!("better-shot-{unix_secs}.{}", format.extension()))
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

pub(crate) fn send(socket: &Path, request: Request) -> Result<Response, String> {
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

fn copy_png_to_clipboard(bytes: &[u8]) -> Result<(), String> {
    let program = std::env::var("BETTER_SHOT_CLIPBOARD").unwrap_or_else(|_| "wl-copy".to_owned());
    let mut child = Command::new(&program)
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| format!("cannot run {program}: {err}"))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| "clipboard stdin unavailable".to_owned())?
        .write_all(bytes)
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

fn detect_hdr(socket: &Path) -> HdrDetection {
    match send(socket, Request::FocusedOutput) {
        Ok(Response::FocusedOutput(Some(output))) => {
            HdrDetection::for_output(Some(output.name), output.max_bpc)
        }
        Ok(Response::FocusedOutput(None)) => HdrDetection::default(),
        Ok(other) => HdrDetection {
            config_error: Some(format!(
                "unexpected niri response for focused output: {other:?}"
            )),
            ..HdrDetection::default()
        },
        Err(err) => HdrDetection {
            config_error: Some(err),
            ..HdrDetection::default()
        },
    }
}

/// niri writes the capture asynchronously after replying, so wait until the
/// file exists and its size has stopped changing before reading it.
fn wait_for_file(path: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_size = None;
    let mut stable_checks = 0;
    loop {
        if let Ok(meta) = std::fs::metadata(path)
            && meta.is_file()
            && meta.len() > 0
        {
            if last_size == Some(meta.len()) {
                stable_checks += 1;
                if stable_checks >= 2 {
                    return Ok(());
                }
            } else {
                last_size = Some(meta.len());
                stable_checks = 0;
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for niri to write {}",
                path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn discard(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Captures a screenshot and runs the requested post-processing.
pub fn run(socket: &Path, options: &Options) -> Result<Outcome, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let format = options
        .format
        .or_else(|| options.path.as_deref().and_then(OutputFormat::from_path))
        .unwrap_or(OutputFormat::Png);
    let path = options
        .path
        .clone()
        .unwrap_or_else(|| generated_path_with_format(timestamp, format));
    // niri requires absolute paths and rejects relative ones.
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|err| format!("cannot resolve relative --path: {err}"))?
            .join(path)
    };

    let tonemap = options.hdr == HdrRequest::Tonemap;
    let rewrite = format != OutputFormat::Png || !options.annotations.is_empty() || tonemap;

    if let Some(parent) = path.parent()
        && !options.dry_run
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    // niri always writes PNG bytes here; for other formats we re-encode in
    // place after decoding. Keeping the final path avoids a notification
    // pointing at a temporary file that is about to disappear.
    let action = build_action(options, &path);
    let hdr = if options.dry_run {
        HdrDetection::default()
    } else {
        detect_hdr(socket)
    };

    if options.dry_run {
        return Ok(Outcome {
            path,
            copied: false,
            annotated: false,
            action,
            format,
            hdr,
            width: None,
            height: None,
            source_color: None,
            metadata_path: None,
        });
    }

    match send(socket, Request::Action(action.clone()))? {
        Response::Handled => (),
        other => return Err(format!("unexpected niri response: {other:?}")),
    }

    let read_image = rewrite
        || options.hdr == HdrRequest::Sdr
        || options.copy
        || options.annotate
        || options.metadata;

    let mut decoded = None;
    let mut source_color = None;
    if read_image {
        wait_for_file(&path)?;
        let image = output::load(&path).inspect_err(|_| discard(&path))?;
        source_color = Some(output::color_name(&image));
        if options.hdr == HdrRequest::Sdr && output::is_hdr_source(&image) {
            discard(&path);
            return Err(
                "capture is high-bit-depth (HDR); use --hdr=tonemap to convert it to SDR"
                    .to_owned(),
            );
        }
        decoded = Some(image);
    }

    let mut processed: Option<RgbaImage> = None;
    if tonemap {
        let image = decoded
            .as_ref()
            .ok_or_else(|| "capture image was not read".to_owned())?;
        let reference_luminance = hdr
            .config
            .as_ref()
            .and_then(|config| config.reference_luminance)
            .unwrap_or(DEFAULT_REFERENCE_LUMINANCE);
        processed = match hdr::tonemap_to_sdr(image, reference_luminance) {
            Ok(image) => Some(image),
            Err(err) => {
                discard(&path);
                return Err(err);
            }
        };
    }

    if !options.annotations.is_empty() {
        let mut canvas = match processed.take() {
            Some(canvas) => canvas,
            None => decoded
                .as_ref()
                .ok_or_else(|| "capture image was not read".to_owned())?
                .to_rgba8(),
        };
        for annotation in &options.annotations {
            if let Err(err) = annotation.draw(&mut canvas) {
                discard(&path);
                return Err(err);
            }
        }
        processed = Some(canvas);
    }

    if rewrite && processed.is_none() {
        processed = Some(
            decoded
                .as_ref()
                .ok_or_else(|| "capture image was not read".to_owned())?
                .to_rgba8(),
        );
    }

    let dimensions = processed
        .as_ref()
        .map(|image| (image.width(), image.height()))
        .or_else(|| {
            decoded
                .as_ref()
                .map(|image| (image.width(), image.height()))
        });

    if rewrite {
        let image = processed
            .as_ref()
            .ok_or_else(|| "processed image missing".to_owned())?;
        let bytes = match format.encode(image, options.quality) {
            Ok(bytes) => bytes,
            Err(err) => {
                discard(&path);
                return Err(err);
            }
        };
        if let Err(err) = std::fs::write(&path, bytes) {
            discard(&path);
            return Err(format!("cannot write {}: {err}", path.display()));
        }
    }

    if options.copy {
        // Always put a PNG on the clipboard, whatever the file format is.
        let bytes = if let Some(image) = &processed {
            OutputFormat::Png.encode(image, 100)?
        } else if format == OutputFormat::Png {
            std::fs::read(&path).map_err(|err| format!("cannot read screenshot: {err}"))?
        } else {
            let image = decoded
                .as_ref()
                .ok_or_else(|| "capture image was not read".to_owned())?
                .to_rgba8();
            OutputFormat::Png.encode(&image, 100)?
        };
        copy_png_to_clipboard(&bytes)?;
    }
    if options.annotate {
        annotate(&path)?;
    }

    let metadata_path = if options.metadata {
        let (width, height) =
            dimensions.ok_or_else(|| "image dimensions unavailable".to_owned())?;
        let reference_luminance = if tonemap {
            Some(
                hdr.config
                    .as_ref()
                    .and_then(|config| config.reference_luminance)
                    .unwrap_or(DEFAULT_REFERENCE_LUMINANCE),
            )
        } else {
            hdr.config
                .as_ref()
                .and_then(|config| config.reference_luminance)
        };
        let metadata = CaptureMetadata {
            mode: options.mode.name(),
            window_id: options.window_id,
            timestamp_unix: timestamp,
            output: path.clone(),
            format: format.name(),
            width,
            height,
            source_color: source_color.unwrap_or("unknown"),
            hdr: hdr.clone(),
            hdr_request: options.hdr,
            reference_luminance,
            annotations: options
                .annotations
                .iter()
                .map(Annotation::describe)
                .collect(),
            copied: options.copy,
            dry_run: false,
            command: std::env::args_os()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        };
        Some(metadata.write()?)
    } else {
        None
    };

    Ok(Outcome {
        path,
        copied: options.copy,
        annotated: options.annotate,
        action,
        format,
        hdr,
        width: dimensions.map(|dims| dims.0),
        height: dimensions.map(|dims| dims.1),
        source_color,
        metadata_path,
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

    fn white_png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
        OutputFormat::Png.encode(&image, 100).unwrap()
    }

    fn fake_output() -> niri_ipc::Output {
        niri_ipc::Output {
            name: "TEST-OUTPUT-1".to_owned(),
            make: "Test".to_owned(),
            model: "Panel".to_owned(),
            serial: None,
            physical_size: None,
            modes: Vec::new(),
            current_mode: None,
            is_custom_mode: false,
            vrr_supported: false,
            vrr_enabled: false,
            logical: None,
            max_bpc: Some(10),
        }
    }

    /// Serves `connections` requests: reports a focused output and writes a
    /// synthetic PNG for every screenshot action.
    fn fake_niri(path: &Path, connections: usize) -> std::thread::JoinHandle<Vec<Request>> {
        fake_niri_with(path, connections, white_png(24, 18))
    }

    fn fake_niri_with(
        path: &Path,
        connections: usize,
        capture: Vec<u8>,
    ) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..connections {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Request = serde_json::from_str(line.trim()).unwrap();
                let response = match &request {
                    Request::FocusedOutput => json!({
                        "Ok": Response::FocusedOutput(Some(fake_output()))
                    })
                    .to_string(),
                    Request::Action(
                        Action::Screenshot { path, .. }
                        | Action::ScreenshotScreen { path, .. }
                        | Action::ScreenshotWindow { path, .. },
                    ) => {
                        if let Some(path) = path {
                            std::fs::write(path, &capture).unwrap();
                        }
                        json!({"Ok": "Handled"}).to_string()
                    }
                    _ => json!({"Ok": "Handled"}).to_string(),
                };
                let mut stream = stream;
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
                requests.push(request);
            }
            requests
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
    fn generated_path_uses_the_selected_format() {
        let path = generated_path(1234);
        assert_eq!(path.file_name().unwrap(), "better-shot-1234.png");
        assert!(path.to_string_lossy().contains("Screenshots"));
        assert_eq!(default_dir().file_name().unwrap(), "Screenshots");

        let webp = generated_path_with_format(99, OutputFormat::WebP);
        assert_eq!(webp.file_name().unwrap(), "better-shot-99.webp");
    }

    #[test]
    fn sends_capture_to_niri_and_reports_hdr_detection() {
        let dir = temp_dir("niri");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let path = dir.join("shot.png");
        let options = Options {
            mode: Mode::Screen,
            path: Some(path.clone()),
            ..Options::default()
        };
        let outcome = run(&socket, &options).unwrap();
        assert_eq!(outcome.path, path);
        assert_eq!(outcome.format, OutputFormat::Png);
        assert_eq!(outcome.hdr.output.as_deref(), Some("TEST-OUTPUT-1"));
        assert_eq!(outcome.hdr.max_bpc, Some(10));
        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::FocusedOutput));
        assert!(matches!(
            requests[1],
            Request::Action(Action::ScreenshotScreen { .. })
        ));
    }

    #[test]
    fn applies_annotations_and_writes_metadata() {
        let dir = temp_dir("annotate");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let path = dir.join("shot.png");
        let options = Options {
            mode: Mode::Screen,
            path: Some(path.clone()),
            annotations: vec![Annotation::Rect {
                x: 2,
                y: 3,
                w: 10,
                h: 8,
            }],
            metadata: true,
            ..Options::default()
        };
        let outcome = run(&socket, &options).unwrap();
        server.join().unwrap();

        let image = output::load(&path).unwrap();
        assert_eq!((image.width(), image.height()), (24, 18));
        assert_eq!(
            image.to_rgba8().get_pixel(2, 3),
            &image::Rgba([255, 59, 48, 255])
        );
        assert_eq!(
            image.to_rgba8().get_pixel(6, 6),
            &image::Rgba([255, 255, 255, 255])
        );

        let sidecar = outcome.metadata_path.unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(sidecar).unwrap()).unwrap();
        assert_eq!(value["capture"]["mode"], "screen");
        assert_eq!(value["capture"]["output"], "TEST-OUTPUT-1");
        assert_eq!(value["file"]["format"], "png");
        assert_eq!(value["file"]["width"], 24);
        assert_eq!(value["annotations"][0], "rect:2,3,10,8");
        assert_eq!(value["hdr"]["requested"], "keep");
        assert_eq!(value["hdr"]["detected"]["max_bpc"], 10);
        assert_eq!(outcome.width, Some(24));
    }

    #[test]
    fn writes_jpeg_in_place() {
        let dir = temp_dir("jpeg");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let path = dir.join("shot.jpg");
        let options = Options {
            mode: Mode::Screen,
            path: Some(path.clone()),
            format: Some(OutputFormat::Jpeg),
            ..Options::default()
        };
        run(&socket, &options).unwrap();
        server.join().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], b"\xff\xd8");
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries
                .iter()
                .all(|name| !name.contains("better-shot-capture")),
            "temporary capture left behind: {entries:?}"
        );
    }

    #[test]
    fn rejects_tonemap_for_eight_bit_niri_captures() {
        let dir = temp_dir("tonemap");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let path = dir.join("shot.png");
        let options = Options {
            mode: Mode::Screen,
            path: Some(path.clone()),
            hdr: HdrRequest::Tonemap,
            ..Options::default()
        };
        let err = run(&socket, &options).unwrap_err();
        server.join().unwrap();
        assert!(err.contains("niri tone-maps"), "{err}");
        assert!(!path.exists());
    }

    #[test]
    fn tone_maps_a_sixteen_bit_capture_end_to_end() {
        let dir = temp_dir("hdr16");
        let socket = dir.join("niri.sock");
        let source = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(
            2,
            1,
            vec![
                0, 0, 0, 0, // black
                65535, 65535, 65535, 65535, // PQ white
            ],
        )
        .unwrap();
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba16(source)
            .write_to(std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let server = fake_niri_with(&socket, 2, png);

        let path = dir.join("shot.png");
        let options = Options {
            mode: Mode::Screen,
            path: Some(path.clone()),
            hdr: HdrRequest::Tonemap,
            metadata: true,
            ..Options::default()
        };
        let outcome = run(&socket, &options).unwrap();
        server.join().unwrap();

        let image = output::load(&path).unwrap();
        assert_eq!(output::color_name(&image), "rgba8");
        let rgba = image.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0), &image::Rgba([0, 0, 0, 0]));
        assert!(rgba.get_pixel(1, 0)[0] >= 245);
        let metadata: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(outcome.metadata_path.unwrap()).unwrap())
                .unwrap();
        assert_eq!(metadata["capture"]["source_color"], "rgba16");
        assert_eq!(metadata["hdr"]["requested"], "tonemap");
        assert_eq!(metadata["hdr"]["reference_luminance"], 203.0);
    }

    #[test]
    fn sdr_mode_accepts_niri_captures() {
        let dir = temp_dir("sdr");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let path = dir.join("shot.png");
        let options = Options {
            mode: Mode::Screen,
            path: Some(path.clone()),
            hdr: HdrRequest::Sdr,
            ..Options::default()
        };
        let outcome = run(&socket, &options).unwrap();
        server.join().unwrap();
        assert_eq!(outcome.hdr.output.as_deref(), Some("TEST-OUTPUT-1"));
        assert!(path.exists());
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
        // SAFETY: tests in this crate only mutate these variables locally.
        unsafe { std::env::set_var("BETTER_SHOT_CLIPBOARD", &copy_script) };
        copy_png_to_clipboard(b"png-bytes").unwrap();
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
