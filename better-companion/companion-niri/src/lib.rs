//! niri IPC bridge and event-driven parking tracker.
//!
//! `compositor.*` performs actions and queries. [`Tracker`] consumes the niri
//! event stream and maintains two auto-parking lots:
//!
//! - `auto:workspaces`: every workspace, keyed by its stable id.
//! - `auto:windows`: every open window, keyed by its stable id.
//!
//! Window closes remove entries; workspace/window focus and activation update
//! them. The lots are ordinary parking lots, so `parking.list` and
//! `parking.unpark` work on them too.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use companion_core::{ActionInfo, Context};
pub use companion_core::{AUTO_WINDOWS, AUTO_WORKSPACES};
use companion_parking::Parking;
use niri_ipc::socket::{Socket, SOCKET_PATH_ENV};
use niri_ipc::{Action, Event, Reply, Request, Response, Window, Workspace};
use serde_json::{json, Value};

mod parker;
mod swallow;
mod windows;
pub use parker::{Parker, DEFAULT_LOT};
pub use swallow::{proc_parent, Swallow, SWALLOW_LOT};
pub use windows::Windows;

pub struct Compositor {
    socket: Option<PathBuf>,
}

impl Default for Compositor {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Compositor {
    /// Uses `$NIRI_SOCKET` at call time.
    pub fn from_env() -> Self {
        Self { socket: None }
    }

    /// Uses an explicit socket path (useful for tests and nested sessions).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            socket: Some(path.into()),
        }
    }

    fn connect(&self) -> Result<Socket, String> {
        match &self.socket {
            Some(path) => Socket::connect_to(path),
            None => Socket::connect(),
        }
        .map_err(|err| format!("cannot connect to niri IPC: {err}"))
    }

    fn send(&self, request: Request) -> Result<Response, String> {
        let reply: Reply = self
            .connect()?
            .send(request)
            .map_err(|err| format!("niri IPC error: {err}"))?;
        reply.map_err(|err| format!("niri returned an error: {err}"))
    }

    fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
        serde_json::to_value(value).map_err(|err| format!("cannot encode niri response: {err}"))
    }
}

fn parse_action(params: &Value) -> Result<Action, String> {
    let action = params
        .get("action")
        .ok_or_else(|| "compositor.action needs an `action` object".to_owned())?;
    serde_json::from_value(action.clone()).map_err(|err| format!("invalid niri action JSON: {err}"))
}

impl Context for Compositor {
    fn namespace(&self) -> &'static str {
        "compositor"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("action", "Perform a niri `Action` given as `action` JSON"),
            ActionInfo::new("outputs", "List connected outputs"),
            ActionInfo::new("workspaces", "List workspaces"),
            ActionInfo::new("windows", "List windows"),
            ActionInfo::new("focused-window", "Focused window, if any"),
            ActionInfo::new("version", "Running niri version"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "action" => {
                let action = parse_action(params)?;
                match self.send(Request::Action(action))? {
                    Response::Handled => Ok(json!({ "handled": true })),
                    other => Self::to_value(other),
                }
            }
            "outputs" => match self.send(Request::Outputs)? {
                Response::Outputs(outputs) => Self::to_value(outputs),
                other => Self::to_value(other),
            },
            "workspaces" => match self.send(Request::Workspaces)? {
                Response::Workspaces(workspaces) => Self::to_value(workspaces),
                other => Self::to_value(other),
            },
            "windows" => match self.send(Request::Windows)? {
                Response::Windows(windows) => Self::to_value(windows),
                other => Self::to_value(other),
            },
            "focused-window" => match self.send(Request::FocusedWindow)? {
                Response::FocusedWindow(window) => Self::to_value(window),
                other => Self::to_value(other),
            },
            "version" => match self.send(Request::Version)? {
                Response::Version(version) => Ok(json!(version)),
                other => Self::to_value(other),
            },
            other => Err(format!("unknown action `compositor.{other}`")),
        }
    }
}

/// Cached compositor state maintained from the event stream.
#[derive(Default)]
pub struct Tracker {
    workspaces: BTreeMap<u64, Workspace>,
    windows: BTreeMap<u64, Window>,
    swallow: Option<Swallow>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_swallow(swallow: Swallow) -> Self {
        Self {
            swallow: Some(swallow),
            ..Self::default()
        }
    }

    /// Applies one event and rewrites the auto-parking lots.
    pub fn apply_event(&mut self, parking: &Parking, event: &Event) -> Result<(), String> {
        match event {
            Event::WorkspacesChanged { workspaces } => {
                self.workspaces = workspaces.iter().map(|ws| (ws.id, ws.clone())).collect();
            }
            Event::WorkspaceActivated { id, focused } => {
                let output = self.workspaces.get(id).and_then(|ws| ws.output.clone());
                for ws in self.workspaces.values_mut() {
                    if ws.id == *id {
                        ws.is_active = true;
                        ws.is_focused = *focused;
                    } else {
                        if *focused {
                            ws.is_focused = false;
                        }
                        if ws.output == output {
                            ws.is_active = false;
                        }
                    }
                }
            }
            Event::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            } => {
                if let Some(ws) = self.workspaces.get_mut(workspace_id) {
                    ws.active_window_id = *active_window_id;
                }
            }
            Event::WorkspaceUrgencyChanged { id, urgent } => {
                if let Some(ws) = self.workspaces.get_mut(id) {
                    ws.is_urgent = *urgent;
                }
            }
            Event::WindowsChanged { windows } => {
                self.windows = windows.iter().map(|win| (win.id, win.clone())).collect();
            }
            Event::WindowOpenedOrChanged { window } => {
                if window.is_focused {
                    for other in self.windows.values_mut() {
                        other.is_focused = false;
                    }
                }
                self.windows.insert(window.id, window.clone());
            }
            Event::WindowClosed { id } => {
                self.windows.remove(id);
                for ws in self.workspaces.values_mut() {
                    if ws.active_window_id == Some(*id) {
                        ws.active_window_id = None;
                    }
                }
            }
            Event::WindowFocusChanged { id: Some(id) } => {
                for win in self.windows.values_mut() {
                    win.is_focused = win.id == *id;
                }
            }
            Event::WindowFocusChanged { id: None } => {
                for win in self.windows.values_mut() {
                    win.is_focused = false;
                }
            }
            Event::WindowUrgencyChanged { id, urgent } => {
                if let Some(win) = self.windows.get_mut(id) {
                    win.is_urgent = *urgent;
                }
            }
            Event::WindowMinimizedChanged { id, is_minimized } => {
                if let Some(win) = self.windows.get_mut(id) {
                    win.is_minimized = *is_minimized;
                }
            }
            _ => return Ok(()),
        }

        self.snapshot(parking)?;

        if let Some(swallow) = &self.swallow {
            let result = match event {
                Event::WindowOpenedOrChanged { window } => swallow.maybe_swallow(window),
                Event::WindowClosed { id } => swallow.on_window_closed(*id),
                _ => Ok(()),
            };
            if let Err(err) = result {
                eprintln!("better-companion: swallow error: {err}");
            }
        }

        Ok(())
    }

    fn snapshot(&self, parking: &Parking) -> Result<(), String> {
        let workspaces = self
            .workspaces
            .iter()
            .map(|(id, ws)| Ok((id.to_string(), serde_json::to_value(ws).map_err(encode)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let windows = self
            .windows
            .iter()
            .map(|(id, win)| Ok((id.to_string(), serde_json::to_value(win).map_err(encode)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        parking.replace_lot(AUTO_WORKSPACES, workspaces)?;
        parking.replace_lot(AUTO_WINDOWS, windows)
    }
}

fn encode(err: serde_json::Error) -> String {
    format!("cannot encode niri state: {err}")
}

/// `$NIRI_SOCKET`, if the daemon is running inside a niri session.
pub fn socket_from_env() -> Option<PathBuf> {
    std::env::var_os(SOCKET_PATH_ENV).map(PathBuf::from)
}

/// Subscribes to the event stream and applies events until the socket closes.
///
/// The tracker is kept across calls, so a daemon reconnecting after a niri
/// restart keeps the last known workspace/window state.
pub fn track_stream(path: &Path, parking: &Parking, tracker: &mut Tracker) -> io::Result<usize> {
    let mut socket = Socket::connect_to(path)?;
    match socket.send(Request::EventStream)? {
        Ok(Response::Handled) => (),
        Ok(other) => {
            return Err(io::Error::other(format!(
                "unexpected event-stream response: {other:?}"
            )))
        }
        Err(message) => return Err(io::Error::other(message)),
    }

    let mut read_event = socket.read_events();
    let mut applied = 0;
    loop {
        let event = match read_event() {
            Ok(event) => event,
            Err(err)
                if err.kind() == io::ErrorKind::UnexpectedEof
                    || err.kind() == io::ErrorKind::ConnectionReset =>
            {
                return Ok(applied);
            }
            Err(err) => return Err(err),
        };
        if let Err(message) = tracker.apply_event(parking, &event) {
            eprintln!("better-companion: event tracking error: {message}");
        } else {
            applied += 1;
        }
    }
}

/// Reconnecting loop used by the daemon.
pub fn run_tracker(path: PathBuf, parking: Parking, swallow: Swallow) {
    let mut tracker = Tracker::with_swallow(swallow);
    loop {
        if let Err(err) = track_stream(&path, &parking, &mut tracker) {
            eprintln!("better-companion: event stream {}: {err}", path.display());
        }
        sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::mpsc;

    use serde_json::json;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-tracker-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn workspace(id: u64, output: &str, active: bool, focused: bool) -> Workspace {
        serde_json::from_value(json!({
            "id": id,
            "idx": id,
            "name": format!("ws-{id}"),
            "output": output,
            "is_urgent": false,
            "is_active": active,
            "is_focused": focused,
            "active_window_id": null,
            "is_hidden": false
        }))
        .unwrap()
    }

    fn window(id: u64, workspace_id: u64, focused: bool) -> Window {
        serde_json::from_value(json!({
            "id": id,
            "title": format!("win-{id}"),
            "app_id": "test",
            "pid": 1,
            "workspace_id": workspace_id,
            "is_focused": focused,
            "is_floating": false,
            "is_sticky": false,
            "is_urgent": false,
            "is_minimized": false,
            "layout": {
                "pos_in_scrolling_layout": [1, 1],
                "tile_size": [100.0, 100.0],
                "window_size": [100, 100],
                "tile_pos_in_workspace_view": [0.0, 0.0],
                "window_offset_in_tile": [0.0, 0.0]
            },
            "focus_timestamp": null
        }))
        .unwrap()
    }

    fn window_full(
        id: u64,
        workspace_id: u64,
        focused: bool,
        pid: i32,
        app_id: &str,
        minimized: bool,
    ) -> Window {
        serde_json::from_value(json!({
            "id": id,
            "title": format!("win-{id}"),
            "app_id": app_id,
            "pid": pid,
            "workspace_id": workspace_id,
            "is_focused": focused,
            "is_floating": false,
            "is_sticky": false,
            "is_urgent": false,
            "is_minimized": minimized,
            "layout": {
                "pos_in_scrolling_layout": [1, 1],
                "tile_size": [100.0, 100.0],
                "window_size": [100, 100],
                "tile_pos_in_workspace_view": [0.0, 0.0],
                "window_offset_in_tile": [0.0, 0.0]
            },
            "focus_timestamp": null
        }))
        .unwrap()
    }

    /// Accepts `expected` one-request connections and records the requests.
    fn fake_actions(path: &PathBuf, expected: usize) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..expected {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                requests.push(serde_json::from_str(line.trim()).unwrap());
                let mut stream = stream;
                stream.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        })
    }

    fn parking(name: &str) -> Parking {
        Parking::load(temp_path(name).join("parking.json")).unwrap()
    }

    #[test]
    fn tracks_workspaces_windows_focus_and_closes() {
        let parking = parking("events");
        let mut tracker = Tracker::new();

        tracker
            .apply_event(
                &parking,
                &Event::WorkspacesChanged {
                    workspaces: vec![
                        workspace(1, "eDP-1", true, true),
                        workspace(2, "eDP-1", false, false),
                    ],
                },
            )
            .unwrap();
        tracker
            .apply_event(
                &parking,
                &Event::WindowOpenedOrChanged {
                    window: window(10, 1, true),
                },
            )
            .unwrap();

        let workspaces = parking.lot(AUTO_WORKSPACES);
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces["1"]["output"], "eDP-1");
        let windows = parking.lot(AUTO_WINDOWS);
        assert_eq!(windows["10"]["title"], "win-10");
        assert_eq!(windows["10"]["is_focused"], true);

        // Activating workspace 2 focuses it and clears the other focus.
        tracker
            .apply_event(
                &parking,
                &Event::WorkspaceActivated {
                    id: 2,
                    focused: true,
                },
            )
            .unwrap();
        let workspaces = parking.lot(AUTO_WORKSPACES);
        assert_eq!(workspaces["2"]["is_active"], true);
        assert_eq!(workspaces["2"]["is_focused"], true);
        assert_eq!(workspaces["1"]["is_focused"], false);

        // Moving focus to window 11 clears window 10's focus.
        tracker
            .apply_event(
                &parking,
                &Event::WindowOpenedOrChanged {
                    window: window(11, 2, false),
                },
            )
            .unwrap();
        tracker
            .apply_event(&parking, &Event::WindowFocusChanged { id: Some(11) })
            .unwrap();
        let windows = parking.lot(AUTO_WINDOWS);
        assert_eq!(windows["10"]["is_focused"], false);
        assert_eq!(windows["11"]["is_focused"], true);

        // Closing window 10 removes it and clears workspace active_window_id.
        tracker
            .apply_event(&parking, &Event::WindowClosed { id: 10 })
            .unwrap();
        let windows = parking.lot(AUTO_WINDOWS);
        assert!(!windows.contains_key("10"));
        assert!(windows.contains_key("11"));
    }

    #[test]
    fn rejects_nothing_and_ignores_unknown_events() {
        let parking = parking("ignore");
        let mut tracker = Tracker::new();
        tracker
            .apply_event(&parking, &Event::WindowClosed { id: 999 })
            .unwrap();
        assert!(parking.lot(AUTO_WINDOWS).is_empty());
    }

    #[test]
    fn streams_events_from_a_socket() {
        let dir = temp_path("stream");
        let socket_path = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();

        let server = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            assert!(matches!(request, Request::EventStream));

            let mut stream = stream;
            stream.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
            stream
                .write_all(
                    serde_json::to_string(&Event::WorkspacesChanged {
                        workspaces: vec![workspace(7, "HDMI-1", true, true)],
                    })
                    .unwrap()
                    .as_bytes(),
                )
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream
                .write_all(
                    serde_json::to_string(&Event::WindowOpenedOrChanged {
                        window: window(70, 7, true),
                    })
                    .unwrap()
                    .as_bytes(),
                )
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        });

        ready_rx.recv().unwrap();
        let parking = parking("stream-parking");
        let mut tracker = Tracker::new();
        let applied = track_stream(&socket_path, &parking, &mut tracker).unwrap();
        assert_eq!(applied, 2);
        server.join().unwrap();

        assert_eq!(parking.lot(AUTO_WORKSPACES)["7"]["output"], "HDMI-1");
        assert_eq!(parking.lot(AUTO_WINDOWS)["70"]["id"], 70);
    }

    #[test]
    fn parses_action_json() {
        let action = parse_action(&json!({ "action": { "FocusColumnRight": {} } })).unwrap();
        assert!(matches!(action, Action::FocusColumnRight {}));
        assert!(parse_action(&json!({})).is_err());
        assert!(parse_action(&json!({ "action": "nope" })).is_err());
    }

    #[test]
    fn compositor_queries_the_socket() {
        let dir = temp_path("version");
        let socket_path = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            assert!(matches!(request, Request::Version));
            let mut stream = stream;
            stream
                .write_all(b"{\"Ok\":{\"Version\":\"26.4-test\"}}\n")
                .unwrap();
            stream.flush().unwrap();
        });
        let compositor = Compositor::at(&socket_path);
        let value = compositor.call("version", &Value::Null).unwrap();
        assert_eq!(value, json!("26.4-test"));
        server.join().unwrap();
    }

    #[test]
    fn compositor_performs_actions() {
        let dir = temp_path("action");
        let socket_path = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            assert!(matches!(
                request,
                Request::Action(Action::FocusColumnRight {})
            ));
            let mut stream = stream;
            stream.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
            stream.flush().unwrap();
        });
        let compositor = Compositor::at(&socket_path);
        let value = compositor
            .call("action", &json!({ "action": { "FocusColumnRight": {} } }))
            .unwrap();
        assert_eq!(value["handled"], true);
        server.join().unwrap();
    }

    #[test]
    fn swallowed_terminal_is_minimized_and_restored() {
        use std::sync::Arc;

        let dir = temp_path("swallow");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 2);
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        let parent: Arc<dyn Fn(i32) -> Option<i32> + Send + Sync> = Arc::new(|pid| match pid {
            200 => Some(100),
            100 => Some(1),
            _ => None,
        });
        let swallow = Swallow::load_with_parent(
            dir.join("swallow.json"),
            parking.clone(),
            Compositor::at(&socket),
            parent,
        )
        .unwrap();
        swallow.call("enable", &serde_json::Value::Null).unwrap();

        let mut tracker = Tracker::with_swallow(swallow);
        tracker
            .apply_event(
                &parking,
                &Event::WindowsChanged {
                    windows: vec![window_full(1, 1, false, 100, "alacritty", false)],
                },
            )
            .unwrap();
        tracker
            .apply_event(
                &parking,
                &Event::WindowOpenedOrChanged {
                    window: window_full(2, 1, false, 200, "myapp", false),
                },
            )
            .unwrap();

        let entries = parking.lot(super::SWALLOW_LOT);
        assert_eq!(entries["child:2"]["parent"], 1);

        tracker
            .apply_event(&parking, &Event::WindowClosed { id: 2 })
            .unwrap();
        assert!(parking.lot(super::SWALLOW_LOT).is_empty());

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(1) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::UnminimizeWindow { id: Some(1) })
        ));
    }
}
