//! Staged windows: park a window out of the way and bring it back later.
//!
//! `stage.*` sits on top of the existing parking lot and [`Parker`]: staging a
//! window minimizes it and records its snapshot in the `stage` lot, activating
//! it unminimizes the window and drops the record. Nothing here talks to niri
//! directly, so there is one compositor bridge and one parking store.
//!
//! Stage state persists in `parking.json` (the `stage` lot), which means a
//! daemon restart keeps the same staged windows. Ordering is by numeric window
//! id, never by insertion or niri's listing order.

use companion_core::{ActionInfo, Context, ParamInfo};
use companion_parking::Parking;
use serde_json::{Value, json};

use crate::{Compositor, Parker, ops};

/// Parking lot used for staged windows.
pub const STAGE_LOT: &str = "stage";

/// A staged-window workflow layered on the parking lot.
#[derive(Clone)]
pub struct Stage {
    parking: Parking,
    compositor: Compositor,
}

impl Stage {
    pub fn new(parking: Parking, compositor: Compositor) -> Self {
        Self {
            parking,
            compositor,
        }
    }

    fn parker(&self) -> Parker {
        Parker::new(self.parking.clone(), self.compositor.clone())
    }

    fn window_id(&self, params: &Value) -> Result<u64, String> {
        match ops::param_window(params) {
            Some(id) => Ok(id),
            None => ops::focused_window_id(&self.parking, &self.compositor),
        }
    }

    /// Numeric ids currently recorded in the stage lot.
    fn staged_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .parking
            .lot(STAGE_LOT)
            .keys()
            .filter_map(|key| key.strip_prefix("window:"))
            .filter_map(|id| id.parse().ok())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn is_staged(&self, id: u64) -> bool {
        self.parking
            .lot(STAGE_LOT)
            .contains_key(&format!("window:{id}"))
    }

    fn stage(&self, params: &Value) -> Result<Value, String> {
        let mut params = params.clone();
        if let Some(object) = params.as_object_mut() {
            object.insert("lot".to_owned(), json!(STAGE_LOT));
        } else {
            params = json!({ "lot": STAGE_LOT });
        }
        let result = self.parker().call("park", &params)?;
        Ok(json!({
            "lot": STAGE_LOT,
            "window": result["window"],
            "staged": true,
            "minimized": true,
        }))
    }

    fn activate(&self, params: &Value) -> Result<Value, String> {
        let id = ops::param_window(params)
            .ok_or_else(|| "stage.activate needs a `window` id".to_owned())?;
        let result = self
            .parker()
            .call("restore", &json!({ "lot": STAGE_LOT, "window": id }))?;
        Ok(json!({
            "lot": STAGE_LOT,
            "window": id,
            "staged": false,
            "activated": result["restored"],
        }))
    }

    fn toggle(&self, params: &Value) -> Result<Value, String> {
        let id = self.window_id(params)?;
        if self.is_staged(id) {
            let mut value = self.activate(&json!({ "window": id }))?;
            value["action"] = json!("activated");
            Ok(value)
        } else {
            let mut value = self.stage(&json!({ "window": id }))?;
            value["action"] = json!("staged");
            Ok(value)
        }
    }

    fn list(&self) -> Value {
        let entries = self.parking.lot(STAGE_LOT);
        let windows: Vec<Value> = self
            .staged_ids()
            .into_iter()
            .map(|id| {
                json!({
                    "window": id,
                    "snapshot": entries.get(&format!("window:{id}")).cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        json!({
            "lot": STAGE_LOT,
            "windows": windows,
            "count": windows.len(),
        })
    }
}

impl Context for Stage {
    fn namespace(&self) -> &'static str {
        "stage"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        let mut actions = vec![
            ActionInfo::new(
                "stage",
                "Stage `window` (or the focused window) by parking it; alias `add`",
            )
            .with_params([ParamInfo::integer(
                "window",
                false,
                "window id, defaults to the focused window",
            )])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "list",
                "List staged windows in numeric window-id order with their snapshots",
            ),
            ActionInfo::new(
                "activate",
                "Bring a staged `window` back; alias `unstage`/`restore`",
            )
            .with_params([ParamInfo::integer("window", true, "staged window id")])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "toggle",
                "Activate `window` when it is staged, otherwise stage it",
            )
            .with_params([ParamInfo::integer(
                "window",
                false,
                "window id, defaults to the focused window",
            )])
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
            "stage" | "add" => self.stage(params),
            "activate" | "unstage" | "restore" => self.activate(params),
            "toggle" => self.toggle(params),
            "list" => Ok(self.list()),
            other => Err(format!("unknown action `stage.{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    use companion_core::AUTO_WINDOWS;
    use niri_ipc::{Action, Request};
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-stage-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed(parking: &Parking) {
        parking
            .replace_lot(
                AUTO_WINDOWS,
                [
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
                .collect(),
            )
            .unwrap();
    }

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
    fn stages_focused_window_and_lists_in_id_order() {
        let dir = temp_dir("order");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let stage = Stage::new(parking.clone(), Compositor::at(&socket));

        let first = stage.call("stage", &json!({})).unwrap();
        assert_eq!(first["window"], 10);
        assert_eq!(first["staged"], true);
        let second = stage.call("add", &json!({ "window": 11 })).unwrap();
        assert_eq!(second["window"], 11);

        let listed = stage.call("list", &Value::Null).unwrap();
        assert_eq!(listed["count"], 2);
        assert_eq!(listed["windows"][0]["window"], 10);
        assert_eq!(listed["windows"][1]["window"], 11);
        assert_eq!(listed["windows"][0]["snapshot"]["title"], "a");

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::MinimizeWindow { id: Some(11) })
        ));
    }

    #[test]
    fn toggle_stages_then_activates_and_is_idempotent() {
        let dir = temp_dir("toggle");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 2);
        let stage = Stage::new(parking.clone(), Compositor::at(&socket));

        let staged = stage.call("toggle", &json!({})).unwrap();
        assert_eq!(staged["action"], "staged");
        assert_eq!(staged["staged"], true);

        let activated = stage.call("toggle", &json!({ "window": 10 })).unwrap();
        assert_eq!(activated["action"], "activated");
        assert_eq!(activated["staged"], false);
        assert_eq!(activated["activated"], true);
        assert!(parking.lot(STAGE_LOT).is_empty());

        // A second activate reports that nothing was restored and sends no
        // extra niri action.
        let again = stage.call("activate", &json!({ "window": 10 })).unwrap();
        assert_eq!(again["activated"], false);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn activate_and_remove_require_a_window_id() {
        let dir = temp_dir("missing-id");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        let stage = Stage::new(parking, Compositor::at(dir.join("unused.sock")));
        assert!(stage.call("activate", &json!({})).is_err());
        assert!(stage.call("unstage", &json!({})).is_err());
    }

    #[test]
    fn stage_state_survives_a_reload() {
        let dir = temp_dir("persist");
        let path = dir.join("parking.json");
        let parking = Parking::load(&path).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_niri(&socket, 1);
        Stage::new(parking, Compositor::at(&socket))
            .call("stage", &json!({ "window": 10 }))
            .unwrap();
        server.join().unwrap();

        // A new daemon start loads the same store and sees the staged window
        // without needing to talk to niri.
        let reloaded = Parking::load(&path).unwrap();
        let stage = Stage::new(reloaded, Compositor::at(dir.join("offline.sock")));
        let listed = stage.call("list", &Value::Null).unwrap();
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["windows"][0]["window"], 10);
    }
}
