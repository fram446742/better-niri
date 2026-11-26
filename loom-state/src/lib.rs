//! Event-driven cache of niri state for the Loom tools.
//!
//! One background thread owns the niri event stream and one request socket.
//! Callers get a snapshot and a change signal; they never poll the compositor.
//! Reconnection is automatic, with the same backoff used by the other tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use niri_ipc::socket::Socket;
use niri_ipc::{Event, Output, Request, Response, Window, Workspace};

const RETRY: Duration = Duration::from_secs(1);

/// A snapshot of the compositor state the tools need.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub outputs: BTreeMap<String, Output>,
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    pub overview_open: bool,
}

impl State {
    /// The focused workspace, if any.
    pub fn focused_workspace(&self) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.is_focused)
    }

    /// The focused window, if any.
    pub fn focused_window(&self) -> Option<&Window> {
        self.windows.iter().find(|window| window.is_focused)
    }

    /// Windows on one workspace, in numeric id order.
    pub fn windows_on(&self, workspace_id: u64) -> Vec<&Window> {
        let mut windows: Vec<_> = self
            .windows
            .iter()
            .filter(|window| window.workspace_id == Some(workspace_id))
            .collect();
        windows.sort_by_key(|window| window.id);
        windows
    }

    /// Workspaces on one output, in niri index order.
    pub fn workspaces_on(&self, output: &str) -> Vec<&Workspace> {
        let mut workspaces: Vec<_> = self
            .workspaces
            .iter()
            .filter(|workspace| workspace.output.as_deref() == Some(output))
            .collect();
        workspaces.sort_by_key(|workspace| workspace.idx);
        workspaces
    }
}

/// A live view of niri state.
pub struct StateClient {
    state: Arc<RwLock<State>>,
    changes: Receiver<()>,
    connected: Arc<AtomicBool>,
}

impl StateClient {
    /// Connects to the socket selected by `NIRI_SOCKET` / `LOOM_NIRI_SOCKET`.
    pub fn connect() -> Result<Self, String> {
        let socket = std::env::var_os(niri_ipc::socket::SOCKET_PATH_ENV)
            .or_else(|| std::env::var_os(niri_ipc::socket::LOOM_SOCKET_PATH_ENV))
            .ok_or_else(|| "neither NIRI_SOCKET nor LOOM_NIRI_SOCKET is set".to_owned())?;
        Ok(Self::connect_to(PathBuf::from(socket)))
    }

    /// Connects to an explicit socket path and starts the reader thread.
    pub fn connect_to(path: PathBuf) -> Self {
        let state = Arc::new(RwLock::new(State::default()));
        let (sender, changes) = channel();
        let connected = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_connected = Arc::clone(&connected);
        let name = path.clone();
        thread::Builder::new()
            .name("loom-state".to_owned())
            .spawn(move || run(path, thread_state, thread_connected, sender))
            .unwrap_or_else(|err| panic!("cannot start loom-state reader for {name:?}: {err}"));
        Self {
            state,
            changes,
            connected,
        }
    }

    /// Current snapshot.
    pub fn snapshot(&self) -> State {
        self.state
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    /// Non-blocking change signal. A closed channel means the client is gone.
    pub fn try_recv(&self) -> Option<()> {
        self.changes.try_recv().ok()
    }

    /// Blocking change signal for simple TUI loops.
    pub fn recv(&self) -> Option<()> {
        self.changes.recv().ok()
    }

    /// Whether the reader currently has a live compositor connection.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}

fn run(path: PathBuf, state: Arc<RwLock<State>>, connected: Arc<AtomicBool>, sender: Sender<()>) {
    loop {
        if session(&path, &state, &connected, &sender).is_ok() {
            return;
        }
        connected.store(false, Ordering::Relaxed);
        let _ = sender.send(());
        thread::sleep(RETRY);
    }
}

fn session(
    path: &Path,
    state: &Arc<RwLock<State>>,
    connected: &AtomicBool,
    sender: &Sender<()>,
) -> Result<(), String> {
    let mut query = Socket::connect_to(path).map_err(|err| format!("{err}"))?;

    if let Some(outputs) = request_outputs(&mut query) {
        state.write().unwrap_or_else(|err| err.into_inner()).outputs = outputs;
    }
    if let Some(workspaces) = request_workspaces(&mut query) {
        state
            .write()
            .unwrap_or_else(|err| err.into_inner())
            .workspaces = workspaces;
    }
    if let Some(windows) = request_windows(&mut query) {
        state.write().unwrap_or_else(|err| err.into_inner()).windows = windows;
    }

    let mut events = Socket::connect_to(path).map_err(|err| format!("{err}"))?;
    match events.send(Request::EventStream) {
        Ok(Ok(Response::Handled)) => {}
        Ok(Ok(other)) => return Err(format!("unexpected EventStream response: {other:?}")),
        Ok(Err(message)) => return Err(message),
        Err(err) => return Err(format!("{err}")),
    }

    connected.store(true, Ordering::Relaxed);
    let _ = sender.send(());
    let mut read = events.read_events();
    loop {
        let event = read().map_err(|err| format!("{err}"))?;
        let mut guard = state.write().unwrap_or_else(|err| err.into_inner());
        apply_event(&mut guard, event);
        drop(guard);
        let _ = sender.send(());
    }
}

fn request_outputs(socket: &mut Socket) -> Option<BTreeMap<String, Output>> {
    match socket.send(Request::Outputs) {
        Ok(Ok(Response::Outputs(outputs))) => Some(outputs.into_iter().collect()),
        _ => None,
    }
}

fn request_workspaces(socket: &mut Socket) -> Option<Vec<Workspace>> {
    match socket.send(Request::Workspaces) {
        Ok(Ok(Response::Workspaces(workspaces))) => Some(workspaces),
        _ => None,
    }
}

fn request_windows(socket: &mut Socket) -> Option<Vec<Window>> {
    match socket.send(Request::Windows) {
        Ok(Ok(Response::Windows(windows))) => Some(windows),
        _ => None,
    }
}

/// Applies one event to a snapshot. Public so tools and tests can reuse the
/// reducer without opening a socket.
pub fn apply_event(state: &mut State, event: Event) {
    match event {
        Event::WorkspacesChanged { workspaces } => state.workspaces = workspaces,
        Event::WindowsChanged { windows } => state.windows = windows,
        Event::WorkspaceActivated { id, focused } => {
            let output = state
                .workspaces
                .iter()
                .find(|workspace| workspace.id == id)
                .and_then(|workspace| workspace.output.clone());
            for workspace in &mut state.workspaces {
                if workspace.id == id {
                    workspace.is_active = true;
                    if focused {
                        workspace.is_focused = true;
                    }
                } else if focused {
                    workspace.is_active = false;
                    workspace.is_focused = false;
                } else if workspace.output == output {
                    workspace.is_active = false;
                }
            }
        }
        Event::WorkspaceActiveWindowChanged {
            workspace_id,
            active_window_id,
        } => {
            if let Some(workspace) = state
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
            {
                workspace.active_window_id = active_window_id;
            }
        }
        Event::WindowOpenedOrChanged { window } => {
            if let Some(existing) = state.windows.iter_mut().find(|item| item.id == window.id) {
                *existing = window;
            } else {
                state.windows.push(window);
            }
        }
        Event::WindowClosed { id } => state.windows.retain(|window| window.id != id),
        Event::WindowFocusChanged { id } => {
            for window in &mut state.windows {
                window.is_focused = Some(window.id) == id;
            }
        }
        Event::WindowFocusTimestampChanged {
            id,
            focus_timestamp,
        } => {
            if let Some(window) = state.windows.iter_mut().find(|window| window.id == id) {
                window.focus_timestamp = focus_timestamp;
            }
        }
        Event::WindowUrgencyChanged { id, urgent } => {
            if let Some(window) = state.windows.iter_mut().find(|window| window.id == id) {
                window.is_urgent = urgent;
            }
        }
        Event::WorkspaceUrgencyChanged { id, urgent } => {
            if let Some(workspace) = state.workspaces.iter_mut().find(|w| w.id == id) {
                workspace.is_urgent = urgent;
            }
        }
        Event::OverviewOpenedOrClosed { is_open } => state.overview_open = is_open,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use niri_ipc::Event;

    use super::*;

    fn workspace(id: u64, output: &str, focused: bool) -> Workspace {
        Workspace {
            id,
            idx: id as u8,
            name: None,
            output: Some(output.to_owned()),
            is_active: focused,
            is_focused: focused,
            is_urgent: false,
            active_window_id: None,
            is_hidden: false,
        }
    }

    #[test]
    fn replacing_workspaces_and_windows_keeps_the_snapshot_consistent() {
        let mut state = State::default();
        apply_event(
            &mut state,
            Event::WorkspacesChanged {
                workspaces: vec![workspace(1, "eDP-1", true), workspace(2, "HDMI-1", false)],
            },
        );
        assert_eq!(state.focused_workspace().unwrap().id, 1);

        apply_event(
            &mut state,
            Event::WorkspaceActivated {
                id: 2,
                focused: true,
            },
        );
        assert_eq!(state.focused_workspace().unwrap().id, 2);
        assert!(!state.workspaces[0].is_focused);
    }

    #[test]
    fn window_events_upsert_and_remove() {
        let mut state = State::default();
        let window = niri_ipc::Window {
            id: 7,
            title: Some("test".into()),
            app_id: Some("app".into()),
            pid: None,
            workspace_id: Some(1),
            is_focused: true,
            is_floating: false,
            is_sticky: false,
            is_urgent: false,
            is_minimized: false,
            is_agent: false,
            content_type: None,
            tearing_allowed: false,
            layout: niri_ipc::WindowLayout {
                pos_in_scrolling_layout: None,
                tile_size: (0., 0.),
                window_size: (0, 0),
                tile_pos_in_workspace_view: None,
                window_offset_in_tile: (0., 0.),
            },
            focus_timestamp: None,
        };
        apply_event(
            &mut state,
            Event::WindowOpenedOrChanged {
                window: window.clone(),
            },
        );
        assert_eq!(state.windows.len(), 1);
        let mut changed = window;
        changed.title = Some("changed".into());
        apply_event(&mut state, Event::WindowOpenedOrChanged { window: changed });
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.windows[0].title.as_deref(), Some("changed"));
        apply_event(&mut state, Event::WindowClosed { id: 7 });
        assert!(state.windows.is_empty());
    }
}
