//! Named scratchpads: one window per slot, toggled from a keybinding.
//!
//! Unlike nsticky's LIFO scratchpad stack, loom-companion scratchpads are
//! named slots. Every slot holds at most one window; assigning a window that is
//! already in another slot moves it, and assigning over an occupied slot is
//! rejected unless `"replace": true` is passed.
//!
//! Hiding a scratchpad window minimizes it (the same parking primitive
//! `parker.*` uses). Showing it moves the window to the focused workspace,
//! unminimizes it and focuses it. Assignments live in the `scratchpad` lot of
//! `parking.json`, so they survive a daemon restart; [`Scratchpad::call`]'s
//! `reconcile` action drops slots whose window no longer exists.

use std::collections::{BTreeMap, BTreeSet};

use companion_core::{ActionInfo, Context, ParamInfo};
use companion_parking::Parking;
use niri_ipc::{Action, WorkspaceReferenceArg};
use serde_json::{Value, json};

use crate::{Compositor, ops};

/// Parking lot used for scratchpad slot assignments.
pub const SCRATCHPAD_LOT: &str = "scratchpad";

/// Named scratchpad slots layered on the parking lot.
#[derive(Clone)]
pub struct Scratchpad {
    parking: Parking,
    compositor: Compositor,
}

impl Scratchpad {
    pub fn new(parking: Parking, compositor: Compositor) -> Self {
        Self {
            parking,
            compositor,
        }
    }

    fn lot(&self) -> BTreeMap<String, Value> {
        self.parking.lot(SCRATCHPAD_LOT)
    }

    fn store(&self, slot: &str, value: Value) -> Result<(), String> {
        self.parking.call(
            "park",
            &json!({ "lot": SCRATCHPAD_LOT, "id": slot, "value": value }),
        )?;
        Ok(())
    }

    fn entry_window(entry: &Value) -> Option<u64> {
        entry.get("window").and_then(Value::as_u64)
    }

    fn hidden(entry: &Value) -> bool {
        entry.get("hidden").and_then(Value::as_bool) == Some(true)
    }

    fn assign(&self, params: &Value) -> Result<Value, String> {
        let slot = ops::slot_name(params)?;
        let id = match ops::param_window(params) {
            Some(id) => id,
            None => ops::focused_window_id(&self.parking, &self.compositor)?,
        };
        let snapshot = ops::window_snapshot(&self.parking, &self.compositor, id)?;

        let lot = self.lot();
        let mut replaced = Value::Null;
        if let Some(existing) = lot.get(&slot)
            && let Some(existing_id) = Self::entry_window(existing)
            && existing_id != id
        {
            if params.get("replace").and_then(Value::as_bool) != Some(true) {
                return Err(format!(
                    "scratchpad slot `{slot}` already holds window {existing_id}; \
                     pass \"replace\":true or remove it first"
                ));
            }
            if Self::hidden(existing) {
                ops::send_action(
                    &self.compositor,
                    Action::UnminimizeWindow {
                        id: Some(existing_id),
                    },
                )?;
            }
            replaced = json!({ "window": existing_id, "slot": slot });
        }

        // One window lives in one slot: rebinding a window removes its old
        // assignment rather than leaving two names for the same window.
        let mut moved_from = Value::Null;
        for (other, entry) in &lot {
            if other != &slot && Self::entry_window(entry) == Some(id) {
                self.parking.take(SCRATCHPAD_LOT, other)?;
                moved_from = json!(other);
            }
        }

        ops::send_action(&self.compositor, Action::MinimizeWindow { id: Some(id) })?;
        self.store(
            &slot,
            json!({
                "window": id,
                "hidden": true,
                "snapshot": snapshot,
            }),
        )?;
        Ok(json!({
            "action": "assigned",
            "slot": slot,
            "window": id,
            "hidden": true,
            "replaced": replaced,
            "moved_from": moved_from,
        }))
    }

    fn hide(&self, params: &Value) -> Result<Value, String> {
        let slot = ops::slot_name(params)?;
        let Some(entry) = self.lot().get(&slot).cloned() else {
            return Err(format!("scratchpad slot `{slot}` is empty"));
        };
        let id = Self::entry_window(&entry)
            .ok_or_else(|| format!("scratchpad slot `{slot}` has no window"))?;
        if Self::hidden(&entry) {
            return Ok(json!({
                "action": "hidden",
                "slot": slot,
                "window": id,
                "hidden": true,
                "changed": false,
            }));
        }
        let snapshot = ops::window_snapshot(&self.parking, &self.compositor, id)?;
        ops::send_action(&self.compositor, Action::MinimizeWindow { id: Some(id) })?;
        self.store(
            &slot,
            json!({ "window": id, "hidden": true, "snapshot": snapshot }),
        )?;
        Ok(json!({
            "action": "hidden",
            "slot": slot,
            "window": id,
            "hidden": true,
            "changed": true,
        }))
    }

    fn show(&self, params: &Value) -> Result<Value, String> {
        let slot = ops::slot_name(params)?;
        let Some(entry) = self.lot().get(&slot).cloned() else {
            return Err(format!("scratchpad slot `{slot}` is empty"));
        };
        let id = Self::entry_window(&entry)
            .ok_or_else(|| format!("scratchpad slot `{slot}` has no window"))?;
        let was_hidden = Self::hidden(&entry);
        let target = ops::focused_workspace_id(&self.parking, &self.compositor)?;
        let current = ops::auto_windows(&self.parking)
            .get(&id.to_string())
            .and_then(|window| window.get("workspace_id"))
            .and_then(Value::as_u64)
            .or_else(|| {
                entry
                    .get("snapshot")
                    .and_then(|snapshot| snapshot.get("workspace_id"))
                    .and_then(Value::as_u64)
            });
        if current != Some(target) {
            ops::send_action(
                &self.compositor,
                Action::MoveWindowToWorkspace {
                    window_id: Some(id),
                    reference: WorkspaceReferenceArg::Id(target),
                    focus: false,
                },
            )?;
        }
        ops::send_action(&self.compositor, Action::UnminimizeWindow { id: Some(id) })?;
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(true);
        if focus {
            ops::send_action(&self.compositor, Action::FocusWindow { id })?;
        }

        let mut snapshot = entry
            .get("snapshot")
            .cloned()
            .unwrap_or_else(|| json!({ "id": id }));
        if let Some(object) = snapshot.as_object_mut() {
            object.insert("workspace_id".to_owned(), json!(target));
            object.insert("is_minimized".to_owned(), json!(false));
        }
        self.store(
            &slot,
            json!({ "window": id, "hidden": false, "snapshot": snapshot }),
        )?;
        Ok(json!({
            "action": "shown",
            "slot": slot,
            "window": id,
            "hidden": false,
            "changed": was_hidden,
            "workspace": target,
            "focused": focus,
        }))
    }

    fn toggle(&self, params: &Value) -> Result<Value, String> {
        let slot = ops::slot_name(params)?;
        let Some(entry) = self.lot().get(&slot).cloned() else {
            return self.assign(params);
        };
        if Self::entry_window(&entry).is_none() {
            self.parking.take(SCRATCHPAD_LOT, &slot)?;
            return self.assign(params);
        }
        if Self::hidden(&entry) {
            self.show(params)
        } else {
            self.hide(params)
        }
    }

    fn list(&self) -> Value {
        let slots: Vec<Value> = self
            .lot()
            .into_iter()
            .map(|(slot, entry)| {
                json!({
                    "slot": slot,
                    "window": entry.get("window"),
                    "hidden": Self::hidden(&entry),
                    "snapshot": entry.get("snapshot"),
                })
            })
            .collect();
        json!({
            "lot": SCRATCHPAD_LOT,
            "scratchpads": slots,
            "count": slots.len(),
        })
    }

    fn remove(&self, params: &Value) -> Result<Value, String> {
        let slot = ops::slot_name(params)?;
        let Some(entry) = self
            .parking
            .take(SCRATCHPAD_LOT, &slot)
            .map_err(|err| format!("cannot remove scratchpad slot `{slot}`: {err}"))?
        else {
            return Ok(json!({
                "action": "removed",
                "slot": slot,
                "window": Value::Null,
                "removed": false,
                "restored": false,
            }));
        };
        let Some(id) = Self::entry_window(&entry) else {
            return Ok(json!({
                "action": "removed",
                "slot": slot,
                "window": Value::Null,
                "removed": true,
                "restored": false,
            }));
        };
        let hidden = Self::hidden(&entry);
        if hidden
            && let Err(err) =
                ops::send_action(&self.compositor, Action::UnminimizeWindow { id: Some(id) })
        {
            // Keep the assignment when the compositor refuses the restore so a
            // transient niri failure does not silently orphan a hidden window.
            self.store(&slot, entry)?;
            return Err(err);
        }
        Ok(json!({
            "action": "removed",
            "slot": slot,
            "window": id,
            "removed": true,
            "restored": hidden,
        }))
    }

    fn reconcile(&self) -> Result<Value, String> {
        let live: BTreeSet<u64> = ops::window_query(&self.compositor)?
            .into_iter()
            .map(|window| window.id)
            .collect();
        let mut removed = Vec::new();
        for (slot, entry) in self.lot() {
            if let Some(id) = Self::entry_window(&entry)
                && !live.contains(&id)
            {
                self.parking.take(SCRATCHPAD_LOT, &slot)?;
                removed.push(json!({ "slot": slot, "window": id }));
            }
        }
        Ok(json!({
            "lot": SCRATCHPAD_LOT,
            "removed": removed,
            "count": removed.len(),
        }))
    }
}

impl Context for Scratchpad {
    fn namespace(&self) -> &'static str {
        "scratchpad"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        let mut actions = vec![
            ActionInfo::new(
                "assign",
                "Assign `window` (or the focused window) to scratchpad `name` and hide it; alias `add`",
            )
            .with_params([
                ParamInfo::string("name", false, "slot name (default `default`)"),
                ParamInfo::integer("window", false, "window id, defaults to the focused window"),
                ParamInfo::boolean("replace", false, "replace an occupied slot (restores its window)"),
            ])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "toggle",
                "Hide or show scratchpad `name`; assigns the focused window when the slot is empty",
            )
            .with_params([
                ParamInfo::string("name", false, "slot name (default `default`)"),
                ParamInfo::integer("window", false, "window to assign when the slot is empty"),
            ])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "show",
                "Show scratchpad `name` on the focused workspace; alias `restore`",
            )
            .with_params([
                ParamInfo::string("name", false, "slot name (default `default`)"),
                ParamInfo::boolean("focus", false, "focus the shown window (default true)"),
            ])
            .with_permissions(&["niri"]),
            ActionInfo::new("hide", "Hide scratchpad `name`")
                .with_params([ParamInfo::string("name", false, "slot name (default `default`)")])
                .with_permissions(&["niri"]),
            ActionInfo::new("list", "List scratchpad slots ordered by name"),
            ActionInfo::new(
                "remove",
                "Drop scratchpad `name`, restoring its window when it was hidden",
            )
            .with_params([ParamInfo::string("name", false, "slot name (default `default`)")])
            .with_permissions(&["niri"]),
            ActionInfo::new(
                "reconcile",
                "Drop slots whose windows no longer exist, leaving state untouched if niri is unreachable",
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
            "assign" | "add" => self.assign(params),
            "toggle" => self.toggle(params),
            "show" | "restore" => self.show(params),
            "hide" => self.hide(params),
            "list" => Ok(self.list()),
            "remove" => self.remove(params),
            "reconcile" => self.reconcile(),
            other => Err(format!("unknown action `scratchpad.{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    use companion_core::{AUTO_WINDOWS, AUTO_WORKSPACES};
    use niri_ipc::{Action, Request, WorkspaceReferenceArg};
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-scratchpad-{}-{name}",
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
                        json!({
                            "id": 10,
                            "workspace_id": 3,
                            "is_focused": true,
                            "title": "terminal",
                            "app_id": "foot",
                        }),
                    ),
                    (
                        "11".to_owned(),
                        json!({
                            "id": 11,
                            "workspace_id": 3,
                            "is_focused": false,
                            "title": "notes",
                            "app_id": "editor",
                        }),
                    ),
                ]
                .into_iter()
                .collect(),
            )
            .unwrap();
        parking
            .replace_lot(
                AUTO_WORKSPACES,
                [(
                    "7".to_owned(),
                    json!({ "id": 7, "is_focused": true, "is_active": true, "name": "dev" }),
                )]
                .into_iter()
                .collect(),
            )
            .unwrap();
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

    fn handled(count: usize) -> Vec<Value> {
        (0..count).map(|_| json!({ "Ok": "Handled" })).collect()
    }

    #[test]
    fn assign_hides_and_toggle_shows_on_the_focused_workspace() {
        let dir = temp_dir("toggle");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_sequence(&socket, handled(4));
        let scratchpad = Scratchpad::new(parking.clone(), Compositor::at(&socket));

        let assigned = scratchpad
            .call("assign", &json!({ "name": "term", "window": 10 }))
            .unwrap();
        assert_eq!(assigned["slot"], "term");
        assert_eq!(assigned["window"], 10);
        assert_eq!(assigned["hidden"], true);

        let shown = scratchpad
            .call("toggle", &json!({ "name": "term" }))
            .unwrap();
        assert_eq!(shown["action"], "shown");
        assert_eq!(shown["window"], 10);
        assert_eq!(shown["hidden"], false);
        assert_eq!(shown["workspace"], 7);
        assert_eq!(shown["focused"], true);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: Some(10),
                reference: WorkspaceReferenceArg::Id(7),
                focus: false,
            })
        ));
        assert!(matches!(
            requests[2],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[3],
            Request::Action(Action::FocusWindow { id: 10 })
        ));
    }

    #[test]
    fn show_focuses_the_window_and_a_second_toggle_hides_it() {
        let dir = temp_dir("show-hide");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_sequence(
            &socket,
            vec![
                json!({ "Ok": "Handled" }),
                json!({ "Ok": "Handled" }),
                json!({ "Ok": "Handled" }),
                json!({ "Ok": "Handled" }),
                json!({ "Ok": "Handled" }),
            ],
        );
        let scratchpad = Scratchpad::new(parking.clone(), Compositor::at(&socket));

        scratchpad
            .call("assign", &json!({ "name": "term" }))
            .unwrap();
        let shown = scratchpad
            .call("toggle", &json!({ "name": "term" }))
            .unwrap();
        assert_eq!(shown["focused"], true);
        let hidden = scratchpad
            .call("toggle", &json!({ "name": "term" }))
            .unwrap();
        assert_eq!(hidden["action"], "hidden");
        assert_eq!(hidden["hidden"], true);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::MoveWindowToWorkspace { .. })
        ));
        assert!(matches!(
            requests[2],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[3],
            Request::Action(Action::FocusWindow { id: 10 })
        ));
    }

    #[test]
    fn multiple_slots_are_sorted_and_a_window_moves_between_them() {
        let dir = temp_dir("slots");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_sequence(&socket, handled(3));
        let scratchpad = Scratchpad::new(parking.clone(), Compositor::at(&socket));

        scratchpad
            .call("assign", &json!({ "name": "term", "window": 10 }))
            .unwrap();
        scratchpad
            .call("assign", &json!({ "name": "notes", "window": 11 }))
            .unwrap();
        let listed = scratchpad.call("list", &Value::Null).unwrap();
        assert_eq!(listed["count"], 2);
        assert_eq!(listed["scratchpads"][0]["slot"], "notes");
        assert_eq!(listed["scratchpads"][1]["slot"], "term");

        // Rebinding window 10 to another slot removes its previous slot.
        let moved = scratchpad
            .call("assign", &json!({ "name": "aux", "window": 10 }))
            .unwrap();
        assert_eq!(moved["moved_from"], "term");
        let listed = scratchpad.call("list", &Value::Null).unwrap();
        let slots: Vec<&str> = listed["scratchpads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|slot| slot["slot"].as_str().unwrap())
            .collect();
        assert_eq!(slots, vec!["aux", "notes"]);
        server.join().unwrap();
    }

    #[test]
    fn an_occupied_slot_requires_replace_and_restores_the_old_window() {
        let dir = temp_dir("replace");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_sequence(&socket, handled(3));
        let scratchpad = Scratchpad::new(parking.clone(), Compositor::at(&socket));

        scratchpad
            .call("assign", &json!({ "name": "term", "window": 10 }))
            .unwrap();
        let refused = scratchpad.call("assign", &json!({ "name": "term", "window": 11 }));
        assert!(refused.is_err());

        let replaced = scratchpad
            .call(
                "assign",
                &json!({ "name": "term", "window": 11, "replace": true }),
            )
            .unwrap();
        assert_eq!(replaced["replaced"]["window"], 10);
        let listed = scratchpad.call("list", &Value::Null).unwrap();
        assert_eq!(listed["scratchpads"][0]["window"], 11);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[2],
            Request::Action(Action::MinimizeWindow { id: Some(11) })
        ));
    }

    #[test]
    fn remove_restores_a_hidden_window_and_drops_the_slot() {
        let dir = temp_dir("remove");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_sequence(&socket, handled(2));
        let scratchpad = Scratchpad::new(parking.clone(), Compositor::at(&socket));

        scratchpad
            .call("assign", &json!({ "name": "term", "window": 10 }))
            .unwrap();
        let removed = scratchpad
            .call("remove", &json!({ "name": "term" }))
            .unwrap();
        assert_eq!(removed["removed"], true);
        assert_eq!(removed["restored"], true);
        assert_eq!(removed["window"], 10);
        assert!(parking.lot(SCRATCHPAD_LOT).is_empty());

        let again = scratchpad
            .call("remove", &json!({ "name": "term" }))
            .unwrap();
        assert_eq!(again["removed"], false);

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
    fn assignments_survive_a_reload_and_reconcile_drops_missing_windows() {
        let dir = temp_dir("persist");
        let path = dir.join("parking.json");
        let parking = Parking::load(&path).unwrap();
        seed(&parking);
        let socket = dir.join("niri.sock");
        let server = fake_sequence(&socket, handled(1));
        Scratchpad::new(parking, Compositor::at(&socket))
            .call("assign", &json!({ "name": "term", "window": 10 }))
            .unwrap();
        server.join().unwrap();

        // Restart: a fresh store and context read the persisted assignment.
        let reloaded = Parking::load(&path).unwrap();
        reloaded
            .call(
                "park",
                &json!({
                    "lot": SCRATCHPAD_LOT,
                    "id": "gone",
                    "value": { "window": 99, "hidden": true, "snapshot": { "workspace_id": 3 } }
                }),
            )
            .unwrap();
        let socket = dir.join("niri2.sock");
        let server = fake_sequence(
            &socket,
            vec![json!({ "Ok": { "Windows": [
                { "id": 10, "title": "terminal", "app_id": "foot", "pid": 1,
                  "workspace_id": 3, "is_focused": false, "is_floating": false,
                  "is_sticky": false, "is_urgent": false, "is_minimized": true,
                  "layout": { "pos_in_scrolling_layout": [1, 1], "tile_size": [100.0, 100.0],
                    "window_size": [100, 100], "tile_pos_in_workspace_view": [0.0, 0.0],
                    "window_offset_in_tile": [0.0, 0.0] }, "focus_timestamp": null },
            ] } })],
        );
        let scratchpad = Scratchpad::new(reloaded.clone(), Compositor::at(&socket));
        let listed = scratchpad.call("list", &Value::Null).unwrap();
        assert_eq!(listed["count"], 2);
        let reconciled = scratchpad.call("reconcile", &Value::Null).unwrap();
        assert_eq!(reconciled["count"], 1);
        assert_eq!(reconciled["removed"][0]["slot"], "gone");
        assert!(matches!(server.join().unwrap()[0], Request::Windows));
        assert_eq!(reloaded.lot(SCRATCHPAD_LOT)["term"]["window"], 10);
    }

    #[test]
    fn persisted_assignment_toggles_after_a_restart() {
        let dir = temp_dir("restart-toggle");
        let path = dir.join("parking.json");
        let parking = Parking::load(&path).unwrap();
        seed(&parking);
        parking
            .call(
                "park",
                &json!({
                    "lot": SCRATCHPAD_LOT,
                    "id": "term",
                    "value": {
                        "window": 10,
                        "hidden": true,
                        "snapshot": { "id": 10, "workspace_id": 3, "title": "terminal" }
                    }
                }),
            )
            .unwrap();

        // A new daemon process loads the assignment and can show the window
        // without being told about the slot again.
        let reloaded = Parking::load(&path).unwrap();
        let socket = dir.join("niri.sock");
        let server = fake_sequence(&socket, handled(3));
        let scratchpad = Scratchpad::new(reloaded, Compositor::at(&socket));
        let shown = scratchpad
            .call("toggle", &json!({ "name": "term" }))
            .unwrap();
        assert_eq!(shown["action"], "shown");
        assert_eq!(shown["window"], 10);
        assert_eq!(shown["hidden"], false);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MoveWindowToWorkspace {
                window_id: Some(10),
                reference: WorkspaceReferenceArg::Id(7),
                focus: false,
            })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
        assert!(matches!(
            requests[2],
            Request::Action(Action::FocusWindow { id: 10 })
        ));
    }

    #[test]
    fn show_with_focus_false_can_reuse_a_visible_window() {
        let dir = temp_dir("visible");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        seed(&parking);
        // The tracked window is already on the focused workspace.
        parking
            .replace_lot(
                AUTO_WINDOWS,
                [(
                    "10".to_owned(),
                    json!({
                        "id": 10, "workspace_id": 7, "is_focused": true,
                        "title": "terminal", "app_id": "foot",
                    }),
                )]
                .into_iter()
                .collect(),
            )
            .unwrap();
        parking
            .call(
                "park",
                &json!({
                    "lot": SCRATCHPAD_LOT,
                    "id": "term",
                    "value": { "window": 10, "hidden": false, "snapshot": { "workspace_id": 7 } }
                }),
            )
            .unwrap();
        let socket = dir.join("niri.sock");
        let server = fake_sequence(&socket, handled(1));
        let scratchpad = Scratchpad::new(parking.clone(), Compositor::at(&socket));

        let shown = scratchpad
            .call("show", &json!({ "name": "term", "focus": false }))
            .unwrap();
        assert_eq!(shown["changed"], false);
        assert_eq!(shown["focused"], false);
        assert_eq!(shown["hidden"], false);

        let requests = server.join().unwrap();
        // Already on the focused workspace, so only the unminimize is sent.
        assert_eq!(requests.len(), 1);
        assert!(matches!(
            requests[0],
            Request::Action(Action::UnminimizeWindow { id: Some(10) })
        ));
    }
}
