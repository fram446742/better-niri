//! Parking lots: named buckets of parked items with a JSON value each.
//!
//! This is the bounded context shared by the window workflows: `parker.*` parks
//! a window or workspace, `stage.*` records staged windows, `scratchpad.*`
//! records named slots and `swallow.*` records terminal/child pairs. The daemon
//! only stores the state; the compositor-side contexts decide what parking means
//! for a window (minimizing it today) and perform the niri actions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use companion_core::{ActionInfo, Context, load_json, save_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Lot = BTreeMap<String, Value>;
type Lots = BTreeMap<String, Lot>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    lots: Lots,
}

#[derive(Clone)]
pub struct Parking {
    path: Arc<PathBuf>,
    state: Arc<Mutex<State>>,
}

impl Parking {
    /// Loads (or starts) the parking store at `path`.
    pub fn load(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path: Arc::new(path),
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// Convenience constructor using `$XDG_STATE_HOME/better-companion/parking.json`.
    pub fn from_state_dir() -> std::io::Result<Self> {
        Self::load(companion_core::state_dir().join("parking.json"))
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(self.path.as_path(), state)
            .map_err(|err| format!("cannot save parking state: {err}"))
    }

    /// Replaces one lot with `entries`, removing the lot when empty.
    pub fn replace_lot(&self, lot: &str, entries: Lot) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "parking state lock poisoned".to_owned())?;
        if entries.is_empty() {
            state.lots.remove(lot);
        } else {
            state.lots.insert(lot.to_owned(), entries);
        }
        self.save(&state)
    }

    /// Returns a copy of one lot.
    pub fn lot(&self, lot: &str) -> Lot {
        self.state
            .lock()
            .ok()
            .and_then(|state| state.lots.get(lot).cloned())
            .unwrap_or_default()
    }

    /// Atomically removes and returns `id` from `lot`.
    ///
    /// Unlike `unpark`, a missing entry is `Ok(None)`, which lets callers make
    /// restore operations idempotent under concurrent requests.
    pub fn take(&self, lot: &str, id: &str) -> Result<Option<Value>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "parking state lock poisoned".to_owned())?;
        let Some(value) = state
            .lots
            .get_mut(lot)
            .and_then(|entries| entries.remove(id))
        else {
            return Ok(None);
        };
        if state.lots.get(lot).is_some_and(Lot::is_empty) {
            state.lots.remove(lot);
        }
        self.save(&state)?;
        Ok(Some(value))
    }

    fn lot_name(params: &Value) -> String {
        params
            .get("lot")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned()
    }
}

impl Context for Parking {
    fn namespace(&self) -> &'static str {
        "parking"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("park", "Park `value` under `id` in `lot`"),
            ActionInfo::new("unpark", "Remove and return `id` from `lot`"),
            ActionInfo::new("list", "List parked entries, optionally for one `lot`"),
            ActionInfo::new("lots", "List lot names"),
            ActionInfo::new("clear", "Clear one `lot`, or every lot when absent"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "parking state lock poisoned".to_owned())?;

        match action {
            "park" => {
                let lot_name = Self::lot_name(params);
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "parking.park needs a string `id`".to_owned())?
                    .to_owned();
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                state
                    .lots
                    .entry(lot_name.clone())
                    .or_default()
                    .insert(id.clone(), value);
                self.save(&state)?;
                Ok(json!({ "lot": lot_name, "id": id, "parked": true }))
            }
            "unpark" => {
                let lot_name = Self::lot_name(params);
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "parking.unpark needs a string `id`".to_owned())?;
                let value = state
                    .lots
                    .get_mut(&lot_name)
                    .and_then(|lot| lot.remove(id))
                    .ok_or_else(|| format!("`{id}` is not parked in `{lot_name}`"))?;
                if state.lots.get(&lot_name).is_some_and(Lot::is_empty) {
                    state.lots.remove(&lot_name);
                }
                self.save(&state)?;
                Ok(json!({ "lot": lot_name, "id": id, "value": value }))
            }
            "list" => {
                let filtered: Lots = match params.get("lot").and_then(Value::as_str) {
                    Some(lot) => state
                        .lots
                        .get(lot)
                        .map(|entries| BTreeMap::from([(lot.to_owned(), entries.clone())]))
                        .unwrap_or_default(),
                    None => state.lots.clone(),
                };
                Ok(json!({ "entries": filtered }))
            }
            "lots" => Ok(json!({ "lots": state.lots.keys().collect::<Vec<_>>() })),
            "clear" => {
                let cleared = match params.get("lot").and_then(Value::as_str) {
                    Some(lot) => usize::from(state.lots.remove(lot).is_some()),
                    None => {
                        let count = state.lots.len();
                        state.lots.clear();
                        count
                    }
                };
                self.save(&state)?;
                Ok(json!({ "cleared": cleared }))
            }
            other => Err(format!("unknown action `parking.{other}`")),
        }
    }
}

/// Default store path helper.
pub fn default_path() -> PathBuf {
    companion_core::state_dir().join("parking.json")
}

/// Loads a store from an explicit path.
pub fn load_at(path: &Path) -> std::io::Result<Parking> {
    Parking::load(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-parking-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("parking.json")
    }

    #[test]
    fn park_unpark_and_persist() {
        let path = temp_path("roundtrip");
        let parking = Parking::load(&path).unwrap();

        parking
            .call(
                "park",
                &json!({ "lot": "main", "id": "win-1", "value": { "title": "editor" } }),
            )
            .unwrap();
        let listed = parking.call("list", &json!({ "lot": "main" })).unwrap();
        assert_eq!(listed["entries"]["main"]["win-1"]["title"], "editor");

        // A second instance sees the persisted entry.
        let reloaded = Parking::load(&path).unwrap();
        let value = reloaded
            .call("unpark", &json!({ "lot": "main", "id": "win-1" }))
            .unwrap();
        assert_eq!(value["value"]["title"], "editor");

        let missing = reloaded.call("unpark", &json!({ "lot": "main", "id": "win-1" }));
        assert!(missing.is_err());
    }

    #[test]
    fn take_removes_atomically_and_reports_missing() {
        let path = temp_path("take");
        let parking = Parking::load(&path).unwrap();
        parking
            .call("park", &json!({ "lot": "main", "id": "a", "value": 1 }))
            .unwrap();
        assert_eq!(parking.take("main", "a").unwrap(), Some(json!(1)));
        assert_eq!(parking.take("main", "a").unwrap(), None);
        assert!(parking.lot("main").is_empty());
    }

    #[test]
    fn clear_one_lot_and_every_lot() {
        let path = temp_path("clear");
        let parking = Parking::load(&path).unwrap();
        for (lot, id) in [("a", "1"), ("a", "2"), ("b", "3")] {
            parking
                .call("park", &json!({ "lot": lot, "id": id, "value": id }))
                .unwrap();
        }
        assert_eq!(
            parking.call("clear", &json!({ "lot": "a" })).unwrap()["cleared"],
            1
        );
        let lots = parking.call("lots", &Value::Null).unwrap();
        assert_eq!(lots["lots"], json!(["b"]));
        assert_eq!(parking.call("clear", &Value::Null).unwrap()["cleared"], 1);
    }
}
