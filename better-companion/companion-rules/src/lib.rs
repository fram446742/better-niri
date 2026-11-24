//! Declarative window/workspace rules (companion Phase B).
//!
//! Rules match the auto-tracked windows from `companion-niri` and carry a list
//! of steps. `rules.apply` explains what would run; `rules.run` is executed by
//! the registry, so steps can target parking, parker, automation, hooks, etc.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use companion_core::{
    AUTO_WINDOWS, AUTO_WORKSPACES, ActionInfo, Context, Step, load_json, save_json,
};
use companion_parking::Parking;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Window/workspace match conditions.
///
/// Substring conditions are case-insensitive. Regex conditions use
/// case-insensitive matching. Negated conditions (`*_not`) pass when the field
/// is absent. Every populated field is ANDed.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Match {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id_regex: Option<String>,
    /// App id must *not* contain this substring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id_not: Option<String>,
    /// App id must *not* match this regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id_not_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_regex: Option<String>,
    /// Title must *not* contain this substring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_not: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<u64>,
    /// Workspace name substring, resolved from `auto:workspaces`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    /// Workspace name regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_floating: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_minimized: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_sticky: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_urgent: Option<bool>,
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

    fn matching_rules(&self, window: Option<u64>) -> Result<Vec<RuleMatch>, String> {
        let windows = self.parking.lot(AUTO_WINDOWS);
        let workspace_names: BTreeMap<u64, String> = self
            .parking
            .lot(AUTO_WORKSPACES)
            .into_iter()
            .filter_map(|(id, value)| {
                let id = id.parse().ok()?;
                let name = value.get("name").and_then(Value::as_str)?;
                Some((id, name.to_owned()))
            })
            .collect();
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
            let workspace_name = value
                .get("workspace_id")
                .and_then(Value::as_u64)
                .and_then(|workspace_id| workspace_names.get(&workspace_id))
                .map(String::as_str);
            for (name, rule) in &state.rules {
                if rule_matches_with(&rule.matcher, value, workspace_name) {
                    matches.push(RuleMatch {
                        window: id,
                        name: name.clone(),
                        matcher: rule.matcher.clone(),
                        steps: rule.steps.clone(),
                        window_state: window_state(value, workspace_name),
                    });
                }
            }
        }
        Ok(matches)
    }

    fn matched_steps_inner(&self, window: Option<u64>) -> Result<Vec<Step>, String> {
        Ok(self
            .matching_rules(window)?
            .into_iter()
            .flat_map(|rule| rule.steps)
            .collect())
    }
}

struct RuleMatch {
    window: u64,
    name: String,
    matcher: Match,
    steps: Vec<Step>,
    window_state: Value,
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

fn regex_matches(haystack: &Value, field: &str, pattern: &str) -> bool {
    haystack
        .get(field)
        .and_then(Value::as_str)
        .is_some_and(|value| {
            RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .is_ok_and(|regex| regex.is_match(value))
        })
}

fn string_regex_matches(value: Option<&str>, pattern: &str) -> bool {
    value.is_some_and(|value| {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .is_ok_and(|regex| regex.is_match(value))
    })
}

fn name_contains(value: Option<&str>, needle: &str) -> bool {
    value.is_some_and(|value| value.to_lowercase().contains(&needle.to_lowercase()))
}

fn window_state(window: &Value, workspace_name: Option<&str>) -> Value {
    json!({
        "id": window.get("id").cloned().unwrap_or(Value::Null),
        "app_id": window.get("app_id").cloned().unwrap_or(Value::Null),
        "title": window.get("title").cloned().unwrap_or(Value::Null),
        "workspace_id": window.get("workspace_id").cloned().unwrap_or(Value::Null),
        "workspace_name": workspace_name,
        "is_floating": window.get("is_floating").and_then(Value::as_bool),
        "is_minimized": window.get("is_minimized").and_then(Value::as_bool),
        "is_sticky": window.get("is_sticky").and_then(Value::as_bool),
        "is_urgent": window.get("is_urgent").and_then(Value::as_bool),
    })
}

fn validate_matcher(matcher: &Match) -> Result<(), String> {
    let patterns = [
        ("app_id_regex", matcher.app_id_regex.as_deref()),
        ("app_id_not_regex", matcher.app_id_not_regex.as_deref()),
        ("title_regex", matcher.title_regex.as_deref()),
        (
            "workspace_name_regex",
            matcher.workspace_name_regex.as_deref(),
        ),
    ];
    for (field, pattern) in patterns {
        let Some(pattern) = pattern else {
            continue;
        };
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|err| format!("invalid `{field}` regex `{pattern}`: {err}"))?;
    }
    Ok(())
}

/// Matches `window` using only fields carried by the window value itself.
pub fn rule_matches(matcher: &Match, window: &Value) -> bool {
    rule_matches_with(matcher, window, None)
}

/// Matches `window`, optionally resolving `workspace_name` from the
/// auto-tracked workspace lot (or a `workspace_name` field on the window).
pub fn rule_matches_with(matcher: &Match, window: &Value, workspace_name: Option<&str>) -> bool {
    if let Some(app_id) = &matcher.app_id
        && !contains(window, "app_id", app_id)
    {
        return false;
    }
    if let Some(pattern) = &matcher.app_id_regex
        && !regex_matches(window, "app_id", pattern)
    {
        return false;
    }
    if let Some(app_id) = &matcher.app_id_not
        && contains(window, "app_id", app_id)
    {
        return false;
    }
    if let Some(pattern) = &matcher.app_id_not_regex
        && regex_matches(window, "app_id", pattern)
    {
        return false;
    }
    if let Some(title) = &matcher.title
        && !contains(window, "title", title)
    {
        return false;
    }
    if let Some(pattern) = &matcher.title_regex
        && !regex_matches(window, "title", pattern)
    {
        return false;
    }
    if let Some(title) = &matcher.title_not
        && contains(window, "title", title)
    {
        return false;
    }
    if let Some(workspace) = matcher.workspace
        && window.get("workspace_id").and_then(Value::as_u64) != Some(workspace)
    {
        return false;
    }
    let workspace_name =
        workspace_name.or_else(|| window.get("workspace_name").and_then(Value::as_str));
    if let Some(name) = &matcher.workspace_name
        && !name_contains(workspace_name, name)
    {
        return false;
    }
    if let Some(pattern) = &matcher.workspace_name_regex
        && !string_regex_matches(workspace_name, pattern)
    {
        return false;
    }
    if let Some(floating) = matcher.is_floating
        && !bool_is(window, "is_floating", floating)
    {
        return false;
    }
    if let Some(minimized) = matcher.is_minimized
        && !bool_is(window, "is_minimized", minimized)
    {
        return false;
    }
    if let Some(sticky) = matcher.is_sticky
        && !bool_is(window, "is_sticky", sticky)
    {
        return false;
    }
    if let Some(urgent) = matcher.is_urgent
        && !bool_is(window, "is_urgent", urgent)
    {
        return false;
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
            ActionInfo::new(
                "explain",
                "Matching rules for `window`, with matcher and window state",
            ),
            ActionInfo::new(
                "apply",
                "Show which rules match and their step counts (no execution)",
            ),
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
                validate_matcher(&matcher)?;
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
                    .map(|matched| {
                        json!({
                            "window": matched.window,
                            "rule": matched.name,
                            "match": matched.matcher,
                            "window_state": matched.window_state,
                            "steps": matched.steps,
                        })
                    })
                    .collect();
                Ok(json!({ "window": window, "matches": matches }))
            }
            "apply" => {
                drop(state);
                let window = params.get("window").and_then(Value::as_u64);
                let matches = self.matching_rules(window)?;
                let matches: Vec<Value> = matches
                    .into_iter()
                    .map(|matched| {
                        json!({
                            "window": matched.window,
                            "rule": matched.name,
                            "match": matched.matcher,
                            "window_state": matched.window_state,
                            "step_count": matched.steps.len(),
                        })
                    })
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

    fn with_workspaces(parking: &Parking) {
        parking
            .replace_lot(
                AUTO_WORKSPACES,
                [
                    ("1".to_owned(), json!({ "id": 1, "name": "dev" })),
                    ("2".to_owned(), json!({ "id": 2, "name": "web" })),
                ]
                .into_iter()
                .collect(),
            )
            .unwrap();
    }

    #[test]
    fn matches_regex_negation_workspace_name_and_flags() {
        let firefox = json!({
            "id": 10,
            "app_id": "firefox",
            "title": "Mozilla",
            "workspace_id": 1,
            "is_floating": false,
            "is_minimized": false,
            "is_sticky": false,
            "is_urgent": true,
        });
        assert!(rule_matches_with(
            &Match {
                app_id_regex: Some("^fire".into()),
                title_regex: Some("zilla$".into()),
                app_id_not: Some("chrome".into()),
                ..Match::default()
            },
            &firefox,
            Some("dev"),
        ));
        assert!(!rule_matches_with(
            &Match {
                app_id_not: Some("FIRE".into()),
                ..Match::default()
            },
            &firefox,
            None,
        ));
        assert!(!rule_matches_with(
            &Match {
                app_id_not_regex: Some("fox$".into()),
                ..Match::default()
            },
            &firefox,
            None,
        ));
        assert!(rule_matches_with(
            &Match {
                workspace_name: Some("DEV".into()),
                is_sticky: Some(false),
                is_urgent: Some(true),
                is_minimized: Some(false),
                ..Match::default()
            },
            &firefox,
            Some("dev"),
        ));
        assert!(!rule_matches_with(
            &Match {
                workspace_name_regex: Some("^web".into()),
                ..Match::default()
            },
            &firefox,
            Some("dev"),
        ));
        // A window carrying its own workspace_name works without the lot.
        let with_name = json!({ "id": 10, "workspace_name": "dev" });
        assert!(rule_matches(
            &Match {
                workspace_name: Some("dev".into()),
                ..Match::default()
            },
            &with_name,
        ));
    }

    #[test]
    fn explain_and_apply_report_matcher_and_window_state() {
        let parking = parking_with_windows("explain");
        with_workspaces(&parking);
        let rules = Rules::load(temp_dir("explain-rules").join("rules.json"), parking).unwrap();
        rules
            .call(
                "set",
                &json!({
                    "name": "browser",
                    "match": { "title_regex": "^moz", "app_id_not": "chrome" },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap();

        let explained = rules.call("explain", &json!({ "window": 10 })).unwrap();
        let matched = &explained["matches"][0];
        assert_eq!(matched["rule"], "browser");
        assert_eq!(matched["match"]["title_regex"], "^moz");
        assert_eq!(matched["match"]["app_id_not"], "chrome");
        assert_eq!(matched["window_state"]["workspace_name"], "dev");
        assert_eq!(matched["window_state"]["title"], "Mozilla");
        assert_eq!(matched["steps"][0]["action"], "ping");

        let applied = rules.call("apply", &json!({ "window": 10 })).unwrap();
        let matched = &applied["matches"][0];
        assert_eq!(matched["window"], 10);
        assert_eq!(matched["step_count"], 1);
        assert_eq!(matched["match"]["app_id_not"], "chrome");
    }

    #[test]
    fn invalid_regex_is_rejected_and_legacy_rules_load() {
        let parking = parking_with_windows("legacy");
        let path = temp_dir("legacy-rules").join("rules.json");
        let rules = Rules::load(&path, parking.clone()).unwrap();
        let error = rules
            .call(
                "set",
                &json!({
                    "name": "bad",
                    "match": { "title_regex": "[" },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap_err();
        assert!(error.contains("invalid `title_regex`"), "{error}");
        assert!(
            rules.call("list", &Value::Null).unwrap()["rules"]
                .as_object()
                .unwrap()
                .is_empty()
        );

        // A rule file written before the new matcher fields still loads.
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "rules": {
                    "old": {
                        "match": { "app_id": "fire" },
                        "steps": [{ "namespace": "system", "action": "ping" }]
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let legacy = Rules::load(&path, parking).unwrap();
        let explained = legacy.call("explain", &json!({ "window": 10 })).unwrap();
        assert_eq!(explained["matches"][0]["rule"], "old");
        assert!(explained["matches"][0]["match"]["title_regex"].is_null());
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
        assert!(
            listed["rules"]["floating"]["match"]["is_floating"]
                .as_bool()
                .unwrap()
        );

        reloaded
            .call("remove", &json!({ "name": "floating" }))
            .unwrap();
        let listed = reloaded.call("list", &Value::Null).unwrap();
        assert!(listed["rules"].as_object().unwrap().is_empty());
    }
}
