//! Compatibility aliases for the standalone `nsticky` workflow.
//!
//! The canonical namespaces are `stage.*`, `scratchpad.*` and `windows.*`. This
//! namespace maps the old `nsticky` verbs onto them so existing keybindings and
//! scripts can be pointed at `companionctl call nsticky ...` without learning a
//! second model. It adds no state and performs no compositor work of its own.
//!
//! The mapping is intentionally partial where the models differ: nsticky has a
//! LIFO scratchpad stack and a selector-driven `stage restore`, while
//! better-companion uses named slots and explicit window ids. Unsupported verbs
//! return the usual unknown-action error instead of pretending to work.

use companion_core::{ActionInfo, Context};
use serde_json::Value;

use crate::{Scratchpad, Stage, Windows};

/// Alias surface for the standalone `nsticky` CLI commands.
#[derive(Clone)]
pub struct Nsticky {
    stage: Stage,
    scratchpad: Scratchpad,
    windows: Windows,
}

impl Nsticky {
    pub fn new(stage: Stage, scratchpad: Scratchpad, windows: Windows) -> Self {
        Self {
            stage,
            scratchpad,
            windows,
        }
    }
}

impl Context for Nsticky {
    fn namespace(&self) -> &'static str {
        "nsticky"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new(
                "sticky.add",
                "Alias of `windows.set-sticky` with `sticky: true`",
            ),
            ActionInfo::new(
                "sticky.remove",
                "Alias of `windows.set-sticky` with `sticky: false`",
            ),
            ActionInfo::new("sticky.toggle-active", "Alias of `windows.toggle-sticky`"),
            ActionInfo::new("sticky.list", "Alias of `windows.list-sticky`"),
            ActionInfo::new("stage.add", "Alias of `stage.stage`"),
            ActionInfo::new("stage.remove", "Alias of `stage.activate`"),
            ActionInfo::new("stage.restore", "Alias of `stage.activate`"),
            ActionInfo::new("stage.toggle-active", "Alias of `stage.toggle`"),
            ActionInfo::new("stage.list", "Alias of `stage.list`"),
            ActionInfo::new(
                "scratchpad.add",
                "Alias of `scratchpad.assign` (named slots, not a LIFO stack)",
            ),
            ActionInfo::new("scratchpad.restore", "Alias of `scratchpad.show`"),
            ActionInfo::new("scratchpad.toggle", "Alias of `scratchpad.toggle`"),
            ActionInfo::new("scratchpad.list", "Alias of `scratchpad.list`"),
            ActionInfo::new("scratchpad.remove", "Alias of `scratchpad.remove`"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "sticky.add" => self
                .windows
                .call("set-sticky", &with_bool(params, "sticky", true)),
            "sticky.remove" => self
                .windows
                .call("set-sticky", &with_bool(params, "sticky", false)),
            "sticky.toggle-active" => self.windows.call("toggle-sticky", params),
            "sticky.list" => self.windows.call("list-sticky", params),
            "stage.add" => self.stage.call("stage", params),
            "stage.remove" | "stage.restore" => self.stage.call("activate", params),
            "stage.toggle-active" => self.stage.call("toggle", params),
            "stage.list" => self.stage.call("list", params),
            "scratchpad.add" => self.scratchpad.call("assign", params),
            "scratchpad.restore" => self.scratchpad.call("show", params),
            "scratchpad.toggle" => self.scratchpad.call("toggle", params),
            "scratchpad.list" => self.scratchpad.call("list", params),
            "scratchpad.remove" => self.scratchpad.call("remove", params),
            other => Err(format!("unknown action `nsticky.{other}`")),
        }
    }
}

/// Copies `params` and forces the boolean field `key`, so a caller cannot turn
/// `nsticky sticky.add` into a removal by passing `"sticky": false`.
fn with_bool(params: &Value, key: &str, value: bool) -> Value {
    let mut params = match params {
        Value::Object(object) => Value::Object(object.clone()),
        _ => Value::Object(Default::default()),
    };
    if let Some(object) = params.as_object_mut() {
        object.insert(key.to_owned(), Value::Bool(value));
    }
    params
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
    use crate::Compositor;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-nsticky-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed(parking: &companion_parking::Parking) {
        parking
            .replace_lot(
                AUTO_WINDOWS,
                [
                    (
                        "10".to_owned(),
                        json!({
                            "id": 10,
                            "workspace_id": 7,
                            "is_focused": true,
                            "title": "a",
                            "app_id": "foot",
                        }),
                    ),
                    (
                        "11".to_owned(),
                        json!({
                            "id": 11,
                            "workspace_id": 7,
                            "is_focused": false,
                            "title": "b",
                            "app_id": "editor",
                        }),
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

    fn context(parking: companion_parking::Parking, socket: &PathBuf) -> Nsticky {
        let compositor = Compositor::at(socket);
        let stage = Stage::new(parking.clone(), compositor.clone());
        let scratchpad = Scratchpad::new(parking.clone(), compositor.clone());
        let windows = Windows::new(compositor);
        Nsticky::new(stage, scratchpad, windows)
    }

    fn fake_sequence(
        path: &PathBuf,
        responses: Vec<Value>,
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
    fn stage_add_is_equivalent_to_stage_stage() {
        let dir = temp_dir("stage-alias");
        let canalical_parking =
            companion_parking::Parking::load(dir.join("canonical.json")).unwrap();
        let alias_parking = companion_parking::Parking::load(dir.join("alias.json")).unwrap();
        seed(&canalical_parking);
        seed(&alias_parking);

        let socket = dir.join("canonical.sock");
        let server = fake_niri(&socket, 1);
        let canonical = Stage::new(canalical_parking.clone(), Compositor::at(&socket))
            .call("stage", &json!({ "window": 10 }))
            .unwrap();
        server.join().unwrap();

        let socket = dir.join("alias.sock");
        let server = fake_niri(&socket, 1);
        let aliased = context(alias_parking.clone(), &socket)
            .call("stage.add", &json!({ "window": 10 }))
            .unwrap();
        server.join().unwrap();

        assert_eq!(canonical, aliased);
        assert_eq!(
            canalical_parking.lot(crate::STAGE_LOT),
            alias_parking.lot(crate::STAGE_LOT)
        );
    }

    #[test]
    fn scratchpad_toggle_is_equivalent_to_the_canonical_toggle() {
        let dir = temp_dir("scratch-alias");
        let canonical_parking =
            companion_parking::Parking::load(dir.join("canonical.json")).unwrap();
        let alias_parking = companion_parking::Parking::load(dir.join("alias.json")).unwrap();
        seed(&canonical_parking);
        seed(&alias_parking);

        let socket = dir.join("canonical.sock");
        let server = fake_niri(&socket, 1);
        let canonical = Scratchpad::new(canonical_parking.clone(), Compositor::at(&socket))
            .call("toggle", &json!({ "name": "term" }))
            .unwrap();
        server.join().unwrap();

        let socket = dir.join("alias.sock");
        let server = fake_niri(&socket, 1);
        let aliased = context(alias_parking.clone(), &socket)
            .call("scratchpad.toggle", &json!({ "name": "term" }))
            .unwrap();
        server.join().unwrap();

        assert_eq!(canonical, aliased);
        assert_eq!(
            canonical_parking.lot(crate::SCRATCHPAD_LOT),
            alias_parking.lot(crate::SCRATCHPAD_LOT)
        );
    }

    #[test]
    fn sticky_add_forces_the_flag_on() {
        let dir = temp_dir("sticky-alias");
        let parking = companion_parking::Parking::load(dir.join("parking.json")).unwrap();
        let socket = dir.join("niri.sock");
        let window = json!({
            "id": 10, "title": "a", "app_id": "foot", "pid": 1,
            "workspace_id": 7, "is_focused": true, "is_floating": false,
            "is_sticky": false, "is_urgent": false, "is_minimized": false,
            "layout": {
                "pos_in_scrolling_layout": [1, 1], "tile_size": [100.0, 100.0],
                "window_size": [100, 100], "tile_pos_in_workspace_view": [0.0, 0.0],
                "window_offset_in_tile": [0.0, 0.0]
            },
            "focus_timestamp": null
        });
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": { "Windows": [window] } }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let nsticky = context(parking, &socket);
        // Even if a stale script passes `"sticky": false`, `sticky.add` means
        // "make it sticky".
        let result = nsticky
            .call("sticky.add", &json!({ "window": 10, "sticky": false }))
            .unwrap();
        assert_eq!(result["changed"], true);
        assert_eq!(result["sticky"], true);

        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Windows));
        assert!(matches!(
            requests[1],
            Request::Action(Action::ToggleWindowSticky { id: Some(10) })
        ));
    }

    #[test]
    fn unknown_nsticky_actions_are_rejected() {
        let dir = temp_dir("unknown");
        let parking = companion_parking::Parking::load(dir.join("parking.json")).unwrap();
        let nsticky = context(parking, &dir.join("unused.sock"));
        assert!(nsticky.call("stage.add-all", &Value::Null).is_err());
        assert!(nsticky.call("sticky.toggle-appid", &Value::Null).is_err());
    }
}
