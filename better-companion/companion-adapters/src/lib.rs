//! Optional ecosystem adapters (companion Phase C).
//!
//! Four bounded contexts share one adapter engine:
//!
//! - `session.*` → better-session / logind
//! - `monitor.*` → better-monitor
//! - `audio.*` → better-audio
//! - `wallpaper.*` → better-wallpaper
//!
//! Each adapter stores explicit commands and event hooks. Nothing runs unless a
//! caller explicitly invokes `dispatch`; hooks only resolve commands. This keeps
//! the adapters optional and free of hard dependencies on the companion apps.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use companion_core::{ActionInfo, Context, load_json, save_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    enabled: bool,
    commands: BTreeMap<String, String>,
    hooks: BTreeMap<String, String>,
}

pub struct Adapter {
    namespace: &'static str,
    path: PathBuf,
    state: Mutex<State>,
}

impl Adapter {
    pub fn load(namespace: &'static str, path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            namespace,
            path,
            state: Mutex::new(state),
        })
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state)
            .map_err(|err| format!("cannot save {} adapter: {err}", self.namespace))
    }

    fn require_enabled<'a>(&self, state: &'a State) -> Result<&'a State, String> {
        if state.enabled {
            Ok(state)
        } else {
            Err(format!(
                "{} adapter is disabled; call {}.enable first",
                self.namespace, self.namespace
            ))
        }
    }

    fn dispatch(state: &State, name: &str) -> Result<Value, String> {
        let command = state
            .commands
            .get(name)
            .ok_or_else(|| format!("no command `{name}` configured"))?;
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|err| format!("cannot run `{name}`: {err}"))?;
        Ok(json!({
            "command": name,
            "success": output.status.success(),
            "code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))
    }
}

impl Context for Adapter {
    fn namespace(&self) -> &'static str {
        self.namespace
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("status", "Enabled state, commands and hooks"),
            ActionInfo::new("enable", "Enable this adapter (required for dispatch)"),
            ActionInfo::new("disable", "Disable this adapter"),
            ActionInfo::new("set", "Store command `name` = `command`"),
            ActionInfo::new("remove", "Remove command `name`"),
            ActionInfo::new("list", "List configured commands"),
            ActionInfo::new("dispatch", "Run command `name` explicitly"),
            ActionInfo::new("set-hook", "Store event → command for this adapter"),
            ActionInfo::new("remove-hook", "Remove an event hook"),
            ActionInfo::new("hooks", "List event → command hooks"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "adapter state lock poisoned".to_owned())?;

        match action {
            "status" => Ok(json!({
                "namespace": self.namespace,
                "enabled": state.enabled,
                "commands": state.commands,
                "hooks": state.hooks,
            })),
            "enable" => {
                state.enabled = true;
                self.save(&state)?;
                Ok(json!({ "enabled": true }))
            }
            "disable" => {
                state.enabled = false;
                self.save(&state)?;
                Ok(json!({ "enabled": false }))
            }
            "set" => {
                let name = param_str(params, "name")?;
                let command = param_str(params, "command")?;
                state.commands.insert(name.clone(), command.clone());
                self.save(&state)?;
                Ok(json!({ "name": name, "command": command }))
            }
            "remove" => {
                let name = param_str(params, "name")?;
                let removed = state.commands.remove(&name).is_some();
                self.save(&state)?;
                Ok(json!({ "name": name, "removed": removed }))
            }
            "list" => Ok(json!({ "commands": state.commands })),
            "dispatch" => {
                let name = param_str(params, "name")?;
                let state = self.require_enabled(&state)?;
                Self::dispatch(state, &name)
            }
            "set-hook" => {
                let event = param_str(params, "event")?;
                let command = param_str(params, "command")?;
                state.hooks.insert(event.clone(), command.clone());
                self.save(&state)?;
                Ok(json!({ "event": event, "command": command }))
            }
            "remove-hook" => {
                let event = param_str(params, "event")?;
                let removed = state.hooks.remove(&event).is_some();
                self.save(&state)?;
                Ok(json!({ "event": event, "removed": removed }))
            }
            "hooks" => Ok(json!({ "hooks": state.hooks })),
            other => Err(format!("unknown action `{}.{other}`", self.namespace)),
        }
    }
}

fn param_str(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing string `{key}`"))
}

/// Loads the four default Phase C adapters from the state directory.
pub fn default_adapters() -> std::io::Result<Vec<Adapter>> {
    let dir = companion_core::state_dir();
    Ok(vec![
        Adapter::load("session", dir.join("adapter-session.json"))?,
        Adapter::load("monitor", dir.join("adapter-monitor.json"))?,
        Adapter::load("audio", dir.join("adapter-audio.json"))?,
        Adapter::load("wallpaper", dir.join("adapter-wallpaper.json"))?,
    ])
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-adapter-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("adapter.json")
    }

    #[test]
    fn dispatch_requires_enable_and_runs_explicit_commands() {
        let adapter = Adapter::load("audio", temp_path("dispatch")).unwrap();
        adapter
            .call(
                "set",
                &json!({ "name": "greet", "command": "printf adapter-ok" }),
            )
            .unwrap();

        let disabled = adapter.call("dispatch", &json!({ "name": "greet" }));
        assert!(disabled.is_err());

        adapter.call("enable", &Value::Null).unwrap();
        let value = adapter
            .call("dispatch", &json!({ "name": "greet" }))
            .unwrap();
        assert_eq!(value["success"], true);
        assert_eq!(value["stdout"], "adapter-ok");
    }

    #[test]
    fn commands_and_hooks_persist_and_remove() {
        let path = temp_path("persist");
        let adapter = Adapter::load("monitor", path.clone()).unwrap();
        adapter
            .call(
                "set",
                &json!({ "name": "cycle", "command": "niri msg action focus-monitor-next" }),
            )
            .unwrap();
        adapter
            .call(
                "set-hook",
                &json!({ "event": "workspace-activated", "command": "notify-send changed" }),
            )
            .unwrap();

        let reloaded = Adapter::load("monitor", path).unwrap();
        let status = reloaded.call("status", &Value::Null).unwrap();
        assert_eq!(
            status["commands"]["cycle"],
            "niri msg action focus-monitor-next"
        );
        assert_eq!(
            status["hooks"]["workspace-activated"],
            "notify-send changed"
        );

        reloaded
            .call("remove", &json!({ "name": "cycle" }))
            .unwrap();
        reloaded
            .call("remove-hook", &json!({ "event": "workspace-activated" }))
            .unwrap();
        let status = reloaded.call("status", &Value::Null).unwrap();
        assert!(status["commands"].as_object().unwrap().is_empty());
        assert!(status["hooks"].as_object().unwrap().is_empty());
    }

    #[test]
    fn default_adapters_have_distinct_namespaces() {
        let adapters = default_adapters().unwrap();
        let names: Vec<_> = adapters.iter().map(Context::namespace).collect();
        assert_eq!(names, ["session", "monitor", "audio", "wallpaper"]);
    }
}
