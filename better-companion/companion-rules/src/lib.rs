//! Declarative window/workspace rules (companion Phase B).
//!
//! Rules match the auto-tracked windows from `companion-niri` and carry a list
//! of steps. `rules.apply` explains what would run; `rules.run` is executed by
//! the registry, so steps can target parking, parker, automation, hooks, etc.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use companion_core::{load_json, save_json, ActionInfo, Context, Step, AUTO_WINDOWS};
use companion_parking::Parking;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_floating: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_minimized: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    #[serde(rename = "match", default)]
    pub matcher: Match,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    rules: BTreeMap<String, Rule>,
}

pub struct Rules {
    path: PathBuf,
    state: Mutex<State>,
    parking: Parking,
}

impl Rules {
    pub fn load(path: impl Into<PathBuf>, parking: Parking) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
            parking,
        })
    }

    pub fn from_state_dir(parking: Parking) -> std::io::Result<Self> {
        Self::load(companion_core::state_dir().join("rules.json"), parking)
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state).map_err(|err| format!("cannot save rules: {err}"))
    }

    fn matching_rules(&self, window: Option<u64>) -> Result<Vec<(u64, String, Vec<Step>)>, String> {
        let windows = self.parking.lot(AUTO_WINDOWS);
        let state = self
            .state
            .lock()
            .map_err(|_| "rules state lock poisoned".to_owned())?;

        let mut matches = Vec::new();
        for (id, value) in &windows {
            let Ok(id) = id.parse::<u64>() else {
                continue;
            };
            if window.is_some_and(|want| want != id) {
                continue;
            }
            for (name, rule) in &state.rules {
                if rule_matches(&rule.matcher, value) {
                    matches.push((id, name.clone(), rule.steps.clone()));
                }
            }
        }
        Ok(matches)
    }

    fn matched_steps_inner(&self, window: Option<u64>) -> Result<Vec<Step>, String> {
        Ok(self
            .matching_rules(window)?
            .into_iter()
            .flat_map(|(_, _, steps)| steps)
            .collect())
    }
}

fn contains(haystack: &Value, field: &str, needle: &str) -> bool {
    haystack
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|value| value.to_lowercase().contains(&needle.to_lowercase()))
}

fn bool_is(value: &Value, field: &str, want: bool) -> bool {
    value.get(field).and_then(Value::as_bool) == Some(want)
}

pub fn rule_matches(matcher: &Match, window: &Value) -> bool {
    if let Some(app_id) = &matcher.app_id {
        if !contains(window, "app_id", app_id) {
            return false;
        }
    }
    if let Some(title) = &matcher.title {
        if !contains(window, "title", title) {
            return false;
        }
    }
    if let Some(workspace) = matcher.workspace {
        if window.get("workspace_id").and_then(Value::as_u64) != Some(workspace) {
            return false;
        }
    }
    if let Some(floating) = matcher.is_floating {
        if !bool_is(window, "is_floating", floating) {
            return false;
        }
    }
    if let Some(minimized) = matcher.is_minimized {
        if !bool_is(window, "is_minimized", minimized) {
            return false;
        }
    }
    true
}

impl Context for Rules {
    fn namespace(&self) -> &'static str {
        "rules"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("set", "Define/replace `name` with `match` and `steps`"),
            ActionInfo::new("remove", "Delete rule `name`"),
            ActionInfo::new("list", "List rules"),
            ActionInfo::new("explain", "Show rules, or matching rules for `window`"),
            ActionInfo::new("apply", "Show which rules match (no execution)"),
            ActionInfo::new("run", "Execute the steps of matching rules"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "rules state lock poisoned".to_owned())?;

        match action {
            "set" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "rules.set needs a string `name`".to_owned())?
                    .to_owned();
                let matcher: Match = serde_json::from_value(
                    params.get("match").cloned().unwrap_or_else(|| json!({})),
                )
                .map_err(|err| format!("invalid rule match: {err}"))?;
                let steps: Vec<Step> = serde_json::from_value(
                    params.get("steps").cloned().unwrap_or_else(|| json!([])),
                )
                .map_err(|err| format!("invalid rule steps: {err}"))?;
                if steps.is_empty() {
                    return Err("rules.set needs at least one step".to_owned());
                }
                state.rules.insert(
                    name.clone(),
                    Rule {
                        matcher,
                        steps: steps.clone(),
                    },
                );
                self.save(&state)?;
                Ok(json!({ "name": name, "steps": steps.len() }))
            }
            "remove" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "rules.remove needs a string `name`".to_owned())?;
                let removed = state.rules.remove(name).is_some();
                self.save(&state)?;
                Ok(json!({ "name": name, "removed": removed }))
            }
            "list" => Ok(json!({ "rules": state.rules })),
            "explain" => {
                drop(state);
                let window = params.get("window").and_then(Value::as_u64);
                let matches = self.matching_rules(window)?;
                let matches: Vec<Value> = matches
                    .into_iter()
                    .map(|(id, rule, steps)| json!({ "window": id, "rule": rule, "steps": steps }))
                    .collect();
                Ok(json!({ "window": window, "matches": matches }))
            }
            "apply" => {
                drop(state);
                let window = params.get("window").and_then(Value::as_u64);
                let matches = self.matching_rules(window)?;
                let matches: Vec<Value> = matches
                    .into_iter()
                    .map(|(id, rule, _)| json!({ "window": id, "rule": rule }))
                    .collect();
                Ok(json!({ "window": window, "matches": matches }))
            }
            "run" => Err("rules.run is executed by the registry".to_owned()),
            other => Err(format!("unknown action `rules.{other}`")),
        }
    }

    fn matched_steps(&self, window: Option<u64>) -> Option<Result<Vec<Step>, String>> {
        Some(self.matched_steps_inner(window))
    }
}

pub fn default_path() -> PathBuf {
    companion_core::state_dir().join("rules.json")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use companion_core::{Registry, Request, Response};
    use companion_parking::Parking;
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-rules-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn parking_with_windows(name: &str) -> Parking {
        let parking = Parking::load(temp_dir(name).join("parking.json")).unwrap();
        parking
            .replace_lot(
                AUTO_WINDOWS,
                [
                    (
                        "10".to_owned(),
                        json!({ "id": 10, "app_id": "firefox", "title": "Mozilla", "workspace_id": 1, "is_floating": false, "is_minimized": false }),
                    ),
                    (
                        "11".to_owned(),
                        json!({ "id": 11, "app_id": "Alacritty", "title": "Terminal", "workspace_id": 2, "is_floating": true, "is_minimized": false }),
                    ),
                ]
                .into_iter()
                .collect(),
            )
            .unwrap();
        parking
    }

    #[test]
    fn matches_case_insensitively_and_combines_predicates() {
        let firefox = json!({ "id": 10, "app_id": "firefox", "title": "Mozilla", "workspace_id": 1, "is_floating": false });
        assert!(rule_matches(
            &Match {
                app_id: Some("FIRE".into()),
                ..Match::default()
            },
            &firefox
        ));
        assert!(rule_matches(
            &Match {
                title: Some("zilla".into()),
                workspace: Some(1),
                is_floating: Some(false),
                ..Match::default()
            },
            &firefox
        ));
        assert!(!rule_matches(
            &Match {
                workspace: Some(2),
                ..Match::default()
            },
            &firefox
        ));
    }

    struct Counter(Arc<Mutex<Vec<String>>>);

    impl Context for Counter {
        fn namespace(&self) -> &'static str {
            "counter"
        }

        fn actions(&self) -> Vec<ActionInfo> {
            vec![ActionInfo::new("push", "Push `value`")]
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
                    Ok(json!(value))
                }
                other => Err(format!("unknown action `counter.{other}`")),
            }
        }
    }

    #[test]
    fn apply_explains_and_run_executes_through_the_registry() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let parking = parking_with_windows("run");
        let rules = Rules::load(temp_dir("run-rules").join("rules.json"), parking.clone()).unwrap();

        let mut registry = Registry::new();
        registry.register(rules);
        registry.register(Counter(log.clone()));

        let response = registry.handle(Request::Call {
            namespace: "rules".into(),
            action: "set".into(),
            params: json!({
                "name": "browser",
                "match": { "app_id": "fire" },
                "steps": [{ "namespace": "counter", "action": "push", "params": { "value": "browser" } }]
            }),
        });
        assert!(matches!(response, Response::Value { .. }), "{response:?}");

        let applied = registry.handle(Request::Call {
            namespace: "rules".into(),
            action: "apply".into(),
            params: json!({ "window": 10 }),
        });
        match applied {
            Response::Value { value } => {
                assert_eq!(value["matches"][0]["rule"], "browser");
            }
            other => panic!("unexpected: {other:?}"),
        }

        let ran = registry.handle(Request::Call {
            namespace: "rules".into(),
            action: "run".into(),
            params: json!({ "window": 10 }),
        });
        assert!(matches!(ran, Response::Value { .. }), "{ran:?}");
        assert_eq!(*log.lock().unwrap(), ["browser"]);
    }

    #[test]
    fn rules_are_persisted_and_removable() {
        let parking = parking_with_windows("persist");
        let path = temp_dir("persist-rules").join("rules.json");
        let rules = Rules::load(&path, parking).unwrap();
        rules
            .call(
                "set",
                &json!({
                    "name": "floating",
                    "match": { "is_floating": true },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap();

        let reloaded = Rules::load(
            &path,
            Parking::load(temp_dir("persist-parking").join("parking.json")).unwrap(),
        )
        .unwrap();
        let listed = reloaded.call("list", &Value::Null).unwrap();
        assert!(listed["rules"]["floating"]["match"]["is_floating"]
            .as_bool()
            .unwrap());

        reloaded
            .call("remove", &json!({ "name": "floating" }))
            .unwrap();
        let listed = reloaded.call("list", &Value::Null).unwrap();
        assert!(listed["rules"].as_object().unwrap().is_empty());
    }
}
