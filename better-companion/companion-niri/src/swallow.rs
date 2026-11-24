//! Terminal swallowing (companion Phase B).
//!
//! When a GUI window opens from a terminal, minimize the terminal while the
//! child lives and restore it when the child closes. Windows are matched
//! through their `/proc` parent chain and the terminal app-id allowlist. This
//! is opt-in (`swallow.enable`).
//!
//! Beyond the default allowlist, a list of ordered [`SwallowRule`]s can add
//! per-rule behavior: terminal/child app-id substrings, case-insensitive
//! regular expressions on app ids and titles, ignore rules, a maximum child
//! process age (a child must have started within that window), and a per-rule
//! `restore_on_exit` override. A global `ignore` list vetoes children by
//! app-id/title substring before any rule is considered.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use companion_core::{ActionInfo, Context, load_json, save_json};
use companion_parking::Parking;
use niri_ipc::{Action, Request, Response, Window};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Compositor;

pub const SWALLOW_LOT: &str = "swallow";

const MAX_PARENT_DEPTH: usize = 16;

const DEFAULT_TERMINALS: &[&str] = &[
    "alacritty",
    "foot",
    "kitty",
    "wezterm",
    "org.wezfurlong.wezterm",
    "ghostty",
    "com.mitchellh.ghostty",
    "konsole",
    "gnome-terminal-server",
    "xterm",
];

fn default_true() -> bool {
    true
}

/// One ordered swallowing rule.
///
/// Every populated field is a condition; all of them must match. An empty
/// condition set matches every terminal/child pair. Rules are evaluated in
/// order and the first match decides: `ignore: true` vetoes, otherwise the
/// rule allows swallowing even when the terminal is not in the global
/// `terminals` allowlist.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SwallowRule {
    /// Unique name; `add-rule` replaces an existing rule with the same name.
    pub name: String,
    /// Case-insensitive terminal app-id substrings. Empty means any terminal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_app_id: Vec<String>,
    /// Case-insensitive terminal app-id regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_app_id_regex: Option<String>,
    /// Case-insensitive terminal title regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title_regex: Option<String>,
    /// Case-insensitive child app-id substrings. Empty means any child.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_app_id: Vec<String>,
    /// Case-insensitive child app-id regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_app_id_regex: Option<String>,
    /// Case-insensitive child title regex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_title_regex: Option<String>,
    /// Veto swallowing when this rule matches.
    #[serde(default)]
    pub ignore: bool,
    /// Only swallow children whose process is younger than this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_age_ms: Option<u64>,
    /// Per-rule override for restoring the terminal on child exit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_on_exit: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    enabled: bool,
    #[serde(default)]
    terminals: Vec<String>,
    /// Child app-id/title substrings that are never swallowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ignore: Vec<String>,
    /// Default maximum child process age for swallowing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child_age_ms: Option<u64>,
    #[serde(default = "default_true")]
    restore_on_exit: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rules: Vec<SwallowRule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            terminals: Vec::new(),
            ignore: Vec::new(),
            child_age_ms: None,
            restore_on_exit: true,
            rules: Vec::new(),
        }
    }
}

type ParentFn = Arc<dyn Fn(i32) -> Option<i32> + Send + Sync>;
type AgeFn = Arc<dyn Fn(i32) -> Option<Duration> + Send + Sync>;

struct Inner {
    path: PathBuf,
    config: Mutex<Config>,
    parking: Parking,
    compositor: Compositor,
    parent: ParentFn,
    age: AgeFn,
}

#[derive(Clone)]
pub struct Swallow {
    inner: Arc<Inner>,
}

/// A tracked auto-window with the fields swallowing cares about.
#[derive(Debug, Clone)]
struct TrackedWindow {
    id: u64,
    pid: i32,
    app_id: Option<String>,
    title: Option<String>,
    minimized: bool,
}

/// Result of evaluating one candidate child window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwallowOutcome {
    /// Swallowing is disabled.
    Disabled,
    /// The window has no PID, so its parent chain cannot be inspected.
    MissingPid,
    /// This child already has a persisted pair.
    AlreadySwallowed,
    /// No tracked terminal window was found in the parent chain, or the
    /// terminal is not allowlisted and no rule allowed it.
    NotTerminal,
    /// The child or terminal matched the ignore list or an ignore rule.
    Ignored { reason: String },
    /// The child process is older than the configured time window.
    TooOld { child_age_ms: u64, limit_ms: u64 },
    /// The terminal was minimized and the pair persisted.
    Swallowed {
        child: u64,
        parent: u64,
        rule: Option<String>,
    },
}

impl Swallow {
    pub fn load(
        path: impl Into<PathBuf>,
        parking: Parking,
        compositor: Compositor,
    ) -> std::io::Result<Self> {
        Self::load_with_resolvers(
            path,
            parking,
            compositor,
            Arc::new(proc_parent),
            Arc::new(proc_age),
        )
    }

    /// Test hook: builds a swallow tracker with a synthetic parent resolver.
    #[doc(hidden)]
    pub fn load_with_parent(
        path: impl Into<PathBuf>,
        parking: Parking,
        compositor: Compositor,
        parent: ParentFn,
    ) -> std::io::Result<Self> {
        Self::load_with_resolvers(path, parking, compositor, parent, Arc::new(proc_age))
    }

    /// Test hook: synthetic parent and child-process-age resolvers.
    #[doc(hidden)]
    pub fn load_with_resolvers(
        path: impl Into<PathBuf>,
        parking: Parking,
        compositor: Compositor,
        parent: ParentFn,
        age: AgeFn,
    ) -> std::io::Result<Self> {
        let path = path.into();
        let mut config: Config = load_json(&path)?;
        if config.terminals.is_empty() {
            config.terminals = DEFAULT_TERMINALS.iter().map(|s| s.to_string()).collect();
        }
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                config: Mutex::new(config),
                parking,
                compositor,
                parent,
                age,
            }),
        })
    }

    pub fn from_state_dir(parking: Parking, compositor: Compositor) -> std::io::Result<Self> {
        Self::load(
            companion_core::state_dir().join("swallow.json"),
            parking,
            compositor,
        )
    }

    fn save(&self, config: &Config) -> Result<(), String> {
        save_json(&self.inner.path, config).map_err(|err| format!("cannot save swallow: {err}"))
    }

    fn config(&self) -> Result<Config, String> {
        self.inner
            .config
            .lock()
            .map(|config| config.clone())
            .map_err(|_| "swallow config lock poisoned".to_owned())
    }

    /// Whether swallowing is enabled in the persisted config.
    pub fn is_enabled(&self) -> bool {
        self.config().map(|config| config.enabled).unwrap_or(false)
    }

    /// Whether `app_id` is in the terminal allowlist.
    pub fn terminal_allowed(&self, app_id: Option<&str>) -> bool {
        self.config()
            .map(|config| allowlisted_terminal(&config, app_id))
            .unwrap_or(false)
    }

    fn send_action(&self, action: Action) -> Result<(), String> {
        match self.inner.compositor.send(Request::Action(action))? {
            Response::Handled => Ok(()),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn entries(&self) -> BTreeMap<String, Value> {
        self.inner.parking.lot(SWALLOW_LOT)
    }

    /// Returns the auto-tracked windows with title and minimization state.
    fn windows(&self) -> Vec<TrackedWindow> {
        self.inner
            .parking
            .lot(crate::AUTO_WINDOWS)
            .into_iter()
            .filter_map(|(id, value)| {
                let id = id.parse().ok()?;
                let pid = value.get("pid").and_then(Value::as_i64)? as i32;
                let app_id = value
                    .get("app_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let title = value
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let minimized = value
                    .get("is_minimized")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Some(TrackedWindow {
                    id,
                    pid,
                    app_id,
                    title,
                    minimized,
                })
            })
            .collect()
    }

    /// Evaluates `window` as a swallow child.
    ///
    /// Returns the decision, including the reason when nothing is swallowed.
    pub fn maybe_swallow(&self, window: &Window) -> Result<SwallowOutcome, String> {
        let config = self.config()?;
        if !config.enabled {
            return Ok(SwallowOutcome::Disabled);
        }
        let Some(child_pid) = window.pid else {
            return Ok(SwallowOutcome::MissingPid);
        };
        if self.entries().contains_key(&format!("child:{}", window.id)) {
            return Ok(SwallowOutcome::AlreadySwallowed);
        }

        let child = TrackedWindow {
            id: window.id,
            pid: child_pid,
            app_id: window.app_id.clone(),
            title: window.title.clone(),
            minimized: window.is_minimized,
        };
        let windows = self.windows();

        let mut pid = child_pid;
        for _ in 0..MAX_PARENT_DEPTH {
            let Some(parent_pid) = (self.inner.parent)(pid) else {
                break;
            };
            if parent_pid <= 1 {
                break;
            }
            let Some(parent) = windows
                .iter()
                .find(|tracked| tracked.id != child.id && tracked.pid == parent_pid)
            else {
                pid = parent_pid;
                continue;
            };
            return self.swallow_pair(&config, &child, parent);
        }
        Ok(SwallowOutcome::NotTerminal)
    }

    fn swallow_pair(
        &self,
        config: &Config,
        child: &TrackedWindow,
        terminal: &TrackedWindow,
    ) -> Result<SwallowOutcome, String> {
        if let Some(reason) = ignored_reason(config, child) {
            return Ok(SwallowOutcome::Ignored { reason });
        }

        let rule = config
            .rules
            .iter()
            .find(|rule| rule_matches(rule, terminal, child));
        let (allowed, rule_name, age_limit, restore_on_exit) = match rule {
            Some(rule) if rule.ignore => {
                return Ok(SwallowOutcome::Ignored {
                    reason: format!("rule `{}`", rule.name),
                });
            }
            Some(rule) => (
                true,
                Some(rule.name.clone()),
                rule.child_age_ms.or(config.child_age_ms),
                rule.restore_on_exit.unwrap_or(config.restore_on_exit),
            ),
            None => (
                allowlisted_terminal(config, terminal.app_id.as_deref()),
                None,
                config.child_age_ms,
                config.restore_on_exit,
            ),
        };
        if !allowed {
            return Ok(SwallowOutcome::NotTerminal);
        }

        if let Some(limit_ms) = age_limit
            && let Some(age) = (self.inner.age)(child.pid)
        {
            let child_age_ms = age.as_millis().min(u128::from(u64::MAX)) as u64;
            if child_age_ms > limit_ms {
                return Ok(SwallowOutcome::TooOld {
                    child_age_ms,
                    limit_ms,
                });
            }
        }

        if !terminal.minimized {
            self.send_action(Action::MinimizeWindow {
                id: Some(terminal.id),
            })?;
        }
        self.inner.parking.call(
            "park",
            &json!({
                "lot": SWALLOW_LOT,
                "id": format!("child:{}", child.id),
                "value": {
                    "child": child.id,
                    "child_pid": child.pid,
                    "child_app_id": child.app_id,
                    "parent": terminal.id,
                    "parent_pid": terminal.pid,
                    "parent_app_id": terminal.app_id,
                    "rule": rule_name,
                    "restore_on_exit": restore_on_exit,
                }
            }),
        )?;
        Ok(SwallowOutcome::Swallowed {
            child: child.id,
            parent: terminal.id,
            rule: rule_name,
        })
    }

    /// Restores and removes the pair for `child_id`, returning its entry when
    /// the pair existed. A missing pair is not an error.
    pub fn on_window_closed(&self, child_id: u64) -> Result<Option<Value>, String> {
        let key = format!("child:{child_id}");
        if !self.entries().contains_key(&key) {
            return Ok(None);
        }
        let entry = self
            .inner
            .parking
            .call("unpark", &json!({ "lot": SWALLOW_LOT, "id": key }))?;
        let value = entry.get("value").cloned().unwrap_or(Value::Null);
        let restore = value
            .get("restore_on_exit")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if restore
            && let Some(parent) = value.get("parent").and_then(Value::as_u64)
            && self.windows().iter().any(|window| window.id == parent)
        {
            let _ = self.send_action(Action::UnminimizeWindow { id: Some(parent) });
        }
        Ok(Some(value))
    }

    /// Drops pairs whose terminal window closed. The terminal is gone, so
    /// there is nothing to restore.
    pub fn on_parent_closed(&self, parent_id: u64) -> Result<Vec<u64>, String> {
        let children: Vec<(String, u64)> = self
            .entries()
            .into_iter()
            .filter_map(|(key, value)| {
                if value.get("parent").and_then(Value::as_u64) == Some(parent_id) {
                    Some((key, value.get("child").and_then(Value::as_u64)?))
                } else {
                    None
                }
            })
            .collect();
        for (key, _) in &children {
            let _ = self
                .inner
                .parking
                .call("unpark", &json!({ "lot": SWALLOW_LOT, "id": key }));
        }
        Ok(children.into_iter().map(|(_, child)| child).collect())
    }

    /// Removes pairs whose child is no longer an open window, restoring any
    /// still-open terminal. This is how pairs persisted across a daemon
    /// restart converge with the compositor state.
    pub fn reconcile(&self) -> Result<Vec<u64>, String> {
        let open: BTreeSet<u64> = self.windows().iter().map(|window| window.id).collect();
        let stale: Vec<u64> = self
            .entries()
            .into_values()
            .filter_map(|value| {
                let child = value.get("child").and_then(Value::as_u64)?;
                (!open.contains(&child)).then_some(child)
            })
            .collect();
        for child in &stale {
            self.on_window_closed(*child)?;
        }
        Ok(stale)
    }

    pub fn restore(&self, child_id: u64) -> Result<Value, String> {
        let entry = self.on_window_closed(child_id)?;
        Ok(json!({
            "child": child_id,
            "restored": entry.is_some(),
            "parent": entry.as_ref().and_then(|value| value.get("parent").cloned()),
            "rule": entry.as_ref().and_then(|value| value.get("rule").cloned()),
        }))
    }
}

fn allowlisted_terminal(config: &Config, app_id: Option<&str>) -> bool {
    app_id.is_some_and(|app_id| {
        config
            .terminals
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(app_id))
    })
}

fn contains_any(patterns: &[String], value: Option<&str>) -> bool {
    patterns.iter().any(|pattern| {
        value.is_some_and(|value| value.to_lowercase().contains(&pattern.to_lowercase()))
    })
}

fn regex_matches(pattern: &str, value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .is_ok_and(|regex| regex.is_match(value))
    })
}

fn validate_regex(field: &str, pattern: Option<&str>) -> Result<(), String> {
    let Some(pattern) = pattern else {
        return Ok(());
    };
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map(|_| ())
        .map_err(|err| format!("invalid `{field}` regex `{pattern}`: {err}"))
}

fn validate_rule(rule: &SwallowRule) -> Result<(), String> {
    if rule.name.trim().is_empty() {
        return Err("swallow rule needs a non-empty `name`".to_owned());
    }
    validate_regex(
        "terminal_app_id_regex",
        rule.terminal_app_id_regex.as_deref(),
    )?;
    validate_regex("terminal_title_regex", rule.terminal_title_regex.as_deref())?;
    validate_regex("child_app_id_regex", rule.child_app_id_regex.as_deref())?;
    validate_regex("child_title_regex", rule.child_title_regex.as_deref())
}

fn ignored_reason(config: &Config, child: &TrackedWindow) -> Option<String> {
    config.ignore.iter().find_map(|pattern| {
        let needle = pattern.to_lowercase();
        let matches = child
            .app_id
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(&needle))
            || child
                .title
                .as_deref()
                .is_some_and(|value| value.to_lowercase().contains(&needle));
        matches.then(|| format!("ignore-list `{pattern}`"))
    })
}

fn rule_matches(rule: &SwallowRule, terminal: &TrackedWindow, child: &TrackedWindow) -> bool {
    if !rule.terminal_app_id.is_empty()
        && !contains_any(&rule.terminal_app_id, terminal.app_id.as_deref())
    {
        return false;
    }
    if let Some(pattern) = &rule.terminal_app_id_regex
        && !regex_matches(pattern, terminal.app_id.as_deref())
    {
        return false;
    }
    if let Some(pattern) = &rule.terminal_title_regex
        && !regex_matches(pattern, terminal.title.as_deref())
    {
        return false;
    }
    if !rule.child_app_id.is_empty() && !contains_any(&rule.child_app_id, child.app_id.as_deref()) {
        return false;
    }
    if let Some(pattern) = &rule.child_app_id_regex
        && !regex_matches(pattern, child.app_id.as_deref())
    {
        return false;
    }
    if let Some(pattern) = &rule.child_title_regex
        && !regex_matches(pattern, child.title.as_deref())
    {
        return false;
    }
    true
}

fn parse_rule(params: &Value) -> Result<SwallowRule, String> {
    let raw = params
        .get("rule")
        .cloned()
        .unwrap_or_else(|| params.clone());
    let rule: SwallowRule =
        serde_json::from_value(raw).map_err(|err| format!("invalid swallow rule: {err}"))?;
    validate_rule(&rule)?;
    Ok(rule)
}

impl Context for Swallow {
    fn namespace(&self) -> &'static str {
        "swallow"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("enable", "Minimize terminals while their children are open"),
            ActionInfo::new("disable", "Stop swallowing new children"),
            ActionInfo::new(
                "status",
                "Enabled state, terminals, rules, ignore list and swallowed pairs",
            ),
            ActionInfo::new("list", "List swallowed child → parent pairs"),
            ActionInfo::new("restore", "Restore the parent of swallowed `child`"),
            ActionInfo::new("add-terminal", "Allow an additional `app_id` term"),
            ActionInfo::new("remove-terminal", "Remove an `app_id` term"),
            ActionInfo::new(
                "add-rule",
                "Add/replace an ordered `rule` (or inline rule fields)",
            ),
            ActionInfo::new("remove-rule", "Remove rule `name`"),
            ActionInfo::new("set-ignore", "Replace the child `ignore` list"),
            ActionInfo::new(
                "configure",
                "Set `child_age_ms` and/or `restore_on_exit` defaults",
            ),
            ActionInfo::new(
                "reconcile",
                "Drop pairs whose child window is gone and restore terminals",
            ),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "enable" => {
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                config.enabled = true;
                self.save(&config)?;
                Ok(json!({ "enabled": true }))
            }
            "disable" => {
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                config.enabled = false;
                self.save(&config)?;
                Ok(json!({ "enabled": false }))
            }
            "status" => {
                let config = self.config()?;
                Ok(json!({
                    "enabled": config.enabled,
                    "terminals": config.terminals,
                    "ignore": config.ignore,
                    "child_age_ms": config.child_age_ms,
                    "restore_on_exit": config.restore_on_exit,
                    "rules": config.rules,
                    "swallowed": self.entries(),
                }))
            }
            "list" => Ok(json!({ "swallowed": self.entries() })),
            "restore" => {
                let child = params
                    .get("child")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "swallow.restore needs a `child` window id".to_owned())?;
                self.restore(child)
            }
            "add-terminal" => {
                let app_id = params
                    .get("app_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "swallow.add-terminal needs `app_id`".to_owned())?;
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                if !config
                    .terminals
                    .iter()
                    .any(|terminal| terminal.eq_ignore_ascii_case(app_id))
                {
                    config.terminals.push(app_id.to_owned());
                }
                self.save(&config)?;
                Ok(json!({ "terminals": config.terminals }))
            }
            "remove-terminal" => {
                let app_id = params
                    .get("app_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "swallow.remove-terminal needs `app_id`".to_owned())?;
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                config
                    .terminals
                    .retain(|terminal| !terminal.eq_ignore_ascii_case(app_id));
                self.save(&config)?;
                Ok(json!({ "terminals": config.terminals }))
            }
            "add-rule" => {
                let rule = parse_rule(params)?;
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                match config
                    .rules
                    .iter_mut()
                    .find(|existing| existing.name == rule.name)
                {
                    Some(existing) => *existing = rule.clone(),
                    None => config.rules.push(rule.clone()),
                }
                self.save(&config)?;
                Ok(json!({ "rule": rule }))
            }
            "remove-rule" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "swallow.remove-rule needs a string `name`".to_owned())?;
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                config.rules.retain(|rule| rule.name != name);
                self.save(&config)?;
                Ok(json!({ "name": name, "rules": config.rules.len() }))
            }
            "set-ignore" => {
                let ignore = params
                    .get("ignore")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "swallow.set-ignore needs an `ignore` array".to_owned())?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "`ignore` entries must be strings".to_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                config.ignore = ignore;
                self.save(&config)?;
                Ok(json!({ "ignore": config.ignore }))
            }
            "configure" => {
                let mut config = self
                    .inner
                    .config
                    .lock()
                    .map_err(|_| "swallow config lock poisoned".to_owned())?;
                let mut changed = false;
                if let Some(value) = params.get("child_age_ms") {
                    config.child_age_ms = if value.is_null() {
                        None
                    } else {
                        Some(value.as_u64().ok_or_else(|| {
                            "`child_age_ms` must be a non-negative integer or null".to_owned()
                        })?)
                    };
                    changed = true;
                }
                if let Some(value) = params.get("restore_on_exit") {
                    config.restore_on_exit = value
                        .as_bool()
                        .ok_or_else(|| "`restore_on_exit` must be a boolean".to_owned())?;
                    changed = true;
                }
                if !changed {
                    return Err(
                        "swallow.configure needs `child_age_ms` or `restore_on_exit`".to_owned(),
                    );
                }
                self.save(&config)?;
                Ok(json!({
                    "child_age_ms": config.child_age_ms,
                    "restore_on_exit": config.restore_on_exit,
                }))
            }
            "reconcile" => Ok(json!({ "removed": self.reconcile()? })),
            other => Err(format!("unknown action `swallow.{other}`")),
        }
    }
}

/// Reads the parent pid from `/proc/<pid>/status`.
pub fn proc_parent(pid: i32) -> Option<i32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("PPid:")
            .and_then(|value| value.trim().parse().ok())
    })
}

/// Age of a process, from `starttime` in `/proc/<pid>/stat` and `/proc/uptime`.
///
/// The kernel reports starttime in clock ticks; Linux userspace uses 100 Hz
/// (`USER_HZ`) on every supported architecture for `/proc` fields.
pub fn proc_age(pid: i32) -> Option<Duration> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let comm_end = stat.rfind(')')?;
    // Field 3 follows the comm; field 22 (starttime) is at index 19 here.
    let start_ticks: u64 = stat[comm_end + 1..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let age = uptime - start_ticks as f64 / 100.0;
    (age >= 0.0).then(|| Duration::from_secs_f64(age))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-swallow-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn window(id: u64, pid: i32, app_id: &str, title: &str) -> Window {
        serde_json::from_value(json!({
            "id": id,
            "title": title,
            "app_id": app_id,
            "pid": pid,
            "workspace_id": 1,
            "is_focused": false,
            "is_floating": false,
            "is_sticky": false,
            "is_urgent": false,
            "is_minimized": false,
            "layout": {
                "pos_in_scrolling_layout": [1, 1],
                "tile_size": [100.0, 100.0],
                "window_size": [100, 100],
                "tile_pos_in_workspace_view": [0.0, 0.0],
                "window_offset_in_tile": [0.0, 0.0]
            },
            "focus_timestamp": null
        }))
        .unwrap()
    }

    fn auto_windows(parking: &Parking, windows: &[(u64, i32, &str, &str, bool)]) {
        let entries = windows
            .iter()
            .map(|(id, pid, app_id, title, minimized)| {
                (
                    id.to_string(),
                    json!({
                        "id": id,
                        "pid": pid,
                        "app_id": app_id,
                        "title": title,
                        "is_minimized": minimized,
                    }),
                )
            })
            .collect();
        parking.replace_lot(crate::AUTO_WINDOWS, entries).unwrap();
    }

    fn parent_map(pairs: &[(i32, i32)]) -> ParentFn {
        let pairs = pairs.to_vec();
        Arc::new(move |pid| {
            pairs
                .iter()
                .find_map(|(child, parent)| (*child == pid).then_some(*parent))
        })
    }

    fn age_map(ages: &[(i32, u64)]) -> AgeFn {
        let ages = ages.to_vec();
        Arc::new(move |pid| {
            ages.iter()
                .find_map(|(child, ms)| (*child == pid).then(|| Duration::from_millis(*ms)))
        })
    }

    fn fake_actions(path: &Path, expected: usize) -> std::thread::JoinHandle<Vec<Request>> {
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

    fn enabled_swallow(dir: &Path, parking: &Parking, socket: &Path) -> Swallow {
        let swallow = Swallow::load_with_resolvers(
            dir.join("swallow.json"),
            parking.clone(),
            Compositor::at(socket),
            parent_map(&[(200, 100), (100, 1)]),
            age_map(&[]),
        )
        .unwrap();
        swallow.call("enable", &Value::Null).unwrap();
        swallow
    }

    #[test]
    fn proc_parent_reads_own_process() {
        let pid = std::process::id() as i32;
        let parent = proc_parent(pid).expect("own ppid");
        assert!(parent > 0);
        assert_ne!(parent, pid);
    }

    #[test]
    fn proc_age_reads_own_process() {
        let pid = std::process::id() as i32;
        assert!(proc_age(pid).is_some());
    }

    #[test]
    fn terminal_allowlist_and_config_persist() {
        let dir = temp_dir("config");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        let swallow = Swallow::load(
            dir.join("swallow.json"),
            parking,
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        assert!(!swallow.is_enabled());
        swallow.call("enable", &Value::Null).unwrap();
        swallow
            .call("add-terminal", &json!({ "app_id": "myterm" }))
            .unwrap();
        // Duplicate additions are case-insensitive.
        swallow
            .call("add-terminal", &json!({ "app_id": "MyTerm" }))
            .unwrap();
        let status = swallow.call("status", &Value::Null).unwrap();
        assert_eq!(status["enabled"], true);
        assert_eq!(
            status["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|value| *value == "myterm")
                .count(),
            1
        );
        // The stock allowlist covers several terminals at once.
        assert!(swallow.terminal_allowed(Some("ALACRITTY")));
        assert!(swallow.terminal_allowed(Some("org.wezfurlong.wezterm")));

        let reloaded = Swallow::load(
            dir.join("swallow.json"),
            Parking::load(dir.join("parking2.json")).unwrap(),
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        assert!(reloaded.is_enabled());
        assert!(reloaded.terminal_allowed(Some("myterm")));
        assert!(reloaded.terminal_allowed(Some("ALACRITTY")));
    }

    #[test]
    fn per_rule_regex_allows_custom_terminal_and_records_rule() {
        let dir = temp_dir("rule-allow");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 1);
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        auto_windows(
            &parking,
            &[
                (1, 100, "myterm", "dev shell", false),
                (2, 200, "editor", "notes.txt", false),
            ],
        );
        let swallow = enabled_swallow(&dir, &parking, &socket);
        swallow
            .call(
                "add-rule",
                &json!({
                    "name": "notes",
                    "terminal_app_id": ["myterm"],
                    "terminal_title_regex": "^dev",
                    "child_app_id": ["edit"],
                    "child_title_regex": "notes\\.txt$",
                }),
            )
            .unwrap();

        let outcome = swallow
            .maybe_swallow(&window(2, 200, "editor", "notes.txt"))
            .unwrap();
        assert_eq!(
            outcome,
            SwallowOutcome::Swallowed {
                child: 2,
                parent: 1,
                rule: Some("notes".into()),
            }
        );
        let entries = parking.lot(SWALLOW_LOT);
        assert_eq!(entries["child:2"]["rule"], "notes");
        assert_eq!(entries["child:2"]["child_app_id"], "editor");

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(1) })
        ));
    }

    #[test]
    fn ignore_list_and_ignore_rule_block_swallowing() {
        let dir = temp_dir("ignore");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        auto_windows(
            &parking,
            &[
                (1, 100, "alacritty", "shell", false),
                (2, 200, "editor", "notes.txt", false),
            ],
        );
        let swallow = enabled_swallow(&dir, &parking, &dir.join("unused.sock"));
        swallow
            .call("set-ignore", &json!({ "ignore": ["EDITOR"] }))
            .unwrap();
        let outcome = swallow
            .maybe_swallow(&window(2, 200, "editor", "notes.txt"))
            .unwrap();
        assert!(matches!(outcome, SwallowOutcome::Ignored { .. }));
        assert!(parking.lot(SWALLOW_LOT).is_empty());

        // An allow rule before an ignore rule wins; the ignore rule vetoes
        // when its extra condition matches.
        swallow
            .call(
                "add-rule",
                &json!({
                    "name": "allow-notes",
                    "child_app_id": ["editor"],
                }),
            )
            .unwrap();
        swallow
            .call("configure", &json!({ "child_age_ms": null }))
            .ok();
        // Remove the global ignore so the rule path is exercised.
        swallow
            .call("set-ignore", &json!({ "ignore": [] }))
            .unwrap();
        auto_windows(
            &parking,
            &[
                (1, 100, "alacritty", "shell", true),
                (2, 200, "editor", "notes.txt", false),
            ],
        );
        // Terminal already minimized: no action is needed, the pair still lands.
        let outcome = swallow
            .maybe_swallow(&window(2, 200, "editor", "notes.txt"))
            .unwrap();
        assert_eq!(
            outcome,
            SwallowOutcome::Swallowed {
                child: 2,
                parent: 1,
                rule: Some("allow-notes".into()),
            }
        );

        parking.replace_lot(SWALLOW_LOT, BTreeMap::new()).unwrap();
        swallow
            .call("remove-rule", &json!({ "name": "allow-notes" }))
            .unwrap();
        swallow
            .call(
                "add-rule",
                &json!({
                    "name": "ignore-notes",
                    "child_title_regex": "notes",
                    "ignore": true,
                }),
            )
            .unwrap();
        let outcome = swallow
            .maybe_swallow(&window(2, 200, "editor", "notes.txt"))
            .unwrap();
        assert_eq!(
            outcome,
            SwallowOutcome::Ignored {
                reason: "rule `ignore-notes`".into()
            }
        );
    }

    #[test]
    fn child_age_window_rejects_old_children() {
        let dir = temp_dir("age");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 1);
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        auto_windows(
            &parking,
            &[
                (1, 100, "alacritty", "shell", false),
                (2, 200, "editor", "old.txt", false),
            ],
        );
        let swallow = Swallow::load_with_resolvers(
            dir.join("swallow.json"),
            parking.clone(),
            Compositor::at(&socket),
            parent_map(&[(200, 100), (100, 1)]),
            age_map(&[(200, 5_000)]),
        )
        .unwrap();
        swallow.call("enable", &Value::Null).unwrap();
        swallow
            .call("configure", &json!({ "child_age_ms": 1_000 }))
            .unwrap();

        let outcome = swallow
            .maybe_swallow(&window(2, 200, "editor", "old.txt"))
            .unwrap();
        assert_eq!(
            outcome,
            SwallowOutcome::TooOld {
                child_age_ms: 5_000,
                limit_ms: 1_000,
            }
        );
        assert!(parking.lot(SWALLOW_LOT).is_empty());

        // A per-rule window overrides the stricter global default.
        swallow
            .call(
                "add-rule",
                &json!({
                    "name": "fresh-notes",
                    "child_title_regex": "old",
                    "child_age_ms": 10_000,
                }),
            )
            .unwrap();
        let outcome = swallow
            .maybe_swallow(&window(2, 200, "editor", "old.txt"))
            .unwrap();
        assert!(matches!(outcome, SwallowOutcome::Swallowed { .. }));

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
    }

    #[test]
    fn invalid_regex_rule_is_rejected_and_not_stored() {
        let dir = temp_dir("bad-regex");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        let swallow = Swallow::load(
            dir.join("swallow.json"),
            parking,
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        let error = swallow
            .call(
                "add-rule",
                &json!({ "name": "bad", "child_title_regex": "[" }),
            )
            .unwrap_err();
        assert!(error.contains("invalid `child_title_regex`"), "{error}");
        assert!(swallow.call("list", &Value::Null).is_ok());
        let status = swallow.call("status", &Value::Null).unwrap();
        assert_eq!(status["rules"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn rules_ignore_list_and_age_persist_across_reload() {
        let dir = temp_dir("rules-persist");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        let swallow = Swallow::load(
            dir.join("swallow.json"),
            parking,
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        swallow
            .call(
                "add-rule",
                &json!({ "name": "keep", "child_app_id": ["mpv"] }),
            )
            .unwrap();
        swallow
            .call(
                "add-rule",
                &json!({ "name": "keep", "child_app_id": ["vlc"] }),
            )
            .unwrap();
        swallow
            .call("set-ignore", &json!({ "ignore": ["yad"] }))
            .unwrap();
        swallow
            .call("configure", &json!({ "child_age_ms": 2500 }))
            .unwrap();

        let reloaded = Swallow::load(
            dir.join("swallow.json"),
            Parking::load(dir.join("parking2.json")).unwrap(),
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        let status = reloaded.call("status", &Value::Null).unwrap();
        assert_eq!(status["rules"].as_array().unwrap().len(), 1);
        assert_eq!(status["rules"][0]["child_app_id"][0], "vlc");
        assert_eq!(status["ignore"][0], "yad");
        assert_eq!(status["child_age_ms"], 2500);
        assert_eq!(status["restore_on_exit"], true);

        reloaded
            .call("remove-rule", &json!({ "name": "keep" }))
            .unwrap();
        let status = reloaded.call("status", &Value::Null).unwrap();
        assert_eq!(status["rules"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn persisted_pairs_survive_restart_and_restore_once() {
        let dir = temp_dir("restart");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 2);
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        auto_windows(
            &parking,
            &[
                (1, 100, "alacritty", "shell", false),
                (2, 200, "editor", "notes.txt", false),
            ],
        );
        let first = enabled_swallow(&dir, &parking, &socket);
        assert!(matches!(
            first
                .maybe_swallow(&window(2, 200, "editor", "notes.txt"))
                .unwrap(),
            SwallowOutcome::Swallowed { .. }
        ));
        drop(first);

        // A new daemon instance loads the same parking lot and keeps the pair.
        let reloaded = Swallow::load_with_resolvers(
            dir.join("swallow.json"),
            parking.clone(),
            Compositor::at(&socket),
            parent_map(&[(200, 100), (100, 1)]),
            age_map(&[]),
        )
        .unwrap();
        assert!(reloaded.is_enabled());
        let status = reloaded.call("status", &Value::Null).unwrap();
        assert_eq!(status["swallowed"]["child:2"]["parent"], 1);
        // It does not minimize a second time.
        assert_eq!(
            reloaded
                .maybe_swallow(&window(2, 200, "editor", "notes.txt"))
                .unwrap(),
            SwallowOutcome::AlreadySwallowed
        );

        let restored = reloaded.call("restore", &json!({ "child": 2 })).unwrap();
        assert_eq!(restored["restored"], true);
        assert_eq!(restored["parent"], 1);
        assert!(parking.lot(SWALLOW_LOT).is_empty());
        // Restoring twice reports no pair rather than failing.
        let again = reloaded.call("restore", &json!({ "child": 2 })).unwrap();
        assert_eq!(again["restored"], false);

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::MinimizeWindow { id: Some(1) })
        ));
        assert!(matches!(
            requests[1],
            Request::Action(Action::UnminimizeWindow { id: Some(1) })
        ));
    }

    #[test]
    fn parent_close_prunes_pairs_without_restoring() {
        let dir = temp_dir("parent-close");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        parking
            .call(
                "park",
                &json!({
                    "lot": SWALLOW_LOT,
                    "id": "child:2",
                    "value": { "child": 2, "parent": 1, "restore_on_exit": true }
                }),
            )
            .unwrap();
        let swallow = Swallow::load(
            dir.join("swallow.json"),
            parking.clone(),
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        assert_eq!(swallow.on_parent_closed(1).unwrap(), vec![2]);
        assert!(parking.lot(SWALLOW_LOT).is_empty());
        // Unrelated parent ids leave pairs alone.
        let _ = parking.call(
            "park",
            &json!({ "lot": SWALLOW_LOT, "id": "child:3", "value": { "child": 3, "parent": 4 } }),
        );
        assert!(swallow.on_parent_closed(1).unwrap().is_empty());
        assert!(parking.lot(SWALLOW_LOT).contains_key("child:3"));
    }

    #[test]
    fn reconcile_restores_stale_children() {
        let dir = temp_dir("reconcile");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 1);
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        parking
            .call(
                "park",
                &json!({
                    "lot": SWALLOW_LOT,
                    "id": "child:2",
                    "value": { "child": 2, "parent": 1, "restore_on_exit": true }
                }),
            )
            .unwrap();
        // Only the terminal is still open.
        auto_windows(&parking, &[(1, 100, "alacritty", "shell", true)]);
        let swallow = Swallow::load(
            dir.join("swallow.json"),
            parking.clone(),
            Compositor::at(&socket),
        )
        .unwrap();
        assert_eq!(swallow.reconcile().unwrap(), vec![2]);
        assert!(parking.lot(SWALLOW_LOT).is_empty());

        let requests = server.join().unwrap();
        assert!(matches!(
            requests[0],
            Request::Action(Action::UnminimizeWindow { id: Some(1) })
        ));
    }

    #[test]
    fn restore_on_exit_false_keeps_terminal_minimized() {
        let dir = temp_dir("no-restore");
        let socket = dir.join("niri.sock");
        let server = fake_actions(&socket, 1);
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        auto_windows(
            &parking,
            &[
                (1, 100, "alacritty", "shell", false),
                (2, 200, "editor", "notes.txt", false),
            ],
        );
        let swallow = enabled_swallow(&dir, &parking, &socket);
        swallow
            .call("configure", &json!({ "restore_on_exit": false }))
            .unwrap();
        assert!(matches!(
            swallow
                .maybe_swallow(&window(2, 200, "editor", "notes.txt"))
                .unwrap(),
            SwallowOutcome::Swallowed { .. }
        ));
        let entry = swallow.on_window_closed(2).unwrap().unwrap();
        assert_eq!(entry["restore_on_exit"], false);
        assert!(parking.lot(SWALLOW_LOT).is_empty());

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1, "only the minimize is sent");
    }
}
