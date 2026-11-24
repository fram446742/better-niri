//! Window-oriented actions: run-or-raise, singleton and placement.
//!
//! Matches live niri window state (falling back to nothing rather than stale
//! cached data), raises an existing match, or spawns the configured command.
//! Placement actions resolve workspace targets by stable id, name or index and
//! move windows to a named output.

use std::collections::BTreeSet;

use companion_core::{ActionInfo, Context};
use niri_ipc::{Action, Request, Response, Window, Workspace, WorkspaceReferenceArg};
use serde_json::{Value, json};

use crate::Compositor;
use crate::layout::{self, WorkspaceTarget};

#[derive(Clone)]
pub struct Windows {
    compositor: Compositor,
}

impl Windows {
    pub fn new(compositor: Compositor) -> Self {
        Self { compositor }
    }

    fn windows(&self) -> Result<Vec<Window>, String> {
        match self.compositor.send(Request::Windows)? {
            Response::Windows(windows) => Ok(windows),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn matches(window: &Window, params: &Value, output_workspaces: Option<&BTreeSet<u64>>) -> bool {
        if let Some(app_id) = params.get("app_id").and_then(Value::as_str)
            && !window
                .app_id
                .as_deref()
                .is_some_and(|value| value.to_lowercase().contains(&app_id.to_lowercase()))
        {
            return false;
        }
        if let Some(title) = params.get("title").and_then(Value::as_str)
            && !window
                .title
                .as_deref()
                .is_some_and(|value| value.to_lowercase().contains(&title.to_lowercase()))
        {
            return false;
        }
        if let Some(workspace) = params.get("workspace").and_then(Value::as_u64)
            && window.workspace_id != Some(workspace)
        {
            return false;
        }
        if let Some(allowed) = output_workspaces
            && !window
                .workspace_id
                .is_some_and(|workspace| allowed.contains(&workspace))
        {
            return false;
        }
        true
    }

    fn apply_action(&self, action: Action) -> Result<(), String> {
        match self.compositor.send(Request::Action(action))? {
            Response::Handled => Ok(()),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    /// Resolves `window`/`id` or the focused window against a live window list.
    fn resolve_window<'a>(windows: &'a [Window], params: &Value) -> Result<&'a Window, String> {
        if let Some(id) = params
            .get("window")
            .or_else(|| params.get("id"))
            .and_then(Value::as_u64)
        {
            return windows
                .iter()
                .find(|window| window.id == id)
                .ok_or_else(|| format!("window {id} is not open"));
        }
        windows
            .iter()
            .find(|window| window.is_focused)
            .ok_or_else(|| "no focused window".to_owned())
    }

    /// `sticky` (required) for `set-sticky`.
    fn sticky_flag(params: &Value) -> Result<bool, String> {
        params
            .get("sticky")
            .and_then(Value::as_bool)
            .ok_or_else(|| "windows.set-sticky needs a boolean `sticky`".to_owned())
    }

    fn toggle_sticky(&self, params: &Value) -> Result<Value, String> {
        let windows = self.windows()?;
        let window = Self::resolve_window(&windows, params)?;
        self.apply_action(Action::ToggleWindowSticky {
            id: Some(window.id),
        })?;
        Ok(json!({
            "action": "toggled",
            "window": window.id,
            "sticky": !window.is_sticky,
            "previous": window.is_sticky,
        }))
    }

    fn set_sticky(&self, params: &Value) -> Result<Value, String> {
        let sticky = Self::sticky_flag(params)?;
        let windows = self.windows()?;
        let window = Self::resolve_window(&windows, params)?;
        let changed = window.is_sticky != sticky;
        if changed {
            self.apply_action(Action::ToggleWindowSticky {
                id: Some(window.id),
            })?;
        }
        Ok(json!({
            "action": "set",
            "window": window.id,
            "sticky": sticky,
            "changed": changed,
        }))
    }

    fn list_sticky(&self) -> Result<Value, String> {
        let mut matched: Vec<Window> = self
            .windows()?
            .into_iter()
            .filter(|window| window.is_sticky)
            .collect();
        matched.sort_by_key(|window| window.id);
        let count = matched.len();
        let windows: Vec<Value> = matched
            .into_iter()
            .map(|window| {
                json!({
                    "id": window.id,
                    "title": window.title,
                    "app_id": window.app_id,
                    "workspace_id": window.workspace_id,
                    "is_focused": window.is_focused,
                })
            })
            .collect();
        Ok(json!({ "windows": windows, "count": count }))
    }

    fn spawn(&self, params: &Value) -> Result<Value, String> {
        if let Some(command) = params.get("spawn_sh").and_then(Value::as_str) {
            self.apply_action(Action::SpawnSh {
                command: command.to_owned(),
            })?;
            return Ok(json!({
                "action": "spawned",
                "spawned": true,
                "shell": command,
                "matches": 0,
                "candidate_ids": [],
            }));
        }
        if let Some(command) = params.get("spawn").and_then(Value::as_array) {
            let command: Vec<String> = command
                .iter()
                .map(|part| {
                    part.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "spawn entries must be strings".to_owned())
                })
                .collect::<Result<_, _>>()?;
            if command.is_empty() {
                return Err("spawn must not be empty".to_owned());
            }
            self.apply_action(Action::Spawn {
                command: command.clone(),
            })?;
            return Ok(json!({
                "action": "spawned",
                "spawned": true,
                "command": command,
                "matches": 0,
                "candidate_ids": [],
            }));
        }
        Err("run-or-raise needs `spawn` (array) or `spawn_sh` (string)".to_owned())
    }

    /// Resolves workspace-name/index and output filters into a numeric
    /// workspace id plus the set of workspace ids on the requested output.
    fn match_params(&self, params: &Value) -> Result<(Value, Option<BTreeSet<u64>>), String> {
        let mut resolved = params.clone();
        let output_workspaces = match params.get("output").and_then(Value::as_str) {
            Some(output) => Some(layout::workspace_ids_on_output(&self.compositor, output)?),
            None => None,
        };
        if let Some(target) = WorkspaceTarget::from_params(params)? {
            let workspace = layout::resolve_live_workspace(&self.compositor, &target)?;
            let object = resolved
                .as_object_mut()
                .ok_or_else(|| "matching parameters must be an object".to_owned())?;
            object.insert("workspace".to_owned(), json!(workspace.id));
        }
        Ok((resolved, output_workspaces))
    }

    /// All matches sorted by window id. Raising prefers the lowest-id focused
    /// match, then the lowest id, so multiple matches behave deterministically
    /// regardless of niri's listing order.
    fn matches_sorted(&self, params: &Value) -> Result<Vec<Window>, String> {
        let (params, output_workspaces) = self.match_params(params)?;
        let mut matched: Vec<Window> = self
            .windows()?
            .into_iter()
            .filter(|window| Self::matches(window, &params, output_workspaces.as_ref()))
            .collect();
        matched.sort_by_key(|window| window.id);
        Ok(matched)
    }

    fn find(&self, params: &Value) -> Result<(Option<Window>, Vec<u64>), String> {
        let matched = self.matches_sorted(params)?;
        let candidate_ids: Vec<u64> = matched.iter().map(|window| window.id).collect();
        let chosen = matched
            .iter()
            .find(|window| window.is_focused)
            .cloned()
            .or_else(|| matched.first().cloned());
        Ok((chosen, candidate_ids))
    }

    fn window_id(&self, params: &Value) -> Result<u64, String> {
        if let Some(id) = params
            .get("window")
            .or_else(|| params.get("id"))
            .and_then(Value::as_u64)
        {
            return Ok(id);
        }
        match self.compositor.send(Request::FocusedWindow)? {
            Response::FocusedWindow(Some(window)) => Ok(window.id),
            Response::FocusedWindow(None) => Err("no focused window".to_owned()),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    /// Resolves the placement target. A plain id needs no query; name/index
    /// targets resolve to a stable id so indices on another output cannot
    /// silently win.
    fn workspace_reference(
        &self,
        target: &WorkspaceTarget,
    ) -> Result<(u64, Option<Workspace>), String> {
        if let Some(id) = target.id
            && target.name.is_none()
            && target.index.is_none()
            && target.output.is_none()
        {
            return Ok((id, None));
        }
        let workspace = layout::resolve_live_workspace(&self.compositor, target)?;
        Ok((workspace.id, Some(workspace)))
    }

    fn move_to_workspace(&self, params: &Value, follow: bool) -> Result<Value, String> {
        let target = WorkspaceTarget::from_params(params)?
            .ok_or_else(|| "needs `workspace`, `workspace_name` or `workspace_idx`".to_owned())?;
        let window = self.window_id(params)?;
        let (workspace_id, workspace) = self.workspace_reference(&target)?;
        let focus = follow || params.get("focus").and_then(Value::as_bool).unwrap_or(true);
        self.apply_action(Action::MoveWindowToWorkspace {
            window_id: Some(window),
            reference: WorkspaceReferenceArg::Id(workspace_id),
            focus,
        })?;
        Ok(json!({
            "action": "moved",
            "window": window,
            "workspace": workspace_id,
            "workspace_name": workspace
                .as_ref()
                .and_then(|workspace| workspace.name.clone()),
            "output": workspace
                .as_ref()
                .and_then(|workspace| workspace.output.clone()),
            "followed": focus,
        }))
    }

    fn move_to_output(&self, params: &Value) -> Result<Value, String> {
        let output = params
            .get("output")
            .and_then(Value::as_str)
            .ok_or_else(|| "windows.move-to-output needs an `output` name".to_owned())?;
        let output = layout::canonical_output(&self.compositor, output)?;
        let window = self.window_id(params)?;
        self.apply_action(Action::MoveWindowToMonitor {
            id: Some(window),
            output: output.clone(),
        })?;
        Ok(json!({ "action": "moved", "window": window, "output": output }))
    }
}

impl Context for Windows {
    fn namespace(&self) -> &'static str {
        "windows"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new(
                "run-or-raise",
                "Focus a matching window or spawn `spawn`/`spawn_sh`",
            ),
            ActionInfo::new(
                "singleton",
                "Alias of run-or-raise for single-instance apps",
            ),
            ActionInfo::new(
                "find",
                "Return matching windows without acting; supports `workspace_name`, \
                 `workspace_idx` and `output`",
            ),
            ActionInfo::new(
                "move-to-workspace",
                "Move `window` (or the focused window) to a workspace id, name or index",
            ),
            ActionInfo::new(
                "follow-to-workspace",
                "Move a window and focus its target workspace",
            ),
            ActionInfo::new(
                "move-to-output",
                "Move `window` (or the focused window) to output `output`",
            ),
            ActionInfo::new("focus-output", "Focus output `output`"),
            ActionInfo::new(
                "toggle-sticky",
                "Toggle niri's native sticky flag on `window` (or the focused window)",
            ),
            ActionInfo::new(
                "set-sticky",
                "Set the native sticky flag on a window to `sticky`; no action when unchanged",
            ),
            ActionInfo::new(
                "list-sticky",
                "List windows with the native sticky flag in numeric id order",
            ),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "find" => {
                let matched = self.matches_sorted(params)?;
                let count = matched.len();
                let matched: Vec<Value> = matched
                    .into_iter()
                    .map(|window| {
                        json!({
                            "id": window.id,
                            "title": window.title,
                            "app_id": window.app_id,
                            "workspace_id": window.workspace_id,
                            "is_focused": window.is_focused,
                            "is_sticky": window.is_sticky,
                        })
                    })
                    .collect();
                Ok(json!({ "windows": matched, "count": count }))
            }
            "run-or-raise" | "singleton" => {
                let (found, candidate_ids) = self.find(params)?;
                if let Some(window) = found {
                    let was_focused = window.is_focused;
                    // Raising an already-focused window is a no-op so repeated
                    // calls cannot spam niri with redundant FocusWindow actions.
                    if !was_focused {
                        self.apply_action(Action::FocusWindow { id: window.id })?;
                    }
                    return Ok(json!({
                        "action": "raised",
                        "raised": window.id,
                        "title": window.title,
                        "app_id": window.app_id,
                        "workspace_id": window.workspace_id,
                        "was_focused": was_focused,
                        "already_focused": was_focused,
                        "matches": candidate_ids.len(),
                        "candidate_ids": candidate_ids,
                    }));
                }
                self.spawn(params)
            }
            "move-to-workspace" => self.move_to_workspace(params, false),
            "follow-to-workspace" => self.move_to_workspace(params, true),
            "move-to-output" => self.move_to_output(params),
            "toggle-sticky" => self.toggle_sticky(params),
            "set-sticky" => self.set_sticky(params),
            "list-sticky" => self.list_sticky(),
            "focus-output" => {
                let output = params
                    .get("output")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "windows.focus-output needs an `output` name".to_owned())?;
                let output = layout::canonical_output(&self.compositor, output)?;
                self.apply_action(Action::FocusMonitor {
                    output: output.clone(),
                })?;
                Ok(json!({ "action": "focused", "output": output }))
            }
            other => Err(format!("unknown action `windows.{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-windows-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn window_json(id: u64, app_id: &str, focused: bool) -> serde_json::Value {
        json!({
            "id": id,
            "title": format!("{app_id} window"),
            "app_id": app_id,
            "pid": 1,
            "workspace_id": 1,
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
        })
    }

    /// Accepts the `Windows` query plus `actions` one-request actions and
    /// records every request.
    fn fake_niri(
        path: &PathBuf,
        windows: Vec<serde_json::Value>,
        actions: usize,
    ) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            for step in 0..=actions {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Request = serde_json::from_str(line.trim()).unwrap();
                let response = if step == 0 {
                    json!({ "Ok": { "Windows": windows } })
                } else {
                    json!({ "Ok": "Handled" })
                };
                requests.push(request);
                let mut stream = stream;
                stream
                    .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                    .unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        })
    }

    #[test]
    fn raises_an_existing_window() {
        let dir = temp_dir("raise");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, vec![window_json(10, "firefox", false)], 1);
        let windows = Windows::new(Compositor::at(&socket));
        let result = windows
            .call(
                "run-or-raise",
                &json!({ "app_id": "fire", "spawn": ["firefox"] }),
            )
            .unwrap();
        assert_eq!(result["raised"], 10);

        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Windows));
        assert!(matches!(
            requests[1],
            Request::Action(Action::FocusWindow { id: 10 })
        ));
    }

    #[test]
    fn spawns_when_no_window_matches() {
        let dir = temp_dir("spawn");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, vec![], 1);
        let windows = Windows::new(Compositor::at(&socket));
        let result = windows
            .call(
                "singleton",
                &json!({ "app_id": "firefox", "spawn": ["firefox", "--new-window"] }),
            )
            .unwrap();
        assert_eq!(result["spawned"], true);
        assert_eq!(result["action"], "spawned");
        assert_eq!(result["matches"], 0);
        assert_eq!(result["candidate_ids"], json!([]));

        let requests = server.join().unwrap();
        assert!(matches!(
            &requests[1],
            Request::Action(Action::Spawn { command })
                if command.iter().map(String::as_str).eq(["firefox", "--new-window"])
        ));
    }

    #[test]
    fn find_returns_matches_without_acting() {
        let dir = temp_dir("find");
        let socket = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let response = json!({ "Ok": { "Windows": [window_json(10, "firefox", true)] } });
            let mut stream = stream;
            stream
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        });
        let windows = Windows::new(Compositor::at(&socket));
        let value = windows.call("find", &json!({ "title": "FIRE" })).unwrap();
        assert_eq!(value["windows"][0]["id"], 10);
        assert_eq!(value["count"], 1);
        server.join().unwrap();
    }

    #[test]
    fn multiple_matches_pick_the_focused_window_and_report_candidates() {
        let dir = temp_dir("multi-focused");
        let socket = dir.join("niri.sock");
        // Deliberately unordered niri output; 11 is focused but not the first.
        let server = fake_niri(
            &socket,
            vec![
                window_json(12, "firefox", false),
                window_json(10, "firefox", false),
                window_json(11, "firefox", true),
            ],
            0,
        );
        let windows = Windows::new(Compositor::at(&socket));
        let result = windows
            .call(
                "run-or-raise",
                &json!({ "app_id": "fire", "spawn": ["firefox"] }),
            )
            .unwrap();
        assert_eq!(result["action"], "raised");
        assert_eq!(result["raised"], 11);
        assert_eq!(result["was_focused"], true);
        assert_eq!(result["already_focused"], true);
        assert_eq!(result["matches"], 3);
        assert_eq!(result["candidate_ids"], json!([10, 11, 12]));

        // A focused match is not refocused: only the Windows query was sent.
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], Request::Windows));
    }

    #[test]
    fn multiple_matches_without_focus_pick_the_lowest_id() {
        let dir = temp_dir("multi-unfocused");
        let socket = dir.join("niri.sock");
        let server = fake_niri(
            &socket,
            vec![
                window_json(12, "firefox", false),
                window_json(10, "firefox", false),
                window_json(11, "firefox", false),
            ],
            1,
        );
        let windows = Windows::new(Compositor::at(&socket));
        let result = windows
            .call(
                "singleton",
                &json!({ "app_id": "firefox", "spawn_sh": "firefox" }),
            )
            .unwrap();
        assert_eq!(result["raised"], 10);
        assert_eq!(result["was_focused"], false);
        assert_eq!(result["matches"], 3);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[1],
            Request::Action(Action::FocusWindow { id: 10 })
        ));
    }

    fn workspace_json(
        id: u64,
        idx: u8,
        name: &str,
        output: &str,
        focused: bool,
    ) -> serde_json::Value {
        json!({
            "id": id,
            "idx": idx,
            "name": name,
            "output": output,
            "is_urgent": false,
            "is_active": focused,
            "is_focused": focused,
            "active_window_id": null,
            "is_hidden": false
        })
    }

    fn output_json(name: &str) -> serde_json::Value {
        json!({
            "name": name,
            "make": "test",
            "model": "test",
            "serial": null,
            "physical_size": null,
            "modes": [],
            "current_mode": null,
            "is_custom_mode": false,
            "vrr_supported": false,
            "vrr_enabled": false,
            "logical": null,
            "max_bpc": null
        })
    }

    /// Accepts one connection per response, recording the request each time.
    fn fake_sequence(
        path: &PathBuf,
        responses: Vec<serde_json::Value>,
    ) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                requests.push(serde_json::from_str(line.trim()).unwrap());
                let mut stream = stream;
                stream
                    .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                    .unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        })
    }

    #[test]
    fn moves_window_to_workspace_by_name_and_reports_metadata() {
        let dir = temp_dir("move-name");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Workspaces": [
                    workspace_json(1, 1, "dev", "eDP-1", true),
                    workspace_json(2, 2, "Web", "HDMI-1", false),
                ] } }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let result = windows
            .call(
                "move-to-workspace",
                &json!({ "window": 10, "workspace_name": "web" }),
            )
            .unwrap();
        assert_eq!(result["action"], "moved");
        assert_eq!(result["window"], 10);
        assert_eq!(result["workspace"], 2);
        assert_eq!(result["workspace_name"], "Web");
        assert_eq!(result["output"], "HDMI-1");
        assert_eq!(result["followed"], true);

        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Workspaces));
        assert!(matches!(
            requests[1],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: Some(10),
                reference: WorkspaceReferenceArg::Id(2),
                focus: true,
            })
        ));
    }

    #[test]
    fn follow_to_workspace_by_index_forces_focus() {
        let dir = temp_dir("follow-index");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Workspaces": [
                    workspace_json(1, 1, "dev", "eDP-1", true),
                    workspace_json(3, 2, "notes", "HDMI-1", false),
                ] } }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let result = windows
            .call(
                "follow-to-workspace",
                &json!({ "window": 10, "workspace_idx": 2, "focus": false }),
            )
            .unwrap();
        assert_eq!(result["workspace"], 3);
        assert_eq!(result["followed"], true);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[1],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: Some(10),
                reference: WorkspaceReferenceArg::Id(3),
                focus: true,
            })
        ));
    }

    #[test]
    fn moves_window_to_a_named_output() {
        let dir = temp_dir("move-output");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Outputs": {
                    "eDP-1": output_json("eDP-1"),
                    "HDMI-1": output_json("HDMI-1"),
                } } }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let result = windows
            .call(
                "move-to-output",
                &json!({ "window": 10, "output": "hdmi-1" }),
            )
            .unwrap();
        assert_eq!(result["output"], "HDMI-1");

        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Outputs));
        assert!(matches!(
            requests[1],
            Request::Action(Action::MoveWindowToMonitor {
                id: Some(10),
                ref output,
            }) if output == "HDMI-1"
        ));
    }

    #[test]
    fn find_matches_by_workspace_name() {
        let dir = temp_dir("find-workspace");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Workspaces": [
                    workspace_json(1, 1, "dev", "eDP-1", true),
                    workspace_json(2, 2, "Web", "HDMI-1", false),
                ] } }),
                json!({ "Ok": { "Windows": [
                    window_json(10, "firefox", false),
                    { "id": 11, "title": "web window", "app_id": "firefox", "pid": 1,
                      "workspace_id": 2, "is_focused": false, "is_floating": false,
                      "is_sticky": false, "is_urgent": false, "is_minimized": false,
                      "layout": { "pos_in_scrolling_layout": [1, 1], "tile_size": [100.0, 100.0],
                        "window_size": [100, 100], "tile_pos_in_workspace_view": [0.0, 0.0],
                        "window_offset_in_tile": [0.0, 0.0] }, "focus_timestamp": null },
                ] } }),
            ],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let value = windows
            .call("find", &json!({ "workspace_name": "web" }))
            .unwrap();
        assert_eq!(value["count"], 1);
        assert_eq!(value["windows"][0]["id"], 11);
        server.join().unwrap();
    }

    #[test]
    fn find_filters_by_output() {
        let dir = temp_dir("find-output");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Workspaces": [
                    workspace_json(1, 1, "dev", "eDP-1", true),
                    workspace_json(2, 2, "Web", "HDMI-1", false),
                ] } }),
                json!({ "Ok": { "Windows": [
                    window_json(10, "firefox", false),
                    { "id": 11, "title": "web window", "app_id": "firefox", "pid": 1,
                      "workspace_id": 2, "is_focused": false, "is_floating": false,
                      "is_sticky": false, "is_urgent": false, "is_minimized": false,
                      "layout": { "pos_in_scrolling_layout": [1, 1], "tile_size": [100.0, 100.0],
                        "window_size": [100, 100], "tile_pos_in_workspace_view": [0.0, 0.0],
                        "window_offset_in_tile": [0.0, 0.0] }, "focus_timestamp": null },
                ] } }),
            ],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let value = windows
            .call("find", &json!({ "output": "HDMI-1" }))
            .unwrap();
        assert_eq!(value["count"], 1);
        assert_eq!(value["windows"][0]["id"], 11);
        server.join().unwrap();
    }

    fn sticky_window_json(id: u64, app_id: &str, focused: bool, sticky: bool) -> serde_json::Value {
        let mut window = window_json(id, app_id, focused);
        window["is_sticky"] = json!(sticky);
        window
    }

    #[test]
    fn toggles_the_native_sticky_flag_and_reports_the_new_state() {
        let dir = temp_dir("sticky-toggle");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Windows": [sticky_window_json(10, "firefox", true, false)] } }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let value = windows.call("toggle-sticky", &json!({})).unwrap();
        assert_eq!(value["window"], 10);
        assert_eq!(value["previous"], false);
        assert_eq!(value["sticky"], true);

        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Windows));
        assert!(matches!(
            requests[1],
            Request::Action(Action::ToggleWindowSticky { id: Some(10) })
        ));
    }

    #[test]
    fn set_sticky_skips_the_action_when_the_state_already_matches() {
        let dir = temp_dir("sticky-idempotent");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![json!({ "Ok": { "Windows": [sticky_window_json(10, "firefox", true, true)] } })],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let value = windows
            .call("set-sticky", &json!({ "window": 10, "sticky": true }))
            .unwrap();
        assert_eq!(value["changed"], false);
        assert_eq!(value["sticky"], true);

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], Request::Windows));
    }

    #[test]
    fn set_sticky_toggles_only_when_the_target_differs() {
        let dir = temp_dir("sticky-set");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Windows": [sticky_window_json(10, "firefox", false, false)] } }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let value = windows
            .call("set-sticky", &json!({ "window": 10, "sticky": true }))
            .unwrap();
        assert_eq!(value["changed"], true);
        assert_eq!(value["sticky"], true);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[1],
            Request::Action(Action::ToggleWindowSticky { id: Some(10) })
        ));
    }

    #[test]
    fn lists_sticky_windows_in_id_order() {
        let dir = temp_dir("sticky-list");
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![json!({ "Ok": { "Windows": [
                sticky_window_json(12, "a", false, true),
                sticky_window_json(10, "b", true, true),
                sticky_window_json(11, "c", false, false),
            ] } })],
        );
        let windows = Windows::new(Compositor::at(&socket));
        let value = windows.call("list-sticky", &Value::Null).unwrap();
        assert_eq!(value["count"], 2);
        assert_eq!(value["windows"][0]["id"], 10);
        assert_eq!(value["windows"][1]["id"], 12);
        server.join().unwrap();
    }
}
