//! niri IPC bridge and event-driven parking tracker.
//!
//! `compositor.*` performs actions and queries. `windows.*` adds run-or-raise,
//! placement (workspace by id/name/index, output by name) and the native sticky
//! flag. `marks.*` persists one unique mark per window id, exposes it in
//! `windows.find`/`list-sticky`, and accepts marks as selectors.
//! `stage.*` and `scratchpad.*` absorb the nsticky staged-window workflow
//! on top of the parking store, and `nsticky.*` offers compatibility aliases for
//! the old CLI verbs. [`Tracker`] consumes the niri event stream and maintains
//! two auto-parking lots:
//!
//! - `auto:workspaces`: every workspace, keyed by its stable id.
//! - `auto:windows`: every open window, keyed by its stable id.
//!
//! Window closes remove entries; workspace/window focus and activation update
//! them. Each new event-stream connection starts from niri's full snapshot, and
//! the window cache is bounded, so a long-lived daemon cannot accumulate stale
//! state. Malformed event lines are skipped instead of ending the loop. The
//! lots are ordinary parking lots, so `parking.list` and `parking.unpark` work
//! on them too.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

pub use companion_core::{AUTO_WINDOWS, AUTO_WORKSPACES};
use companion_core::{ActionInfo, Context, ParamInfo};
use companion_parking::Parking;
use niri_ipc::socket::{SOCKET_PATH_ENV, Socket};
use niri_ipc::{Action, Event, Reply, Request, Response, Window, Workspace};
use serde_json::{Value, json};

mod layout;
mod marks;
mod nsticky;
mod ops;
mod parker;
mod scratchpad;
mod stage;
mod swallow;
mod windows;
pub use layout::{WorkspaceTarget, resolve_live_workspace};
pub use marks::Marks;
pub use nsticky::Nsticky;
pub use parker::{DEFAULT_LOT, Parker};
pub use scratchpad::{SCRATCHPAD_LOT, Scratchpad};
pub use stage::{STAGE_LOT, Stage};
pub use swallow::{SWALLOW_LOT, Swallow, SwallowOutcome, SwallowRule, proc_age, proc_parent};
pub use windows::Windows;

/// The niri IPC client. Cheap to clone; clones share no mutable state.
#[derive(Clone)]
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

    /// Whether a niri IPC socket is currently reachable/configured.
    ///
    /// Action metadata uses this to mark compositor-dependent actions
    /// unavailable when the daemon was started outside a niri session. The
    /// check is cheap (no connection is opened).
    pub fn is_available(&self) -> bool {
        match &self.socket {
            Some(path) => path.exists(),
            None => socket_from_env().is_some_and(|path| path.exists()),
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

    /// Sends one raw IPC request.
    ///
    /// Output configuration (`Request::Output`) is not an `Action`, so contexts
    /// such as `idle.*` and `rotate.*` need a request-level entry point instead
    /// of `compositor.action`.
    pub fn request(&self, request: Request) -> Result<Response, String> {
        self.send(request)
    }

    /// Queries the compositor's capability report.
    ///
    /// Returns an error on a niri build that predates `Request::Capabilities`; callers should
    /// treat that as "no optional features" rather than failing outright.
    pub fn capabilities(&self) -> Result<niri_ipc::Capabilities, String> {
        match self.send(Request::Capabilities)? {
            Response::Capabilities(capabilities) => Ok(capabilities),
            other => Err(format!("unexpected response to Capabilities: {other:?}")),
        }
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
            ActionInfo::new("action", "Perform a niri `Action` given as `action` JSON")
                .with_params([ParamInfo::object("action", true, "niri action JSON")])
                .with_permissions(&["niri"]),
            ActionInfo::new("outputs", "List connected outputs").with_permissions(&["niri"]),
            ActionInfo::new("workspaces", "List workspaces").with_permissions(&["niri"]),
            ActionInfo::new("windows", "List windows").with_permissions(&["niri"]),
            ActionInfo::new("focused-window", "Focused window, if any").with_permissions(&["niri"]),
            ActionInfo::new("version", "Running niri version").with_permissions(&["niri"]),
            ActionInfo::new(
                "capabilities",
                "Features the running compositor supports (sticky, outputs, shaders, HDR, ...)",
            )
            .with_permissions(&["niri"]),
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
            "capabilities" => match self.send(Request::Capabilities)? {
                Response::Capabilities(capabilities) => Self::to_value(capabilities),
                other => Self::to_value(other),
            },
            other => Err(format!("unknown action `compositor.{other}`")),
        }
    }
}

/// Upper bound on cached windows. A reconnect or a lost close event must not
/// let a long-lived daemon grow this map without bound.
pub const MAX_TRACKED_WINDOWS: usize = 4096;

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

    /// Clears cached state when a new event-stream connection is established.
    ///
    /// niri sends a complete `WorkspacesChanged`/`WindowsChanged` snapshot on
    /// subscribe, so dropping the previous connection's cache prevents windows
    /// that vanished while disconnected from accumulating forever.
    pub fn begin_stream(&mut self, parking: &Parking) -> Result<(), String> {
        self.workspaces.clear();
        self.windows.clear();
        self.snapshot(parking)
    }

    /// Applies one event and rewrites the auto-parking lots.
    pub fn apply_event(&mut self, parking: &Parking, event: &Event) -> Result<(), String> {
        let previous: BTreeSet<u64> = self.windows.keys().copied().collect();

        // Windows that were not known before this event. Swallowing only runs
        // for these, so title/focus/minimize updates cannot re-trigger it.
        // `WindowsChanged` is authoritative; duplicate ids in a malformed or
        // replayed snapshot collapse to one entry.
        let opened: Vec<Window> = match event {
            Event::WindowsChanged { windows } => {
                let mut unique: BTreeMap<u64, Window> = BTreeMap::new();
                for window in windows {
                    unique.insert(window.id, window.clone());
                }
                unique
                    .into_values()
                    .filter(|window| !previous.contains(&window.id))
                    .collect()
            }
            Event::WindowOpenedOrChanged { window } => (!previous.contains(&window.id))
                .then(|| window.clone())
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };

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
                let mut unique: BTreeMap<u64, Window> = BTreeMap::new();
                for window in windows {
                    unique.insert(window.id, window.clone());
                }
                self.windows = unique;
                self.enforce_window_limit(MAX_TRACKED_WINDOWS);
            }
            Event::WindowOpenedOrChanged { window } => {
                if window.is_focused {
                    for other in self.windows.values_mut() {
                        other.is_focused = false;
                    }
                }
                self.windows.insert(window.id, window.clone());
                self.enforce_window_limit(MAX_TRACKED_WINDOWS);
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
            let result = (|| -> Result<(), String> {
                match event {
                    Event::WindowClosed { id } => {
                        // Closing the child restores the terminal; closing the
                        // terminal drops any pairs that pointed at it.
                        swallow.on_window_closed(*id)?;
                        swallow.on_parent_closed(*id)?;
                    }
                    Event::WindowsChanged { .. } => {
                        for window in &opened {
                            swallow.maybe_swallow(window)?;
                        }
                        // Pairs persisted before a daemon restart converge with
                        // the compositor's current window set.
                        swallow.reconcile()?;
                    }
                    Event::WindowOpenedOrChanged { window } => {
                        // Keep pair metadata in sync when the child or the
                        // (minimized) terminal moves to another workspace or
                        // output while the pair is live.
                        swallow.track_window(window)?;
                        for window in &opened {
                            swallow.maybe_swallow(window)?;
                        }
                    }
                    _ => {}
                }
                Ok(())
            })();
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

    /// Evicts the lowest window ids when the cache exceeds `limit`.
    ///
    /// niri ids grow monotonically in practice, so the lowest ids are the
    /// oldest entries. Kept separate from insertion for a deterministic test
    /// with a tiny limit.
    fn enforce_window_limit(&mut self, limit: usize) {
        while self.windows.len() > limit {
            let Some(oldest) = self.windows.keys().next().copied() else {
                break;
            };
            self.windows.remove(&oldest);
        }
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
/// restart starts each connection from the full snapshot niri sends instead of
/// accumulating windows that disappeared while disconnected.
///
/// The event loop is deliberately tolerant: malformed JSON, unknown event
/// kinds and truncated final lines are logged and skipped rather than tearing
/// the connection down. Only transport errors end the loop.
pub fn track_stream(path: &Path, parking: &Parking, tracker: &mut Tracker) -> io::Result<usize> {
    track_stream_with(path, parking, tracker, |_| {})
}

/// Subscribes to the event stream and forwards every decoded event to
/// `on_event` after the tracker has applied it.
///
/// Adapters use this to observe the same connection the tracker consumes
/// instead of opening a rival subscription. A failing observer must not stop
/// tracking, so it runs after the snapshot update and its own errors are the
/// caller's responsibility.
pub fn track_stream_with<F>(
    path: &Path,
    parking: &Parking,
    tracker: &mut Tracker,
    mut on_event: F,
) -> io::Result<usize>
where
    F: FnMut(&Event),
{
    let stream = UnixStream::connect(path)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let mut request = serde_json::to_string(&Request::EventStream)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    request.push('\n');
    writer.write_all(request.as_bytes())?;
    writer.flush()?;

    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "event-stream handshake closed",
        ));
    }
    let reply: Reply = serde_json::from_str(line.trim())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    match reply {
        Ok(Response::Handled) => (),
        Ok(other) => {
            return Err(io::Error::other(format!(
                "unexpected event-stream response: {other:?}"
            )));
        }
        Err(message) => return Err(io::Error::other(message)),
    }
    let _ = writer.shutdown(Shutdown::Write);

    tracker.begin_stream(parking).map_err(io::Error::other)?;

    let mut applied = 0;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(applied),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err)
                if err.kind() == io::ErrorKind::UnexpectedEof
                    || err.kind() == io::ErrorKind::ConnectionReset =>
            {
                return Ok(applied);
            }
            Err(err) => return Err(err),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event = match serde_json::from_str::<Event>(trimmed) {
            Ok(event) => event,
            Err(err) => {
                eprintln!("better-companion: skipping malformed event: {err}");
                continue;
            }
        };
        match tracker.apply_event(parking, &event) {
            Ok(()) => applied += 1,
            Err(message) => eprintln!("better-companion: event tracking error: {message}"),
        }
        on_event(&event);
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

/// Reconnecting loop with per-connection reset and event observation.
///
/// `on_connect` runs before each subscription attempt (for example to reset an
/// event mapper, because niri replays a full snapshot) and `on_event` receives
/// every decoded event after the tracker applied it.
pub fn run_tracker_with<F, C>(
    path: PathBuf,
    parking: Parking,
    swallow: Swallow,
    mut on_connect: C,
    mut on_event: F,
) where
    F: FnMut(&Event),
    C: FnMut(),
{
    let mut tracker = Tracker::with_swallow(swallow);
    loop {
        on_connect();
        if let Err(err) = track_stream_with(&path, &parking, &mut tracker, &mut on_event) {
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

    #[test]
    fn swallows_windows_from_the_initial_snapshot() {
        use std::sync::Arc;

        let dir = temp_path("swallow-initial");
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

        // The daemon can start after the terminal and its child already exist.
        tracker
            .apply_event(
                &parking,
                &Event::WindowsChanged {
                    windows: vec![
                        window_full(1, 1, false, 100, "alacritty", false),
                        window_full(2, 1, false, 200, "myapp", false),
                    ],
                },
            )
            .unwrap();
        assert_eq!(parking.lot(super::SWALLOW_LOT)["child:2"]["parent"], 1);

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

    #[test]
    fn initial_snapshot_reconciles_pairs_whose_child_is_gone() {
        use std::sync::Arc;

        let dir = temp_path("swallow-reconcile");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 1);
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        parking
            .call(
                "park",
                &serde_json::json!({
                    "lot": super::SWALLOW_LOT,
                    "id": "child:2",
                    "value": { "child": 2, "parent": 1, "restore_on_exit": true }
                }),
            )
            .unwrap();
        let parent: Arc<dyn Fn(i32) -> Option<i32> + Send + Sync> = Arc::new(|_| Some(1));
        let swallow = Swallow::load_with_parent(
            dir.join("swallow.json"),
            parking.clone(),
            Compositor::at(&socket),
            parent,
        )
        .unwrap();
        let mut tracker = Tracker::with_swallow(swallow);

        // Only the minimized terminal survived the restart.
        tracker
            .apply_event(
                &parking,
                &Event::WindowsChanged {
                    windows: vec![window_full(1, 1, false, 100, "alacritty", true)],
                },
            )
            .unwrap();
        assert!(parking.lot(super::SWALLOW_LOT).is_empty());

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::UnminimizeWindow { id: Some(1) })
        ));
    }

    #[test]
    fn swallow_pairs_survive_cross_workspace_child_and_parent_move() {
        use std::sync::Arc;

        let dir = temp_path("swallow-cross-workspace");
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
                &Event::WorkspacesChanged {
                    workspaces: vec![
                        workspace(1, "eDP-1", true, true),
                        workspace(2, "HDMI-1", false, false),
                        workspace(3, "HDMI-1", false, false),
                    ],
                },
            )
            .unwrap();
        tracker
            .apply_event(
                &parking,
                &Event::WindowsChanged {
                    windows: vec![window_full(1, 1, false, 100, "alacritty", false)],
                },
            )
            .unwrap();
        // The child opens on a different workspace and output.
        tracker
            .apply_event(
                &parking,
                &Event::WindowOpenedOrChanged {
                    window: window_full(2, 2, false, 200, "editor", false),
                },
            )
            .unwrap();
        let entry = &parking.lot(super::SWALLOW_LOT)["child:2"];
        assert_eq!(entry["child_output"], "HDMI-1");
        assert_eq!(entry["parent_output"], "eDP-1");

        // The minimized terminal moves; the pair follows its new context.
        tracker
            .apply_event(
                &parking,
                &Event::WindowOpenedOrChanged {
                    window: window_full(1, 3, false, 100, "alacritty", true),
                },
            )
            .unwrap();
        let entry = &parking.lot(super::SWALLOW_LOT)["child:2"];
        assert_eq!(entry["parent_workspace_id"], 3);
        assert_eq!(entry["parent_output"], "HDMI-1");

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

    #[test]
    fn duplicate_window_ids_in_a_snapshot_swallow_once() {
        use std::sync::Arc;

        let dir = temp_path("swallow-duplicates");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 1);
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
                    windows: vec![
                        window_full(1, 1, false, 100, "alacritty", false),
                        window_full(1, 1, false, 100, "alacritty", false),
                        window_full(2, 1, false, 200, "editor", false),
                        window_full(2, 1, false, 200, "editor", false),
                    ],
                },
            )
            .unwrap();
        assert_eq!(parking.lot(super::SWALLOW_LOT).len(), 1);
        // One minimize for the one terminal, despite the duplicate ids.
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn window_cache_limit_evicts_oldest_ids() {
        let parking = parking("limit");
        let mut tracker = Tracker::new();
        tracker
            .apply_event(
                &parking,
                &Event::WindowsChanged {
                    windows: vec![
                        window(10, 1, false),
                        window(20, 1, false),
                        window(30, 1, false),
                    ],
                },
            )
            .unwrap();
        tracker.enforce_window_limit(2);
        tracker.snapshot(&parking).unwrap();
        let windows = parking.lot(AUTO_WINDOWS);
        assert_eq!(
            windows.keys().cloned().collect::<Vec<_>>(),
            vec!["20", "30"]
        );
    }

    #[test]
    fn begin_stream_drops_state_from_the_previous_connection() {
        let parking = parking("begin-stream");
        let mut tracker = Tracker::new();
        tracker
            .apply_event(
                &parking,
                &Event::WindowsChanged {
                    windows: vec![window(10, 1, false)],
                },
            )
            .unwrap();
        assert!(parking.lot(AUTO_WINDOWS).contains_key("10"));

        tracker.begin_stream(&parking).unwrap();
        assert!(parking.lot(AUTO_WINDOWS).is_empty());
        assert!(parking.lot(AUTO_WORKSPACES).is_empty());
    }

    /// A server that completes the event-stream handshake and then writes
    /// `body` verbatim before closing the connection.
    fn fake_event_stream(path: &PathBuf, body: String) -> std::thread::JoinHandle<()> {
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
            stream.write_all(body.as_bytes()).unwrap();
            stream.flush().unwrap();
        })
    }

    #[test]
    fn event_stream_skips_malformed_and_unknown_lines() {
        let dir = temp_path("stream-malformed");
        let socket_path = dir.join("niri.sock");
        let event = serde_json::to_string(&Event::WorkspacesChanged {
            workspaces: vec![workspace(7, "HDMI-1", true, true)],
        })
        .unwrap();
        let body = format!("this is not json\n{{\"NoSuchEvent\":{{}}}}\n{event}\n");
        let server = fake_event_stream(&socket_path, body);
        let parking = parking("stream-malformed-parking");
        let mut tracker = Tracker::new();
        let applied = track_stream(&socket_path, &parking, &mut tracker).unwrap();
        assert_eq!(applied, 1);
        server.join().unwrap();
        assert_eq!(parking.lot(AUTO_WORKSPACES)["7"]["output"], "HDMI-1");
    }

    #[test]
    fn event_stream_handles_a_truncated_final_line() {
        let dir = temp_path("stream-truncated");
        let socket_path = dir.join("niri.sock");
        let event = serde_json::to_string(&Event::WindowOpenedOrChanged {
            window: window(70, 7, true),
        })
        .unwrap();
        let body = format!("{event}\n{{\"WindowClosed\":");
        let server = fake_event_stream(&socket_path, body);
        let parking = parking("stream-truncated-parking");
        let mut tracker = Tracker::new();
        let applied = track_stream(&socket_path, &parking, &mut tracker).unwrap();
        // The complete event is applied; the truncated line is skipped and the
        // connection still ends cleanly.
        assert_eq!(applied, 1);
        server.join().unwrap();
        assert!(parking.lot(AUTO_WINDOWS).contains_key("70"));
    }

    #[test]
    fn queries_and_reports_compositor_capabilities() {
        let dir = temp_path("capabilities");
        let socket_path = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                requests.push(serde_json::from_str::<Request>(line.trim()).unwrap());
                let response = json!({ "Ok": { "Capabilities": {
                    "sticky": true,
                    "virtual_outputs": true,
                    "isolated_outputs": true,
                    "hidden_workspaces": true,
                    "window_minimize": true,
                    "grid_overview": true,
                    "magnifier": true,
                    "carousel": true,
                    "vertical_axis": true,
                    "multi_action_binds": true,
                    "per_keyboard_config": true,
                    "float_above_fullscreen": true,
                    "svg_cursors": true,
                    "shake_to_enlarge": true,
                    "shaders": true,
                    "decoration_light": true,
                    "liquid_glass": true,
                    "hdr": true,
                    "vulkan": true,
                    "tearing": true,
                    "fifo": true,
                    "explicit_sync": true,
                    "pointer_warp": true,
                    "async_flips": true,
                    "screencopy": true,
                    "ext_image_copy_capture": true,
                    "screencast": true,
                    "screenshot": true,
                    "pick_color": true,
                    "agent_seat": false,
                    "kde_blur": true,
                    "kde_screenshot": true,
                    "kde_output_order": true,
                    "xdp_gnome_screencast": true,
                    "portal_screencast_selection": true
                } } });
                let mut stream = stream;
                stream
                    .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                    .unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        });

        let compositor = Compositor::at(&socket_path);
        let capabilities = compositor.capabilities().unwrap();
        assert!(capabilities.sticky);
        assert!(capabilities.vulkan);
        assert!(!capabilities.agent_seat);

        let value = Context::call(&compositor, "capabilities", &json!({})).unwrap();
        assert_eq!(value["hdr"], true);
        assert_eq!(value["agent_seat"], false);

        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Capabilities));
        assert!(matches!(requests[1], Request::Capabilities));
    }
}
