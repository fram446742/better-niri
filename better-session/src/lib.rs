//! `better-session`: save and restore window/workspace sessions.
//!
//! A session records workspaces and their windows (app id, title, workspace,
//! floating state, focus) plus optional launch commands for apps that are not
//! running when the session is restored. Restore matches current windows by
//! app id/title, moves them to the saved workspace, restores the floating
//! layout, spawns configured missing apps, and finally focuses the saved window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use niri_ipc::{Action, Output, Request, Response, Window, Workspace, WorkspaceReferenceArg};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSnapshot {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub idx: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_focused: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowSnapshot {
    pub app_id: String,
    pub title: String,
    pub workspace_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    pub workspace_idx: u8,
    #[serde(default)]
    pub is_floating: bool,
    #[serde(default)]
    pub is_focused: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub command_map: BTreeMap<String, String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSnapshot>,
    #[serde(default)]
    pub windows: Vec<WindowSnapshot>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub sessions: BTreeMap<String, Session>,
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
    dir.join("better-session/sessions.json")
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
}

/// Captures workspaces and windows from niri IPC responses.
pub fn snapshot(
    workspaces: &[Workspace],
    windows: &[Window],
    command_map: BTreeMap<String, String>,
) -> Session {
    let by_id: BTreeMap<u64, &Workspace> = workspaces.iter().map(|ws| (ws.id, ws)).collect();
    Session {
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        command_map,
        workspaces: workspaces
            .iter()
            .map(|ws| WorkspaceSnapshot {
                id: ws.id,
                name: ws.name.clone(),
                idx: ws.idx,
                output: ws.output.clone(),
                is_active: ws.is_active,
                is_focused: ws.is_focused,
            })
            .collect(),
        windows: windows
            .iter()
            .map(|win| {
                let workspace = win.workspace_id.and_then(|id| by_id.get(&id).copied());
                WindowSnapshot {
                    app_id: win.app_id.clone().unwrap_or_default(),
                    title: win.title.clone().unwrap_or_default(),
                    workspace_id: win.workspace_id.unwrap_or(0),
                    workspace_name: workspace.and_then(|ws| ws.name.clone()),
                    workspace_idx: workspace.map_or(0, |ws| ws.idx),
                    is_floating: win.is_floating,
                    is_focused: win.is_focused,
                }
            })
            .collect(),
    }
}

fn same_window(saved: &WindowSnapshot, current: &Window) -> bool {
    if current.app_id.as_deref().unwrap_or_default() != saved.app_id {
        return false;
    }
    if saved.title.is_empty() {
        return true;
    }
    current.title.as_deref().unwrap_or_default() == saved.title
}

fn workspace_reference(saved: &WindowSnapshot) -> WorkspaceReferenceArg {
    match &saved.workspace_name {
        Some(name) => WorkspaceReferenceArg::Name(name.clone()),
        None => WorkspaceReferenceArg::Index(saved.workspace_idx),
    }
}

/// Plans the actions needed to restore `session` given the current state.
///
/// Returns the actions and the app ids that could not be matched or launched.
pub fn plan(
    session: &Session,
    current_workspaces: &[Workspace],
    current_windows: &[Window],
) -> (Vec<(String, Action)>, Vec<String>) {
    let mut actions = Vec::new();
    let mut missing = Vec::new();

    // Focus named workspaces first so they exist before moving windows into them.
    let mut named: Vec<&str> = session
        .workspaces
        .iter()
        .filter_map(|ws| ws.name.as_deref())
        .collect();
    named.sort_unstable();
    named.dedup();
    for name in named {
        actions.push((
            format!("focus workspace {name}"),
            Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Name(name.to_owned()),
            },
        ));
    }

    let current_by_id: BTreeMap<u64, &Window> =
        current_windows.iter().map(|win| (win.id, win)).collect();
    let _ = current_by_id;

    let mut focused_current = None;
    for saved in &session.windows {
        let matched = current_windows
            .iter()
            .find(|current| same_window(saved, current));
        let Some(current) = matched else {
            if let Some(command) = session.command_map.get(&saved.app_id) {
                actions.push((
                    format!("spawn {}", saved.app_id),
                    Action::SpawnSh {
                        command: command.clone(),
                    },
                ));
            } else {
                missing.push(saved.app_id.clone());
            }
            continue;
        };

        let reference = workspace_reference(saved);
        let current_workspace_matches = match (&reference, current.workspace_id) {
            (WorkspaceReferenceArg::Name(name), Some(id)) => {
                current_workspaces
                    .iter()
                    .find(|ws| ws.id == id)
                    .and_then(|ws| ws.name.as_deref())
                    == Some(name.as_str())
            }
            (WorkspaceReferenceArg::Index(idx), Some(id)) => current_workspaces
                .iter()
                .find(|ws| ws.id == id)
                .map(|ws| ws.idx == *idx)
                .unwrap_or(false),
            _ => false,
        };
        if !current_workspace_matches {
            actions.push((
                format!("move window {} to workspace", current.id),
                Action::MoveWindowToWorkspace {
                    window_id: Some(current.id),
                    reference,
                    focus: false,
                },
            ));
        }
        if current.is_floating != saved.is_floating {
            actions.push((
                format!("set floating {}", current.id),
                if saved.is_floating {
                    Action::MoveWindowToFloating {
                        id: Some(current.id),
                    }
                } else {
                    Action::MoveWindowToTiling {
                        id: Some(current.id),
                    }
                },
            ));
        }
        if saved.is_focused {
            focused_current = Some(current.id);
        }
    }

    if let Some(id) = focused_current {
        actions.push((format!("focus window {id}"), Action::FocusWindow { id }));
    }

    (actions, missing)
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

pub type Outputs = BTreeMap<String, Output>;

pub fn query(socket: &Path) -> Result<(Vec<Workspace>, Vec<Window>, Outputs), String> {
    let outputs = match send(socket, Request::Outputs)? {
        Response::Outputs(outputs) => outputs.into_iter().collect(),
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    let workspaces = match send(socket, Request::Workspaces)? {
        Response::Workspaces(workspaces) => workspaces,
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    let windows = match send(socket, Request::Windows)? {
        Response::Windows(windows) => windows,
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    Ok((workspaces, windows, outputs))
}

/// Applies a plan. With `dry_run`, returns the serialized actions only.
pub fn execute(
    socket: &Path,
    actions: &[(String, Action)],
    dry_run: bool,
) -> Result<Vec<Value>, String> {
    let mut results = Vec::with_capacity(actions.len());
    for (label, action) in actions {
        let value = serde_json::to_value(action).unwrap_or(Value::Null);
        if !dry_run {
            match send(socket, Request::Action(action.clone()))? {
                Response::Handled => (),
                other => return Err(format!("unexpected niri response: {other:?}")),
            }
        }
        results.push(serde_json::json!({ "action": label, "value": value }));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("better-session-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("sessions.json")
    }

    fn workspace(id: u64, idx: u8, name: Option<&str>) -> Workspace {
        serde_json::from_value(json!({
            "id": id,
            "idx": idx,
            "name": name,
            "output": "eDP-1",
            "is_urgent": false,
            "is_active": idx == 1,
            "is_focused": idx == 1,
            "active_window_id": null,
            "is_hidden": false
        }))
        .unwrap()
    }

    fn window(id: u64, app_id: &str, workspace_id: u64, floating: bool) -> Window {
        serde_json::from_value(json!({
            "id": id,
            "title": format!("{app_id}-title"),
            "app_id": app_id,
            "pid": 1,
            "workspace_id": workspace_id,
            "is_focused": true,
            "is_floating": floating,
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

    #[test]
    fn snapshots_workspaces_and_windows() {
        let workspaces = vec![workspace(1, 1, Some("main")), workspace(2, 2, Some("web"))];
        let windows = vec![window(10, "firefox", 2, true)];
        let session = snapshot(&workspaces, &windows, BTreeMap::new());
        assert_eq!(session.workspaces.len(), 2);
        assert_eq!(session.windows[0].workspace_name.as_deref(), Some("web"));
        assert_eq!(session.windows[0].workspace_idx, 2);
        assert!(session.windows[0].is_floating);
    }

    #[test]
    fn plan_moves_floating_and_missing_windows() {
        let saved_workspaces = vec![workspace(1, 1, Some("main")), workspace(2, 2, Some("web"))];
        let saved_windows = vec![
            window(10, "firefox", 2, true),
            window(11, "editor", 2, false),
        ];
        let mut commands = BTreeMap::new();
        commands.insert("editor".to_owned(), "editor".to_owned());
        let session = snapshot(&saved_workspaces, &saved_windows, commands);

        // Current state: firefox on main, tiled; editor is missing.
        let current_workspaces = vec![workspace(7, 1, Some("main"))];
        let current_windows = vec![window(20, "firefox", 7, false)];
        let (actions, missing) = plan(&session, &current_workspaces, &current_windows);
        assert!(missing.is_empty());

        let labels: Vec<&str> = actions.iter().map(|(label, _)| label.as_str()).collect();
        assert!(labels.contains(&"focus workspace main"));
        assert!(labels.contains(&"focus workspace web"));
        assert!(labels.contains(&"move window 20 to workspace"));
        assert!(labels.contains(&"set floating 20"));
        assert!(labels.contains(&"spawn editor"));
        assert!(labels.contains(&"focus window 20"));
        assert!(matches!(
            actions.last().unwrap().1,
            Action::FocusWindow { id: 20 }
        ));
    }

    #[test]
    fn missing_apps_without_commands_are_reported() {
        let session = snapshot(
            &[workspace(1, 1, None)],
            &[window(10, "gone", 1, false)],
            BTreeMap::new(),
        );
        let (actions, missing) = plan(&session, &[], &[]);
        assert!(actions.is_empty());
        assert_eq!(missing, ["gone"]);
    }

    #[test]
    fn store_round_trips() {
        let path = temp_path("store");
        let mut store = Store::default();
        store.sessions.insert("work".to_owned(), Session::default());
        store.save(&path).unwrap();
        let reloaded = Store::load(&path).unwrap();
        assert!(reloaded.sessions.contains_key("work"));
    }
}
