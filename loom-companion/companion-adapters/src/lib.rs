//! Optional ecosystem adapters (companion Phase C).
//!
//! Four bounded contexts share one adapter engine:
//!
//! - `session.*` → loom-session / logind
//! - `monitor.*` → loom-monitor
//! - `audio.*` → loom-audio
//! - `wallpaper.*` → loom-wallpaper
//!
//! Each adapter stores explicit commands, event subscriptions and execution
//! limits in `$XDG_STATE_HOME/loom-companion/adapter-<name>.json`. Adapters
//! are disabled by default and nothing executes unless `enabled` is `true`
//! (the explicit `adapters.test`/`<adapter>.test` debugging action is the one
//! documented exception; it never changes persisted state).
//!
//! # Configuration
//!
//! Old files with only `enabled`, `commands` and `hooks` keep loading. New
//! fields are optional:
//!
//! ```json
//! {
//!   "enabled": true,
//!   "commands": { "apply": "loom-wallpaper apply" },
//!   "hooks": { "workspace-activated": "notify-send switched" },
//!   "subscriptions": {
//!     "workspace-activated": [
//!       {
//!         "command": "loom-wallpaper apply",
//!         "timeout_ms": 2000,
//!         "dedup_ms": 500,
//!         "failure_policy": "log"
//!       }
//!     ],
//!     "window-focused": [
//!       { "command": "loom-audio apply", "dedup_ms": 250 }
//!     ]
//!   },
//!   "timeout_ms": 5000,
//!   "dedup_ms": 1000,
//!   "max_concurrent": 2,
//!   "failure_policy": "disable-after-3"
//! }
//! ```
//!
//! Command templates support `{{workspace_id}}`, `{{workspace_name}}`,
//! `{{workspace_index}}`, `{{output}}`, `{{window_id}}`, `{{app_id}}`,
//! `{{title}}`, `{{focused}}`, `{{phase}}`, `{{event}}`, `{{adapter}}` and any
//! other payload field. Substituted values are shell-quoted as one word. The
//! same values are exported as `COMPANION_*` environment variables (for
//! example `COMPANION_WORKSPACE_NAME`, `COMPANION_APP_ID`,
//! `COMPANION_FOCUSED`), alongside `COMPANION_EVENT`, `COMPANION_ADAPTER` and
//! `COMPANION_EVENT_JSON`.
//!
//! # Events
//!
//! niri supplies snapshots and low-level changes; [`NiriEventMapper`] derives
//! `workspace-added`/`removed`/`moved`, `output-added`/`removed` and
//! `window-opened`/`moved` from consecutive events. The first snapshot after a
//! connect only seeds state, so a restart does not replay every window.
//! Companion actions are observed after they succeed: `park`, `restore`,
//! `stage`, `unstage`, `scratchpad-*`, `idle-transition`, `idle-active` and
//! `rotation-applied`.
//!
//! | Event | Source |
//! | --- | --- |
//! | `workspace-activated` | niri `WorkspaceActivated` |
//! | `workspace-added`, `workspace-removed`, `workspace-moved` | derived from `WorkspacesChanged` |
//! | `output-added`, `output-removed` | derived from outputs in the workspace snapshot |
//! | `window-opened`, `window-closed`, `window-focused`, `window-moved` | niri window events and snapshot diff |
//! | `park`, `restore` | completed `parker.*` actions |
//! | `stage`, `unstage` | completed `stage.*` actions |
//! | `scratchpad-toggle`, `scratchpad-show`, `scratchpad-hide` | completed `scratchpad.*` actions |
//! | `idle-transition`, `idle-active` | executed `idle.tick` timeouts / `idle.reset` |
//! | `rotation-applied` | `rotate.tick`/`rotate.set` with `applied: true` |
//!
//! `adapters.events` returns the same table at runtime, and
//! [`EVENT_TABLE`] carries the descriptions.
//!
//! # Safety
//!
//! - `enabled` defaults to `false`; enabling is explicit per adapter.
//! - Commands run through `sh -c` with a hard timeout and at most `max_concurrent` simultaneous
//!   processes per adapter.
//! - The same event, subscription and payload within `dedup_ms` runs once.
//! - `ignore`/`log`/`disable-after-N` control failures; the adapter disables itself after N
//!   consecutive failures without restarting the daemon.
//! - Adapter commands are optional: no `loom-*` binary is linked or required.

mod companion;
mod config;
mod event;
mod exec;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use companion::companion_events;
use companion_core::{
    ActionInfo, CallObserver, Context, Event, EventBus, ParamInfo, load_json, save_json,
};
use config::State;
pub use config::{
    DEFAULT_DEDUP_MS, DEFAULT_MAX_CONCURRENT, DEFAULT_TIMEOUT_MS, EVENT_KINDS, FailurePolicy,
    Subscription, is_known_event, normalize_event,
};
pub use event::{AdapterEvent, NiriEventMapper};
pub use exec::{Expansion, expand_template, shell_quote};
use serde_json::{Value, json};

/// Human-readable event table used by `adapters.events` and the README.
pub const EVENT_TABLE: &[(&str, &str)] = &[
    (
        "workspace-activated",
        "A workspace became active (and optionally focused)",
    ),
    (
        "workspace-added",
        "A new workspace appeared in the snapshot",
    ),
    (
        "workspace-removed",
        "A workspace disappeared from the snapshot",
    ),
    ("workspace-moved", "A workspace changed output or index"),
    (
        "output-added",
        "An output appears in the workspace snapshot",
    ),
    ("output-removed", "No workspace remains on an output"),
    ("window-opened", "A window id was seen for the first time"),
    ("window-closed", "A tracked window closed"),
    (
        "window-focused",
        "Window focus changed; `window_id` is null when focus was lost",
    ),
    ("window-moved", "A window changed workspace"),
    ("park", "parker.park or parker.park-workspace succeeded"),
    ("restore", "parker.restore or restore-workspace succeeded"),
    ("stage", "stage.stage/add/toggle succeeded"),
    ("unstage", "stage.activate/unstage/restore succeeded"),
    ("scratchpad-toggle", "scratchpad.toggle succeeded"),
    ("scratchpad-show", "scratchpad.show/restore succeeded"),
    ("scratchpad-hide", "scratchpad.assign or hide succeeded"),
    ("idle-transition", "An idle timeout action executed"),
    ("idle-active", "idle.reset marked activity"),
    (
        "rotation-applied",
        "rotate.tick or rotate.set applied an orientation",
    ),
];

const NAMESPACES: [&str; 4] = ["session", "monitor", "audio", "wallpaper"];

/// Adapter namespaces accepted by the config file's `adapters` section.
pub const ADAPTER_NAMESPACES: [&str; 4] = NAMESPACES;

const ADAPTER_STATE_KEYS: &[&str] = &[
    "enabled",
    "commands",
    "hooks",
    "subscriptions",
    "timeout_ms",
    "dedup_ms",
    "max_concurrent",
    "failure_policy",
];

#[derive(Debug, Clone, Default)]
struct Stats {
    events_received: u64,
    events_enabled: u64,
    skipped_disabled: u64,
    skipped_dedup: u64,
    executed: u64,
    succeeded: u64,
    failed: u64,
    timed_out: u64,
    disabled_after_failure: u64,
    consecutive_failures: u32,
    last_event: Option<String>,
    last_command: Option<String>,
    last_error: Option<String>,
    last_duration_ms: Option<u64>,
}

impl Stats {
    fn to_json(&self, peak_concurrency: usize) -> Value {
        json!({
            "events_received": self.events_received,
            "events_enabled": self.events_enabled,
            "skipped_disabled": self.skipped_disabled,
            "skipped_dedup": self.skipped_dedup,
            "executed": self.executed,
            "succeeded": self.succeeded,
            "failed": self.failed,
            "timed_out": self.timed_out,
            "disabled_after_failure": self.disabled_after_failure,
            "consecutive_failures": self.consecutive_failures,
            "peak_concurrency": peak_concurrency,
            "last_event": self.last_event,
            "last_command": self.last_command,
            "last_error": self.last_error,
            "last_duration_ms": self.last_duration_ms,
        })
    }
}

struct AdapterCore {
    namespace: &'static str,
    path: PathBuf,
    state: Mutex<State>,
    stats: Mutex<Stats>,
    dedup: Mutex<BTreeMap<String, Instant>>,
    gate: exec::Gate,
}

impl AdapterCore {
    fn load(namespace: &'static str, path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        let core = Self {
            namespace,
            path,
            gate: exec::Gate::new(state.max_concurrent),
            state: Mutex::new(state),
            stats: Mutex::new(Stats::default()),
            dedup: Mutex::new(BTreeMap::new()),
        };
        Ok(core)
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state)
            .map_err(|err| format!("cannot save {} adapter: {err}", self.namespace))
    }

    /// Merges a config-file patch over the current state without writing it.
    fn merged_state(&self, patch: &Value) -> Result<State, String> {
        let object = patch
            .as_object()
            .ok_or_else(|| format!("adapter `{}` config must be an object", self.namespace))?;
        if let Some(key) = object
            .keys()
            .find(|key| !ADAPTER_STATE_KEYS.contains(&key.as_str()))
        {
            return Err(format!(
                "adapter `{}` has unknown config field `{key}`",
                self.namespace
            ));
        }
        let mut merged = serde_json::to_value(&*self.lock_state()?)
            .map_err(|err| format!("cannot read adapter state: {err}"))?;
        let Some(merged_object) = merged.as_object_mut() else {
            return Err("adapter state is not an object".to_owned());
        };
        for (key, value) in object {
            merged_object.insert(key.clone(), value.clone());
        }
        let state: State = serde_json::from_value(merged)
            .map_err(|err| format!("invalid `{}` adapter config: {err}", self.namespace))?;
        if !(1..=64).contains(&state.max_concurrent) {
            return Err(format!(
                "adapter `{}` max_concurrent must be between 1 and 64",
                self.namespace
            ));
        }
        if object.contains_key("timeout_ms") && state.timeout_ms == 0 {
            return Err(format!(
                "adapter `{}` timeout_ms must be greater than zero",
                self.namespace
            ));
        }
        Ok(state)
    }

    fn validate_config(&self, patch: &Value) -> Result<(), String> {
        self.merged_state(patch).map(|_| ())
    }

    fn apply_config(&self, patch: &Value) -> Result<(), String> {
        let state = self.merged_state(patch)?;
        self.gate.set_limit(state.max_concurrent);
        self.save(&state)?;
        *self.lock_state()? = state;
        Ok(())
    }

    fn status(&self) -> Result<Value, String> {
        let state = self.lock_state()?;
        let stats = self
            .stats
            .lock()
            .map_err(|_| "adapter stats lock poisoned".to_owned())?;
        Ok(json!({
            "namespace": self.namespace,
            "enabled": state.enabled,
            "commands": state.commands,
            "hooks": state.hooks,
            "subscriptions": state.subscriptions,
            "timeout_ms": state.timeout_ms,
            "dedup_ms": state.dedup_ms,
            "max_concurrent": state.max_concurrent,
            "failure_policy": state.failure_policy.as_json(),
            "stats": stats.to_json(self.gate.peak()),
        }))
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, State>, String> {
        self.state
            .lock()
            .map_err(|_| format!("{} adapter state lock poisoned", self.namespace))
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "status" => self.status(),
            "enable" | "disable" => {
                let enabled = action == "enable";
                let mut state = self.lock_state()?;
                state.enabled = enabled;
                self.save(&state)?;
                Ok(json!({ "enabled": enabled }))
            }
            "set" => {
                let name = param_str(params, "name")?;
                let command = param_str(params, "command")?;
                let mut state = self.lock_state()?;
                state.commands.insert(name.clone(), command.clone());
                self.save(&state)?;
                Ok(json!({ "name": name, "command": command }))
            }
            "remove" => {
                let name = param_str(params, "name")?;
                let mut state = self.lock_state()?;
                let removed = state.commands.remove(&name).is_some();
                self.save(&state)?;
                Ok(json!({ "name": name, "removed": removed }))
            }
            "list" => {
                let state = self.lock_state()?;
                Ok(json!({ "commands": state.commands }))
            }
            "dispatch" => self.dispatch(params),
            "set-hook" => {
                let event = normalize_event(&param_str(params, "event")?);
                let command = param_str(params, "command")?;
                let mut state = self.lock_state()?;
                state.hooks.insert(event.clone(), command.clone());
                self.save(&state)?;
                Ok(json!({ "event": event, "command": command }))
            }
            "remove-hook" => {
                let event = param_str(params, "event")?;
                let event = normalize_event(&event);
                let mut state = self.lock_state()?;
                let removed = state.hooks.remove(&event).is_some();
                self.save(&state)?;
                Ok(json!({ "event": event, "removed": removed }))
            }
            "hooks" => {
                let state = self.lock_state()?;
                Ok(json!({ "hooks": state.hooks }))
            }
            "subscribe" => self.subscribe(params),
            "unsubscribe" => self.unsubscribe(params),
            "subscriptions" => self.subscriptions(params),
            "configure" => self.configure(params),
            "explain" => {
                let event = adapter_event_from_params(params)?;
                Ok(explain_for(self, &event))
            }
            "test" => self.test(params),
            "preset" => {
                let (commands, subscriptions) = preset_for(self.namespace);
                Ok(json!({
                    "namespace": self.namespace,
                    "commands": commands,
                    "subscriptions": subscriptions,
                    "enabled": self.lock_state()?.enabled,
                }))
            }
            "apply-preset" => self.apply_preset(params),
            other => Err(format!("unknown action `{}.{other}`", self.namespace)),
        }
    }

    fn dispatch(&self, params: &Value) -> Result<Value, String> {
        let name = param_str(params, "name")?;
        let payload = params.get("payload").cloned().unwrap_or(Value::Null);
        let event = params
            .get("event")
            .and_then(Value::as_str)
            .map(normalize_event)
            .unwrap_or_else(|| "dispatch".to_owned());
        let (command, timeout) = {
            let state = self.lock_state()?;
            if !state.enabled {
                return Err(format!(
                    "{} adapter is disabled; call {}.enable first",
                    self.namespace, self.namespace
                ));
            }
            (
                state
                    .commands
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| format!("no command `{name}` configured"))?,
                state.timeout_ms,
            )
        };
        let context = template_context(self.namespace, &event, &payload);
        let expansion = expand_template(&command, &context);
        let envs = exec::environment(self.namespace, &event, &context);
        let outcome = exec::run_command(&expansion.command, timeout, &envs);
        Ok(json!({
            "command": name,
            "expanded": expansion.command,
            "unresolved": expansion.unresolved,
            "success": outcome.success,
            "code": outcome.code,
            "stdout": outcome.stdout,
            "stderr": outcome.stderr,
            "timed_out": outcome.timed_out,
            "duration_ms": outcome.duration_ms,
        }))
    }

    fn subscribe(&self, params: &Value) -> Result<Value, String> {
        let event = normalize_known_event(params)?;
        let command = param_str(params, "command")?;
        let subscription = Subscription {
            command: command.clone(),
            label: params
                .get("label")
                .and_then(Value::as_str)
                .map(str::to_owned),
            timeout_ms: optional_u64(params, "timeout_ms")?,
            dedup_ms: optional_u64(params, "dedup_ms")?,
            failure_policy: params
                .get("failure_policy")
                .filter(|value| !value.is_null())
                .map(|value| {
                    serde_json::from_value::<FailurePolicy>(value.clone())
                        .map_err(|err| format!("invalid failure_policy: {err}"))
                })
                .transpose()?,
        };
        let mut state = self.lock_state()?;
        let entries = state.subscriptions.entry(event.clone()).or_default();
        if let Some(existing) = entries.iter_mut().find(|existing| {
            existing.command == subscription.command && existing.label == subscription.label
        }) {
            *existing = subscription.clone();
        } else {
            entries.push(subscription.clone());
        }
        self.save(&state)?;
        Ok(json!({
            "event": event,
            "subscription": subscription,
            "count": state.subscriptions.get(&event).map(Vec::len).unwrap_or(0),
        }))
    }

    fn unsubscribe(&self, params: &Value) -> Result<Value, String> {
        let event = normalize_event(&param_str(params, "event")?);
        let command = params.get("command").and_then(Value::as_str);
        let index = params
            .get("index")
            .and_then(Value::as_u64)
            .map(|v| v as usize);
        let mut state = self.lock_state()?;
        let removed = match state.subscriptions.get_mut(&event) {
            Some(entries) => {
                let before = entries.len();
                match (command, index) {
                    (_, Some(index)) => {
                        if index < entries.len() {
                            entries.remove(index);
                        }
                    }
                    (Some(command), None) => entries.retain(|entry| entry.command != command),
                    (None, None) => entries.clear(),
                }
                let after = entries.len();
                if after == 0 {
                    state.subscriptions.remove(&event);
                }
                before - after
            }
            None => 0,
        };
        self.save(&state)?;
        Ok(json!({ "event": event, "removed": removed }))
    }

    fn subscriptions(&self, params: &Value) -> Result<Value, String> {
        let state = self.lock_state()?;
        let filtered: BTreeMap<_, _> = match params.get("event").and_then(Value::as_str) {
            Some(event) => {
                let event = normalize_event(event);
                state
                    .subscriptions
                    .get(&event)
                    .map(|entries| BTreeMap::from([(event, entries.clone())]))
                    .unwrap_or_default()
            }
            None => state.subscriptions.clone(),
        };
        Ok(json!({ "subscriptions": filtered, "hooks": state.hooks }))
    }

    fn configure(&self, params: &Value) -> Result<Value, String> {
        let mut state = self.lock_state()?;
        if let Some(value) = params.get("timeout_ms") {
            let timeout = value
                .as_u64()
                .ok_or_else(|| "`timeout_ms` must be an integer".to_owned())?;
            if timeout == 0 {
                return Err("`timeout_ms` must be at least 1".to_owned());
            }
            state.timeout_ms = timeout;
        }
        if let Some(value) = params.get("dedup_ms") {
            state.dedup_ms = value
                .as_u64()
                .ok_or_else(|| "`dedup_ms` must be an integer".to_owned())?;
        }
        if let Some(value) = params.get("max_concurrent") {
            let limit = value
                .as_u64()
                .ok_or_else(|| "`max_concurrent` must be an integer".to_owned())?;
            if !(1..=64).contains(&limit) {
                return Err("`max_concurrent` must be between 1 and 64".to_owned());
            }
            state.max_concurrent = limit as usize;
            self.gate.set_limit(state.max_concurrent);
        }
        if let Some(value) = params.get("failure_policy") {
            state.failure_policy = serde_json::from_value(value.clone())
                .map_err(|err| format!("invalid failure_policy: {err}"))?;
        }
        self.save(&state)?;
        Ok(json!({
            "enabled": state.enabled,
            "timeout_ms": state.timeout_ms,
            "dedup_ms": state.dedup_ms,
            "max_concurrent": state.max_concurrent,
            "failure_policy": state.failure_policy.as_json(),
        }))
    }

    fn test(&self, params: &Value) -> Result<Value, String> {
        let dry_run = params
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (event, jobs) = if let Some(command) = params.get("command").and_then(Value::as_str) {
            let event = params
                .get("event")
                .and_then(Value::as_str)
                .map(normalize_event)
                .unwrap_or_else(|| "test".to_owned());
            let payload = params.get("payload").cloned().unwrap_or(Value::Null);
            let context = template_context(self.namespace, &event, &payload);
            let (timeout, policy) = {
                let state = self.lock_state()?;
                (state.timeout_ms, state.failure_policy)
            };
            let job = make_job(
                self.namespace,
                &event,
                Subscription {
                    command: command.to_owned(),
                    label: Some("test".to_owned()),
                    timeout_ms: None,
                    dedup_ms: None,
                    failure_policy: None,
                },
                &context,
                timeout,
                policy,
                false,
            );
            (event, vec![job])
        } else {
            let event = adapter_event_from_params(params)?;
            let plan = jobs_for(self, &event, Mode::Test)?;
            (event.kind, plan.jobs)
        };

        let results: Vec<Value> = if dry_run {
            jobs.iter()
                .map(|job| {
                    json!({
                        "command": job.subscription.command,
                        "expanded": job.expanded,
                        "unresolved": job.unresolved,
                        "timeout_ms": job.timeout_ms,
                        "failure_policy": job.policy.as_json(),
                        "label": job.subscription.label,
                        "dry_run": true,
                    })
                })
                .collect()
        } else {
            execute_jobs(self, &jobs)
                .into_iter()
                .map(|(job, outcome)| job_result(&job, &outcome))
                .collect()
        };
        let enabled = self.lock_state()?.enabled;
        Ok(json!({
            "namespace": self.namespace,
            "event": event,
            "enabled": enabled,
            "dry_run": dry_run,
            "results": results,
        }))
    }

    fn apply_preset(&self, params: &Value) -> Result<Value, String> {
        let (commands, subscriptions) = preset_for(self.namespace);
        let enable = params
            .get("enable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut state = self.lock_state()?;
        for (name, command) in commands {
            state.commands.insert(name, command);
        }
        for (event, entries) in subscriptions {
            let existing = state.subscriptions.entry(event).or_default();
            for entry in entries {
                if !existing.iter().any(|old| old.command == entry.command) {
                    existing.push(entry);
                }
            }
        }
        if enable {
            state.enabled = true;
        }
        self.save(&state)?;
        Ok(json!({
            "namespace": self.namespace,
            "enabled": state.enabled,
            "commands": state.commands,
            "subscriptions": state.subscriptions,
        }))
    }
}

/// One prepared command execution.
#[derive(Clone)]
struct Job {
    subscription: Subscription,
    expanded: String,
    unresolved: Vec<String>,
    timeout_ms: u64,
    policy: FailurePolicy,
    envs: Vec<(String, String)>,
    deduped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Deliver,
    Explain,
    Test,
}

struct Plan {
    enabled: bool,
    jobs: Vec<Job>,
}

/// Resolves event subscriptions without executing anything.
///
/// `Deliver` enforces `enabled`, records stats and marks the dedup map;
/// `Explain` and `Test` peek at subscriptions but never mutate configuration.
fn jobs_for(core: &AdapterCore, event: &AdapterEvent, mode: Mode) -> Result<Plan, String> {
    if mode == Mode::Deliver {
        let mut stats = core
            .stats
            .lock()
            .map_err(|_| "adapter stats lock poisoned".to_owned())?;
        stats.events_received += 1;
        stats.last_event = Some(event.kind.clone());
    }

    let state = core.lock_state()?;
    if mode == Mode::Deliver && !state.enabled {
        let mut stats = core
            .stats
            .lock()
            .map_err(|_| "adapter stats lock poisoned".to_owned())?;
        stats.skipped_disabled += 1;
        return Ok(Plan {
            enabled: false,
            jobs: Vec::new(),
        });
    }
    if mode == Mode::Deliver {
        let mut stats = core
            .stats
            .lock()
            .map_err(|_| "adapter stats lock poisoned".to_owned())?;
        stats.events_enabled += 1;
    }

    let subscriptions = state.subscriptions_for(&event.kind);
    if subscriptions.is_empty() {
        return Ok(Plan {
            enabled: state.enabled,
            jobs: Vec::new(),
        });
    }

    let context = template_context(core.namespace, &event.kind, &event.payload);
    let now = Instant::now();
    let payload_key = serde_json::to_string(&event.payload).unwrap_or_default();
    let mut dedup = core
        .dedup
        .lock()
        .map_err(|_| "adapter dedup lock poisoned".to_owned())?;
    if dedup.len() > 4096 {
        dedup.retain(|_, seen| now.duration_since(*seen) < Duration::from_secs(600));
        if dedup.len() > 4096 {
            dedup.clear();
        }
    }

    let mut jobs = Vec::with_capacity(subscriptions.len());
    for subscription in subscriptions {
        let key = format!("{}|{}|{}", event.kind, subscription.command, payload_key);
        let window = Duration::from_millis(subscription.dedup_ms.unwrap_or(state.dedup_ms));
        let deduped = window > Duration::ZERO
            && dedup
                .get(&key)
                .is_some_and(|seen| now.duration_since(*seen) < window);
        if deduped {
            if mode == Mode::Deliver {
                let mut stats = core
                    .stats
                    .lock()
                    .map_err(|_| "adapter stats lock poisoned".to_owned())?;
                stats.skipped_dedup += 1;
                continue;
            }
        } else if mode == Mode::Deliver {
            dedup.insert(key, now);
        }

        let timeout_ms = subscription.timeout_ms.unwrap_or(state.timeout_ms);
        let policy = subscription.failure_policy.unwrap_or(state.failure_policy);
        jobs.push(make_job(
            core.namespace,
            &event.kind,
            subscription,
            &context,
            timeout_ms,
            policy,
            deduped,
        ));
    }
    Ok(Plan {
        enabled: state.enabled,
        jobs,
    })
}

#[allow(clippy::too_many_arguments)]
fn make_job(
    adapter: &str,
    event: &str,
    subscription: Subscription,
    context: &Value,
    timeout_ms: u64,
    policy: FailurePolicy,
    deduped: bool,
) -> Job {
    let expansion = expand_template(&subscription.command, context);
    Job {
        subscription,
        expanded: expansion.command,
        unresolved: expansion.unresolved,
        timeout_ms,
        policy,
        envs: exec::environment(adapter, event, context),
        deduped,
    }
}

fn execute_jobs(core: &AdapterCore, jobs: &[Job]) -> Vec<(Job, exec::CommandOutcome)> {
    let mut results = Vec::with_capacity(jobs.len());
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(jobs.len());
        for job in jobs {
            handles.push(scope.spawn(|| {
                core.gate.acquire();
                let outcome = exec::run_command(&job.expanded, job.timeout_ms, &job.envs);
                core.gate.release();
                outcome
            }));
        }
        for (job, handle) in jobs.iter().zip(handles) {
            if let Ok(outcome) = handle.join() {
                results.push((job.clone(), outcome));
            }
        }
    });
    results
}

fn job_result(job: &Job, outcome: &exec::CommandOutcome) -> Value {
    json!({
        "command": job.subscription.command,
        "label": job.subscription.label,
        "expanded": job.expanded,
        "unresolved": job.unresolved,
        "timeout_ms": job.timeout_ms,
        "failure_policy": job.policy.as_json(),
        "deduped": job.deduped,
        "success": outcome.success,
        "code": outcome.code,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "timed_out": outcome.timed_out,
        "duration_ms": outcome.duration_ms,
    })
}

fn apply_results(core: &AdapterCore, results: Vec<(Job, exec::CommandOutcome)>) -> Vec<Value> {
    let mut values = Vec::with_capacity(results.len());
    let mut disabled = false;
    let mut diagnostics = Vec::new();
    {
        let Ok(mut stats) = core.stats.lock() else {
            return values;
        };
        for (job, outcome) in &results {
            stats.executed += 1;
            stats.last_command = Some(job.expanded.clone());
            stats.last_duration_ms = Some(outcome.duration_ms);
            if outcome.success {
                stats.succeeded += 1;
                stats.consecutive_failures = 0;
            } else {
                stats.failed += 1;
                if outcome.timed_out {
                    stats.timed_out += 1;
                }
                stats.consecutive_failures = stats.consecutive_failures.saturating_add(1);
                let detail = if outcome.timed_out {
                    format!("timed out after {} ms", job.timeout_ms)
                } else {
                    let stderr = outcome.stderr.trim();
                    if stderr.is_empty() {
                        format!("exited with {:?}", outcome.code)
                    } else {
                        truncate(stderr)
                    }
                };
                stats.last_error = Some(detail.clone());
                match job.policy {
                    FailurePolicy::Ignore => {}
                    FailurePolicy::Log => {
                        eprintln!(
                            "loom-companion: {} adapter `{}` failed: {detail}",
                            core.namespace, job.expanded
                        );
                    }
                    FailurePolicy::DisableAfter(limit) => {
                        eprintln!(
                            "loom-companion: {} adapter `{}` failed ({}/{}): {detail}",
                            core.namespace, job.expanded, stats.consecutive_failures, limit
                        );
                        if stats.consecutive_failures >= limit {
                            disabled = true;
                            diagnostics.push(format!(
                                "disabled after {} consecutive failures",
                                stats.consecutive_failures
                            ));
                        }
                    }
                }
            }
            values.push(job_result(job, outcome));
        }
    }

    if disabled
        && let Ok(mut state) = core.lock_state()
        && state.enabled
    {
        state.enabled = false;
        if let Err(err) = core.save(&state) {
            eprintln!("loom-companion: {} adapter: {err}", core.namespace);
        } else if let Ok(mut stats) = core.stats.lock() {
            stats.disabled_after_failure += 1;
        }
    }
    if let Some(value) = values.last_mut() {
        value["disabled"] = json!(disabled);
        if !diagnostics.is_empty() {
            value["diagnostics"] = json!(diagnostics);
        }
    }
    values
}

fn truncate(text: &str) -> String {
    const LIMIT: usize = 2_000;
    if text.chars().count() <= LIMIT {
        return text.to_owned();
    }
    text.chars().take(LIMIT).collect::<String>() + "…"
}

fn deliver_event(core: &AdapterCore, event: &AdapterEvent) -> Result<Value, String> {
    let plan = jobs_for(core, event, Mode::Deliver)?;
    if !plan.enabled {
        return Ok(json!({
            "enabled": false,
            "event": event.kind,
            "executed": 0,
        }));
    }
    if plan.jobs.is_empty() {
        return Ok(json!({
            "enabled": true,
            "event": event.kind,
            "executed": 0,
        }));
    }
    let results = apply_results(core, execute_jobs(core, &plan.jobs));
    Ok(json!({
        "enabled": true,
        "event": event.kind,
        "executed": results.len(),
        "results": results,
    }))
}

fn explain_for(core: &AdapterCore, event: &AdapterEvent) -> Value {
    match jobs_for(core, event, Mode::Explain) {
        Ok(plan) => {
            let subscriptions = plan
                .jobs
                .iter()
                .map(|job| {
                    json!({
                        "command": job.subscription.command,
                        "label": job.subscription.label,
                        "expanded": job.expanded,
                        "unresolved": job.unresolved,
                        "timeout_ms": job.timeout_ms,
                        "failure_policy": job.policy.as_json(),
                        "deduped": job.deduped,
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "namespace": core.namespace,
                "enabled": plan.enabled,
                "event": event.kind,
                "payload": event.payload,
                "subscriptions": subscriptions,
            })
        }
        Err(message) => json!({
            "namespace": core.namespace,
            "error": message,
        }),
    }
}

fn template_context(adapter: &str, event: &str, payload: &Value) -> Value {
    let mut context = payload.clone();
    if let Some(object) = context.as_object_mut() {
        object.insert("event".to_owned(), json!(event));
        object.insert("adapter".to_owned(), json!(adapter));
    }
    context
}

fn param_str(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing string `{key}`"))
}

fn optional_u64(params: &Value, key: &str) -> Result<Option<u64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be an integer")),
    }
}

fn normalize_known_event(params: &Value) -> Result<String, String> {
    let event = normalize_event(&param_str(params, "event")?);
    if is_known_event(&event) {
        Ok(event)
    } else {
        Err(format!(
            "unknown event `{event}`; call adapters.events for the event table"
        ))
    }
}

fn adapter_event_from_params(params: &Value) -> Result<AdapterEvent, String> {
    let event = normalize_known_event(params)?;
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);
    Ok(AdapterEvent::new(event, payload))
}

/// A suggestion for each built-in adapter. Commands are optional and nothing is
/// enabled or executed by `preset`; `apply-preset` is the explicit write.
fn preset_for(
    namespace: &str,
) -> (
    BTreeMap<String, String>,
    BTreeMap<String, Vec<Subscription>>,
) {
    let mut commands = BTreeMap::new();
    let mut subscriptions: BTreeMap<String, Vec<Subscription>> = BTreeMap::new();
    match namespace {
        "session" => {
            commands.insert(
                "capture".to_owned(),
                "loom-session capture companion-{{workspace_name}}".to_owned(),
            );
            commands.insert(
                "restore".to_owned(),
                "loom-session restore companion-{{workspace_name}} --wait-ms 800".to_owned(),
            );
        }
        "monitor" => {
            let apply = r#"loom-monitor apply "${LOOM_MONITOR_PROFILE:-default}""#;
            commands.insert("apply".to_owned(), apply.to_owned());
            commands.insert("restore".to_owned(), "loom-monitor restore".to_owned());
            subscriptions.insert("output-added".to_owned(), vec![Subscription::new(apply)]);
            subscriptions.insert("output-removed".to_owned(), vec![Subscription::new(apply)]);
        }
        "audio" => {
            commands.insert("apply".to_owned(), "loom-audio apply".to_owned());
            subscriptions.insert(
                "window-focused".to_owned(),
                vec![Subscription::new("loom-audio apply")],
            );
            subscriptions.insert(
                "workspace-activated".to_owned(),
                vec![Subscription::new("loom-audio apply")],
            );
        }
        "wallpaper" => {
            commands.insert("apply".to_owned(), "loom-wallpaper apply".to_owned());
            subscriptions.insert(
                "workspace-activated".to_owned(),
                vec![Subscription::new("loom-wallpaper apply")],
            );
        }
        _ => {}
    }
    (commands, subscriptions)
}

/// Shared adapter engine. Cheap to clone; clones share the same state.
#[derive(Clone)]
pub struct Adapters {
    inner: Arc<HubInner>,
}

struct HubInner {
    adapters: BTreeMap<&'static str, Arc<AdapterCore>>,
    mapper: Mutex<NiriEventMapper>,
    sender: Mutex<Option<SyncSender<AdapterEvent>>>,
    bus: Mutex<Option<EventBus>>,
    running: AtomicBool,
    dropped: AtomicU64,
}

impl Adapters {
    /// Loads the four built-in adapters from `dir`.
    pub fn load(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref();
        let mut adapters = BTreeMap::new();
        for namespace in NAMESPACES {
            let path = dir.join(format!("adapter-{namespace}.json"));
            adapters.insert(namespace, Arc::new(AdapterCore::load(namespace, path)?));
        }
        Ok(Self {
            inner: Arc::new(HubInner {
                adapters,
                mapper: Mutex::new(NiriEventMapper::new()),
                sender: Mutex::new(None),
                bus: Mutex::new(None),
                running: AtomicBool::new(false),
                dropped: AtomicU64::new(0),
            }),
        })
    }

    pub fn from_state_dir() -> std::io::Result<Self> {
        Self::load(companion_core::state_dir())
    }

    /// A distinct `Context` per namespace, preserving the old API.
    pub fn adapters(&self) -> Vec<Adapter> {
        NAMESPACES
            .iter()
            .filter_map(|namespace| self.inner.adapters.get(namespace))
            .map(|core| Adapter { core: core.clone() })
            .collect()
    }

    /// The aggregate `adapters.*` context.
    pub fn context(&self) -> AdaptersContext {
        AdaptersContext {
            adapters: self.clone(),
        }
    }

    /// Observer that turns completed companion actions into events.
    pub fn observer(&self) -> Arc<AdapterObserver> {
        Arc::new(AdapterObserver {
            adapters: self.clone(),
        })
    }

    /// Starts the single background worker used by the daemon.
    ///
    /// Without a worker, [`Self::submit`] executes synchronously. That keeps
    /// unit tests deterministic while the daemon never blocks the niri tracker
    /// on a shell command.
    pub fn start_worker(&self) {
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let (sender, receiver) = sync_channel(256);
        if let Ok(mut slot) = self.inner.sender.lock() {
            *slot = Some(sender);
        }
        let inner = self.inner.clone();
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                inner.deliver(&event);
            }
        });
    }

    /// Publishes normalized events and adapter deliveries on `bus`.
    ///
    /// This is optional; without a bus the adapter engine behaves exactly as
    /// before. The bus is non-blocking, so a slow external subscriber never
    /// delays the niri tracker or an adapter command.
    pub fn set_event_bus(&self, bus: EventBus) {
        if let Ok(mut slot) = self.inner.bus.lock() {
            *slot = Some(bus);
        }
    }

    /// Queues an event for the worker, or runs it now when no worker exists.
    pub fn submit(&self, event: AdapterEvent) {
        if let Ok(bus) = self.inner.bus.lock()
            && let Some(bus) = bus.as_ref()
        {
            bus.publish(Event::new(event.kind.clone(), event.payload.clone()));
        }
        let sender = self.inner.sender.lock().ok().and_then(|slot| slot.clone());
        match sender {
            Some(sender) => {
                if sender.try_send(event).is_err() {
                    self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                    eprintln!("loom-companion: adapter event queue full; dropping an event");
                }
            }
            None => {
                self.inner.deliver(&event);
            }
        }
    }

    /// Delivers one event synchronously to every adapter.
    pub fn deliver(&self, event: &AdapterEvent) -> Value {
        self.inner.deliver(event)
    }

    /// Converts and delivers one niri event.
    pub fn niri_event(&self, event: &niri_ipc::Event) {
        let events = match self.inner.mapper.lock() {
            Ok(mut mapper) => mapper.map(event),
            Err(_) => return,
        };
        for event in events {
            self.submit(event);
        }
    }

    /// Call before each new niri subscription.
    pub fn reset_niri(&self) {
        if let Ok(mut mapper) = self.inner.mapper.lock() {
            mapper.reset();
        }
    }

    /// Validates one `adapters.<name>` config-file section.
    pub fn validate_config(&self, namespace: &str, patch: &Value) -> Result<(), String> {
        let core = self
            .inner
            .adapters
            .get(namespace)
            .ok_or_else(|| format!("unknown adapter `{namespace}`"))?;
        core.validate_config(patch)
    }

    /// Merges one `adapters.<name>` config-file section into the live adapter.
    pub fn apply_config(&self, namespace: &str, patch: &Value) -> Result<(), String> {
        let core = self
            .inner
            .adapters
            .get(namespace)
            .ok_or_else(|| format!("unknown adapter `{namespace}`"))?;
        core.apply_config(patch)
    }
}

impl HubInner {
    fn deliver(&self, event: &AdapterEvent) -> Value {
        let mut report = BTreeMap::new();
        for (namespace, core) in &self.adapters {
            match deliver_event(core, event) {
                Ok(value) => {
                    report.insert(*namespace, value);
                }
                Err(message) => {
                    report.insert(*namespace, json!({ "error": message }));
                }
            }
        }
        let report = json!({
            "event": event.kind,
            "adapters": report,
        });
        if let Ok(bus) = self.bus.lock()
            && let Some(bus) = bus.as_ref()
        {
            bus.publish(Event::new("adapter-delivery", report.clone()));
        }
        report
    }
}

/// One adapter namespace (`session`, `monitor`, `audio`, `wallpaper`).
#[derive(Clone)]
pub struct Adapter {
    core: Arc<AdapterCore>,
}

impl Context for Adapter {
    fn namespace(&self) -> &'static str {
        self.core.namespace
    }

    fn actions(&self) -> Vec<ActionInfo> {
        adapter_actions()
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        self.core.call(action, params)
    }
}

fn adapter_actions() -> Vec<ActionInfo> {
    vec![
        ActionInfo::new(
            "status",
            "Enabled state, commands, hooks, subscriptions and stats",
        ),
        ActionInfo::new(
            "enable",
            "Enable this adapter (required for automatic dispatch)",
        ),
        ActionInfo::new("disable", "Disable this adapter"),
        ActionInfo::new("set", "Store command `name` = `command`")
            .with_params([
                ParamInfo::string("name", true, "command name"),
                ParamInfo::string("command", true, "shell command template"),
            ])
            .with_permissions(&["shell"]),
        ActionInfo::new("remove", "Remove command `name`").with_params([ParamInfo::string(
            "name",
            true,
            "command name",
        )]),
        ActionInfo::new("list", "List configured commands"),
        ActionInfo::new("dispatch", "Run command `name` explicitly")
            .with_params([ParamInfo::string("name", true, "command name")])
            .with_permissions(&["shell"]),
        ActionInfo::new("set-hook", "Store legacy event → command for this adapter")
            .with_params([
                ParamInfo::string("event", true, "event name"),
                ParamInfo::string("command", true, "shell command template"),
            ])
            .with_permissions(&["shell"]),
        ActionInfo::new("remove-hook", "Remove a legacy event hook").with_params([
            ParamInfo::string("event", true, "event name"),
            ParamInfo::string("command", false, "command to remove; all when absent"),
        ]),
        ActionInfo::new("hooks", "List legacy event → command hooks")
            .with_params([ParamInfo::string("event", false, "filter by event")]),
        ActionInfo::new(
            "subscribe",
            "Add an event `subscription` with a command template",
        )
        .with_params([
            ParamInfo::string("event", true, "event name"),
            ParamInfo::string("command", true, "shell command template"),
            ParamInfo::string("label", false, "label for this subscription"),
            ParamInfo::integer("timeout_ms", false, "hard timeout in ms"),
            ParamInfo::integer("dedup_ms", false, "deduplication window in ms"),
            ParamInfo::string("failure_policy", false, "ignore, log or disable-after-N"),
        ])
        .with_permissions(&["shell"]),
        ActionInfo::new("unsubscribe", "Remove subscriptions for `event`")
            .with_params([ParamInfo::string("event", true, "event name")]),
        ActionInfo::new("subscriptions", "List event subscriptions")
            .with_params([ParamInfo::string("event", false, "filter by event")]),
        ActionInfo::new(
            "configure",
            "Update `timeout_ms`, `dedup_ms`, `max_concurrent` or `failure_policy`",
        )
        .with_params([
            ParamInfo::integer("timeout_ms", false, "default hard timeout in ms"),
            ParamInfo::integer("dedup_ms", false, "default deduplication window"),
            ParamInfo::integer("max_concurrent", false, "simultaneous processes (1-64)"),
            ParamInfo::string("failure_policy", false, "ignore, log or disable-after-N"),
        ]),
        ActionInfo::new(
            "explain",
            "Show matching templates for `event` without running",
        )
        .with_params([
            ParamInfo::string("event", true, "event name"),
            ParamInfo::object("payload", false, "event payload for template expansion"),
        ]),
        ActionInfo::new("test", "Run or dry-run `event` subscriptions explicitly")
            .with_params([
                ParamInfo::string("event", false, "event name"),
                ParamInfo::string("command", false, "explicit command template to test"),
                ParamInfo::object("payload", false, "event payload"),
                ParamInfo::boolean("dry_run", false, "expand without executing"),
            ])
            .with_permissions(&["shell"]),
        ActionInfo::new("preset", "Show documented commands for this adapter"),
        ActionInfo::new(
            "apply-preset",
            "Store documented commands (does not enable by default)",
        )
        .with_params([ParamInfo::boolean("enable", false, "enable after applying")]),
    ]
}

/// Aggregate `adapters.*` namespace.
pub struct AdaptersContext {
    adapters: Adapters,
}

impl AdaptersContext {
    fn core(&self, params: &Value) -> Result<&Arc<AdapterCore>, String> {
        let namespace = param_str(params, "adapter")?;
        let namespace = NAMESPACES
            .iter()
            .find(|candidate| **candidate == namespace)
            .ok_or_else(|| format!("unknown adapter `{namespace}`"))?;
        self.adapters
            .inner
            .adapters
            .get(namespace)
            .ok_or_else(|| format!("unknown adapter `{namespace}`"))
    }
}

impl Context for AdaptersContext {
    fn namespace(&self) -> &'static str {
        "adapters"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("status", "Every adapter's config and runtime stats"),
            ActionInfo::new("events", "Event table and subscription counts"),
            ActionInfo::new(
                "explain",
                "Explain subscriptions for `event` (optional `adapter`)",
            )
            .with_params([
                ParamInfo::string("event", true, "event name"),
                ParamInfo::string("adapter", false, "limit to one adapter"),
                ParamInfo::object("payload", false, "event payload"),
            ]),
            ActionInfo::new("dispatch", "Run `adapter` command `name` explicitly")
                .with_params([
                    ParamInfo::string("adapter", true, "adapter namespace"),
                    ParamInfo::string("name", true, "command name"),
                ])
                .with_permissions(&["shell"]),
            ActionInfo::new("test", "Run or dry-run `adapter` for `event` or `command`")
                .with_params([
                    ParamInfo::string("adapter", true, "adapter namespace"),
                    ParamInfo::string("event", false, "event name"),
                    ParamInfo::string("command", false, "explicit command"),
                    ParamInfo::object("payload", false, "event payload"),
                    ParamInfo::boolean("dry_run", false, "expand without executing"),
                ])
                .with_permissions(&["shell"]),
            ActionInfo::new("enable", "Enable `adapter`, or every adapter when absent")
                .with_params([ParamInfo::string("adapter", false, "limit to one adapter")]),
            ActionInfo::new("disable", "Disable `adapter`, or every adapter when absent")
                .with_params([ParamInfo::string("adapter", false, "limit to one adapter")]),
            ActionInfo::new("subscribe", "Add a subscription to `adapter`")
                .with_params([
                    ParamInfo::string("adapter", true, "adapter namespace"),
                    ParamInfo::string("event", true, "event name"),
                    ParamInfo::string("command", true, "shell command template"),
                ])
                .with_permissions(&["shell"]),
            ActionInfo::new("unsubscribe", "Remove subscriptions from `adapter`").with_params([
                ParamInfo::string("adapter", true, "adapter namespace"),
                ParamInfo::string("event", true, "event name"),
            ]),
            ActionInfo::new("configure", "Update `adapter` execution limits").with_params([
                ParamInfo::string("adapter", true, "adapter namespace"),
                ParamInfo::integer("timeout_ms", false, "default timeout"),
                ParamInfo::integer("dedup_ms", false, "default dedup window"),
                ParamInfo::integer("max_concurrent", false, "simultaneous processes (1-64)"),
                ParamInfo::string("failure_policy", false, "ignore, log or disable-after-N"),
            ]),
            ActionInfo::new("preset", "Show documented commands for `adapter`")
                .with_params([ParamInfo::string("adapter", true, "adapter namespace")]),
            ActionInfo::new("apply-preset", "Store documented commands for `adapter`").with_params(
                [
                    ParamInfo::string("adapter", true, "adapter namespace"),
                    ParamInfo::boolean("enable", false, "enable after applying"),
                ],
            ),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "status" => {
                let mut statuses = BTreeMap::new();
                for (namespace, core) in &self.adapters.inner.adapters {
                    statuses.insert(*namespace, core.status()?);
                }
                Ok(json!({
                    "adapters": statuses,
                    "worker": self.adapters.inner.running.load(Ordering::Relaxed),
                    "dropped_events": self.adapters.inner.dropped.load(Ordering::Relaxed),
                }))
            }
            "events" => {
                let counts = self
                    .adapters
                    .inner
                    .adapters
                    .iter()
                    .map(|(namespace, core)| {
                        let state = core.lock_state()?;
                        Ok((
                            *namespace,
                            json!({
                                "subscriptions": state.subscriptions.keys().cloned().collect::<BTreeSet<_>>(),
                                "hooks": state.hooks.keys().cloned().collect::<BTreeSet<_>>(),
                            }),
                        ))
                    })
                    .collect::<Result<BTreeMap<_, _>, String>>()?;
                let table = EVENT_TABLE
                    .iter()
                    .map(|(event, description)| json!({ "event": event, "description": description }))
                    .collect::<Vec<_>>();
                Ok(json!({ "events": table, "subscribed": counts }))
            }
            "enable" | "disable" => {
                let enabled = action == "enable";
                let names: Vec<&'static str> = match params.get("adapter").and_then(Value::as_str) {
                    Some(_) => vec![self.core(params)?.namespace],
                    None => NAMESPACES.to_vec(),
                };
                let mut changed = Vec::new();
                for namespace in names {
                    if let Some(core) = self.adapters.inner.adapters.get(namespace) {
                        core.call(if enabled { "enable" } else { "disable" }, &Value::Null)?;
                        changed.push(namespace);
                    }
                }
                Ok(json!({ "enabled": enabled, "adapters": changed }))
            }
            "explain" => {
                let event = adapter_event_from_params(params)?;
                let mut adapters = BTreeMap::new();
                match params.get("adapter").and_then(Value::as_str) {
                    Some(_) => {
                        let core = self.core(params)?;
                        adapters.insert(core.namespace, explain_for(core, &event));
                    }
                    None => {
                        for (namespace, core) in &self.adapters.inner.adapters {
                            adapters.insert(*namespace, explain_for(core, &event));
                        }
                    }
                }
                Ok(json!({
                    "event": event.kind,
                    "payload": event.payload,
                    "adapters": adapters,
                }))
            }
            "dispatch" => {
                let core = self.core(params)?;
                core.dispatch(params)
            }
            "test" => {
                let core = self.core(params)?;
                core.test(params)
            }
            "subscribe" | "unsubscribe" | "configure" | "preset" | "apply-preset" => {
                let core = self.core(params)?;
                core.call(action, params)
            }
            other => Err(format!("unknown action `adapters.{other}`")),
        }
    }
}

/// Observes completed registry actions and enqueues companion adapter events.
pub struct AdapterObserver {
    adapters: Adapters,
}

impl CallObserver for AdapterObserver {
    fn after_call(
        &self,
        namespace: &str,
        action: &str,
        params: &Value,
        result: &Result<Value, String>,
    ) {
        if namespace == "adapters" || NAMESPACES.contains(&namespace) {
            return;
        }
        let Ok(value) = result else {
            return;
        };
        for event in companion_events(namespace, action, params, value) {
            self.adapters.submit(event);
        }
    }
}

/// Legacy helper: loads the four adapters from the default state directory.
pub fn default_adapters() -> std::io::Result<Vec<Adapter>> {
    Ok(Adapters::from_state_dir()?.adapters())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    use companion_core::{Registry, Request, Response};
    use niri_ipc::{Event, Window, WindowLayout, Workspace};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-adapters-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn harness(name: &str) -> (Adapters, PathBuf) {
        let dir = temp_dir(name);
        (Adapters::load(&dir).unwrap(), dir)
    }

    fn core<'a>(adapters: &'a Adapters, namespace: &str) -> &'a Arc<AdapterCore> {
        adapters.inner.adapters.get(namespace).unwrap()
    }

    fn try_call(
        adapters: &Adapters,
        namespace: &str,
        action: &str,
        params: Value,
    ) -> Result<Value, String> {
        if namespace == "adapters" {
            return adapters.context().call(action, &params);
        }
        core(adapters, namespace).call(action, &params)
    }

    fn call(adapters: &Adapters, namespace: &str, action: &str, params: Value) -> Value {
        try_call(adapters, namespace, action, params).unwrap()
    }

    fn enable(adapters: &Adapters, namespace: &str) {
        call(adapters, namespace, "enable", Value::Null);
    }

    fn subscribe(adapters: &Adapters, namespace: &str, event: &str, command: &str) {
        call(
            adapters,
            namespace,
            "subscribe",
            json!({ "event": event, "command": command }),
        );
    }

    fn recording(template: &str, path: &Path) -> String {
        template.replace("%REC%", &shell_quote(&path.display().to_string()))
    }

    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn event(kind: &str, payload: Value) -> AdapterEvent {
        AdapterEvent::new(kind, payload)
    }

    #[test]
    fn dispatch_requires_enable_and_runs_explicit_commands() {
        let (adapters, _dir) = harness("dispatch");
        let _ = call(
            &adapters,
            "audio",
            "set",
            json!({ "name": "greet", "command": "printf adapter-ok" }),
        );

        let disabled = core(&adapters, "audio").call("dispatch", &json!({ "name": "greet" }));
        assert!(disabled.is_err());

        enable(&adapters, "audio");
        let value = call(&adapters, "audio", "dispatch", json!({ "name": "greet" }));
        assert_eq!(value["success"], true);
        assert_eq!(value["stdout"], "adapter-ok");
    }

    #[test]
    fn commands_and_hooks_persist_and_remove() {
        let dir = temp_dir("persist");
        let adapters = Adapters::load(&dir).unwrap();
        call(
            &adapters,
            "monitor",
            "set",
            json!({ "name": "cycle", "command": "niri msg action focus-monitor-next" }),
        );
        call(
            &adapters,
            "monitor",
            "set-hook",
            json!({ "event": "workspace-activated", "command": "notify-send changed" }),
        );

        let reloaded = Adapters::load(&dir).unwrap();
        let status = call(&reloaded, "monitor", "status", Value::Null);
        assert_eq!(
            status["commands"]["cycle"],
            "niri msg action focus-monitor-next"
        );
        assert_eq!(
            status["hooks"]["workspace-activated"],
            "notify-send changed"
        );
        assert_eq!(status["timeout_ms"], DEFAULT_TIMEOUT_MS);
        assert_eq!(status["dedup_ms"], DEFAULT_DEDUP_MS);
        assert_eq!(status["failure_policy"], "log");

        call(&reloaded, "monitor", "remove", json!({ "name": "cycle" }));
        call(
            &reloaded,
            "monitor",
            "remove-hook",
            json!({ "event": "workspace-activated" }),
        );
        let status = call(&reloaded, "monitor", "status", Value::Null);
        assert!(status["commands"].as_object().unwrap().is_empty());
        assert!(status["hooks"].as_object().unwrap().is_empty());
    }

    #[test]
    fn default_adapters_have_distinct_namespaces() {
        let adapters = default_adapters().unwrap();
        let names: Vec<_> = adapters.iter().map(Context::namespace).collect();
        assert_eq!(names, ["session", "monitor", "audio", "wallpaper"]);
    }

    #[test]
    fn legacy_config_is_the_same_schema_with_new_defaults() {
        let dir = temp_dir("legacy");
        let path = dir.join("adapter-session.json");
        let command = recording("printf legacy >> %REC%", &dir.join("legacy.log"));
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "enabled": true,
                "commands": { "lock": "loginctl lock-session" },
                "hooks": { "workspace-activated": command }
            }))
            .unwrap(),
        )
        .unwrap();

        let adapters = Adapters::load(&dir).unwrap();
        let status = call(&adapters, "session", "status", Value::Null);
        assert_eq!(status["enabled"], true);
        assert_eq!(status["commands"]["lock"], "loginctl lock-session");
        assert_eq!(status["timeout_ms"], DEFAULT_TIMEOUT_MS);
        assert_eq!(status["max_concurrent"], DEFAULT_MAX_CONCURRENT);

        adapters.deliver(&event(
            "workspace-activated",
            json!({ "workspace_name": "dev" }),
        ));
        assert_eq!(lines(&dir.join("legacy.log")), ["legacy"]);
    }

    #[test]
    fn adapters_are_disabled_by_default_and_delivery_never_executes() {
        let (adapters, dir) = harness("disabled-default");
        let recorder = dir.join("disabled.log");
        subscribe(
            &adapters,
            "session",
            "workspace-added",
            &recording("printf ran >> %REC%", &recorder),
        );

        let report = adapters.deliver(&event(
            "workspace-added",
            json!({ "workspace_name": "dev" }),
        ));
        assert_eq!(report["adapters"]["session"]["enabled"], false);
        assert_eq!(report["adapters"]["session"]["executed"], 0);
        assert!(lines(&recorder).is_empty());

        let status = call(&adapters, "session", "status", Value::Null);
        assert_eq!(status["enabled"], false);
        assert_eq!(status["stats"]["skipped_disabled"], 1);
        assert_eq!(status["stats"]["executed"], 0);
    }

    #[test]
    fn subscriptions_only_run_for_matching_events() {
        let (adapters, dir) = harness("routing");
        let terminal = dir.join("terminal.log");
        let workspace = dir.join("workspace.log");
        enable(&adapters, "audio");
        enable(&adapters, "wallpaper");
        subscribe(
            &adapters,
            "audio",
            "window-focused",
            &recording("printf audio >> %REC%", &terminal),
        );
        subscribe(
            &adapters,
            "wallpaper",
            "workspace-activated",
            &recording("printf wallpaper >> %REC%", &workspace),
        );

        adapters.deliver(&event(
            "window-focused",
            json!({ "app_id": "firefox", "focused": true }),
        ));
        assert_eq!(lines(&terminal), ["audio"]);
        assert!(lines(&workspace).is_empty());

        adapters.deliver(&event(
            "workspace-activated",
            json!({ "workspace_name": "dev" }),
        ));
        assert_eq!(lines(&workspace), ["wallpaper"]);
        assert_eq!(lines(&terminal), ["audio"]);
    }

    #[test]
    fn templates_expand_quoted_payload_fields_and_environment() {
        let (adapters, dir) = harness("templates");
        let recorder = dir.join("template.log");
        enable(&adapters, "wallpaper");
        subscribe(
            &adapters,
            "wallpaper",
            "workspace-activated",
            &recording(
                "printf '%s\\n' \"$COMPANION_WORKSPACE_NAME|$COMPANION_WORKSPACE_INDEX|$COMPANION_OUTPUT|$COMPANION_FOCUSED|$COMPANION_PHASE|{{workspace_name}}|{{workspace_index}}|{{output}}|{{focused}}|{{phase}}|{{event}}|{{adapter}}\" >> %REC%",
                &recorder,
            ),
        );

        adapters.deliver(&event(
            "workspace-activated",
            json!({
                "workspace_name": "dev",
                "workspace_index": 2,
                "output": "eDP-1",
                "focused": true,
                "phase": "activated",
            }),
        ));
        assert_eq!(
            lines(&recorder),
            [
                "dev|2|eDP-1|true|activated|'dev'|'2'|'eDP-1'|'true'|'activated'|'workspace-activated'|'wallpaper'"
            ]
        );
    }

    #[test]
    fn template_values_cannot_break_out_of_shell_quoting() {
        let (adapters, dir) = harness("quoting");
        let recorder = dir.join("quoted.log");
        let pwned = dir.join("pwned");
        enable(&adapters, "audio");
        subscribe(
            &adapters,
            "audio",
            "window-focused",
            &recording("printf '%s\\n' {{title}} >> %REC%", &recorder),
        );

        let title = format!("x'; touch {}; echo '", pwned.display());
        adapters.deliver(&event(
            "window-focused",
            json!({ "app_id": "kitty", "title": title, "focused": true }),
        ));
        assert_eq!(
            lines(&recorder),
            [format!("x'; touch {}; echo '", pwned.display())]
        );
        assert!(!pwned.exists());
    }

    #[test]
    fn dedup_suppresses_the_same_event_payload_within_the_window() {
        let (adapters, dir) = harness("dedup");
        let recorder = dir.join("dedup.log");
        enable(&adapters, "audio");
        call(
            &adapters,
            "audio",
            "configure",
            json!({ "dedup_ms": 60_000 }),
        );
        subscribe(
            &adapters,
            "audio",
            "window-focused",
            &recording("printf '%s\\n' {{workspace_name}} >> %REC%", &recorder),
        );

        let focused = event(
            "window-focused",
            json!({ "workspace_name": "dev", "focused": true }),
        );
        adapters.deliver(&focused);
        adapters.deliver(&focused);
        assert_eq!(lines(&recorder), ["dev"]);

        adapters.deliver(&event(
            "window-focused",
            json!({ "workspace_name": "web", "focused": true }),
        ));
        assert_eq!(lines(&recorder), ["dev", "web"]);

        call(&adapters, "audio", "configure", json!({ "dedup_ms": 0 }));
        adapters.deliver(&focused);
        assert_eq!(lines(&recorder), ["dev", "web", "dev"]);
        let status = call(&adapters, "audio", "status", Value::Null);
        assert!(status["stats"]["skipped_dedup"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn disable_after_n_disables_the_adapter_and_status_explains_why() {
        let (adapters, _dir) = harness("disable-after");
        enable(&adapters, "session");
        call(
            &adapters,
            "session",
            "configure",
            json!({ "failure_policy": "disable-after-2" }),
        );
        subscribe(&adapters, "session", "workspace-added", "exit 3");

        adapters.deliver(&event(
            "workspace-added",
            json!({ "workspace_name": "one" }),
        ));
        assert_eq!(
            call(&adapters, "session", "status", Value::Null)["enabled"],
            true
        );

        adapters.deliver(&event(
            "workspace-added",
            json!({ "workspace_name": "two" }),
        ));
        let status = call(&adapters, "session", "status", Value::Null);
        assert_eq!(status["enabled"], false);
        assert_eq!(status["stats"]["failed"], 2);
        assert_eq!(status["stats"]["disabled_after_failure"], 1);

        adapters.deliver(&event(
            "workspace-added",
            json!({ "workspace_name": "three" }),
        ));
        let status = call(&adapters, "session", "status", Value::Null);
        assert!(status["stats"]["skipped_disabled"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn ignore_policy_keeps_the_adapter_enabled() {
        let (adapters, _dir) = harness("ignore");
        enable(&adapters, "monitor");
        call(
            &adapters,
            "monitor",
            "configure",
            json!({ "failure_policy": "ignore" }),
        );
        subscribe(&adapters, "monitor", "output-added", "exit 1");

        for output in ["eDP-1", "HDMI-A-1", "DP-1"] {
            adapters.deliver(&event("output-added", json!({ "output": output })));
        }
        let status = call(&adapters, "monitor", "status", Value::Null);
        assert_eq!(status["enabled"], true);
        assert_eq!(status["stats"]["failed"], 3);
        assert_eq!(status["stats"]["disabled_after_failure"], 0);
    }

    #[test]
    fn command_timeout_kills_the_process_and_is_reported() {
        let (adapters, _dir) = harness("timeout");
        enable(&adapters, "monitor");
        call(
            &adapters,
            "monitor",
            "subscribe",
            json!({
                "event": "output-added",
                "command": "sleep 2",
                "timeout_ms": 50
            }),
        );

        let report = adapters.deliver(&event("output-added", json!({ "output": "eDP-1" })));
        let result = &report["adapters"]["monitor"]["results"][0];
        assert_eq!(result["timed_out"], true);
        assert_eq!(result["success"], false);
        assert!(result["duration_ms"].as_u64().unwrap() < 1000);
    }

    #[test]
    fn concurrency_limit_is_honoured_for_one_event() {
        let (adapters, _dir) = harness("concurrency");
        enable(&adapters, "monitor");
        call(
            &adapters,
            "monitor",
            "configure",
            json!({ "max_concurrent": 1 }),
        );
        for command in ["sleep 0.15; true", "sleep 0.15; :"] {
            call(
                &adapters,
                "monitor",
                "subscribe",
                json!({ "event": "output-added", "command": command }),
            );
        }

        adapters.deliver(&event("output-added", json!({ "output": "eDP-1" })));
        let status = call(&adapters, "monitor", "status", Value::Null);
        assert_eq!(status["stats"]["peak_concurrency"], 1);
        assert_eq!(status["stats"]["executed"], 2);
    }

    #[test]
    fn explain_test_and_hub_dispatch_are_debuggable() {
        let (adapters, dir) = harness("explain");
        let recorder = dir.join("explain.log");
        subscribe(
            &adapters,
            "wallpaper",
            "workspace-activated",
            &recording("printf ran >> %REC%", &recorder),
        );

        let explained = call(
            &adapters,
            "adapters",
            "explain",
            json!({
                "event": "workspace-activated",
                "payload": { "workspace_name": "dev" }
            }),
        );
        assert_eq!(explained["adapters"]["wallpaper"]["enabled"], false);
        assert_eq!(
            explained["adapters"]["wallpaper"]["subscriptions"][0]["expanded"],
            recording("printf ran >> %REC%", &recorder)
        );
        assert!(lines(&recorder).is_empty());

        let dry = call(
            &adapters,
            "adapters",
            "test",
            json!({
                "adapter": "wallpaper",
                "event": "workspace-activated",
                "payload": { "workspace_name": "dev" },
                "dry_run": true
            }),
        );
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["enabled"], false);
        assert!(dry["results"][0]["expanded"].is_string());
        assert!(lines(&recorder).is_empty());

        // An explicit test is the documented way to execute while disabled.
        let ran = call(
            &adapters,
            "adapters",
            "test",
            json!({
                "adapter": "wallpaper",
                "event": "workspace-activated",
                "payload": { "workspace_name": "dev" }
            }),
        );
        assert_eq!(ran["enabled"], false);
        assert_eq!(ran["results"][0]["success"], true);
        assert_eq!(lines(&recorder), ["ran"]);
        assert_eq!(
            call(&adapters, "wallpaper", "status", Value::Null)["enabled"],
            false
        );

        let disabled = core(&adapters, "wallpaper").call("dispatch", &json!({ "name": "missing" }));
        assert!(disabled.is_err());
    }

    #[test]
    fn hub_dispatch_and_test_can_run_named_or_inline_commands() {
        let (adapters, dir) = harness("hub-dispatch");
        let recorder = dir.join("hub-dispatch.log");
        call(
            &adapters,
            "monitor",
            "set",
            json!({
                "name": "apply",
                "command": recording("printf '%s' {{output}} >> %REC%", &recorder)
            }),
        );

        let disabled = try_call(
            &adapters,
            "adapters",
            "dispatch",
            json!({
                "adapter": "monitor",
                "name": "apply",
                "event": "output-added",
                "payload": { "output": "eDP-1" }
            }),
        );
        assert!(disabled.unwrap_err().contains("disabled"));

        enable(&adapters, "monitor");
        let ran = call(
            &adapters,
            "adapters",
            "dispatch",
            json!({
                "adapter": "monitor",
                "name": "apply",
                "event": "output-added",
                "payload": { "output": "eDP-1" }
            }),
        );
        assert_eq!(ran["success"], true);
        assert_eq!(
            ran["expanded"],
            recording("printf '%s' 'eDP-1' >> %REC%", &recorder)
        );
        assert_eq!(lines(&recorder), ["eDP-1"]);

        let inline = call(
            &adapters,
            "adapters",
            "test",
            json!({
                "adapter": "monitor",
                "event": "output-added",
                "command": recording("printf 'inline' >> %REC%", &recorder),
                "payload": { "output": "HDMI-A-1" }
            }),
        );
        assert_eq!(inline["results"][0]["success"], true);
        assert_eq!(lines(&recorder), ["eDP-1inline"]);
    }

    #[test]
    fn hub_status_and_events_list_every_adapter_and_event() {
        let (adapters, _dir) = harness("hub-status");
        let status = call(&adapters, "adapters", "status", Value::Null);
        for namespace in NAMESPACES {
            assert_eq!(status["adapters"][namespace]["enabled"], false);
        }
        assert_eq!(status["worker"], false);

        let events = call(&adapters, "adapters", "events", Value::Null);
        assert_eq!(
            events["events"].as_array().unwrap().len(),
            EVENT_TABLE.len()
        );
        for kind in EVENT_KINDS {
            assert!(
                events["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["event"] == *kind),
                "{kind}"
            );
        }

        let enabled = call(&adapters, "adapters", "enable", Value::Null);
        assert_eq!(enabled["adapters"].as_array().unwrap().len(), 4);
        let status = call(&adapters, "adapters", "status", Value::Null);
        for namespace in NAMESPACES {
            assert_eq!(status["adapters"][namespace]["enabled"], true);
        }
        call(&adapters, "adapters", "disable", Value::Null);
    }

    #[test]
    fn presets_are_opt_in_and_do_not_enable_the_adapter() {
        let (adapters, _dir) = harness("preset");
        let preset = call(&adapters, "monitor", "preset", Value::Null);
        assert_eq!(preset["namespace"], "monitor");
        assert!(preset["commands"]["apply"].is_string());
        assert!(preset["subscriptions"]["output-added"].is_array());

        let applied = call(&adapters, "monitor", "apply-preset", json!({}));
        assert_eq!(applied["enabled"], false);
        assert!(applied["commands"]["apply"].is_string());
        assert!(
            applied["subscriptions"]["output-added"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["command"]
                    == "loom-monitor apply \"${LOOM_MONITOR_PROFILE:-default}\"")
        );
    }

    #[test]
    fn subscribe_validates_events_and_unsubscribe_filters() {
        let (adapters, _dir) = harness("unsubscribe");
        let error = core(&adapters, "audio").call(
            "subscribe",
            &json!({ "event": "not-an-event", "command": "true" }),
        );
        assert!(error.unwrap_err().contains("unknown event"));

        subscribe(&adapters, "audio", "window-focused", "true");
        subscribe(&adapters, "audio", "window_focused", "notify-send focus");
        subscribe(&adapters, "audio", "workspace-activated", "true");

        let removed = call(
            &adapters,
            "audio",
            "unsubscribe",
            json!({ "event": "window-focused", "command": "true" }),
        );
        assert_eq!(removed["removed"], 1);
        let listed = call(&adapters, "audio", "subscriptions", Value::Null);
        assert_eq!(
            listed["subscriptions"]["window-focused"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let removed = call(
            &adapters,
            "audio",
            "unsubscribe",
            json!({ "event": "workspace-activated" }),
        );
        assert_eq!(removed["removed"], 1);
    }

    #[test]
    fn hub_delivery_covers_hotplug_workspace_and_focus_from_niri_events() {
        let (adapters, dir) = harness("niri-flows");
        let monitor = dir.join("monitor.log");
        let audio = dir.join("audio.log");
        let wallpaper = dir.join("wallpaper.log");
        enable(&adapters, "monitor");
        enable(&adapters, "audio");
        enable(&adapters, "wallpaper");
        subscribe(
            &adapters,
            "monitor",
            "output-added",
            &recording("printf '%s' {{output}} >> %REC%", &monitor),
        );
        subscribe(
            &adapters,
            "audio",
            "window-focused",
            &recording("printf '%s' {{app_id}} >> %REC%", &audio),
        );
        subscribe(
            &adapters,
            "wallpaper",
            "workspace-activated",
            &recording("printf '%s' {{workspace_name}} >> %REC%", &wallpaper),
        );

        adapters.reset_niri();
        // First snapshot only seeds the mapper.
        adapters.niri_event(&Event::WorkspacesChanged {
            workspaces: vec![workspace(1, 1, "main", "eDP-1", true)],
        });
        assert!(lines(&monitor).is_empty());

        // A new output and workspace derive output-added + workspace-added.
        adapters.niri_event(&Event::WorkspacesChanged {
            workspaces: vec![
                workspace(1, 1, "main", "eDP-1", true),
                workspace(2, 2, "dev", "HDMI-A-1", false),
            ],
        });
        assert_eq!(lines(&monitor), ["HDMI-A-1"]);

        adapters.niri_event(&Event::WorkspaceActivated {
            id: 2,
            focused: true,
        });
        assert_eq!(lines(&wallpaper), ["dev"]);

        adapters.niri_event(&Event::WindowOpenedOrChanged {
            window: window(20, "firefox", "Mozilla", 2, true),
        });
        adapters.niri_event(&Event::WindowFocusChanged { id: Some(20) });
        assert_eq!(lines(&audio), ["firefox"]);
    }

    #[test]
    fn fake_niri_stream_reaches_adapter_subscriptions() {
        let (adapters, dir) = harness("niri-stream");
        let recorder = dir.join("stream.log");
        enable(&adapters, "audio");
        subscribe(
            &adapters,
            "audio",
            "window-focused",
            &recording(
                "printf '%s|%s' \"$COMPANION_APP_ID\" {{title}} >> %REC%",
                &recorder,
            ),
        );

        let socket_path = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let body = [
            Event::WorkspacesChanged {
                workspaces: vec![workspace(1, 1, "main", "eDP-1", true)],
            },
            Event::WindowsChanged { windows: vec![] },
            Event::WindowOpenedOrChanged {
                window: window(30, "kitty", "terminal", 1, false),
            },
            Event::WindowFocusChanged { id: Some(30) },
        ]
        .into_iter()
        .map(|event| serde_json::to_string(&event).unwrap())
        .collect::<Vec<_>>();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("EventStream"));
            stream.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
            for event in body {
                stream.write_all(event.as_bytes()).unwrap();
                stream.write_all(b"\n").unwrap();
            }
            stream.flush().unwrap();
        });

        let parking = companion_parking::Parking::load(dir.join("parking.json")).unwrap();
        let mut tracker = companion_niri::Tracker::new();
        let applied =
            companion_niri::track_stream_with(&socket_path, &parking, &mut tracker, |event| {
                adapters.niri_event(event)
            })
            .unwrap();
        server.join().unwrap();

        assert_eq!(applied, 4);
        assert_eq!(lines(&recorder), ["kitty|terminal"]);
    }

    #[test]
    fn registry_observer_turns_companion_actions_into_adapter_events() {
        struct FakeParker;

        impl Context for FakeParker {
            fn namespace(&self) -> &'static str {
                "parker"
            }

            fn actions(&self) -> Vec<ActionInfo> {
                vec![ActionInfo::new("park", "test")]
            }

            fn call(&self, action: &str, _params: &Value) -> Result<Value, String> {
                match action {
                    "park" => Ok(json!({ "window": 9, "parked": true })),
                    other => Err(format!("unknown action `parker.{other}`")),
                }
            }
        }

        let (adapters, dir) = harness("observer");
        let recorder = dir.join("observer.log");
        enable(&adapters, "session");
        subscribe(
            &adapters,
            "session",
            "park",
            &recording("printf '%s:%s' {{phase}} {{window_id}} >> %REC%", &recorder),
        );

        let mut registry = Registry::new();
        registry.register(FakeParker);
        registry.set_observer(adapters.observer());
        let response = registry.handle(Request::Call {
            namespace: "parker".into(),
            action: "park".into(),
            params: json!({ "window": 9 }),
            id: None,
        });
        assert!(matches!(response, Response::Value { .. }));
        assert_eq!(lines(&recorder), ["park:9"]);
    }

    #[test]
    fn adapter_status_keeps_old_fields() {
        let (adapters, _dir) = harness("status-compat");
        let status = call(&adapters, "audio", "status", Value::Null);
        assert_eq!(status["namespace"], "audio");
        assert_eq!(status["enabled"], false);
        assert!(status["commands"].is_object());
        assert!(status["hooks"].is_object());
        assert!(status["stats"].is_object());
    }

    fn workspace(id: u64, idx: u8, name: &str, output: &str, focused: bool) -> Workspace {
        Workspace {
            id,
            idx,
            name: Some(name.to_owned()),
            output: Some(output.to_owned()),
            is_urgent: false,
            is_active: focused,
            is_focused: focused,
            active_window_id: None,
            is_hidden: false,
        }
    }

    fn window(id: u64, app_id: &str, title: &str, workspace: u64, focused: bool) -> Window {
        Window {
            id,
            title: Some(title.to_owned()),
            app_id: Some(app_id.to_owned()),
            pid: Some(42),
            workspace_id: Some(workspace),
            is_focused: focused,
            is_floating: false,
            is_sticky: false,
            is_urgent: false,
            is_minimized: false,
            is_agent: false,
            content_type: None,
            tearing_allowed: false,
            layout: WindowLayout {
                pos_in_scrolling_layout: Some((1, 1)),
                tile_size: (800.0, 600.0),
                window_size: (800, 600),
                tile_pos_in_workspace_view: Some((0.0, 0.0)),
                window_offset_in_tile: (0.0, 0.0),
            },
            focus_timestamp: None,
        }
    }

    #[test]
    fn event_bus_receives_mapped_and_companion_events() {
        let (adapters, _dir) = harness("event-bus");
        let bus = companion_core::EventBus::new();
        let park = bus.subscribe(["park".to_owned()], 8);
        let focused = bus.subscribe(["window-focused".to_owned()], 8);
        let deliveries = bus.subscribe(["adapter-delivery".to_owned()], 8);
        adapters.set_event_bus(bus.clone());

        adapters.submit(AdapterEvent::new("park", json!({ "window": 4 })));
        adapters.niri_event(&Event::WindowFocusChanged { id: Some(9) });

        let park = park.recv().unwrap();
        assert_eq!(park.event, "park");
        assert_eq!(park.payload["window"], 4);
        let focused = focused.recv().unwrap();
        assert_eq!(focused.event, "window-focused");
        let delivery = deliveries.recv().unwrap();
        assert_eq!(delivery.event, "adapter-delivery");
        assert_eq!(delivery.payload["event"], "park");
        let delivery = deliveries.recv().unwrap();
        assert_eq!(delivery.payload["event"], "window-focused");
    }
}
