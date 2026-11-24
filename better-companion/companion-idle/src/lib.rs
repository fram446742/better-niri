//! Idle and suspend policy for `better-companion` (Phase D).
//!
//! `idle.*` is a bounded context with no display requirement. It owns a
//! configurable timeout list (`lock`, `dpms-off`, `suspend`, `command`),
//! inhibitors (audio, fullscreen, user-requested) and a pluggable activity
//! source. The daemon feeds activity from niri's event stream; tests and
//! embedders inject their own probe, executor and clock.
//!
//! Safety model:
//!
//! - the default policy is **disabled** with no timeouts,
//! - `idle.tick` is the only action that executes a timeout, and only when the policy is enabled,
//!   not in dry-run, and no inhibitor is active,
//! - every step is remembered by `(after_secs, action)`, so a repeated tick is idempotent until
//!   `idle.reset` clears the fired set,
//! - logind and DPMS calls go through [`IdleExecutor`], which tests replace with a recorder.
//!
//! `idle.explain` (and `idle.status`) never execute; `idle.explain` accepts an
//! optional `idle_secs` to evaluate a hypothetical idle duration. `idle.tick`
//! is what a timer or the user calls to run due steps.
//!
//! Configuration is JSON, flat or nested under `policy`:
//!
//! ```json
//! {
//!   "policy": {
//!     "enabled": true,
//!     "timeouts": [
//!       {"after_secs": 300, "action": "lock"},
//!       {"after_secs": 600, "action": "dpms-off"},
//!       {"after_secs": 1800, "action": "suspend"}
//!     ],
//!     "audio_inhibitor": true,
//!     "fullscreen_inhibitor": true,
//!     "logind_idle_hint": false
//!   }
//! }
//! ```
//!
//! Limitations:
//!
//! - the idle clock starts when the daemon starts or after `idle.reset`; it is not persisted across
//!   restarts,
//! - activity comes from niri focus/window/workspace events; there is no pointer/keyboard snooping
//!   outside the compositor,
//! - niri IPC exposes no `is_fullscreen` flag, so the fullscreen inhibitor treats a focused window
//!   covering an output as fullscreen,
//! - logind and the sensor/audio probes shell out to `busctl`, `loginctl`, `systemctl` and `pactl`
//!   because the cached D-Bus crate needs libdbus at build time; missing tools degrade to
//!   `unavailable`/fallback behavior.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

mod executor;
mod inhibit;
mod policy;
mod probe;

use companion_core::{ActionInfo, Context, load_json, save_json};
pub use executor::{HostExecutor, IdleExecutor};
pub use inhibit::{
    AudioInhibitor, FullscreenInhibitor, InhibitState, Inhibitor, covering_output, state_json,
};
pub use policy::{IdleAction, Policy, Timeout};
pub use probe::{
    ActivityClock, ActivityProbe, Now, SystemNow, SystemProbe, logind_idle_hint, logind_status,
    run_activity_stream, track_activity_stream,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Persisted state. Policy changes are explicit; the activity baseline is
/// process-local.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    policy: Policy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_inhibit: Option<UserInhibit>,
    #[serde(default)]
    dpms_off: bool,
    #[serde(default)]
    fired: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct UserInhibit {
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_millis: Option<u64>,
}

/// The `idle.*` context.
pub struct Idle {
    path: PathBuf,
    state: Mutex<State>,
    probe: Box<dyn ActivityProbe>,
    executor: Box<dyn IdleExecutor>,
    audio: Box<dyn Inhibitor>,
    fullscreen: Box<dyn Inhibitor>,
    now: Arc<dyn Now>,
}

impl Idle {
    /// Loads state and wires the host activity clock, logind executor and
    /// built-in inhibitors.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let now: Arc<dyn Now> = Arc::new(SystemNow);
        let clock = ActivityClock::with_now(now.clone());
        Self::with_parts(
            path,
            Box::new(SystemProbe::new(clock)),
            Box::new(HostExecutor::from_env()),
            Box::new(AudioInhibitor),
            Box::new(FullscreenInhibitor::from_env()),
            now,
        )
    }

    /// Loads state while sharing an activity clock with an event thread.
    pub fn with_activity(path: impl Into<PathBuf>, clock: ActivityClock) -> io::Result<Self> {
        let now = clock.now_handle();
        Self::with_parts(
            path,
            Box::new(SystemProbe::new(clock)),
            Box::new(HostExecutor::from_env()),
            Box::new(AudioInhibitor),
            Box::new(FullscreenInhibitor::from_env()),
            now,
        )
    }

    /// Loads the daemon's `idle.json`.
    pub fn from_state_dir() -> io::Result<Self> {
        Self::load(companion_core::state_dir().join("idle.json"))
    }

    /// Fully injectable constructor used by the daemon, tests and embedders.
    pub fn with_parts(
        path: impl Into<PathBuf>,
        probe: Box<dyn ActivityProbe>,
        executor: Box<dyn IdleExecutor>,
        audio: Box<dyn Inhibitor>,
        fullscreen: Box<dyn Inhibitor>,
        now: Arc<dyn Now>,
    ) -> io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
            probe,
            executor,
            audio,
            fullscreen,
            now,
        })
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state).map_err(|err| format!("cannot save idle state: {err}"))
    }

    fn user_state(state: &State, now_millis: u64) -> InhibitState {
        match &state.user_inhibit {
            None => InhibitState::Inactive,
            Some(inhibit)
                if inhibit
                    .expires_at_millis
                    .is_some_and(|expires| now_millis >= expires) =>
            {
                InhibitState::Inactive
            }
            Some(inhibit) => InhibitState::Active {
                detail: Some(inhibit.reason.clone()),
            },
        }
    }

    fn timeout_state(
        timeout: &Timeout,
        idle_millis: u64,
        inhibited: bool,
        fired: bool,
    ) -> &'static str {
        if fired {
            "fired"
        } else if timeout.after_secs.saturating_mul(1000) > idle_millis {
            "pending"
        } else if inhibited {
            "inhibited"
        } else {
            "due"
        }
    }

    /// Evaluates the policy. `execute` is only true from `idle.tick`.
    fn evaluate(&self, execute: bool, idle_override_secs: Option<u64>) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "idle state lock poisoned".to_owned())?;
        let idle_millis = idle_override_secs
            .map(|secs| secs.saturating_mul(1000))
            .unwrap_or_else(|| self.probe.idle_millis());
        let policy = state.policy.clone();
        let now_millis = self.now.now_millis();

        let user = Self::user_state(&state, now_millis);
        // A disabled policy should not spawn probes on every timer tick. Status
        // (`execute == false`) still reports the real inhibitor state.
        let check_inhibitors = !execute || policy.enabled;
        let audio = if check_inhibitors {
            self.audio.check(&policy)
        } else {
            InhibitState::Disabled
        };
        let fullscreen = if check_inhibitors {
            self.fullscreen.check(&policy)
        } else {
            InhibitState::Disabled
        };
        let inhibited = user.is_active() || audio.is_active() || fullscreen.is_active();

        let due: Vec<Timeout> = policy
            .timeouts
            .iter()
            .filter(|timeout| {
                timeout.after_secs.saturating_mul(1000) <= idle_millis
                    && !state.fired.contains(&timeout.key())
            })
            .cloned()
            .collect();

        let mut skipped: Option<&str> = None;
        if execute {
            if !policy.enabled {
                skipped = Some("disabled");
            } else if policy.dry_run {
                skipped = Some("dry-run");
            } else if inhibited {
                skipped = Some("inhibited");
            }
        }

        let mut executed = Vec::new();
        if execute && skipped.is_none() {
            for timeout in &due {
                let result = match timeout.action {
                    IdleAction::Lock => self.executor.lock(policy.lock_command.as_deref()),
                    IdleAction::Suspend => self.executor.suspend(policy.suspend_command.as_deref()),
                    IdleAction::DpmsOff => self.executor.dpms(false),
                    IdleAction::Command => timeout
                        .command
                        .as_deref()
                        .ok_or_else(|| "command timeout without a command".to_owned())
                        .and_then(|command| self.executor.shell(command)),
                };
                match result {
                    Ok(detail) => {
                        state.fired.insert(timeout.key());
                        if timeout.action == IdleAction::DpmsOff {
                            state.dpms_off = true;
                        }
                        executed.push(json!({
                            "after_secs": timeout.after_secs,
                            "action": timeout.action,
                            "ok": true,
                            "detail": detail,
                        }));
                    }
                    Err(error) => executed.push(json!({
                        "after_secs": timeout.after_secs,
                        "action": timeout.action,
                        "ok": false,
                        "error": error,
                    })),
                }
            }
        }

        // Resume monitors once the DPMS deadline is no longer reached. This is
        // the counterpart to `dpms-off`, not an extra implicit command.
        let dpms_due = policy.timeouts.iter().any(|timeout| {
            timeout.action == IdleAction::DpmsOff
                && timeout.after_secs.saturating_mul(1000) <= idle_millis
        });
        let mut powered_on = false;
        let mut power_on_error = None;
        let would_resume = state.dpms_off && !dpms_due;
        if would_resume && execute && skipped.is_none() {
            match self.executor.dpms(true) {
                Ok(detail) => {
                    state.dpms_off = false;
                    powered_on = true;
                    executed.push(json!({
                        "after_secs": 0,
                        "action": "dpms-on",
                        "ok": true,
                        "detail": detail,
                    }));
                }
                Err(error) => power_on_error = Some(error),
            }
        }
        if execute && skipped.is_none() {
            self.save(&state)?;
        }

        let timeout_values = policy
            .timeouts
            .iter()
            .map(|timeout| {
                let fired = state.fired.contains(&timeout.key());
                let status = Self::timeout_state(timeout, idle_millis, inhibited, fired);
                let mut value = serde_json::to_value(timeout)
                    .map_err(|err| format!("cannot encode timeout: {err}"))?;
                value["state"] = json!(status);
                value["would_run"] = json!(status == "due");
                Ok(value)
            })
            .collect::<Result<Vec<_>, String>>()?;

        let next = policy
            .timeouts
            .iter()
            .filter(|timeout| {
                timeout.after_secs.saturating_mul(1000) > idle_millis
                    && !state.fired.contains(&timeout.key())
            })
            .min_by_key(|timeout| timeout.after_secs)
            .map(|timeout| {
                json!({
                    "after_secs": timeout.after_secs,
                    "action": timeout.action,
                    "remaining_millis": timeout
                        .after_secs
                        .saturating_mul(1000)
                        .saturating_sub(idle_millis),
                })
            });

        let due_values = due
            .iter()
            .map(|timeout| {
                json!({
                    "after_secs": timeout.after_secs,
                    "action": timeout.action,
                    "command": timeout.command,
                })
            })
            .collect::<Vec<_>>();

        let mut limitations = vec![
            "idle clock starts at daemon start or on idle.reset and is not persisted",
            "activity is fed by niri focus/window/workspace events; without niri only idle.reset counts",
            "fullscreen detection is a focused-window-covering-output heuristic",
            "logind, pactl and DPMS tool absence degrades to unavailable/fallback",
        ];
        limitations.extend_from_slice(executor::executor_notes());

        Ok(json!({
            "idle_millis": idle_millis,
            "idle_secs": idle_millis / 1000,
            "source": self.probe.source(),
            "enabled": policy.enabled,
            "dry_run": policy.dry_run,
            "inhibited": inhibited,
            "inhibitors": {
                "user": state_json(&user),
                "audio": state_json(&audio),
                "fullscreen": state_json(&fullscreen),
            },
            "policy": policy,
            "timeouts": timeout_values,
            "due": due_values,
            "next": next,
            "fired": state.fired,
            "dpms_off": state.dpms_off,
            "executed": executed,
            "skipped": skipped,
            "would_resume_monitors": would_resume,
            "powered_on": powered_on,
            "power_on_error": power_on_error,
            "logind": if policy.logind_idle_hint {
                logind_status()
            } else {
                json!(null)
            },
            "limitations": limitations,
        }))
    }

    fn set_enabled(&self, enabled: bool) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "idle state lock poisoned".to_owned())?;
        let changed = state.policy.enabled != enabled;
        state.policy.enabled = enabled;
        if changed {
            if enabled {
                state.fired.clear();
            }
            self.save(&state)?;
        }
        Ok(json!({ "enabled": enabled, "changed": changed }))
    }

    fn configure(&self, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "idle state lock poisoned".to_owned())?;
        let patch = params.get("policy").unwrap_or(params);
        if !patch.is_object() {
            return Err("idle.configure needs an object (flat fields or `policy`)".to_owned());
        }
        let old = state.policy.clone();
        let mut policy = old.clone();

        patch_bool(patch, "enabled", &mut policy.enabled)?;
        patch_bool(patch, "dry_run", &mut policy.dry_run)?;
        patch_bool(patch, "audio_inhibitor", &mut policy.audio_inhibitor)?;
        patch_bool(
            patch,
            "fullscreen_inhibitor",
            &mut policy.fullscreen_inhibitor,
        )?;
        patch_bool(patch, "logind_idle_hint", &mut policy.logind_idle_hint)?;
        patch_optional_string(patch, "audio_command", &mut policy.audio_command)?;
        patch_optional_string(patch, "fullscreen_command", &mut policy.fullscreen_command)?;
        patch_optional_string(patch, "lock_command", &mut policy.lock_command)?;
        patch_optional_string(patch, "suspend_command", &mut policy.suspend_command)?;
        if let Some(value) = patch.get("timeouts") {
            policy.timeouts = if value.is_null() {
                Vec::new()
            } else {
                serde_json::from_value(value.clone())
                    .map_err(|err| format!("invalid `timeouts`: {err}"))?
            };
        }
        policy.normalize()?;

        let changed = policy != old;
        if changed && (policy.timeouts != old.timeouts || (policy.enabled && !old.enabled)) {
            state.fired.clear();
        }
        state.policy = policy;
        if changed {
            self.save(&state)?;
        }
        Ok(json!({
            "changed": changed,
            "policy": state.policy,
        }))
    }

    fn inhibit(&self, params: &Value) -> Result<Value, String> {
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .trim()
            .to_owned();
        if reason.is_empty() {
            return Err("`reason` must not be empty".to_owned());
        }
        if reason.len() > 256 {
            return Err("`reason` is longer than 256 bytes".to_owned());
        }
        let expires_at_millis = match params.get("duration_secs") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let secs = value
                    .as_u64()
                    .ok_or_else(|| "`duration_secs` must be a positive integer".to_owned())?;
                if secs == 0 {
                    return Err("`duration_secs` must be greater than zero".to_owned());
                }
                Some(
                    self.now
                        .now_millis()
                        .saturating_add(secs.saturating_mul(1000)),
                )
            }
        };

        let mut state = self
            .state
            .lock()
            .map_err(|_| "idle state lock poisoned".to_owned())?;
        let inhibit = UserInhibit {
            reason,
            expires_at_millis,
        };
        let changed = state.user_inhibit.as_ref() != Some(&inhibit);
        if changed {
            state.user_inhibit = Some(inhibit.clone());
            self.save(&state)?;
        }
        Ok(json!({
            "inhibited": true,
            "reason": inhibit.reason,
            "expires_at_millis": inhibit.expires_at_millis,
            "changed": changed,
        }))
    }

    fn uninhibit(&self) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "idle state lock poisoned".to_owned())?;
        let changed = state.user_inhibit.take().is_some();
        if changed {
            self.save(&state)?;
        }
        Ok(json!({ "inhibited": false, "changed": changed }))
    }

    fn reset(&self) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "idle state lock poisoned".to_owned())?;
        self.probe.reset();
        let fired_cleared = !state.fired.is_empty();
        state.fired.clear();
        let mut powered_on = false;
        let mut power_on_error = None;
        if state.dpms_off {
            match self.executor.dpms(true) {
                Ok(_) => {
                    state.dpms_off = false;
                    powered_on = true;
                }
                Err(error) => power_on_error = Some(error),
            }
        }
        self.save(&state)?;
        Ok(json!({
            "reset": true,
            "idle_millis": self.probe.idle_millis(),
            "fired_cleared": fired_cleared,
            "powered_on": powered_on,
            "power_on_error": power_on_error,
        }))
    }
}

fn patch_bool(patch: &Value, key: &str, field: &mut bool) -> Result<(), String> {
    if let Some(value) = patch.get(key) {
        *field = value
            .as_bool()
            .ok_or_else(|| format!("`{key}` must be a boolean"))?;
    }
    Ok(())
}

fn patch_optional_string(
    patch: &Value,
    key: &str,
    field: &mut Option<String>,
) -> Result<(), String> {
    if let Some(value) = patch.get(key) {
        *field = match value {
            Value::Null => None,
            Value::String(text) if text.trim().is_empty() => {
                return Err(format!(
                    "`{key}` must not be an empty string; use null to clear it"
                ));
            }
            Value::String(text) => Some(text.clone()),
            _ => return Err(format!("`{key}` must be a string or null")),
        };
    }
    Ok(())
}

impl Context for Idle {
    fn namespace(&self) -> &'static str {
        "idle"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new(
                "status",
                "Policy, idle duration, inhibitors and pending steps",
            ),
            ActionInfo::new(
                "explain",
                "Explain due steps without executing; optional `idle_secs`",
            ),
            ActionInfo::new("tick", "Execute due steps once (enabled, not dry-run)"),
            ActionInfo::new(
                "configure",
                "Update policy fields or replace `timeouts` (idempotent)",
            ),
            ActionInfo::new("enable", "Enable the idle policy"),
            ActionInfo::new("disable", "Disable the idle policy"),
            ActionInfo::new(
                "inhibit",
                "Block timeouts (`reason`, optional `duration_secs`)",
            ),
            ActionInfo::new("uninhibit", "Clear the user-requested inhibitor"),
            ActionInfo::new(
                "reset",
                "Mark activity now, clear fired steps and power monitors back on",
            ),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "status" => self.evaluate(false, None),
            "explain" => {
                let idle_secs = match params.get("idle_secs") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        value
                            .as_u64()
                            .ok_or_else(|| "`idle_secs` must be an integer".to_owned())?,
                    ),
                };
                self.evaluate(false, idle_secs)
            }
            "tick" => self.evaluate(true, None),
            "configure" => self.configure(params),
            "enable" => self.set_enabled(true),
            "disable" => self.set_enabled(false),
            "inhibit" => self.inhibit(params),
            "uninhibit" => self.uninhibit(),
            "reset" => self.reset(),
            other => Err(format!("unknown action `idle.{other}`")),
        }
    }
}

/// Path used by the daemon for idle state.
pub fn state_path() -> PathBuf {
    companion_core::state_dir().join("idle.json")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    struct FakeNow(AtomicU64);

    impl Now for FakeNow {
        fn now_millis(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct FakeProbe(Arc<AtomicU64>);

    impl ActivityProbe for FakeProbe {
        fn idle_millis(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }

        fn source(&self) -> String {
            "fake".to_owned()
        }

        fn reset(&self) {
            self.0.store(0, Ordering::SeqCst);
        }
    }

    struct FakeExecutor {
        calls: Arc<Mutex<Vec<String>>>,
        failures: BTreeMap<&'static str, String>,
    }

    impl FakeExecutor {
        fn record(&self, call: String) -> Result<String, String> {
            if let Some(error) = self.failures.get(call.split(':').next().unwrap_or(&call)) {
                return Err(error.clone());
            }
            self.calls.lock().unwrap().push(call.clone());
            Ok(call)
        }
    }

    impl IdleExecutor for FakeExecutor {
        fn lock(&self, command: Option<&str>) -> Result<String, String> {
            self.record(match command {
                Some(command) => format!("lock-command:{command}"),
                None => "lock".to_owned(),
            })
        }

        fn suspend(&self, command: Option<&str>) -> Result<String, String> {
            self.record(match command {
                Some(command) => format!("suspend-command:{command}"),
                None => "suspend".to_owned(),
            })
        }

        fn dpms(&self, on: bool) -> Result<String, String> {
            self.record(if on { "dpms-on" } else { "dpms-off" }.to_owned())
        }

        fn shell(&self, command: &str) -> Result<String, String> {
            self.record(format!("shell:{command}"))
        }
    }

    struct Fixed(InhibitState);

    impl Inhibitor for Fixed {
        fn check(&self, _policy: &Policy) -> InhibitState {
            self.0.clone()
        }
    }

    struct Harness {
        idle: Idle,
        idle_millis: Arc<AtomicU64>,
        calls: Arc<Mutex<Vec<String>>>,
        now: Arc<FakeNow>,
    }

    fn harness(name: &str, policy: Policy) -> Harness {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-idle-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("idle.json");
        harness_at(path, policy)
    }

    fn harness_at(path: PathBuf, policy: Policy) -> Harness {
        let idle_millis = Arc::new(AtomicU64::new(0));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let now = Arc::new(FakeNow(AtomicU64::new(1_000)));
        let idle = Idle::with_parts(
            &path,
            Box::new(FakeProbe(idle_millis.clone())),
            Box::new(FakeExecutor {
                calls: calls.clone(),
                failures: BTreeMap::new(),
            }),
            Box::new(Fixed(InhibitState::Disabled)),
            Box::new(Fixed(InhibitState::Disabled)),
            now.clone(),
        )
        .unwrap();
        idle.configure(&serde_json::to_value(&policy).unwrap())
            .unwrap();
        Harness {
            idle,
            idle_millis,
            calls,
            now,
        }
    }

    fn timeout(after_secs: u64, action: IdleAction) -> Timeout {
        Timeout {
            after_secs,
            action,
            command: None,
        }
    }

    fn enabled_policy(timeouts: Vec<Timeout>) -> Policy {
        Policy {
            enabled: true,
            timeouts,
            ..Policy::default()
        }
    }

    fn calls(harness: &Harness) -> Vec<String> {
        harness.calls.lock().unwrap().clone()
    }

    #[test]
    fn explain_reports_due_steps_without_executing() {
        let harness = harness(
            "explain",
            enabled_policy(vec![
                timeout(300, IdleAction::Lock),
                timeout(600, IdleAction::DpmsOff),
            ]),
        );
        harness.idle_millis.store(700_000, Ordering::SeqCst);

        let value = harness.idle.call("explain", &json!({})).unwrap();
        assert_eq!(value["due"].as_array().unwrap().len(), 2);
        assert_eq!(value["due"][0]["action"], "lock");
        assert_eq!(value["due"][1]["action"], "dpms-off");
        assert_eq!(value["executed"].as_array().unwrap().len(), 0);
        assert!(calls(&harness).is_empty());
        assert_eq!(value["dpms_off"], false);

        // A hypothetical idle time works without moving the clock.
        let value = harness
            .idle
            .call("explain", &json!({ "idle_secs": 100 }))
            .unwrap();
        assert!(value["due"].as_array().unwrap().is_empty());
        assert_eq!(value["next"]["after_secs"], 300);
    }

    #[test]
    fn tick_executes_timeouts_once_and_is_idempotent() {
        let harness = harness(
            "tick",
            enabled_policy(vec![
                timeout(300, IdleAction::Lock),
                timeout(600, IdleAction::DpmsOff),
            ]),
        );
        harness.idle_millis.store(700_000, Ordering::SeqCst);

        let value = harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(calls(&harness), ["lock", "dpms-off"]);
        assert_eq!(value["dpms_off"], true);
        assert_eq!(value["executed"].as_array().unwrap().len(), 2);

        // Second tick: every due step is already in the fired set.
        let value = harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(calls(&harness), ["lock", "dpms-off"]);
        assert!(value["executed"].as_array().unwrap().is_empty());
        assert_eq!(value["dpms_off"], true);
    }

    #[test]
    fn dry_run_never_executes() {
        let mut policy = enabled_policy(vec![timeout(1, IdleAction::Lock)]);
        policy.dry_run = true;
        let harness = harness("dry-run", policy);
        harness.idle_millis.store(5_000, Ordering::SeqCst);

        let value = harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(value["skipped"], "dry-run");
        assert!(calls(&harness).is_empty());
        assert_eq!(value["due"].as_array().unwrap().len(), 1);
        assert_eq!(value["timeouts"][0]["state"], "due");
    }

    #[test]
    fn active_inhibitor_blocks_and_is_visible_in_status() {
        let mut harness = harness(
            "inhibit",
            enabled_policy(vec![timeout(1, IdleAction::Lock)]),
        );
        harness.idle.audio = Box::new(Fixed(InhibitState::Active {
            detail: Some("music".into()),
        }));
        harness.idle_millis.store(5_000, Ordering::SeqCst);

        let value = harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(value["skipped"], "inhibited");
        assert_eq!(value["inhibitors"]["audio"]["state"], "active");
        assert!(calls(&harness).is_empty());

        let status = harness.idle.call("status", &Value::Null).unwrap();
        assert_eq!(status["timeouts"][0]["state"], "inhibited");
    }

    #[test]
    fn unavailable_inhibitor_does_not_block() {
        let mut harness = harness(
            "unavailable",
            enabled_policy(vec![timeout(1, IdleAction::Lock)]),
        );
        harness.idle.audio = Box::new(Fixed(InhibitState::Unavailable {
            detail: "no pactl".into(),
        }));
        harness.idle_millis.store(5_000, Ordering::SeqCst);

        let value = harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(value["skipped"], Value::Null);
        assert_eq!(calls(&harness), ["lock"]);
    }

    #[test]
    fn user_inhibit_blocks_until_uninhibited_and_expires() {
        let harness = harness(
            "user-inhibit",
            enabled_policy(vec![timeout(1, IdleAction::Lock)]),
        );
        harness.idle_millis.store(5_000, Ordering::SeqCst);

        let value = harness
            .idle
            .call("inhibit", &json!({ "reason": "meeting" }))
            .unwrap();
        assert_eq!(value["changed"], true);
        assert_eq!(
            harness.idle.call("tick", &Value::Null).unwrap()["skipped"],
            "inhibited"
        );
        assert!(calls(&harness).is_empty());

        let value = harness.idle.call("uninhibit", &Value::Null).unwrap();
        assert_eq!(value["changed"], true);
        harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(calls(&harness), ["lock"]);
        assert_eq!(
            harness.idle.call("uninhibit", &Value::Null).unwrap()["changed"],
            false
        );

        // Duration-limited inhibitor expires against the injected clock.
        harness.now.0.store(10_000, Ordering::SeqCst);
        harness
            .idle
            .call("inhibit", &json!({ "reason": "call", "duration_secs": 60 }))
            .unwrap();
        assert_eq!(
            harness.idle.call("tick", &Value::Null).unwrap()["skipped"],
            "inhibited"
        );
        harness.now.0.store(200_000, Ordering::SeqCst);
        harness.idle.call("reset", &Value::Null).unwrap();
        harness.idle_millis.store(5_000, Ordering::SeqCst);
        // Reset cleared the fired set; the expired inhibitor no longer blocks.
        harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(calls(&harness), ["lock", "lock"]);
    }

    #[test]
    fn reset_clears_fired_steps_and_powers_monitors_back_on() {
        let harness = harness(
            "reset",
            enabled_policy(vec![timeout(1, IdleAction::DpmsOff)]),
        );
        harness.idle_millis.store(5_000, Ordering::SeqCst);
        harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(calls(&harness), ["dpms-off"]);

        let value = harness.idle.call("reset", &Value::Null).unwrap();
        assert_eq!(value["fired_cleared"], true);
        assert_eq!(value["powered_on"], true);
        assert_eq!(calls(&harness), ["dpms-off", "dpms-on"]);
        assert_eq!(value["idle_millis"], 0);

        let value = harness.idle.call("reset", &Value::Null).unwrap();
        assert_eq!(value["fired_cleared"], false);
        assert_eq!(value["powered_on"], false);
        assert_eq!(calls(&harness), ["dpms-off", "dpms-on"]);
    }

    #[test]
    fn failed_action_stays_pending_and_is_retried() {
        let mut harness = harness(
            "retry",
            enabled_policy(vec![timeout(1, IdleAction::DpmsOff)]),
        );
        harness.idle.executor = Box::new(FakeExecutor {
            calls: harness.calls.clone(),
            failures: BTreeMap::from([("dpms-off", "no niri socket".to_owned())]),
        });
        harness.idle_millis.store(5_000, Ordering::SeqCst);

        let value = harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(value["executed"][0]["ok"], false);
        assert_eq!(value["dpms_off"], false);
        assert!(value["fired"].as_array().unwrap().is_empty());

        harness.idle.executor = Box::new(FakeExecutor {
            calls: harness.calls.clone(),
            failures: BTreeMap::new(),
        });
        harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(calls(&harness), ["dpms-off"]);
    }

    #[test]
    fn configure_is_idempotent_persists_and_validates() {
        let path = std::env::temp_dir()
            .join(format!(
                "better-companion-idle-config-{}",
                std::process::id()
            ))
            .join("idle.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let harness = harness_at(path.clone(), Policy::default());
        let patch = json!({
            "enabled": true,
            "audio_inhibitor": false,
            "fullscreen_inhibitor": false,
            "timeouts": [
                {"after_secs": 900, "action": "suspend"},
                {"after_secs": 300, "action": "lock"}
            ]
        });
        let first = harness.idle.call("configure", &patch).unwrap();
        assert_eq!(first["changed"], true);
        assert_eq!(first["policy"]["timeouts"][0]["after_secs"], 300);
        let second = harness.idle.call("configure", &patch).unwrap();
        assert_eq!(second["changed"], false);

        // Reload from disk with fresh (inert) components.
        let idle_millis = Arc::new(AtomicU64::new(0));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let now = Arc::new(FakeNow(AtomicU64::new(1)));
        let reloaded = Idle::with_parts(
            &path,
            Box::new(FakeProbe(idle_millis)),
            Box::new(FakeExecutor {
                calls,
                failures: BTreeMap::new(),
            }),
            Box::new(Fixed(InhibitState::Disabled)),
            Box::new(Fixed(InhibitState::Disabled)),
            now,
        )
        .unwrap();
        let value = reloaded
            .call("explain", &json!({ "idle_secs": 1000 }))
            .unwrap();
        assert_eq!(value["policy"]["enabled"], true);
        assert_eq!(value["due"].as_array().unwrap().len(), 2);
        assert_eq!(value["due"][0]["action"], "lock");

        let invalid = reloaded.call(
            "configure",
            &json!({ "timeouts": [{"after_secs": 30, "action": "command"}] }),
        );
        assert!(invalid.is_err());

        let duplicate = reloaded.call(
            "configure",
            &json!({
                "timeouts": [
                    {"after_secs": 30, "action": "lock"},
                    {"after_secs": 30, "action": "lock"}
                ]
            }),
        );
        assert!(duplicate.is_err());
    }

    #[test]
    fn command_timeout_runs_only_its_configured_command() {
        let harness = harness("command", Policy::default());
        harness
            .idle
            .call(
                "configure",
                &json!({
                    "enabled": true,
                    "timeouts": [
                        {"after_secs": 1, "action": "command", "command": "notify-send idle"}
                    ]
                }),
            )
            .unwrap();
        harness.idle_millis.store(2_000, Ordering::SeqCst);
        harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(calls(&harness), ["shell:notify-send idle"]);
    }

    #[test]
    fn enable_and_disable_are_idempotent() {
        let harness = harness("toggle", Policy::default());
        let value = harness.idle.call("disable", &Value::Null).unwrap();
        assert_eq!(value["changed"], false);
        let value = harness.idle.call("enable", &Value::Null).unwrap();
        assert_eq!(value["changed"], true);
        let value = harness.idle.call("enable", &Value::Null).unwrap();
        assert_eq!(value["changed"], false);
        let value = harness.idle.call("disable", &Value::Null).unwrap();
        assert_eq!(value["changed"], true);
    }

    #[test]
    fn status_is_deterministic_and_side_effect_free() {
        let harness = harness(
            "status-deterministic",
            enabled_policy(vec![timeout(1, IdleAction::Lock)]),
        );
        harness.idle_millis.store(5_000, Ordering::SeqCst);
        let first = harness.idle.call("status", &Value::Null).unwrap();
        let second = harness.idle.call("status", &Value::Null).unwrap();
        assert_eq!(first, second);
        assert!(calls(&harness).is_empty());
    }

    #[test]
    fn disabled_policy_reports_due_steps_but_does_not_run_them() {
        let harness = harness(
            "disabled",
            Policy {
                timeouts: vec![timeout(1, IdleAction::Lock)],
                ..Policy::default()
            },
        );
        harness.idle_millis.store(5_000, Ordering::SeqCst);
        let value = harness.idle.call("tick", &Value::Null).unwrap();
        assert_eq!(value["skipped"], "disabled");
        assert_eq!(value["due"].as_array().unwrap().len(), 1);
        assert!(calls(&harness).is_empty());
    }
}
