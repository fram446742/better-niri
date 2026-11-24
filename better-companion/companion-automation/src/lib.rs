//! Named automation sequences.
//!
//! A sequence is an ordered list of `(namespace, action, params)` steps. The
//! registry's `automation.run` executes the steps through the same registry, so
//! a sequence can orchestrate parking, hooks, system queries and other
//! contexts without the daemon growing a monolith.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use companion_core::{ActionInfo, Context, ParamInfo, Step, load_json, save_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    sequences: BTreeMap<String, Vec<Step>>,
}

pub struct Automation {
    path: PathBuf,
    state: Mutex<State>,
}

impl Automation {
    pub fn load(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn from_state_dir() -> std::io::Result<Self> {
        Self::load(companion_core::state_dir().join("automation.json"))
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state).map_err(|err| format!("cannot save automation state: {err}"))
    }

    /// Merges config-file sequences into stored state.
    pub fn apply_config(&self, sequences: &BTreeMap<String, Vec<Step>>) -> Result<usize, String> {
        for (name, steps) in sequences {
            if name.trim().is_empty() {
                return Err("automation sequence needs a non-empty name".to_owned());
            }
            if steps.is_empty() {
                return Err(format!(
                    "automation sequence `{name}` needs at least one step"
                ));
            }
            for step in steps {
                if step.namespace.trim().is_empty() || step.action.trim().is_empty() {
                    return Err(format!(
                        "automation sequence `{name}` has a step without namespace/action"
                    ));
                }
            }
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "automation state lock poisoned".to_owned())?;
        for (name, steps) in sequences {
            state.sequences.insert(name.clone(), steps.clone());
        }
        self.save(&state)?;
        Ok(sequences.len())
    }
}

fn steps_from(params: &Value) -> Result<Vec<Step>, String> {
    let steps = params
        .get("steps")
        .ok_or_else(|| "automation.set needs a `steps` array".to_owned())?;
    let steps: Vec<Step> = serde_json::from_value(steps.clone())
        .map_err(|err| format!("invalid automation steps: {err}"))?;
    for step in &steps {
        if step.namespace.trim().is_empty() || step.action.trim().is_empty() {
            return Err("automation step needs a non-empty namespace and action".to_owned());
        }
    }
    Ok(steps)
}

impl Context for Automation {
    fn namespace(&self) -> &'static str {
        "automation"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("set", "Define/replace `name` with a `steps` array").with_params([
                ParamInfo::string("name", true, "sequence name"),
                ParamInfo::array("steps", true, "ordered {namespace, action, params} steps"),
            ]),
            ActionInfo::new("remove", "Delete sequence `name`").with_params([ParamInfo::string(
                "name",
                true,
                "sequence name",
            )]),
            ActionInfo::new("list", "List sequence names"),
            ActionInfo::new("get", "Return the steps of `name`").with_params([ParamInfo::string(
                "name",
                true,
                "sequence name",
            )]),
            ActionInfo::new("run", "Execute sequence `name` through the registry")
                .with_params([ParamInfo::string("name", true, "sequence name")])
                .with_permissions(&["niri", "shell"]),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "automation state lock poisoned".to_owned())?;

        match action {
            "set" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "automation.set needs a string `name`".to_owned())?
                    .to_owned();
                let steps = steps_from(params)?;
                let count = steps.len();
                state.sequences.insert(name.clone(), steps);
                self.save(&state)?;
                Ok(json!({ "name": name, "steps": count }))
            }
            "remove" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "automation.remove needs a string `name`".to_owned())?;
                let removed = state.sequences.remove(name).is_some();
                self.save(&state)?;
                Ok(json!({ "name": name, "removed": removed }))
            }
            "list" => Ok(json!({ "names": state.sequences.keys().collect::<Vec<_>>() })),
            "get" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "automation.get needs a string `name`".to_owned())?;
                let steps = state
                    .sequences
                    .get(name)
                    .ok_or_else(|| format!("unknown automation sequence `{name}`"))?;
                Ok(json!({ "name": name, "steps": steps }))
            }
            "run" => Err("automation.run is executed by the registry".to_owned()),
            other => Err(format!("unknown action `automation.{other}`")),
        }
    }

    fn sequence(&self, name: &str) -> Option<Vec<Step>> {
        self.state.lock().ok()?.sequences.get(name).cloned()
    }
}

pub fn default_path() -> PathBuf {
    companion_core::state_dir().join("automation.json")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use companion_core::{Registry, Request, Response};
    use serde_json::json;

    use super::*;

    struct Counter(Arc<Mutex<Vec<String>>>);

    impl Context for Counter {
        fn namespace(&self) -> &'static str {
            "counter"
        }

        fn actions(&self) -> Vec<ActionInfo> {
            vec![ActionInfo::new("push", "Push `value` onto the log")]
        }

        fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
            match action {
                "push" => {
                    let value = params
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    self.0.lock().unwrap().push(value.clone());
                    Ok(json!({ "pushed": value }))
                }
                other => Err(format!("unknown action `counter.{other}`")),
            }
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-automation-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("automation.json")
    }

    #[test]
    fn sequence_runs_in_order_through_the_registry() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = Registry::new();
        registry.register(Automation::load(temp_path("order")).unwrap());
        registry.register(Counter(log.clone()));

        registry.handle(Request::Call {
            namespace: "automation".into(),
            action: "set".into(),
            params: json!({
                "name": "greet",
                "steps": [
                    { "namespace": "counter", "action": "push", "params": { "value": "one" } },
                    { "namespace": "counter", "action": "push", "params": { "value": "two" } }
                ]
            }),
            id: None,
        });

        let response = registry.handle(Request::Call {
            namespace: "automation".into(),
            action: "run".into(),
            params: json!({ "name": "greet" }),
            id: None,
        });
        assert!(matches!(response, Response::Value { .. }), "{response:?}");
        assert_eq!(*log.lock().unwrap(), ["one", "two"]);
    }

    #[test]
    fn recursive_automation_run_is_rejected() {
        let mut registry = Registry::new();
        registry.register(Automation::load(temp_path("recursive")).unwrap());
        registry.handle(Request::Call {
            namespace: "automation".into(),
            action: "set".into(),
            params: json!({
                "name": "loop",
                "steps": [
                    { "namespace": "automation", "action": "run", "params": { "name": "loop" } }
                ]
            }),
            id: None,
        });
        let response = registry.handle(Request::Call {
            namespace: "automation".into(),
            action: "run".into(),
            params: json!({ "name": "loop" }),
            id: None,
        });
        assert!(matches!(response, Response::Error { .. }));
    }
}
