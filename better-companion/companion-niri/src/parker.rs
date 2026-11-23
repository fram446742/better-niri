//! Compositor-side parking actions.
//!
//! `parker.*` consumes the auto-tracked parking lots and the niri IPC bridge:
//! parking a window minimizes it and records its snapshot; restoring
//! unminimizes it. Parking a workspace minimizes all of its windows and records
//! the group, so it can be restored (and optionally focused) later.

use std::collections::BTreeMap;

use companion_core::{ActionInfo, Context};
use companion_parking::Parking;
use niri_ipc::{Action, Request, Response, Window, Workspace, WorkspaceReferenceArg};
use serde_json::{json, Value};

use crate::{Compositor, AUTO_WINDOWS, AUTO_WORKSPACES};

pub const DEFAULT_LOT: &str = "parker";

pub struct Parker {
    parking: Parking,
    compositor: Compositor,
}

impl Parker {
    pub fn new(parking: Parking, compositor: Compositor) -> Self {
        Self {
            parking,
            compositor,
        }
    }

    fn lot_name(params: &Value) -> String {
        params
            .get("lot")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_LOT)
            .to_owned()
    }

    fn auto_windows(&self) -> BTreeMap<String, Value> {
        self.parking.lot(AUTO_WINDOWS)
    }

    fn window_query(&self) -> Result<Vec<Window>, String> {
        match self.compositor.send(Request::Windows)? {
            Response::Windows(windows) => Ok(windows),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn workspace_query(&self) -> Result<Vec<Workspace>, String> {
        match self.compositor.send(Request::Workspaces)? {
            Response::Workspaces(workspaces) => Ok(workspaces),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn focused_window_id(&self) -> Result<u64, String> {
        if let Some((id, _)) = self
            .auto_windows()
            .iter()
            .find(|(_, value)| value.get("is_focused").and_then(Value::as_bool) == Some(true))
        {
            return id
                .parse()
                .map_err(|_| format!("invalid window id `{id}` in {AUTO_WINDOWS}"));
        }

        match self.compositor.send(Request::FocusedWindow)? {
            Response::FocusedWindow(Some(window)) => Ok(window.id),
            Response::FocusedWindow(None) => Err("no focused window".to_owned()),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn focused_workspace_id(&self) -> Result<u64, String> {
        let workspaces = self.parking.lot(AUTO_WORKSPACES);
        let from_auto = workspaces
            .values()
            .find(|value| value.get("is_focused").and_then(Value::as_bool) == Some(true))
            .or_else(|| {
                workspaces
                    .values()
                    .find(|value| value.get("is_active").and_then(Value::as_bool) == Some(true))
            })
            .and_then(|value| value.get("id").and_then(Value::as_u64));
        if let Some(id) = from_auto {
            return Ok(id);
        }

        self.workspace_query()?
            .into_iter()
            .find(|ws| ws.is_focused)
            .map(|ws| ws.id)
            .ok_or_else(|| "no focused workspace".to_owned())
    }

    fn window_snapshot(&self, id: u64) -> Result<Value, String> {
        if let Some(value) = self.auto_windows().get(&id.to_string()) {
            return Ok(value.clone());
        }
        let window = self
            .window_query()?
            .into_iter()
            .find(|window| window.id == id)
            .ok_or_else(|| format!("window {id} is not open"))?;
        serde_json::to_value(window).map_err(|err| format!("encode window: {err}"))
    }

    fn workspace_snapshot(&self, id: u64) -> Result<Value, String> {
        if let Some(value) = self.parking.lot(AUTO_WORKSPACES).get(&id.to_string()) {
            return Ok(value.clone());
        }
        let workspace = self
            .workspace_query()?
            .into_iter()
            .find(|workspace| workspace.id == id)
            .ok_or_else(|| format!("workspace {id} does not exist"))?;
        serde_json::to_value(workspace).map_err(|err| format!("encode workspace: {err}"))
    }

    fn workspace_window_ids(&self, workspace_id: u64) -> Result<Vec<u64>, String> {
        let from_auto: Vec<u64> = self
            .auto_windows()
            .iter()
            .filter_map(|(id, value)| {
                (value.get("workspace_id").and_then(Value::as_u64) == Some(workspace_id))
                    .then(|| id.parse().ok())
                    .flatten()
            })
            .collect();
        if !from_auto.is_empty() {
            return Ok(from_auto);
        }
        Ok(self
            .window_query()?
            .into_iter()
            .filter(|window| window.workspace_id == Some(workspace_id))
            .map(|window| window.id)
            .collect())
    }

    fn send_action(&self, action: Action) -> Result<(), String> {
        match self.compositor.send(Request::Action(action))? {
            Response::Handled => Ok(()),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn param_id(params: &Value) -> Option<u64> {
        params
            .get("window")
            .or_else(|| params.get("id"))
            .and_then(Value::as_u64)
    }
}

impl Context for Parker {
    fn namespace(&self) -> &'static str {
        "parker"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new(
                "park",
                "Minimize `window` (or the focused window) and record it",
            ),
            ActionInfo::new(
                "restore",
                "Unminimize a parked `window` and drop its record",
            ),
            ActionInfo::new(
                "park-workspace",
                "Minimize every window on `workspace` and record the group",
            ),
            ActionInfo::new(
                "restore-workspace",
                "Unminimize a parked `workspace`; `focus` it optionally",
            ),
            ActionInfo::new("list", "List the parking lot (`lot`, default `parker`)"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let lot = Self::lot_name(params);
        match action {
            "park" => {
                let id = match Self::param_id(params) {
                    Some(id) => id,
                    None => self.focused_window_id()?,
                };
                let snapshot = self.window_snapshot(id)?;
                self.send_action(Action::MinimizeWindow { id: Some(id) })?;
                self.parking.call(
                    "park",
                    &json!({ "lot": lot, "id": format!("window:{id}"), "value": snapshot }),
                )?;
                Ok(json!({ "lot": lot, "window": id, "parked": true }))
            }
            "restore" => {
                let id = Self::param_id(params)
                    .ok_or_else(|| "parker.restore needs a `window` id".to_owned())?;
                let entry = self.parking.call(
                    "unpark",
                    &json!({ "lot": lot, "id": format!("window:{id}") }),
                )?;
                self.send_action(Action::UnminimizeWindow { id: Some(id) })?;
                Ok(json!({ "lot": lot, "window": id, "restored": true, "entry": entry["value"] }))
            }
            "park-workspace" => {
                let workspace_id = match params.get("workspace").and_then(Value::as_u64) {
                    Some(id) => id,
                    None => self.focused_workspace_id()?,
                };
                let snapshot = self.workspace_snapshot(workspace_id)?;
                let window_ids = self.workspace_window_ids(workspace_id)?;
                if window_ids.is_empty() {
                    return Err(format!("workspace {workspace_id} has no windows to park"));
                }
                let mut minimized = Vec::with_capacity(window_ids.len());
                for id in &window_ids {
                    self.send_action(Action::MinimizeWindow { id: Some(*id) })?;
                    minimized.push(*id);
                }
                self.parking.call(
                    "park",
                    &json!({
                        "lot": lot,
                        "id": format!("workspace:{workspace_id}"),
                        "value": {
                            "workspace": snapshot,
                            "windows": minimized,
                        }
                    }),
                )?;
                Ok(json!({ "lot": lot, "workspace": workspace_id, "windows": minimized }))
            }
            "restore-workspace" => {
                let workspace_id = params
                    .get("workspace")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "parker.restore-workspace needs a `workspace` id".to_owned())?;
                let entry = self.parking.call(
                    "unpark",
                    &json!({ "lot": lot, "id": format!("workspace:{workspace_id}") }),
                )?;
                let windows = entry["value"]["windows"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut restored = Vec::with_capacity(windows.len());
                for id in windows {
                    if let Some(id) = id.as_u64() {
                        self.send_action(Action::UnminimizeWindow { id: Some(id) })?;
                        restored.push(id);
                    }
                }
                if params
                    .get("focus")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    self.send_action(Action::FocusWorkspace {
                        reference: WorkspaceReferenceArg::Id(workspace_id),
                    })?;
                }
                Ok(json!({
                    "lot": lot,
                    "workspace": workspace_id,
                    "windows": restored,
                    "focused": params.get("focus").and_then(Value::as_bool).unwrap_or(false),
                }))
            }
            "list" => Ok(json!({ "lot": lot, "entries": self.parking.lot(&lot) })),
            other => Err(format!("unknown action `parker.{other}`")),
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
            "better-companion-parker-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed(parking: &Parking) {
        let windows = [
            (
                "10".to_owned(),
                json!({ "id": 10, "workspace_id": 7, "is_focused": true, "title": "a" }),
            ),
            (
                "11".to_owned(),
                json!({ "id": 11, "workspace_id": 7, "is_focused": false, "title": "b" }),
            ),
        ]
        .into_iter()
        .collect();
        parking.replace_lot(AUTO_WINDOWS, windows).unwrap();
        parking
            .replace_lot(
                AUTO_WORKSPACES,
                [(
                    "7".to_owned(),
                    json!({ "id": 7, "is_focused": true, "is_active": true, "name": "main" }),
                )]
                .into_iter()
                .collect(),
            )
            .unwrap();
    }

    /// Accepts `expected` one-request connections and records the requests.
    fn fake_niri(path: &PathBuf, expected: usize) -> std::thread::JoinHandle<Vec<Request>> {
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

    #[test]
    fn parks_the_focused_window_and_restores_it() {
        let dir = temp_dir("window");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);

        let parker = Parker::new(parking.clone(), Compositor::at(&socket));
        let parked = parker.call("park", &json!({})).unwrap();
        assert_eq!(parked["window"], 10);
        let lot = parking.lot(DEFAULT_LOT);
        assert_eq!(lot["window:10"]["title"], "a");

        let restored = parker.call("restore", &json!({ "window": 10 })).unwrap();
        assert_eq!(restored["restored"], true);
        assert!(parking.lot(DEFAULT_LOT).is_empty());

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
    }

    #[test]
    fn parks_a_workspace_and_restores_it_with_focus() {
        let dir = temp_dir("workspace");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 5);

        let parker = Parker::new(parking.clone(), Compositor::at(&socket));
        let parked = parker
            .call("park-workspace", &json!({ "workspace": 7 }))
            .unwrap();
        assert_eq!(parked["windows"], json!([10, 11]));
        let lot = parking.lot(DEFAULT_LOT);
        assert_eq!(lot["workspace:7"]["windows"], json!([10, 11]));
        assert_eq!(lot["workspace:7"]["workspace"]["name"], "main");

        let restored = parker
            .call(
                "restore-workspace",
                &json!({ "workspace": 7, "focus": true }),
            )
            .unwrap();
        assert_eq!(restored["windows"], json!([10, 11]));
        assert_eq!(restored["focused"], true);
        assert!(parking.lot(DEFAULT_LOT).is_empty());

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::MinimizeWindow { id: Some(11) })
        ));
        assert!(matches!(
            requests[2],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[3],
            Request::Action(Action::UnminimizeWindow { id: Some(11) })
        ));
        assert!(matches!(
            requests[4],
            Request::Action(Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Id(7)
            })
        ));
    }

    #[test]
    fn restore_requires_an_id() {
        let dir = temp_dir("missing-id");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        let parker = Parker::new(parking, Compositor::at(dir.join("unused.sock")));
        assert!(parker.call("restore", &json!({})).is_err());
        assert!(parker.call("restore-workspace", &json!({})).is_err());
    }
}
