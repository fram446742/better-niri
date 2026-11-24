//! Persistent window marks.
//!
//! A mark is one short, unique name attached to a window id. Marks are stored in
//! `$XDG_STATE_HOME/better-companion/marks.json` so they survive restarts, are
//! exposed by `windows.find`, and can be used as a rule condition and as a
//! `windows.*` selector.
//!
//! Marks are intentionally separate from niri's window state: niri has no mark
//! primitive, and window ids are only unique for the lifetime of a window.
//! `marks.reconcile` drops marks whose window is gone.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use companion_core::{ActionInfo, Context, ParamInfo, load_json, save_json};
use niri_ipc::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Compositor;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    marks: BTreeMap<String, String>,
}

/// Shared mark store. Cheap to clone; clones share the same state.
#[derive(Clone)]
pub struct Marks {
    path: Arc<PathBuf>,
    state: Arc<Mutex<State>>,
    compositor: Compositor,
}

impl Marks {
    pub fn load(path: impl Into<PathBuf>, compositor: Compositor) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path: Arc::new(path),
            state: Arc::new(Mutex::new(state)),
            compositor,
        })
    }

    pub fn from_state_dir(compositor: Compositor) -> std::io::Result<Self> {
        Self::load(default_path(), compositor)
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(self.path.as_path(), state).map_err(|err| format!("cannot save marks: {err}"))
    }

    /// Returns the mark attached to `window`, if any.
    pub fn mark_of(&self, window: u64) -> Option<String> {
        self.state
            .lock()
            .ok()?
            .marks
            .get(&window.to_string())
            .cloned()
    }

    /// Returns the window carrying exactly `mark`, if any.
    pub fn window_of(&self, mark: &str) -> Option<u64> {
        let state = self.state.lock().ok()?;
        state
            .marks
            .iter()
            .find(|(_, value)| value.as_str() == mark)
            .and_then(|(window, _)| window.parse().ok())
    }

    /// All marks as `window -> mark`, sorted by numeric window id.
    pub fn snapshot(&self) -> BTreeMap<u64, String> {
        self.state
            .lock()
            .map(|state| {
                state
                    .marks
                    .iter()
                    .filter_map(|(window, mark)| Some((window.parse().ok()?, mark.clone())))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn resolve_window(&self, params: &Value) -> Result<u64, String> {
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

    fn set(&self, params: &Value) -> Result<Value, String> {
        let mark = params
            .get("mark")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|mark| !mark.is_empty())
            .ok_or_else(|| "marks.set needs a non-empty string `mark`".to_owned())?
            .to_owned();
        if mark.len() > 128 {
            return Err("`mark` must be at most 128 bytes".to_owned());
        }
        let window = self.resolve_window(params)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "marks state lock poisoned".to_owned())?;
        let previous = state.marks.insert(window.to_string(), mark.clone());
        let mut moved_from: Option<u64> = None;
        state.marks.retain(|other, value| {
            if other != &window.to_string() && value.as_str() == mark {
                moved_from = other.parse().ok();
                false
            } else {
                true
            }
        });
        self.save(&state)?;
        Ok(json!({
            "window": window,
            "mark": mark,
            "previous": previous,
            "moved_from": moved_from,
        }))
    }

    fn remove(&self, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "marks state lock poisoned".to_owned())?;
        let (window, mark) = if let Some(id) = params
            .get("window")
            .or_else(|| params.get("id"))
            .and_then(Value::as_u64)
        {
            let key = id.to_string();
            match state.marks.remove(&key) {
                Some(mark) => (Some(id), mark),
                None => (Some(id), String::new()),
            }
        } else if let Some(mark) = params.get("mark").and_then(Value::as_str) {
            let found = state
                .marks
                .iter()
                .find(|(_, value)| value.as_str() == mark)
                .map(|(window, _)| window.clone());
            match found {
                Some(key) => {
                    let mark = state.marks.remove(&key).unwrap_or_default();
                    (key.parse().ok(), mark)
                }
                None => (None, mark.to_owned()),
            }
        } else {
            return Err("marks.remove needs `window` or `mark`".to_owned());
        };
        let removed = !mark.is_empty();
        self.save(&state)?;
        Ok(json!({ "window": window, "mark": mark, "removed": removed }))
    }

    fn get(&self, params: &Value) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "marks state lock poisoned".to_owned())?;
        if let Some(mark) = params.get("mark").and_then(Value::as_str) {
            let window = state
                .marks
                .iter()
                .find(|(_, value)| value.as_str() == mark)
                .and_then(|(window, _)| window.parse::<u64>().ok());
            return Ok(json!({ "mark": mark, "window": window }));
        }
        let window = self.resolve_window(params)?;
        let mark = state.marks.get(&window.to_string()).cloned();
        Ok(json!({ "window": window, "mark": mark }))
    }

    fn list(&self) -> Value {
        let mut entries: Vec<Value> = self
            .snapshot()
            .into_iter()
            .map(|(window, mark)| json!({ "window": window, "mark": mark }))
            .collect();
        entries.sort_by_key(|entry| entry["window"].as_u64());
        json!({ "marks": entries, "count": entries.len() })
    }

    fn reconcile(&self) -> Result<Value, String> {
        let open: Vec<u64> = match self.compositor.send(Request::Windows)? {
            Response::Windows(windows) => windows.into_iter().map(|window| window.id).collect(),
            other => return Err(format!("unexpected niri response: {other:?}")),
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "marks state lock poisoned".to_owned())?;
        let mut removed = Vec::new();
        state.marks.retain(|window, _| {
            let keep = window.parse::<u64>().is_ok_and(|id| open.contains(&id));
            if !keep && let Ok(id) = window.parse::<u64>() {
                removed.push(id);
            }
            keep
        });
        removed.sort_unstable();
        self.save(&state)?;
        Ok(json!({ "removed": removed, "count": removed.len() }))
    }
}

impl Context for Marks {
    fn namespace(&self) -> &'static str {
        "marks"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        let mut actions = vec![
            ActionInfo::new("set", "Attach `mark` to `window` (or the focused window)")
                .with_params([
                    ParamInfo::integer(
                        "window",
                        false,
                        "window id, defaults to the focused window",
                    ),
                    ParamInfo::string("mark", true, "unique mark name"),
                ])
                .with_permissions(&["niri"]),
            ActionInfo::new("remove", "Remove a mark by `window` or `mark`").with_params([
                ParamInfo::integer("window", false, "window id"),
                ParamInfo::string("mark", false, "mark name"),
            ]),
            ActionInfo::new("get", "Look up a mark by `window` or `mark`").with_params([
                ParamInfo::integer("window", false, "window id"),
                ParamInfo::string("mark", false, "mark name"),
            ]),
            ActionInfo::new("list", "List marks in numeric window-id order"),
            ActionInfo::new(
                "reconcile",
                "Drop marks whose window no longer exists (queries niri)",
            )
            .with_permissions(&["niri"]),
        ];
        if !self.compositor.is_available() {
            actions
                .iter_mut()
                .for_each(|action| action.available = false);
        }
        actions
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "set" => self.set(params),
            "remove" => self.remove(params),
            "get" => self.get(params),
            "list" => Ok(self.list()),
            "reconcile" => self.reconcile(),
            other => Err(format!("unknown action `marks.{other}`")),
        }
    }
}

/// Default mark store path.
pub fn default_path() -> PathBuf {
    companion_core::state_dir().join("marks.json")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-marks-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_niri(socket: &PathBuf, replies: Vec<serde_json::Value>) -> std::thread::JoinHandle<()> {
        let listener = UnixListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                stream
                    .write_all(serde_json::to_string(&reply).unwrap().as_bytes())
                    .unwrap();
                stream.write_all(b"\n").unwrap();
            }
        })
    }

    fn focused_reply(id: u64) -> serde_json::Value {
        json!({ "Ok": { "FocusedWindow": {
            "id": id, "title": "t", "app_id": "a", "pid": 1,
            "workspace_id": 1, "is_focused": true, "is_floating": false,
            "is_sticky": false, "is_urgent": false, "is_minimized": false,
            "layout": { "pos_in_scrolling_layout": [1,1], "tile_size": [1.0,1.0],
                "window_size": [1,1], "tile_pos_in_workspace_view": [0.0,0.0],
                "window_offset_in_tile": [0.0,0.0] },
            "focus_timestamp": null
        } } })
    }

    #[test]
    fn set_get_list_move_and_persist_marks() {
        let dir = temp_dir("persist");
        let marks = Marks::load(
            dir.join("marks.json"),
            Compositor::at(dir.join("niri.sock")),
        )
        .unwrap();

        marks
            .call("set", &json!({ "window": 10, "mark": "editor" }))
            .unwrap();
        marks
            .call("set", &json!({ "window": 11, "mark": "browser" }))
            .unwrap();
        assert_eq!(marks.mark_of(10).as_deref(), Some("editor"));
        assert_eq!(marks.window_of("browser"), Some(11));

        // Re-using a mark moves it and reports the previous window.
        let moved = marks
            .call("set", &json!({ "window": 12, "mark": "editor" }))
            .unwrap();
        assert_eq!(moved["moved_from"], 10);
        assert_eq!(marks.mark_of(10), None);
        assert_eq!(marks.mark_of(12).as_deref(), Some("editor"));

        let listed = marks.call("list", &Value::Null).unwrap();
        assert_eq!(listed["count"], 2);
        assert_eq!(listed["marks"][0]["window"], 11);

        let reloaded = Marks::load(
            dir.join("marks.json"),
            Compositor::at(dir.join("niri.sock")),
        )
        .unwrap();
        assert_eq!(reloaded.mark_of(12).as_deref(), Some("editor"));
    }

    #[test]
    fn remove_by_window_or_mark_and_get_reports_missing() {
        let dir = temp_dir("remove");
        let marks = Marks::load(
            dir.join("marks.json"),
            Compositor::at(dir.join("niri.sock")),
        )
        .unwrap();
        marks
            .call("set", &json!({ "window": 1, "mark": "a" }))
            .unwrap();
        marks
            .call("set", &json!({ "window": 2, "mark": "b" }))
            .unwrap();

        assert_eq!(
            marks.call("remove", &json!({ "mark": "a" })).unwrap()["removed"],
            true
        );
        assert_eq!(
            marks.call("remove", &json!({ "window": 2 })).unwrap()["removed"],
            true
        );
        assert_eq!(
            marks.call("remove", &json!({ "window": 2 })).unwrap()["removed"],
            false
        );
        assert!(marks.call("get", &json!({ "window": 1 })).unwrap()["mark"].is_null());
        assert!(marks.call("remove", &json!({})).is_err());
    }

    #[test]
    fn set_defaults_to_the_focused_window() {
        let dir = temp_dir("focused");
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, vec![focused_reply(42)]);
        let marks = Marks::load(dir.join("marks.json"), Compositor::at(&socket)).unwrap();
        let value = marks.call("set", &json!({ "mark": "term" })).unwrap();
        assert_eq!(value["window"], 42);
        server.join().unwrap();
    }

    #[test]
    fn actions_are_marked_unavailable_without_a_niri_socket() {
        let dir = temp_dir("availability");
        let marks = Marks::load(
            dir.join("marks.json"),
            Compositor::at(dir.join("missing.sock")),
        )
        .unwrap();
        assert!(marks.actions().iter().all(|action| !action.available));

        let socket = dir.join("niri.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let marks = Marks::load(dir.join("marks.json"), Compositor::at(&socket)).unwrap();
        assert!(marks.actions().iter().all(|action| action.available));
    }

    #[test]
    fn reconcile_drops_stale_marks() {
        let dir = temp_dir("reconcile");
        let socket = dir.join("niri.sock");
        let server = fake_niri(
            &socket,
            vec![json!({ "Ok": { "Windows": [
                { "id": 10, "title": "t", "app_id": "a", "pid": 1,
                  "workspace_id": 1, "is_focused": true, "is_floating": false,
                  "is_sticky": false, "is_urgent": false, "is_minimized": false,
                  "layout": { "pos_in_scrolling_layout": [1,1], "tile_size": [1.0,1.0],
                    "window_size": [1,1], "tile_pos_in_workspace_view": [0.0,0.0],
                    "window_offset_in_tile": [0.0,0.0] },
                  "focus_timestamp": null }
            ] } })],
        );
        let marks = Marks::load(dir.join("marks.json"), Compositor::at(&socket)).unwrap();
        marks
            .call("set", &json!({ "window": 10, "mark": "live" }))
            .unwrap();
        marks
            .call("set", &json!({ "window": 99, "mark": "stale" }))
            .unwrap();
        let value = marks.call("reconcile", &Value::Null).unwrap();
        assert_eq!(value["removed"], json!([99]));
        assert_eq!(marks.mark_of(10).as_deref(), Some("live"));
        server.join().unwrap();
    }
}
