//! `better-wallpaper`: per-workspace wallpaper scenes for niri.
//!
//! Scenes map workspace names (or `default`) to image paths. A watcher follows
//! niri's event stream and runs an external wallpaper command for the focused
//! workspace, replacing the previous process on that output. The command is a
//! shell template with `{image}`, `{workspace}` and `{output}` placeholders,
//! defaulting to `swaybg -i {image}`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use niri_ipc::{Event, Request, Response, Workspace};
use serde::{Deserialize, Serialize};

pub const DEFAULT_COMMAND: &str = "swaybg -i {image}";

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Scene {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub wallpapers: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default)]
    pub scenes: BTreeMap<String, Scene>,
}

pub fn state_path() -> PathBuf {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".local/state")
        });
    dir.join("better-wallpaper/scenes.json")
}

impl Store {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, path)
    }

    pub fn active_scene(&self) -> Option<&Scene> {
        self.active
            .as_deref()
            .and_then(|name| self.scenes.get(name))
    }
}

/// Resolves the image for a workspace: exact name, then scene default.
pub fn resolve<'a>(scene: &'a Scene, workspace: Option<&str>) -> Option<&'a String> {
    workspace
        .and_then(|workspace| scene.wallpapers.get(workspace))
        .or(scene.default.as_ref())
}

pub fn command_template() -> String {
    std::env::var("BETTER_WALLPAPER_COMMAND").unwrap_or_else(|_| DEFAULT_COMMAND.to_owned())
}

pub fn render_command(template: &str, image: &str, workspace: &str, output: &str) -> String {
    template
        .replace("{image}", image)
        .replace("{workspace}", workspace)
        .replace("{output}", output)
}

fn spawn(command: &str) -> Result<Child, String> {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("cannot run wallpaper command: {err}"))
}

/// Stops a wallpaper process: gives short-lived commands a brief grace period
/// so a just-spawned command is not killed before it runs, then kills it.
fn stop_child(mut child: Child) {
    for _ in 0..10 {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub id: u64,
    pub name: Option<String>,
    pub output: Option<String>,
    pub is_focused: bool,
}

pub fn workspace_infos(workspaces: &[Workspace]) -> Vec<WorkspaceInfo> {
    workspaces
        .iter()
        .map(|ws| WorkspaceInfo {
            id: ws.id,
            name: ws.name.clone(),
            output: ws.output.clone(),
            is_focused: ws.is_focused,
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct Watcher {
    template: String,
    dry_run: bool,
    applied: HashMap<String, String>,
    children: HashMap<String, Child>,
}

impl Watcher {
    pub fn new(template: impl Into<String>, dry_run: bool) -> Self {
        Self {
            template: template.into(),
            dry_run,
            applied: HashMap::new(),
            children: HashMap::new(),
        }
    }

    pub fn apply(
        &mut self,
        scene: &Scene,
        workspace: Option<&str>,
        output: &str,
    ) -> Result<Option<String>, String> {
        let Some(image) = resolve(scene, workspace) else {
            return Ok(None);
        };
        if self.applied.get(output) == Some(image) {
            return Ok(Some(image.clone()));
        }
        let workspace = workspace.unwrap_or("default");
        let command = render_command(&self.template, image, workspace, output);
        if !self.dry_run {
            if let Some(old) = self.children.remove(output) {
                stop_child(old);
            }
            let child = spawn(&command)?;
            self.children.insert(output.to_owned(), child);
        }
        self.applied.insert(output.to_owned(), image.clone());
        Ok(Some(image.clone()))
    }

    pub fn finish(&mut self) {
        for (_, child) in self.children.drain() {
            stop_child(child);
        }
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

/// Applies the active scene once to the currently focused workspace.
pub fn apply_once(socket: &Path, store: &Store, dry_run: bool) -> Result<Option<String>, String> {
    let scene = store
        .active_scene()
        .ok_or_else(|| "no active scene; use `scene use <name>` first".to_owned())?;
    let workspaces = match send(socket, Request::Workspaces)? {
        Response::Workspaces(workspaces) => workspaces,
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    let focused = workspaces
        .iter()
        .find(|ws| ws.is_focused)
        .ok_or_else(|| "no focused workspace".to_owned())?;
    let mut watcher = Watcher::new(command_template(), dry_run);
    let result = watcher.apply(
        scene,
        focused.name.as_deref(),
        focused.output.as_deref().unwrap_or(""),
    );
    watcher.finish();
    result
}

/// Watches the niri event stream until it closes, reapplying on focus changes.
pub fn watch(
    socket_path: &Path,
    store: &Store,
    template: &str,
    dry_run: bool,
) -> Result<usize, String> {
    use niri_ipc::socket::Socket;

    let scene = store
        .active_scene()
        .ok_or_else(|| "no active scene; use `scene use <name>` first".to_owned())?;
    let mut stream = Socket::connect_to(socket_path)
        .map_err(|err| format!("cannot connect to niri IPC: {err}"))?;
    match stream
        .send(Request::EventStream)
        .map_err(|err| format!("niri IPC error: {err}"))?
    {
        Ok(Response::Handled) => (),
        Ok(other) => return Err(format!("unexpected niri response: {other:?}")),
        Err(message) => return Err(message),
    }

    let mut watcher = Watcher::new(template, dry_run);
    let mut applied = 0;
    let mut read_event = stream.read_events();
    loop {
        let event = match read_event() {
            Ok(event) => event,
            Err(err)
                if err.kind() == std::io::ErrorKind::UnexpectedEof
                    || err.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                break;
            }
            Err(err) => return Err(format!("event stream: {err}")),
        };
        match event {
            Event::WorkspacesChanged { workspaces } => {
                if let Some(ws) = workspaces.iter().find(|ws| ws.is_focused) {
                    if watcher
                        .apply(
                            scene,
                            ws.name.as_deref(),
                            ws.output.as_deref().unwrap_or(""),
                        )
                        .map_err(|err| format!("event stream: {err}"))?
                        .is_some()
                    {
                        applied += 1;
                    }
                }
            }
            Event::WorkspaceActivated { id, focused } => {
                if !focused {
                    continue;
                }
                let workspaces = match send(socket_path, Request::Workspaces) {
                    Ok(Response::Workspaces(workspaces)) => workspaces,
                    _ => continue,
                };
                if let Some(ws) = workspaces.iter().find(|ws| ws.id == id) {
                    if watcher
                        .apply(
                            scene,
                            ws.name.as_deref(),
                            ws.output.as_deref().unwrap_or(""),
                        )
                        .map_err(|err| format!("event stream: {err}"))?
                        .is_some()
                    {
                        applied += 1;
                    }
                }
            }
            _ => {}
        }
    }
    watcher.finish();
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("better-wallpaper-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn focused_workspace() -> serde_json::Value {
        json!({
            "id": 7,
            "idx": 1,
            "name": "main",
            "output": "eDP-1",
            "is_urgent": false,
            "is_active": true,
            "is_focused": true,
            "active_window_id": null,
            "is_hidden": false
        })
    }

    fn event_server(path: &PathBuf) -> std::thread::JoinHandle<()> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            assert!(matches!(request, Request::EventStream));
            let mut stream = stream;
            stream.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
            let event = json!({ "WorkspacesChanged": { "workspaces": [focused_workspace()] } });
            stream
                .write_all(serde_json::to_string(&event).unwrap().as_bytes())
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        })
    }

    #[test]
    fn resolves_named_and_default_images() {
        let scene = Scene {
            default: Some("/default.png".to_owned()),
            wallpapers: BTreeMap::from([("main".to_owned(), "/main.png".to_owned())]),
        };
        assert_eq!(resolve(&scene, Some("main")).unwrap(), "/main.png");
        assert_eq!(resolve(&scene, Some("other")).unwrap(), "/default.png");
        assert_eq!(resolve(&scene, None).unwrap(), "/default.png");
        let empty = Scene::default();
        assert!(resolve(&empty, Some("main")).is_none());
    }

    #[test]
    fn renders_all_placeholders() {
        assert_eq!(
            render_command(
                "swaybg -i {image} --output {output} --workspace {workspace}",
                "/img.png",
                "main",
                "eDP-1"
            ),
            "swaybg -i /img.png --output eDP-1 --workspace main"
        );
    }

    #[test]
    fn store_round_trips() {
        let path = temp_dir("store").join("scenes.json");
        let mut store = Store::default();
        let mut scene = Scene::default();
        scene
            .wallpapers
            .insert("main".to_owned(), "/main.png".to_owned());
        store.scenes.insert("work".to_owned(), scene);
        store.active = Some("work".to_owned());
        store.save(&path).unwrap();

        let reloaded = Store::load(&path).unwrap();
        assert_eq!(reloaded.active.as_deref(), Some("work"));
        assert_eq!(
            reloaded.active_scene().unwrap().wallpapers["main"],
            "/main.png"
        );
    }

    #[test]
    fn watcher_applies_images_and_dry_run_spawns_nothing() {
        let dir = temp_dir("watcher");
        let log = dir.join("log");
        let template = format!("printf '%s\\n' {{image}} >> {}", log.display());
        let scene = Scene {
            default: None,
            wallpapers: BTreeMap::from([
                ("main".to_owned(), "/a.png".to_owned()),
                ("web".to_owned(), "/b.png".to_owned()),
            ]),
        };

        let mut dry = Watcher::new(&template, true);
        assert_eq!(
            dry.apply(&scene, Some("main"), "eDP-1").unwrap(),
            Some("/a.png".to_owned())
        );
        dry.finish();
        assert!(!log.exists());

        // Different outputs keep independent processes; the same output would
        // replace the previous wallpaper (covered by `replaces_same_output`).
        let mut watcher = Watcher::new(&template, false);
        watcher.apply(&scene, Some("main"), "eDP-1").unwrap();
        watcher.apply(&scene, Some("web"), "HDMI-1").unwrap();
        watcher.finish();
        let contents = std::fs::read_to_string(&log).unwrap();
        assert_eq!(contents, "/a.png\n/b.png\n");
    }

    #[test]
    fn watcher_replaces_previous_process_on_same_output() {
        let dir = temp_dir("replace");
        let log = dir.join("log");
        let pids = dir.join("pids");
        let template = format!(
            "printf '%s\\n' {{image}} >> {}; printf '%s\\n' $$ >> {}; sleep 0.2",
            log.display(),
            pids.display()
        );
        let scene = Scene {
            default: None,
            wallpapers: BTreeMap::from([
                ("main".to_owned(), "/a.png".to_owned()),
                ("web".to_owned(), "/b.png".to_owned()),
            ]),
        };
        let mut watcher = Watcher::new(&template, false);
        watcher.apply(&scene, Some("main"), "eDP-1").unwrap();
        watcher.apply(&scene, Some("web"), "eDP-1").unwrap();
        // The second apply must have killed the first process: only the second
        // image is current for the output.
        assert_eq!(watcher.applied["eDP-1"], "/b.png");
        assert_eq!(watcher.children.len(), 1);
        watcher.finish();
        let lines = std::fs::read_to_string(&log).unwrap();
        assert_eq!(lines.lines().count(), 2);
        assert!(lines.ends_with("/b.png\n"));
    }

    #[test]
    fn watch_stream_applies_focused_workspace() {
        let dir = temp_dir("watch");
        let log = dir.join("log");
        let socket = dir.join("niri.sock");
        let server = event_server(&socket);
        let template = format!("printf '%s' {{image}} >> {}", log.display());
        let store = Store {
            active: Some("work".to_owned()),
            scenes: BTreeMap::from([(
                "work".to_owned(),
                Scene {
                    default: None,
                    wallpapers: BTreeMap::from([("main".to_owned(), "/main.png".to_owned())]),
                },
            )]),
        };

        let applied = watch(&socket, &store, &template, false).unwrap();
        assert_eq!(applied, 1);
        server.join().unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "/main.png");
    }

    #[test]
    fn apply_once_queries_focused_workspace() {
        let dir = temp_dir("once");
        let socket = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            assert!(matches!(request, Request::Workspaces));
            let response = json!({ "Ok": { "Workspaces": [focused_workspace()] } });
            let mut stream = stream;
            stream
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        });
        let store = Store {
            active: Some("work".to_owned()),
            scenes: BTreeMap::from([(
                "work".to_owned(),
                Scene {
                    default: Some("/default.png".to_owned()),
                    wallpapers: BTreeMap::new(),
                },
            )]),
        };
        // Dry run: verifies the IPC query + resolution without spawning a
        // wallpaper process (spawning is covered by the watcher test).
        let image = apply_once(&socket, &store, true).unwrap();
        server.join().unwrap();
        assert_eq!(image.as_deref(), Some("/default.png"));
    }

    #[test]
    fn command_executable_bit_is_preserved() {
        // Sanity check for the fake-command helpers: a script must be executable.
        let dir = temp_dir("exec");
        let script = dir.join("cmd");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let meta = std::fs::metadata(&script).unwrap();
        assert_eq!(meta.permissions().mode() & 0o111, 0o111);
    }
}
