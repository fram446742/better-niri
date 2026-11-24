//! Window-oriented actions: run-or-raise and singleton.
//!
//! Matches live niri window state (falling back to nothing rather than stale
//! cached data), raises an existing match, or spawns the configured command.

use companion_core::{ActionInfo, Context};
use niri_ipc::{Action, Request, Response, Window};
use serde_json::{Value, json};

use crate::Compositor;

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

    fn matches(window: &Window, params: &Value) -> bool {
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
        true
    }

    fn apply_action(&self, action: Action) -> Result<(), String> {
        match self.compositor.send(Request::Action(action))? {
            Response::Handled => Ok(()),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
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

    /// All matches sorted by window id. Raising prefers the lowest-id focused
    /// match, then the lowest id, so multiple matches behave deterministically
    /// regardless of niri's listing order.
    fn matches_sorted(&self, params: &Value) -> Result<Vec<Window>, String> {
        let mut matched: Vec<Window> = self
            .windows()?
            .into_iter()
            .filter(|window| Self::matches(window, params))
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
            ActionInfo::new("find", "Return matching windows without acting"),
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
                        })
                    })
                    .collect();
                Ok(json!({ "windows": matched, "count": count }))
            }
            "run-or-raise" | "singleton" => {
                let (found, candidate_ids) = self.find(params)?;
                if let Some(window) = found {
                    let was_focused = window.is_focused;
                    self.apply_action(Action::FocusWindow { id: window.id })?;
                    return Ok(json!({
                        "action": "raised",
                        "raised": window.id,
                        "title": window.title,
                        "app_id": window.app_id,
                        "workspace_id": window.workspace_id,
                        "was_focused": was_focused,
                        "matches": candidate_ids.len(),
                        "candidate_ids": candidate_ids,
                    }));
                }
                self.spawn(params)
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

    /// Accepts two requests: the Windows query and one Action.
    fn fake_niri(
        path: &PathBuf,
        windows: Vec<serde_json::Value>,
    ) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            for step in 0..2 {
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
        let server = fake_niri(&socket, vec![window_json(10, "firefox", false)]);
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
        let server = fake_niri(&socket, vec![]);
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
        assert_eq!(result["matches"], 3);
        assert_eq!(result["candidate_ids"], json!([10, 11, 12]));

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[1],
            Request::Action(Action::FocusWindow { id: 11 })
        ));
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
}
