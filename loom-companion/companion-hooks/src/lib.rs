//! Event hooks: commands registered against named compositor events.
//!
//! Hooks are stored, listed and resolved by the daemon. `hooks.fire` returns the
//! matching commands and the event payload; execution is deliberately left to a
//! caller that opts in, so the daemon never runs arbitrary commands implicitly.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use companion_core::{ActionInfo, Context, ParamInfo, load_json, save_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    hooks: BTreeMap<String, Vec<String>>,
}

pub struct Hooks {
    path: PathBuf,
    state: Mutex<State>,
}

impl Hooks {
    pub fn load(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn from_state_dir() -> std::io::Result<Self> {
        Self::load(companion_core::state_dir().join("hooks.json"))
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state).map_err(|err| format!("cannot save hooks state: {err}"))
    }

    /// Replaces the command list for each configured event.
    pub fn apply_config(&self, hooks: &BTreeMap<String, Vec<String>>) -> Result<usize, String> {
        for (event, commands) in hooks {
            if event.trim().is_empty() {
                return Err("hook event must not be empty".to_owned());
            }
            if commands.iter().any(|command| command.trim().is_empty()) {
                return Err(format!("hook `{event}` contains an empty command"));
            }
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "hooks state lock poisoned".to_owned())?;
        for (event, commands) in hooks {
            if commands.is_empty() {
                state.hooks.remove(event);
            } else {
                state.hooks.insert(event.clone(), commands.clone());
            }
        }
        self.save(&state)?;
        Ok(hooks.len())
    }
}

impl Context for Hooks {
    fn namespace(&self) -> &'static str {
        "hooks"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("add", "Register `command` for `event`").with_params([
                ParamInfo::string("event", true, "event name"),
                ParamInfo::string("command", true, "shell command (resolved, not executed)"),
            ]),
            ActionInfo::new("remove", "Remove `command` from `event`").with_params([
                ParamInfo::string("event", true, "event name"),
                ParamInfo::string("command", true, "command to remove"),
            ]),
            ActionInfo::new("list", "List hooks, optionally for one `event`")
                .with_params([ParamInfo::string("event", false, "filter by event name")]),
            ActionInfo::new(
                "fire",
                "Return matching commands for `event` (does not execute)",
            )
            .with_params([
                ParamInfo::string("event", true, "event name"),
                ParamInfo::object("payload", false, "event payload to echo back"),
            ]),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "hooks state lock poisoned".to_owned())?;

        match action {
            "add" => {
                let event = params
                    .get("event")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "hooks.add needs a string `event`".to_owned())?
                    .to_owned();
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "hooks.add needs a string `command`".to_owned())?
                    .to_owned();
                let commands = state.hooks.entry(event.clone()).or_default();
                if !commands.contains(&command) {
                    commands.push(command.clone());
                }
                self.save(&state)?;
                Ok(json!({ "event": event, "command": command }))
            }
            "remove" => {
                let event = params
                    .get("event")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "hooks.remove needs a string `event`".to_owned())?;
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "hooks.remove needs a string `command`".to_owned())?;
                let removed = match state.hooks.get_mut(event) {
                    Some(commands) => {
                        let before = commands.len();
                        commands.retain(|c| c != command);
                        let after = commands.len();
                        if after == 0 {
                            state.hooks.remove(event);
                        }
                        after != before
                    }
                    None => false,
                };
                self.save(&state)?;
                Ok(json!({ "event": event, "removed": removed }))
            }
            "list" => {
                let filtered: BTreeMap<_, _> = match params.get("event").and_then(Value::as_str) {
                    Some(event) => state
                        .hooks
                        .get(event)
                        .map(|commands| BTreeMap::from([(event.to_owned(), commands.clone())]))
                        .unwrap_or_default(),
                    None => state.hooks.clone(),
                };
                Ok(json!({ "hooks": filtered }))
            }
            "fire" => {
                let event = params
                    .get("event")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "hooks.fire needs a string `event`".to_owned())?;
                let commands = state.hooks.get(event).cloned().unwrap_or_default();
                let payload = params.get("payload").cloned().unwrap_or(Value::Null);
                Ok(json!({ "event": event, "commands": commands, "payload": payload }))
            }
            other => Err(format!("unknown action `hooks.{other}`")),
        }
    }
}

pub fn default_path() -> PathBuf {
    companion_core::state_dir().join("hooks.json")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-hooks-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("hooks.json")
    }

    #[test]
    fn add_fire_and_remove_hooks() {
        let hooks = Hooks::load(temp_path("fire")).unwrap();
        let add = |event: &str, command: &str| {
            hooks
                .call("add", &json!({ "event": event, "command": command }))
                .unwrap()
        };
        add("workspace-activated", "notify-send up");
        add("workspace-activated", "echo up");
        add("workspace-activated", "notify-send up");

        let fired = hooks
            .call(
                "fire",
                &json!({ "event": "workspace-activated", "payload": { "id": 2 } }),
            )
            .unwrap();
        assert_eq!(fired["commands"], json!(["notify-send up", "echo up"]));
        assert_eq!(fired["payload"]["id"], 2);

        hooks
            .call(
                "remove",
                &json!({ "event": "workspace-activated", "command": "echo up" }),
            )
            .unwrap();
        let listed = hooks.call("list", &Value::Null).unwrap();
        assert_eq!(
            listed["hooks"]["workspace-activated"],
            json!(["notify-send up"])
        );
    }
}
