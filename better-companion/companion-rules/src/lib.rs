//! Declarative window/workspace rules (companion Phase B).
//!
//! Rules match the auto-tracked windows from `companion-niri` and carry a list
//! of steps. `rules.apply` explains what would run; `rules.run` is executed by
//! the registry, so steps can target parking, parker, automation, hooks, etc.
//!
//! Workspace conditions cover the stable id, name (substring/regex), monitor
//! index and output. Matches are reported in numeric window order and then rule
//! name, keeping explain/apply/run output stable across calls.
//!
//! Rules can also be triggered by workspace events: set
//! `"trigger": "workspace-activated"` (or `workspace-added`,
//! `workspace-removed`, `workspace-moved`) and match workspace fields only.
//! `rules.explain`/`rules.apply`/`rules.run` accept `event` and an optional
//! `workspace_state` object for a workspace that no longer exists. Workspace
//! matches are ordered by numeric workspace id, then rule name.
//!
//! Window marks (`marks.*`) are available as `mark`, `mark_regex` and
//! `mark_not` conditions; the store is shared with `companion-niri` so rule
//! evaluation sees moves and removals immediately.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use companion_core::{
    AUTO_WINDOWS, AUTO_WORKSPACES, ActionInfo, Context, ParamInfo, Step, load_json, save_json,
};
use companion_niri::Marks;
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
    /// Workspace index on its monitor (niri's `idx`), resolved from
    /// `auto:workspaces`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_idx: Option<u8>,
    /// Output name substring, resolved from `auto:workspaces`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_output: Option<String>,
    /// Whether the workspace has no tracked windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_empty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_floating: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_minimized: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_sticky: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_urgent: Option<bool>,
    /// Exact window mark (`marks.set`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<String>,
    /// Window mark regular expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_regex: Option<String>,
    /// Window must *not* carry this mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark_not: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    #[serde(rename = "match", default)]
    pub matcher: Match,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// `window` (or absent) for the legacy window rules, or one of the
    /// workspace events: `workspace-activated`, `workspace-added`,
    /// `workspace-removed`, `workspace-moved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Evaluation order among rules matching the same window or workspace.
    /// Higher runs first; equal priorities fall back to the rule name.
    #[serde(default)]
    pub priority: i32,
}

/// Workspace events that can trigger a rule.
pub const WORKSPACE_EVENTS: &[&str] = &[
    "workspace-activated",
    "workspace-added",
    "workspace-removed",
    "workspace-moved",
];

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    rules: BTreeMap<String, Rule>,
}

pub struct Rules {
    path: PathBuf,
    state: Mutex<State>,
    parking: Parking,
    marks: Option<Marks>,
}

impl Rules {
    pub fn load(path: impl Into<PathBuf>, parking: Parking) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
            parking,
            marks: None,
        })
    }

    /// Shares the mark store so `mark` conditions see current marks.
    pub fn with_marks(mut self, marks: Marks) -> Self {
        self.marks = Some(marks);
        self
    }

    pub fn from_state_dir(parking: Parking) -> std::io::Result<Self> {
        Self::load(companion_core::state_dir().join("rules.json"), parking)
    }

    /// Replaces/inserts the given rules after validation. Used by the config
    /// loader; unrelated stored rules are kept.
    pub fn apply_config(&self, rules: &BTreeMap<String, Rule>) -> Result<usize, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "rules state lock poisoned".to_owned())?;
        for (name, rule) in rules {
            validate_rule(rule)?;
            state.rules.insert(name.clone(), rule.clone());
        }
        self.save(&state)?;
        Ok(rules.len())
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state).map_err(|err| format!("cannot save rules: {err}"))
    }

    fn matching_rules(&self, window: Option<u64>) -> Result<Vec<RuleMatch>, String> {
        let windows = self.parking.lot(AUTO_WINDOWS);
        let counts = windows_per_workspace(&self.parking);
        let workspace_facts: BTreeMap<u64, WorkspaceFacts> = self
            .parking
            .lot(AUTO_WORKSPACES)
            .into_iter()
            .filter_map(|(id, value)| {
                let id = id.parse().ok()?;
                let mut facts = WorkspaceFacts::from_value(&value);
                facts.empty = Some(counts.get(&id).copied().unwrap_or(0) == 0);
                Some((id, facts))
            })
            .collect();
        let state = self
            .state
            .lock()
            .map_err(|_| "rules state lock poisoned".to_owned())?;

        // Numeric id order, not the string order of the lot keys, so explain,
        // apply and run report stable results for ids like 2 and 10.
        let mut ordered: Vec<(u64, Value)> = windows
            .iter()
            .filter_map(|(id, value)| Some((id.parse().ok()?, value.clone())))
            .filter(|(id, _)| window.is_none_or(|want| want == *id))
            .collect();
        ordered.sort_by_key(|(id, _)| *id);

        let marks = self.marks.as_ref().map(Marks::snapshot).unwrap_or_default();
        let mut matches = Vec::new();
        for (id, mut value) in ordered {
            if let Some(mark) = marks.get(&id) {
                value["mark"] = json!(mark);
            }
            let facts = value
                .get("workspace_id")
                .and_then(Value::as_u64)
                .and_then(|workspace_id| workspace_facts.get(&workspace_id));
            for (name, rule) in &state.rules {
                if is_workspace_trigger(rule.trigger.as_deref()) {
                    continue;
                }
                if rule_matches_in(&rule.matcher, &value, facts) {
                    matches.push(RuleMatch {
                        window: id,
                        name: name.clone(),
                        priority: rule.priority,
                        matcher: rule.matcher.clone(),
                        steps: rule.steps.clone(),
                        window_state: window_state(&value, facts),
                    });
                }
            }
        }
        // Same window: higher priority first, then a stable name tie-break.
        matches.sort_by(|a, b| {
            a.window
                .cmp(&b.window)
                .then(b.priority.cmp(&a.priority))
                .then(a.name.cmp(&b.name))
        });
        Ok(matches)
    }

    fn matching_workspace_rules(
        &self,
        event: &str,
        state: Option<&Value>,
    ) -> Result<Vec<WorkspaceRuleMatch>, String> {
        if !WORKSPACE_EVENTS.contains(&event) {
            return Err(format!(
                "unknown workspace event `{event}`; expected one of {}",
                WORKSPACE_EVENTS.join(", ")
            ));
        }
        let state = state
            .map(|value| vec![value.clone()])
            .unwrap_or_else(|| self.parking.lot(AUTO_WORKSPACES).into_values().collect());
        let mut workspaces: Vec<(u64, Value)> = state
            .into_iter()
            .filter_map(|value| Some((value.get("id").and_then(Value::as_u64)?, value)))
            .collect();
        workspaces.sort_by_key(|(id, _)| *id);
        let counts = windows_per_workspace(&self.parking);

        let rules = self
            .state
            .lock()
            .map_err(|_| "rules state lock poisoned".to_owned())?;
        let mut matches = Vec::new();
        for (id, mut value) in workspaces {
            value["empty"] = json!(counts.get(&id).copied().unwrap_or(0) == 0);
            let facts = WorkspaceFacts::from_value(&value);
            for (name, rule) in &rules.rules {
                if rule.trigger.as_deref() != Some(event) {
                    continue;
                }
                if workspace_matches(&rule.matcher, &value) {
                    matches.push(WorkspaceRuleMatch {
                        workspace: id,
                        name: name.clone(),
                        priority: rule.priority,
                        matcher: rule.matcher.clone(),
                        steps: rule.steps.clone(),
                        workspace_state: workspace_state(&value, &facts),
                    });
                }
            }
        }
        matches.sort_by(|a, b| {
            a.workspace
                .cmp(&b.workspace)
                .then(b.priority.cmp(&a.priority))
                .then(a.name.cmp(&b.name))
        });
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
    priority: i32,
    matcher: Match,
    steps: Vec<Step>,
    window_state: Value,
}

struct WorkspaceRuleMatch {
    workspace: u64,
    name: String,
    priority: i32,
    matcher: Match,
    steps: Vec<Step>,
    workspace_state: Value,
}

fn is_workspace_trigger(trigger: Option<&str>) -> bool {
    trigger.is_some_and(|trigger| WORKSPACE_EVENTS.contains(&trigger))
}

fn workspace_matches(matcher: &Match, workspace: &Value) -> bool {
    let facts = WorkspaceFacts::from_value(workspace);
    rule_matches_in(matcher, workspace, Some(&facts))
}

fn windows_per_workspace(parking: &Parking) -> BTreeMap<u64, u64> {
    let mut counts = BTreeMap::new();
    for window in parking.lot(AUTO_WINDOWS).values() {
        if let Some(id) = window.get("workspace_id").and_then(Value::as_u64) {
            *counts.entry(id).or_insert(0) += 1;
        }
    }
    counts
}

fn workspace_state(workspace: &Value, facts: &WorkspaceFacts) -> Value {
    json!({
        "workspace_id": workspace.get("id").cloned().unwrap_or(Value::Null),
        "workspace_name": facts.name,
        "workspace_idx": facts.idx,
        "workspace_output": facts.output,
        "workspace_empty": facts.empty,
        "is_focused": workspace.get("is_focused").cloned().unwrap_or(Value::Null),
        "is_active": workspace.get("is_active").cloned().unwrap_or(Value::Null),
        "is_urgent": workspace.get("is_urgent").cloned().unwrap_or(Value::Null),
    })
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

/// Workspace facts resolved from the auto-tracked workspace lot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceFacts {
    pub name: Option<String>,
    pub idx: Option<u8>,
    pub output: Option<String>,
    /// `None` when window tracking has not supplied a count.
    pub empty: Option<bool>,
}

impl WorkspaceFacts {
    pub fn from_value(value: &Value) -> Self {
        Self {
            name: value.get("name").and_then(Value::as_str).map(str::to_owned),
            idx: value
                .get("idx")
                .and_then(Value::as_u64)
                .and_then(|idx| u8::try_from(idx).ok()),
            output: value
                .get("output")
                .and_then(Value::as_str)
                .map(str::to_owned),
            empty: value.get("empty").and_then(Value::as_bool),
        }
    }
}

fn window_state(window: &Value, workspace: Option<&WorkspaceFacts>) -> Value {
    json!({
        "id": window.get("id").cloned().unwrap_or(Value::Null),
        "app_id": window.get("app_id").cloned().unwrap_or(Value::Null),
        "title": window.get("title").cloned().unwrap_or(Value::Null),
        "workspace_id": window.get("workspace_id").cloned().unwrap_or(Value::Null),
        "workspace_name": workspace.and_then(|facts| facts.name.clone()),
        "workspace_idx": workspace.and_then(|facts| facts.idx),
        "workspace_output": workspace.and_then(|facts| facts.output.clone()),
        "is_floating": window.get("is_floating").and_then(Value::as_bool),
        "is_minimized": window.get("is_minimized").and_then(Value::as_bool),
        "is_sticky": window.get("is_sticky").and_then(Value::as_bool),
        "is_urgent": window.get("is_urgent").and_then(Value::as_bool),
    })
}

pub fn validate_matcher(matcher: &Match) -> Result<(), String> {
    let patterns = [
        ("app_id_regex", matcher.app_id_regex.as_deref()),
        ("app_id_not_regex", matcher.app_id_not_regex.as_deref()),
        ("title_regex", matcher.title_regex.as_deref()),
        ("mark_regex", matcher.mark_regex.as_deref()),
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

fn validate_workspace_matcher(matcher: &Match) -> Result<(), String> {
    if matcher.app_id.is_some()
        || matcher.app_id_regex.is_some()
        || matcher.app_id_not.is_some()
        || matcher.app_id_not_regex.is_some()
        || matcher.title.is_some()
        || matcher.title_regex.is_some()
        || matcher.title_not.is_some()
        || matcher.is_floating.is_some()
        || matcher.is_minimized.is_some()
        || matcher.is_sticky.is_some()
        || matcher.mark.is_some()
        || matcher.mark_regex.is_some()
        || matcher.mark_not.is_some()
    {
        return Err(
            "workspace rules can match workspace fields only (`workspace`, `workspace_name`, \
             `workspace_name_regex`, `workspace_idx`, `workspace_output`, `workspace_empty`)"
                .to_owned(),
        );
    }
    Ok(())
}

/// Validates a whole rule, including its trigger and matcher shape.
pub fn validate_rule(rule: &Rule) -> Result<(), String> {
    if let Some(trigger) = rule.trigger.as_deref()
        && trigger != "window"
        && !WORKSPACE_EVENTS.contains(&trigger)
    {
        return Err(format!(
            "unknown rule trigger `{trigger}`; expected `window` or one of {}",
            WORKSPACE_EVENTS.join(", ")
        ));
    }
    validate_matcher(&rule.matcher)?;
    if is_workspace_trigger(rule.trigger.as_deref()) {
        validate_workspace_matcher(&rule.matcher)?;
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
    let facts = workspace_name.map(|name| WorkspaceFacts {
        name: Some(name.to_owned()),
        ..WorkspaceFacts::default()
    });
    rule_matches_in(matcher, window, facts.as_ref())
}

/// Matches `window` against workspace facts (name, index, output).
pub fn rule_matches_in(
    matcher: &Match,
    window: &Value,
    workspace: Option<&WorkspaceFacts>,
) -> bool {
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
    if let Some(mark) = &matcher.mark
        && window.get("mark").and_then(Value::as_str) != Some(mark.as_str())
    {
        return false;
    }
    if let Some(pattern) = &matcher.mark_regex
        && !regex_matches(window, "mark", pattern)
    {
        return false;
    }
    if let Some(mark) = &matcher.mark_not
        && window.get("mark").and_then(Value::as_str) == Some(mark.as_str())
    {
        return false;
    }
    if let Some(workspace_id) = matcher.workspace
        && window.get("workspace_id").and_then(Value::as_u64) != Some(workspace_id)
    {
        return false;
    }
    let workspace_name = workspace
        .and_then(|facts| facts.name.as_deref())
        .or_else(|| window.get("workspace_name").and_then(Value::as_str));
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
    if let Some(index) = matcher.workspace_idx
        && workspace.and_then(|facts| facts.idx) != Some(index)
    {
        return false;
    }
    if let Some(output) = &matcher.workspace_output
        && !name_contains(workspace.and_then(|facts| facts.output.as_deref()), output)
    {
        return false;
    }
    if let Some(empty) = matcher.workspace_empty
        && workspace.and_then(|facts| facts.empty) != Some(empty)
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
            ActionInfo::new("set", "Define/replace `name` with `match` and `steps`")
                .with_params([
                    ParamInfo::string("name", true, "rule name"),
                    ParamInfo::object("match", false, "window/workspace matcher"),
                    ParamInfo::array("steps", true, "actions to run: {namespace, action, params}"),
                    ParamInfo::string(
                        "trigger",
                        false,
                        "`window` (default) or a workspace event: workspace-activated, \
                         workspace-added, workspace-removed, workspace-moved",
                    ),
                ])
                .with_permissions(&["niri"]),
            ActionInfo::new("remove", "Delete rule `name`").with_params([ParamInfo::string(
                "name",
                true,
                "rule name",
            )]),
            ActionInfo::new("list", "List rules"),
            ActionInfo::new(
                "explain",
                "Matching rules for `window`, or workspace rules for `event`",
            )
            .with_params([
                ParamInfo::integer("window", false, "window id, defaults to all"),
                ParamInfo::string(
                    "event",
                    false,
                    "workspace event: workspace-activated, workspace-added, \
                     workspace-removed, workspace-moved",
                ),
                ParamInfo::object(
                    "workspace_state",
                    false,
                    "explicit workspace state for a removed/moved event",
                ),
            ]),
            ActionInfo::new(
                "apply",
                "Show which rules match and their step counts (no execution)",
            )
            .with_params([
                ParamInfo::integer("window", false, "window id, defaults to all"),
                ParamInfo::string("event", false, "workspace event"),
                ParamInfo::object("workspace_state", false, "explicit workspace state"),
            ]),
            ActionInfo::new(
                "run",
                "Execute the steps of matching window or workspace-event rules",
            )
            .with_params([
                ParamInfo::integer("window", false, "window id, defaults to all"),
                ParamInfo::string("event", false, "workspace event"),
                ParamInfo::object("workspace_state", false, "explicit workspace state"),
            ])
            .with_permissions(&["niri", "shell"]),
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
                let trigger = params
                    .get("trigger")
                    .or_else(|| params.get("event"))
                    .and_then(Value::as_str)
                    .filter(|trigger| *trigger != "window")
                    .map(str::to_owned);
                let priority = match params.get("priority") {
                    None => 0,
                    Some(value) => value
                        .as_i64()
                        .and_then(|value| i32::try_from(value).ok())
                        .ok_or_else(|| {
                            "rules.set needs an integer `priority` within i32".to_owned()
                        })?,
                };
                let rule = Rule {
                    matcher,
                    steps: steps.clone(),
                    trigger,
                    priority,
                };
                validate_rule(&rule)?;
                state.rules.insert(name.clone(), rule);
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
                if let Some(event) = params.get("event").and_then(Value::as_str) {
                    let matches =
                        self.matching_workspace_rules(event, params.get("workspace_state"))?;
                    let matches: Vec<Value> = matches
                        .into_iter()
                        .map(|matched| {
                            json!({
                                "workspace": matched.workspace,
                                "rule": matched.name,
                                "priority": matched.priority,
                                "trigger": event,
                                "match": matched.matcher,
                                "workspace_state": matched.workspace_state,
                                "steps": matched.steps,
                            })
                        })
                        .collect();
                    return Ok(json!({ "event": event, "matches": matches }));
                }
                let window = params.get("window").and_then(Value::as_u64);
                let matches = self.matching_rules(window)?;
                let matches: Vec<Value> = matches
                    .into_iter()
                    .map(|matched| {
                        json!({
                            "window": matched.window,
                            "rule": matched.name,
                            "priority": matched.priority,
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
                if let Some(event) = params.get("event").and_then(Value::as_str) {
                    let matches =
                        self.matching_workspace_rules(event, params.get("workspace_state"))?;
                    let matches: Vec<Value> = matches
                        .into_iter()
                        .map(|matched| {
                            json!({
                                "workspace": matched.workspace,
                                "rule": matched.name,
                                "priority": matched.priority,
                                "trigger": event,
                                "match": matched.matcher,
                                "workspace_state": matched.workspace_state,
                                "step_count": matched.steps.len(),
                            })
                        })
                        .collect();
                    return Ok(json!({ "event": event, "matches": matches }));
                }
                let window = params.get("window").and_then(Value::as_u64);
                let matches = self.matching_rules(window)?;
                let matches: Vec<Value> = matches
                    .into_iter()
                    .map(|matched| {
                        json!({
                            "window": matched.window,
                            "rule": matched.name,
                            "priority": matched.priority,
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

    fn event_steps(
        &self,
        event: &str,
        workspace: Option<&Value>,
    ) -> Option<Result<Vec<Step>, String>> {
        Some(
            self.matching_workspace_rules(event, workspace)
                .map(|matches| {
                    matches
                        .into_iter()
                        .flat_map(|matched| matched.steps)
                        .collect()
                }),
        )
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
            id: None,
        });
        assert!(matches!(response, Response::Value { .. }), "{response:?}");

        let applied = registry.handle(Request::Call {
            namespace: "rules".into(),
            action: "apply".into(),
            params: json!({ "window": 10 }),
            id: None,
        });
        match applied {
            Response::Value { value, .. } => {
                assert_eq!(value["matches"][0]["rule"], "browser");
            }
            other => panic!("unexpected: {other:?}"),
        }

        let ran = registry.handle(Request::Call {
            namespace: "rules".into(),
            action: "run".into(),
            params: json!({ "window": 10 }),
            id: None,
        });
        assert!(matches!(ran, Response::Value { .. }), "{ran:?}");
        assert_eq!(*log.lock().unwrap(), ["browser"]);
    }

    fn with_workspaces(parking: &Parking) {
        parking
            .replace_lot(
                AUTO_WORKSPACES,
                [
                    (
                        "1".to_owned(),
                        json!({ "id": 1, "idx": 1, "name": "dev", "output": "eDP-1" }),
                    ),
                    (
                        "2".to_owned(),
                        json!({ "id": 2, "idx": 2, "name": "web", "output": "HDMI-1" }),
                    ),
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
        assert_eq!(matched["window_state"]["workspace_idx"], 1);
        assert_eq!(matched["window_state"]["workspace_output"], "eDP-1");
        assert_eq!(matched["window_state"]["title"], "Mozilla");
        assert_eq!(matched["steps"][0]["action"], "ping");

        let applied = rules.call("apply", &json!({ "window": 10 })).unwrap();
        let matched = &applied["matches"][0];
        assert_eq!(matched["window"], 10);
        assert_eq!(matched["step_count"], 1);
        assert_eq!(matched["match"]["app_id_not"], "chrome");
    }

    #[test]
    fn matches_workspace_index_and_output() {
        let parking = parking_with_windows("workspace-facts");
        with_workspaces(&parking);
        let rules = Rules::load(
            temp_dir("workspace-facts-rules").join("rules.json"),
            parking.clone(),
        )
        .unwrap();
        rules
            .call(
                "set",
                &json!({
                    "name": "dev-output",
                    "match": { "workspace_idx": 1, "workspace_output": "edp" },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap();
        let explained = rules.call("explain", &json!({ "window": 10 })).unwrap();
        assert_eq!(explained["matches"][0]["rule"], "dev-output");

        let firefox = json!({ "id": 10, "app_id": "firefox", "workspace_id": 1 });
        assert!(rule_matches_in(
            &Match {
                workspace_idx: Some(1),
                workspace_output: Some("eDP-1".into()),
                ..Match::default()
            },
            &firefox,
            Some(&WorkspaceFacts {
                name: Some("dev".into()),
                idx: Some(1),
                output: Some("eDP-1".into()),
                empty: Some(false),
            }),
        ));
        assert!(!rule_matches_in(
            &Match {
                workspace_idx: Some(2),
                ..Match::default()
            },
            &firefox,
            Some(&WorkspaceFacts {
                name: Some("dev".into()),
                idx: Some(1),
                output: Some("eDP-1".into()),
                empty: Some(false),
            }),
        ));
        assert!(!rule_matches_in(
            &Match {
                workspace_output: Some("HDMI".into()),
                ..Match::default()
            },
            &firefox,
            Some(&WorkspaceFacts {
                name: Some("dev".into()),
                idx: Some(1),
                output: Some("eDP-1".into()),
                empty: Some(false),
            }),
        ));
    }

    #[test]
    fn matches_report_numeric_window_order() {
        let parking = Parking::load(temp_dir("order-parking").join("parking.json")).unwrap();
        parking
            .replace_lot(
                AUTO_WINDOWS,
                [
                    ("10".to_owned(), json!({ "id": 10, "app_id": "x" })),
                    ("2".to_owned(), json!({ "id": 2, "app_id": "x" })),
                ]
                .into_iter()
                .collect(),
            )
            .unwrap();
        let rules = Rules::load(temp_dir("order-rules").join("rules.json"), parking).unwrap();
        rules
            .call(
                "set",
                &json!({
                    "name": "all",
                    "match": {},
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap();
        let explained = rules.call("explain", &Value::Null).unwrap();
        assert_eq!(explained["matches"][0]["window"], 2);
        assert_eq!(explained["matches"][1]["window"], 10);
    }

    #[test]
    fn rules_run_in_priority_order() {
        let parking = parking_with_windows("priority");
        let rules = Rules::load(temp_dir("priority-rules").join("rules.json"), parking).unwrap();
        for (name, priority) in [("low", -1), ("high", 10), ("mid", 0)] {
            rules
                .call(
                    "set",
                    &json!({
                        "name": name,
                        "priority": priority,
                        "match": { "app_id": "firefox" },
                        "steps": [{ "namespace": "system", "action": "ping" }]
                    }),
                )
                .unwrap();
        }

        let explained = rules.call("explain", &json!({ "window": 10 })).unwrap();
        let order: Vec<&str> = explained["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|matched| matched["rule"].as_str().unwrap())
            .collect();
        assert_eq!(order, ["high", "mid", "low"]);
        assert_eq!(explained["matches"][0]["priority"], 10);
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
    fn rules_match_window_marks() {
        let parking = parking_with_windows("marks");
        let marks = companion_niri::Marks::load(
            temp_dir("marks-store").join("marks.json"),
            companion_niri::Compositor::at(temp_dir("marks-niri").join("niri.sock")),
        )
        .unwrap();
        marks
            .call("set", &json!({ "window": 10, "mark": "browser" }))
            .unwrap();
        let rules = Rules::load(temp_dir("mark-rules").join("rules.json"), parking)
            .unwrap()
            .with_marks(marks);

        rules
            .call(
                "set",
                &json!({
                    "name": "marked",
                    "match": { "mark": "browser" },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap();
        let explained = rules.call("explain", &Value::Null).unwrap();
        assert_eq!(explained["matches"][0]["rule"], "marked");
        assert_eq!(explained["matches"][0]["window"], 10);
        assert_eq!(explained["matches"][0]["window_state"]["id"], 10);

        // A regex and a negated mark both work.
        rules
            .call(
                "set",
                &json!({
                    "name": "regex",
                    "match": { "mark_regex": "^brow" },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap();
        assert_eq!(
            rules.call("explain", &Value::Null).unwrap()["matches"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn workspace_event_rules_explain_and_run_in_stable_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let parking = parking_with_windows("workspace-rules");
        with_workspaces(&parking);
        let rules = Rules::load(
            temp_dir("workspace-rules-store").join("rules.json"),
            parking.clone(),
        )
        .unwrap();

        rules
            .call(
                "set",
                &json!({
                    "name": "dev",
                    "trigger": "workspace-activated",
                    "match": { "workspace_name": "dev" },
                    "steps": [{ "namespace": "counter", "action": "push", "params": { "value": "dev" } }]
                }),
            )
            .unwrap();
        rules
            .call(
                "set",
                &json!({
                    "name": "any-add",
                    "trigger": "workspace-added",
                    "match": {},
                    "steps": [{ "namespace": "counter", "action": "push", "params": { "value": "added" } }]
                }),
            )
            .unwrap();

        let explained = rules
            .call("explain", &json!({ "event": "workspace-activated" }))
            .unwrap();
        assert_eq!(explained["event"], "workspace-activated");
        assert_eq!(explained["matches"][0]["rule"], "dev");
        assert_eq!(explained["matches"][0]["workspace"], 1);
        assert_eq!(
            explained["matches"][0]["workspace_state"]["workspace_name"],
            "dev"
        );

        // An explicit state lets a removed workspace be explained.
        let removed = rules
            .call(
                "explain",
                &json!({
                    "event": "workspace-removed",
                    "workspace_state": { "id": 9, "name": "gone", "idx": 9, "output": "eDP-1" }
                }),
            )
            .unwrap();
        assert!(removed["matches"].as_array().unwrap().is_empty());

        // Window rules do not leak into workspace runs and vice versa.
        let mut registry = Registry::new();
        registry.register(rules);
        registry.register(Counter(log.clone()));
        let ran = registry.handle(Request::Call {
            namespace: "rules".into(),
            action: "run".into(),
            params: json!({ "event": "workspace-activated" }),
            id: None,
        });
        assert!(matches!(ran, Response::Value { .. }), "{ran:?}");
        assert_eq!(*log.lock().unwrap(), ["dev"]);
    }

    #[test]
    fn workspace_empty_matches_only_workspaces_without_windows() {
        let parking = parking_with_windows("workspace-empty");
        with_workspaces(&parking);
        let mut spaces = parking.lot(AUTO_WORKSPACES);
        spaces.insert(
            "3".to_owned(),
            json!({ "id": 3, "idx": 3, "name": "empty", "output": "HDMI-1" }),
        );
        parking.replace_lot(AUTO_WORKSPACES, spaces).unwrap();

        let rules = Rules::load(
            temp_dir("workspace-empty-store").join("rules.json"),
            parking,
        )
        .unwrap();
        rules
            .call(
                "set",
                &json!({
                    "name": "empty-workspace",
                    "trigger": "workspace-activated",
                    "match": { "workspace_empty": true },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap();

        let explained = rules
            .call("explain", &json!({ "event": "workspace-activated" }))
            .unwrap();
        let matches = explained["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1, "{explained}");
        assert_eq!(matches[0]["workspace"], 3);
        assert_eq!(matches[0]["workspace_state"]["workspace_empty"], true);

        let firefox = json!({ "id": 10, "workspace_id": 1 });
        assert!(!rule_matches_in(
            &Match {
                workspace_empty: Some(true),
                ..Match::default()
            },
            &firefox,
            Some(&WorkspaceFacts {
                empty: Some(false),
                ..WorkspaceFacts::default()
            }),
        ));
    }

    #[test]
    fn workspace_rules_reject_window_only_matchers() {
        let parking = parking_with_windows("workspace-invalid");
        let rules = Rules::load(
            temp_dir("workspace-invalid-store").join("rules.json"),
            parking,
        )
        .unwrap();
        let error = rules
            .call(
                "set",
                &json!({
                    "name": "bad",
                    "trigger": "workspace-activated",
                    "match": { "app_id": "firefox" },
                    "steps": [{ "namespace": "system", "action": "ping" }]
                }),
            )
            .unwrap_err();
        assert!(error.contains("workspace fields only"), "{error}");
        assert!(rules.call("explain", &json!({ "event": "nope" })).is_err());
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
