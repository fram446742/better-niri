//! Compositor-side parking actions.
//!
//! `parker.*` consumes the auto-tracked parking lots and the niri IPC bridge:
//! parking a window minimizes it and records its snapshot; restoring
//! unminimizes it. Parking a workspace minimizes all of its windows and records
//! the group, so it can be restored (and optionally focused) later.

use std::collections::BTreeMap;

use companion_core::{ActionInfo, Context, ParamInfo};
use companion_parking::Parking;
use niri_ipc::{Action, Workspace, WorkspaceReferenceArg};
use serde_json::{Value, json};

use crate::layout::{self, WorkspaceTarget};
use crate::{AUTO_WORKSPACES, Compositor, ops};

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
        ops::auto_windows(&self.parking)
    }

    fn workspace_query(&self) -> Result<Vec<Workspace>, String> {
        ops::workspace_query(&self.compositor)
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
        let mut from_auto: Vec<u64> = self
            .auto_windows()
            .iter()
            .filter_map(|(id, value)| {
                (value.get("workspace_id").and_then(Value::as_u64) == Some(workspace_id))
                    .then(|| id.parse().ok())
                    .flatten()
            })
            .collect();
        if !from_auto.is_empty() {
            from_auto.sort_unstable();
            return Ok(from_auto);
        }
        let mut ids: Vec<u64> = ops::window_query(&self.compositor)?
            .into_iter()
            .filter(|window| window.workspace_id == Some(workspace_id))
            .map(|window| window.id)
            .collect();
        ids.sort_unstable();
        Ok(ids)
    }

    /// Resolves an optional workspace selector to a stable id.
    ///
    /// A plain id uses the auto-tracked workspace state when it is present and
    /// does not require an extra niri query; names and indices resolve through
    /// the live workspace list (falling back to the cache when IPC is down).
    fn workspace_target_id(&self, params: &Value) -> Result<Option<u64>, String> {
        let Some(target) = WorkspaceTarget::from_params(params)? else {
            return Ok(None);
        };
        if let Some(id) = target.id
            && target.name.is_none()
            && target.index.is_none()
            && target.output.is_none()
        {
            // niri validates the id; the auto lot is only a fast path.
            return Ok(Some(id));
        }
        let workspaces = layout::query_or_cached_workspaces(&self.compositor, &self.parking)?;
        layout::resolve_workspace(&target, &workspaces)
            .map(|workspace| Some(workspace.id))
            .map_err(|err| {
                format!(
                    "{err}; known workspaces: {}",
                    layout::describe_workspaces(&workspaces)
                )
            })
    }
}

impl Context for Parker {
    fn namespace(&self) -> &'static str {
        "parker"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        let mut actions = vec![
            ActionInfo::new(
                "park",
                "Minimize `window` (or the focused window) and record it",
            )
            .with_params([
                ParamInfo::integer("window", false, "window id, defaults to the focused window"),
                ParamInfo::string("lot", false, "parking lot (default `parker`)"),
            ])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "restore",
                "Unminimize a parked `window` and drop its record",
            )
            .with_params([
                ParamInfo::integer("window", true, "parked window id"),
                ParamInfo::string("lot", false, "parking lot (default `parker`)"),
            ])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "park-workspace",
                "Minimize every window on `workspace` (id, name or index) and record the group",
            )
            .with_params([
                ParamInfo::integer("workspace", false, "workspace id"),
                ParamInfo::string("workspace_name", false, "workspace name"),
                ParamInfo::integer("workspace_idx", false, "per-output workspace index"),
                ParamInfo::string(
                    "workspace_output",
                    false,
                    "restrict name/index to an output",
                ),
                ParamInfo::string("lot", false, "parking lot (default `parker`)"),
            ])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "restore-workspace",
                "Unminimize a parked `workspace`; `focus` it optionally",
            )
            .with_params([
                ParamInfo::integer("workspace", false, "workspace id"),
                ParamInfo::string("workspace_name", false, "workspace name"),
                ParamInfo::integer("workspace_idx", false, "per-output workspace index"),
                ParamInfo::string(
                    "workspace_output",
                    false,
                    "restrict name/index to an output",
                ),
                ParamInfo::boolean("focus", false, "focus the restored workspace"),
                ParamInfo::string("lot", false, "parking lot (default `parker`)"),
            ])
            .with_permissions(&["niri"]),
            ActionInfo::new("list", "List the parking lot (`lot`, default `parker`)").with_params(
                [ParamInfo::string(
                    "lot",
                    false,
                    "parking lot (default `parker`)",
                )],
            ),
        ];
        if !self.compositor.is_available() {
            actions
                .iter_mut()
                .for_each(|action| action.available = false);
        }
        actions
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let lot = Self::lot_name(params);
        match action {
            "park" => {
                let id = match ops::param_window(params) {
                    Some(id) => id,
                    None => ops::focused_window_id(&self.parking, &self.compositor)?,
                };
                let snapshot = ops::window_snapshot(&self.parking, &self.compositor, id)?;
                ops::send_action(&self.compositor, Action::MinimizeWindow { id: Some(id) })?;
                self.parking.call(
                    "park",
                    &json!({ "lot": lot, "id": format!("window:{id}"), "value": snapshot }),
                )?;
                Ok(json!({ "lot": lot, "window": id, "parked": true }))
            }
            "restore" => {
                let id = ops::param_window(params)
                    .ok_or_else(|| "parker.restore needs a `window` id".to_owned())?;
                // Atomically remove: a repeated or concurrent restore reports
                // `restored: false` and sends no second unminimize.
                let Some(entry) = self
                    .parking
                    .take(&lot, &format!("window:{id}"))
                    .map_err(|err| format!("cannot restore window {id}: {err}"))?
                else {
                    return Ok(json!({
                        "lot": lot,
                        "window": id,
                        "restored": false,
                    }));
                };
                ops::send_action(&self.compositor, Action::UnminimizeWindow { id: Some(id) })?;
                Ok(json!({ "lot": lot, "window": id, "restored": true, "entry": entry }))
            }
            "park-workspace" => {
                let workspace_id = match self.workspace_target_id(params)? {
                    Some(id) => id,
                    None => ops::focused_workspace_id(&self.parking, &self.compositor)?,
                };
                let snapshot = self.workspace_snapshot(workspace_id)?;
                let window_ids = self.workspace_window_ids(workspace_id)?;
                if window_ids.is_empty() {
                    return Err(format!("workspace {workspace_id} has no windows to park"));
                }
                let mut minimized = Vec::with_capacity(window_ids.len());
                for id in &window_ids {
                    ops::send_action(&self.compositor, Action::MinimizeWindow { id: Some(*id) })?;
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
                let workspace_id = self.workspace_target_id(params)?.ok_or_else(|| {
                    "parker.restore-workspace needs a `workspace` id, \
                         `workspace_name` or `workspace_idx`"
                        .to_owned()
                })?;
                let Some(entry) = self
                    .parking
                    .take(&lot, &format!("workspace:{workspace_id}"))
                    .map_err(|err| format!("cannot restore workspace {workspace_id}: {err}"))?
                else {
                    return Ok(json!({
                        "lot": lot,
                        "workspace": workspace_id,
                        "windows": [],
                        "restored": false,
                        "focused": false,
                    }));
                };
                let mut windows: Vec<u64> = entry
                    .get("windows")
                    .and_then(Value::as_array)
                    .map(|windows| windows.iter().filter_map(Value::as_u64).collect::<Vec<_>>())
                    .unwrap_or_default();
                windows.sort_unstable();
                let mut restored = Vec::with_capacity(windows.len());
                for id in windows {
                    ops::send_action(&self.compositor, Action::UnminimizeWindow { id: Some(id) })?;
                    restored.push(id);
                }
                let focus = params
                    .get("focus")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if focus {
                    ops::send_action(
                        &self.compositor,
                        Action::FocusWorkspace {
                            reference: WorkspaceReferenceArg::Id(workspace_id),
                        },
                    )?;
                }
                Ok(json!({
                    "lot": lot,
                    "workspace": workspace_id,
                    "windows": restored,
                    "restored": true,
                    "focused": focus,
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

    use companion_core::AUTO_WINDOWS;
    use niri_ipc::Request;
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-parker-{}-{name}",
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

    fn workspace_json(id: u64, idx: u8, name: &str, output: &str) -> serde_json::Value {
        json!({
            "id": id,
            "idx": idx,
            "name": name,
            "output": output,
            "is_urgent": false,
            "is_active": true,
            "is_focused": true,
            "active_window_id": null,
            "is_hidden": false
        })
    }

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
    fn parks_and_restores_a_workspace_by_name() {
        let dir = temp_dir("workspace-name");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Workspaces": [
                    workspace_json(7, 1, "main", "eDP-1"),
                ] } }),
                json!({ "Ok": "Handled" }),
                json!({ "Ok": "Handled" }),
                json!({ "Ok": { "Workspaces": [
                    workspace_json(7, 1, "main", "eDP-1"),
                ] } }),
                json!({ "Ok": "Handled" }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let parker = Parker::new(parking.clone(), Compositor::at(&socket));
        let parked = parker
            .call("park-workspace", &json!({ "workspace_name": "main" }))
            .unwrap();
        assert_eq!(parked["workspace"], 7);
        assert_eq!(parked["windows"], json!([10, 11]));

        let restored = parker
            .call("restore-workspace", &json!({ "workspace_name": "main" }))
            .unwrap();
        assert_eq!(restored["workspace"], 7);
        assert_eq!(restored["windows"], json!([10, 11]));
        assert_eq!(restored["restored"], true);
        assert!(parking.lot(DEFAULT_LOT).is_empty());

        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Workspaces));
        assert!(matches!(
            requests[1],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[2],
            Request::Action(Action::MinimizeWindow { id: Some(11) })
        ));
        assert!(matches!(requests[3], Request::Workspaces));
        assert!(matches!(
            requests[4],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[5],
            Request::Action(Action::UnminimizeWindow { id: Some(11) })
        ));
    }

    #[test]
    fn restore_is_idempotent_and_does_not_resend_actions() {
        let dir = temp_dir("restore-twice");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let parker = Parker::new(parking.clone(), Compositor::at(&socket));

        parker.call("park", &json!({ "window": 10 })).unwrap();
        let first = parker.call("restore", &json!({ "window": 10 })).unwrap();
        assert_eq!(first["restored"], true);
        let second = parker.call("restore", &json!({ "window": 10 })).unwrap();
        assert_eq!(second["restored"], false);

        // Repeated workspace restore behaves the same way.
        let socket = dir.join("niri2.sock");
        let server2 = fake_niri(&socket, 4);
        let parker2 = Parker::new(parking.clone(), Compositor::at(&socket));
        parker2
            .call("park-workspace", &json!({ "workspace": 7 }))
            .unwrap();
        let first = parker2
            .call("restore-workspace", &json!({ "workspace": 7 }))
            .unwrap();
        assert_eq!(first["restored"], true);
        let second = parker2
            .call("restore-workspace", &json!({ "workspace": 7 }))
            .unwrap();
        assert_eq!(second["restored"], false);
        assert_eq!(second["windows"], json!([]));

        assert_eq!(server.join().unwrap().len(), 2);
        assert_eq!(server2.join().unwrap().len(), 4);
    }
}
