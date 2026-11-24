//! Accelerometer-driven output rotation for `better-companion` (Phase D).
//!
//! `rotate.*` reads an orientation from a pluggable source, debounces it with
//! hysteresis and maps it to niri output configuration. It is a companion
//! module, not a separate service: it shares the daemon socket, state directory
//! and IPC conventions.
//!
//! - `rotate.status` / `rotate.explain` are read-only.
//! - `rotate.tick` is the auto-apply entry point; it requires an enabled, unlocked policy, a target
//!   output and a stable orientation.
//! - `rotate.set` applies an explicit orientation or transform and bypasses `enabled`/`lock` (but
//!   not `dry_run`).
//!
//! Default mapping (counter-clockwise, matching niri's `Transform`):
//!
//! | orientation | transform |
//! | --- | --- |
//! | `normal` | `normal` |
//! | `left-up` | `90` |
//! | `bottom-up` | `180` |
//! | `right-up` | `270` |
//! | `left-down` | `flipped-90` |
//! | `right-down` | `flipped-270` |
//!
//! Sensors come from `sensor_command` when configured, then from
//! `iio-sensor-proxy` over D-Bus using `busctl`. The shell-out avoids a libdbus
//! build dependency; a missing proxy or tool is reported as
//! `source-unavailable`, never as a hard failure of the daemon. Hysteresis and
//! the last applied transform are process/state-local; the last transform is
//! persisted so status survives a daemon restart, but the debounce timer is
//! not.
//!
//! ```json
//! {
//!   "enabled": true,
//!   "output": "eDP-1",
//!   "hysteresis_ms": 1000,
//!   "position": [0, 0],
//!   "sensor_command": "monitor-sensor --accel"
//! }
//! ```

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

mod orientation;
mod outputs;
mod policy;
mod source;

use companion_core::{ActionInfo, Context, load_json, save_json};
use niri_ipc::Transform;
pub use orientation::Orientation;
pub use outputs::{NiriOutputs, OutputController, transform_name};
pub use policy::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use source::{AutoSource, OrientationSource, parse_busctl_string};

/// Current wall-clock time in milliseconds.
pub trait Now: Send + Sync {
    fn now_millis(&self) -> u64;
}

/// System clock used by the daemon.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemNow;

impl Now for SystemNow {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    policy: Policy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_orientation: Option<Orientation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_transform: Option<Transform>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Candidate {
    orientation: Orientation,
    since_millis: u64,
}

/// The `rotate.*` context.
pub struct Rotate {
    path: PathBuf,
    state: Mutex<State>,
    candidate: Mutex<Option<Candidate>>,
    source: Box<dyn OrientationSource>,
    outputs: Box<dyn OutputController>,
    now: Arc<dyn Now>,
}

impl Rotate {
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::with_parts(
            path,
            Box::new(AutoSource),
            Box::new(NiriOutputs::from_env()),
            Arc::new(SystemNow),
        )
    }

    pub fn from_state_dir() -> io::Result<Self> {
        Self::load(companion_core::state_dir().join("rotate.json"))
    }

    /// Fully injectable constructor used by tests and embedders.
    pub fn with_parts(
        path: impl Into<PathBuf>,
        source: Box<dyn OrientationSource>,
        outputs: Box<dyn OutputController>,
        now: Arc<dyn Now>,
    ) -> io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(state),
            candidate: Mutex::new(None),
            source,
            outputs,
            now,
        })
    }

    fn save(&self, state: &State) -> Result<(), String> {
        save_json(&self.path, state).map_err(|err| format!("cannot save rotate state: {err}"))
    }

    fn candidate_json(&self, now_millis: u64) -> Result<Value, String> {
        let candidate = self
            .candidate
            .lock()
            .map_err(|_| "rotate candidate lock poisoned".to_owned())?;
        Ok(match *candidate {
            None => Value::Null,
            Some(candidate) => json!({
                "orientation": candidate.orientation,
                "since_millis": candidate.since_millis,
                "elapsed_millis": now_millis.saturating_sub(candidate.since_millis),
            }),
        })
    }

    /// Debounce with mutation. Returns whether the orientation may be applied.
    fn observe_candidate(
        &self,
        orientation: Orientation,
        now_millis: u64,
        hysteresis_ms: u64,
    ) -> Result<bool, String> {
        let mut candidate = self
            .candidate
            .lock()
            .map_err(|_| "rotate candidate lock poisoned".to_owned())?;
        let ready = match *candidate {
            Some(current) if current.orientation == orientation => {
                now_millis.saturating_sub(current.since_millis) >= hysteresis_ms
            }
            _ => {
                *candidate = Some(Candidate {
                    orientation,
                    since_millis: now_millis,
                });
                hysteresis_ms == 0
            }
        };
        Ok(ready)
    }

    /// Debounce without mutation, for status/explain.
    fn candidate_ready(
        &self,
        orientation: Orientation,
        now_millis: u64,
        hysteresis_ms: u64,
    ) -> Result<bool, String> {
        let candidate = self
            .candidate
            .lock()
            .map_err(|_| "rotate candidate lock poisoned".to_owned())?;
        Ok(match *candidate {
            Some(current) if current.orientation == orientation => {
                now_millis.saturating_sub(current.since_millis) >= hysteresis_ms
            }
            _ => hysteresis_ms == 0,
        })
    }

    fn current_output(&self, output: Option<&str>) -> Value {
        let Some(output) = output else {
            return Value::Null;
        };
        match self.outputs.current_transform(output) {
            Ok(Some(transform)) => json!({
                "output": output,
                "transform": transform_name(transform),
            }),
            Ok(None) => json!({
                "output": output,
                "transform": null,
                "detail": "output is not mapped to a logical output",
            }),
            Err(error) => json!({
                "output": output,
                "error": error,
            }),
        }
    }

    fn snapshot(&self, execute: bool, include_current: bool) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "rotate state lock poisoned".to_owned())?;
        let policy = state.policy.clone();
        let now_millis = self.now.now_millis();

        let mut value = json!({
            "enabled": policy.enabled,
            "dry_run": policy.dry_run,
            "locked": policy.lock,
            "output": policy.output,
            "hysteresis_ms": policy.hysteresis_ms,
            "position": policy.position,
            "policy": policy,
            "source": self.source.describe(&policy),
            "last_orientation": state.last_orientation,
            "last_transform": state.last_transform.map(transform_name),
            "current": if include_current {
                self.current_output(policy.output.as_deref())
            } else {
                Value::Null
            },
            "orientation": null,
            "transform": null,
            "candidate": self.candidate_json(now_millis)?,
            "applied": false,
            "would_apply": false,
            "skipped": null,
            "error": null,
            "calls": [],
            "limitations": [
                "orientation mapping is fixed; rotate.set accepts a raw niri transform",
                "hysteresis state is process-local and restarts with the daemon",
                "iio-sensor-proxy is read through busctl; missing tools report source-unavailable",
                "auto-apply needs enabled, an unlocked policy and a configured output",
            ],
        });

        // The daemon ticker can call this every second; a disabled or locked
        // policy must not read the sensor or run a command.
        if execute && !policy.enabled {
            value["skipped"] = json!("disabled");
            return Ok(value);
        }
        if execute && policy.lock {
            value["skipped"] = json!("locked");
            return Ok(value);
        }

        let orientation = match self.source.read(&policy) {
            Ok(orientation) => orientation,
            Err(error) => {
                value["skipped"] = json!("source-unavailable");
                value["error"] = json!(error);
                return Ok(value);
            }
        };
        let transform = orientation.transform();
        value["orientation"] = json!(orientation);
        value["transform"] = json!(transform_name(transform));

        let Some(output) = policy.output.clone() else {
            value["skipped"] = json!("no-output");
            return Ok(value);
        };

        if state.last_orientation == Some(orientation) || state.last_transform == Some(transform) {
            if execute {
                *self
                    .candidate
                    .lock()
                    .map_err(|_| "rotate candidate lock poisoned".to_owned())? = None;
            }
            value["candidate"] = self.candidate_json(now_millis)?;
            value["skipped"] = json!("no-change");
            return Ok(value);
        }

        let can_auto = policy.enabled && !policy.lock && !policy.dry_run;
        let ready = if execute && can_auto {
            self.observe_candidate(orientation, now_millis, policy.hysteresis_ms)?
        } else {
            self.candidate_ready(orientation, now_millis, policy.hysteresis_ms)?
        };
        value["candidate"] = self.candidate_json(now_millis)?;
        value["would_apply"] = json!(policy.enabled && !policy.lock && ready);

        if !execute {
            value["skipped"] = json!(if !policy.enabled {
                Some("disabled")
            } else if policy.lock {
                Some("locked")
            } else if !ready {
                Some("debouncing")
            } else if policy.dry_run {
                Some("dry-run")
            } else {
                None
            });
            return Ok(value);
        }

        if policy.dry_run {
            value["skipped"] = json!("dry-run");
            return Ok(value);
        }
        if !ready {
            value["skipped"] = json!("debouncing");
            return Ok(value);
        }

        match self.outputs.apply(&output, transform, policy.position) {
            Ok(calls) => {
                state.last_orientation = Some(orientation);
                state.last_transform = Some(transform);
                *self
                    .candidate
                    .lock()
                    .map_err(|_| "rotate candidate lock poisoned".to_owned())? = None;
                self.save(&state)?;
                value["candidate"] = Value::Null;
                value["applied"] = json!(true);
                value["calls"] = json!(calls);
            }
            Err(error) => {
                value["skipped"] = json!("apply-failed");
                value["error"] = json!(error);
            }
        }
        Ok(value)
    }

    fn set(&self, params: &Value) -> Result<Value, String> {
        let orientation_param = params.get("orientation");
        let transform_param = params.get("transform");
        let (orientation, transform) = match (orientation_param, transform_param) {
            (Some(_), Some(_)) => {
                return Err("rotate.set takes `orientation` or `transform`, not both".to_owned());
            }
            (None, None) => {
                return Err("rotate.set needs `orientation` or `transform`".to_owned());
            }
            (Some(value), None) => {
                let orientation = Orientation::parse(
                    value
                        .as_str()
                        .ok_or_else(|| "`orientation` must be a string".to_owned())?,
                )?;
                (Some(orientation), orientation.transform())
            }
            (None, Some(value)) => {
                let text = value
                    .as_str()
                    .ok_or_else(|| "`transform` must be a string".to_owned())?;
                let transform = text
                    .parse::<Transform>()
                    .or_else(|_| Orientation::parse(text).map(Orientation::transform))
                    .map_err(|err| format!("invalid transform: {err}"))?;
                (Orientation::from_transform(transform), transform)
            }
        };

        let mut state = self
            .state
            .lock()
            .map_err(|_| "rotate state lock poisoned".to_owned())?;
        if state.policy.dry_run {
            return Ok(json!({
                "applied": false,
                "dry_run": true,
                "would_apply": true,
                "orientation": orientation,
                "transform": transform_name(transform),
                "output": state.policy.output,
            }));
        }
        let output = match params.get("output").and_then(Value::as_str) {
            Some(output) if !output.trim().is_empty() => output.to_owned(),
            Some(_) => return Err("`output` must not be empty".to_owned()),
            None => state
                .policy
                .output
                .clone()
                .ok_or_else(|| "rotate.set needs `output` or a configured output".to_owned())?,
        };
        let calls = self
            .outputs
            .apply(&output, transform, state.policy.position)?;
        state.last_orientation = orientation;
        state.last_transform = Some(transform);
        *self
            .candidate
            .lock()
            .map_err(|_| "rotate candidate lock poisoned".to_owned())? = None;
        self.save(&state)?;
        Ok(json!({
            "applied": true,
            "orientation": orientation,
            "transform": transform_name(transform),
            "output": output,
            "calls": calls,
        }))
    }

    fn configure(&self, params: &Value) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "rotate state lock poisoned".to_owned())?;
        let patch = params.get("policy").unwrap_or(params);
        if !patch.is_object() {
            return Err("rotate.configure needs an object (flat fields or `policy`)".to_owned());
        }
        let old = state.policy.clone();
        let mut policy = old.clone();

        patch_bool(patch, "enabled", &mut policy.enabled)?;
        patch_bool(patch, "dry_run", &mut policy.dry_run)?;
        patch_bool(patch, "lock", &mut policy.lock)?;
        patch_optional_string(patch, "output", &mut policy.output)?;
        patch_optional_string(patch, "sensor_command", &mut policy.sensor_command)?;
        if let Some(value) = patch.get("hysteresis_ms") {
            policy.hysteresis_ms = value
                .as_u64()
                .ok_or_else(|| "`hysteresis_ms` must be a non-negative integer".to_owned())?;
        }
        if let Some(value) = patch.get("position") {
            policy.position = parse_position(value)?;
        }
        policy.normalize()?;

        let changed = policy != old;
        state.policy = policy;
        let candidate_cleared = changed && {
            let mut candidate = self
                .candidate
                .lock()
                .map_err(|_| "rotate candidate lock poisoned".to_owned())?;
            candidate.take().is_some()
        };
        if changed {
            self.save(&state)?;
        }
        Ok(json!({
            "changed": changed,
            "candidate_cleared": candidate_cleared,
            "policy": state.policy,
        }))
    }

    fn set_enabled(&self, enabled: bool) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "rotate state lock poisoned".to_owned())?;
        let changed = state.policy.enabled != enabled;
        state.policy.enabled = enabled;
        if changed {
            self.save(&state)?;
        }
        Ok(json!({ "enabled": enabled, "changed": changed }))
    }

    fn set_lock(&self, locked: bool) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "rotate state lock poisoned".to_owned())?;
        let changed = state.policy.lock != locked;
        state.policy.lock = locked;
        if changed {
            if locked {
                *self
                    .candidate
                    .lock()
                    .map_err(|_| "rotate candidate lock poisoned".to_owned())? = None;
            }
            self.save(&state)?;
        }
        Ok(json!({ "locked": locked, "changed": changed }))
    }

    fn reset(&self) -> Result<Value, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "rotate state lock poisoned".to_owned())?;
        let had_last = state.last_orientation.is_some() || state.last_transform.is_some();
        state.last_orientation = None;
        state.last_transform = None;
        let had_candidate = self
            .candidate
            .lock()
            .map_err(|_| "rotate candidate lock poisoned".to_owned())?
            .take()
            .is_some();
        let cleared = had_last || had_candidate;
        if had_last {
            self.save(&state)?;
        }
        Ok(json!({ "reset": true, "cleared": cleared }))
    }
}

fn parse_position(value: &Value) -> Result<Option<(i32, i32)>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Array(values) if values.len() == 2 => {
            let x = values[0]
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| "`position` x must be a 32-bit integer".to_owned())?;
            let y = values[1]
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| "`position` y must be a 32-bit integer".to_owned())?;
            Ok(Some((x, y)))
        }
        Value::Object(map) => {
            let x = map
                .get("x")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| "`position.x` must be a 32-bit integer".to_owned())?;
            let y = map
                .get("y")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .ok_or_else(|| "`position.y` must be a 32-bit integer".to_owned())?;
            Ok(Some((x, y)))
        }
        _ => Err("`position` must be null, [x, y] or {\"x\": x, \"y\": y}".to_owned()),
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

impl Context for Rotate {
    fn namespace(&self) -> &'static str {
        "rotate"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("status", "Policy, source, orientation and output state"),
            ActionInfo::new("explain", "Explain what auto-rotation would do"),
            ActionInfo::new("tick", "Debounce the source and apply a stable orientation"),
            ActionInfo::new(
                "configure",
                "Update policy fields (`output`, `sensor_command`, hysteresis, position)",
            ),
            ActionInfo::new("enable", "Enable auto-rotation"),
            ActionInfo::new("disable", "Disable auto-rotation"),
            ActionInfo::new("lock", "Pause auto-rotation (manual set still works)"),
            ActionInfo::new("unlock", "Resume auto-rotation"),
            ActionInfo::new(
                "set",
                "Apply an explicit `orientation` or `transform` to an output",
            ),
            ActionInfo::new("reset", "Forget the last applied transform and debounce"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "status" => self.snapshot(false, true),
            "explain" => self.snapshot(false, true),
            "tick" => self.snapshot(true, false),
            "configure" => self.configure(params),
            "enable" => self.set_enabled(true),
            "disable" => self.set_enabled(false),
            "lock" => self.set_lock(true),
            "unlock" => self.set_lock(false),
            "set" => self.set(params),
            "reset" => self.reset(),
            other => Err(format!("unknown action `rotate.{other}`")),
        }
    }
}

/// Path used by the daemon for rotation state.
pub fn state_path() -> PathBuf {
    companion_core::state_dir().join("rotate.json")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    type Call = (String, Transform, Option<(i32, i32)>);
    type Calls = Arc<Mutex<Vec<Call>>>;

    struct FakeNow(AtomicU64);

    impl Now for FakeNow {
        fn now_millis(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct FakeSource(Arc<Mutex<Result<Orientation, String>>>);

    impl OrientationSource for FakeSource {
        fn read(&self, _policy: &Policy) -> Result<Orientation, String> {
            self.0.lock().unwrap().clone()
        }

        fn describe(&self, _policy: &Policy) -> String {
            "fake".to_owned()
        }
    }

    struct FakeOutputs {
        calls: Calls,
        current: Arc<Mutex<Option<Transform>>>,
        failure: Arc<Mutex<Option<String>>>,
    }

    impl OutputController for FakeOutputs {
        fn apply(
            &self,
            output: &str,
            transform: Transform,
            position: Option<(i32, i32)>,
        ) -> Result<Vec<String>, String> {
            if let Some(error) = self.failure.lock().unwrap().clone() {
                return Err(error);
            }
            self.calls
                .lock()
                .unwrap()
                .push((output.to_owned(), transform, position));
            Ok(vec![format!(
                "transform {} on {output}",
                transform_name(transform)
            )])
        }

        fn current_transform(&self, _output: &str) -> Result<Option<Transform>, String> {
            Ok(*self.current.lock().unwrap())
        }
    }

    struct Harness {
        rotate: Rotate,
        now: Arc<FakeNow>,
        source: Arc<Mutex<Result<Orientation, String>>>,
        calls: Calls,
        current: Arc<Mutex<Option<Transform>>>,
        failure: Arc<Mutex<Option<String>>>,
    }

    fn harness(name: &str, policy: Policy) -> Harness {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-rotate-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        harness_at(dir.join("rotate.json"), policy)
    }

    fn harness_at(path: PathBuf, policy: Policy) -> Harness {
        let now = Arc::new(FakeNow(AtomicU64::new(0)));
        let source = Arc::new(Mutex::new(Ok(Orientation::Normal)));
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let current = Arc::new(Mutex::new(None));
        let failure = Arc::new(Mutex::new(None));
        let rotate = Rotate::with_parts(
            &path,
            Box::new(FakeSource(source.clone())),
            Box::new(FakeOutputs {
                calls: calls.clone(),
                current: current.clone(),
                failure: failure.clone(),
            }),
            now.clone(),
        )
        .unwrap();
        rotate
            .configure(&serde_json::to_value(&policy).unwrap())
            .unwrap();
        Harness {
            rotate,
            now,
            source,
            calls,
            current,
            failure,
        }
    }

    fn enabled(output: &str, hysteresis_ms: u64) -> Policy {
        Policy {
            enabled: true,
            output: Some(output.to_owned()),
            hysteresis_ms,
            ..Policy::default()
        }
    }

    fn call(harness: &Harness, action: &str) -> Value {
        harness.rotate.call(action, &json!({})).unwrap()
    }

    fn calls(harness: &Harness) -> Vec<Call> {
        harness.calls.lock().unwrap().clone()
    }

    #[test]
    fn applies_a_stable_orientation_once_after_hysteresis() {
        let harness = harness("stable", enabled("eDP-1", 500));
        harness.now.0.store(1_000, Ordering::SeqCst);
        assert_eq!(call(&harness, "tick")["skipped"], "debouncing");
        assert!(calls(&harness).is_empty());

        harness.now.0.store(1_499, Ordering::SeqCst);
        assert_eq!(call(&harness, "tick")["skipped"], "debouncing");

        harness.now.0.store(1_500, Ordering::SeqCst);
        let value = call(&harness, "tick");
        assert_eq!(value["applied"], true);
        assert_eq!(value["orientation"], "normal");
        assert_eq!(value["transform"], "normal");
        assert_eq!(
            calls(&harness),
            [("eDP-1".to_owned(), Transform::Normal, None)]
        );

        harness.now.0.store(2_000, Ordering::SeqCst);
        let value = call(&harness, "tick");
        assert_eq!(value["skipped"], "no-change");
        assert_eq!(calls(&harness).len(), 1);
    }

    #[test]
    fn an_orientation_change_restarts_the_hysteresis() {
        let harness = harness("change", enabled("eDP-1", 1000));
        harness.now.0.store(1_000, Ordering::SeqCst);
        call(&harness, "tick");

        *harness.source.lock().unwrap() = Ok(Orientation::LeftUp);
        harness.now.0.store(1_500, Ordering::SeqCst);
        let value = call(&harness, "tick");
        assert_eq!(value["skipped"], "debouncing");
        assert_eq!(value["candidate"]["orientation"], "left-up");
        assert_eq!(value["candidate"]["since_millis"], 1_500);

        harness.now.0.store(2_499, Ordering::SeqCst);
        assert_eq!(call(&harness, "tick")["skipped"], "debouncing");
        harness.now.0.store(2_500, Ordering::SeqCst);
        let value = call(&harness, "tick");
        assert_eq!(value["applied"], true);
        assert_eq!(value["transform"], "90");
        assert_eq!(
            calls(&harness),
            [("eDP-1".to_owned(), Transform::_90, None)]
        );
    }

    #[test]
    fn dry_run_and_explain_never_apply() {
        let mut policy = enabled("eDP-1", 0);
        policy.dry_run = true;
        let harness = harness("dry-run", policy);

        let tick = call(&harness, "tick");
        assert_eq!(tick["skipped"], "dry-run");
        assert_eq!(tick["would_apply"], true);
        assert!(calls(&harness).is_empty());

        let explain = call(&harness, "explain");
        assert_eq!(explain["would_apply"], true);
        assert_eq!(explain["applied"], false);
        assert!(calls(&harness).is_empty());
    }

    #[test]
    fn lock_blocks_auto_apply_but_not_manual_set() {
        let mut policy = enabled("eDP-1", 0);
        policy.lock = true;
        let harness = harness("lock", policy);

        assert_eq!(call(&harness, "tick")["skipped"], "locked");
        assert!(calls(&harness).is_empty());

        let value = harness
            .rotate
            .call("set", &json!({ "orientation": "left-up" }))
            .unwrap();
        assert_eq!(value["applied"], true);
        assert_eq!(
            calls(&harness),
            [("eDP-1".to_owned(), Transform::_90, None)]
        );

        let value = harness.rotate.call("unlock", &Value::Null).unwrap();
        assert_eq!(value["changed"], true);
        let value = harness.rotate.call("unlock", &Value::Null).unwrap();
        assert_eq!(value["changed"], false);
        let value = harness.rotate.call("lock", &Value::Null).unwrap();
        assert_eq!(value["changed"], true);
    }

    #[test]
    fn a_missing_sensor_degrades_without_breaking_other_actions() {
        let harness = harness("missing-sensor", enabled("eDP-1", 0));
        *harness.source.lock().unwrap() = Err("no sensor".into());

        let tick = call(&harness, "tick");
        assert_eq!(tick["skipped"], "source-unavailable");
        assert_eq!(tick["error"], "no sensor");
        assert!(calls(&harness).is_empty());

        let status = call(&harness, "status");
        assert_eq!(status["skipped"], "source-unavailable");
        assert_eq!(status["source"], "fake");

        let value = harness
            .rotate
            .call("configure", &json!({ "hysteresis_ms": 250 }))
            .unwrap();
        assert_eq!(value["changed"], true);
        assert_eq!(value["policy"]["hysteresis_ms"], 250);
    }

    #[test]
    fn output_failure_keeps_state_and_is_retried() {
        let harness = harness("retry", enabled("eDP-1", 0));
        *harness.failure.lock().unwrap() = Some("niri down".into());

        let value = call(&harness, "tick");
        assert_eq!(value["skipped"], "apply-failed");
        assert_eq!(value["error"], "niri down");
        assert_eq!(value["last_transform"], Value::Null);
        assert_eq!(call(&harness, "status")["last_transform"], Value::Null);

        *harness.failure.lock().unwrap() = None;
        let value = call(&harness, "tick");
        assert_eq!(value["applied"], true);
        assert_eq!(calls(&harness).len(), 1);
    }

    #[test]
    fn configure_is_idempotent_persists_and_validates() {
        let path = std::env::temp_dir()
            .join(format!(
                "better-companion-rotate-config-{}",
                std::process::id()
            ))
            .join("rotate.json");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let harness = harness_at(path.clone(), Policy::default());

        let patch = json!({
            "enabled": true,
            "output": "eDP-1",
            "sensor_command": "printf normal",
            "hysteresis_ms": 0,
            "position": [10, 20]
        });
        let first = harness.rotate.call("configure", &patch).unwrap();
        assert_eq!(first["changed"], true);
        let second = harness.rotate.call("configure", &patch).unwrap();
        assert_eq!(second["changed"], false);

        // Reload with inert fakes; policy survives.
        let now = Arc::new(FakeNow(AtomicU64::new(0)));
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let reloaded = Rotate::with_parts(
            &path,
            Box::new(FakeSource(Arc::new(Mutex::new(Ok(Orientation::Normal))))),
            Box::new(FakeOutputs {
                calls,
                current: Arc::new(Mutex::new(None)),
                failure: Arc::new(Mutex::new(None)),
            }),
            now,
        )
        .unwrap();
        let status = reloaded.call("status", &Value::Null).unwrap();
        assert_eq!(status["policy"]["enabled"], true);
        assert_eq!(status["policy"]["output"], "eDP-1");
        assert_eq!(status["policy"]["hysteresis_ms"], 0);
        assert_eq!(status["policy"]["position"], json!([10, 20]));
        assert_eq!(status["would_apply"], true);

        for invalid in [
            json!({ "hysteresis_ms": 60_001 }),
            json!({ "output": "  " }),
            json!({ "sensor_command": "" }),
            json!({ "position": [1] }),
            json!({ "position": "here" }),
        ] {
            assert!(
                reloaded.call("configure", &invalid).is_err(),
                "expected {invalid} to fail"
            );
        }
    }

    #[test]
    fn position_is_applied_with_the_transform() {
        let mut policy = enabled("eDP-1", 0);
        policy.position = Some((100, 50));
        let harness = harness("position", policy);
        let value = call(&harness, "tick");
        assert_eq!(value["applied"], true);
        assert_eq!(
            calls(&harness),
            [("eDP-1".to_owned(), Transform::Normal, Some((100, 50)))]
        );
    }

    #[test]
    fn set_requires_exactly_one_explicit_value_and_an_output() {
        let mut policy = enabled("eDP-1", 0);
        policy.dry_run = true;
        let harness = harness("set-args", policy);

        assert!(harness.rotate.call("set", &json!({})).is_err());
        assert!(
            harness
                .rotate
                .call(
                    "set",
                    &json!({ "orientation": "normal", "transform": "90" })
                )
                .is_err()
        );
        assert!(
            harness
                .rotate
                .call("set", &json!({ "orientation": "sideways" }))
                .is_err()
        );

        let value = harness
            .rotate
            .call("set", &json!({ "orientation": "normal" }))
            .unwrap();
        assert_eq!(value["dry_run"], true);
        assert_eq!(value["would_apply"], true);
        assert!(calls(&harness).is_empty());

        // A flipped transform has no pure orientation; it still applies.
        harness
            .rotate
            .call("configure", &json!({ "dry_run": false }))
            .unwrap();
        let value = harness
            .rotate
            .call(
                "set",
                &json!({ "transform": "flipped-90", "output": "DP-2" }),
            )
            .unwrap();
        assert_eq!(value["applied"], true);
        assert_eq!(value["output"], "DP-2");
        assert_eq!(value["orientation"], Value::Null);
        assert_eq!(
            calls(&harness),
            [("DP-2".to_owned(), Transform::Flipped90, None)]
        );
    }

    #[test]
    fn no_output_means_no_auto_apply() {
        let harness = harness(
            "no-output",
            Policy {
                enabled: true,
                hysteresis_ms: 0,
                ..Policy::default()
            },
        );
        let value = call(&harness, "tick");
        assert_eq!(value["skipped"], "no-output");
        assert!(calls(&harness).is_empty());
        assert_eq!(value["orientation"], "normal");
    }

    #[test]
    fn status_reports_the_current_output_transform() {
        let harness = harness("status", enabled("eDP-1", 0));
        *harness.current.lock().unwrap() = Some(Transform::_270);
        let status = call(&harness, "status");
        assert_eq!(status["source"], "fake");
        assert_eq!(status["current"]["transform"], "270");
        assert_eq!(status["current"]["output"], "eDP-1");
        assert_eq!(status["orientation"], "normal");
        assert_eq!(status["would_apply"], true);
        // Status never applies.
        assert!(calls(&harness).is_empty());
    }

    #[test]
    fn reset_forgets_the_last_transform_and_candidate() {
        let harness = harness("reset", enabled("eDP-1", 0));
        call(&harness, "tick");
        let value = call(&harness, "reset");
        assert_eq!(value["cleared"], true);
        assert_eq!(call(&harness, "status")["last_transform"], Value::Null);
        assert_eq!(call(&harness, "reset")["cleared"], false);
    }

    #[test]
    fn enable_and_disable_are_idempotent() {
        let harness = harness("toggle", Policy::default());
        assert_eq!(
            harness.rotate.call("disable", &Value::Null).unwrap()["changed"],
            false
        );
        assert_eq!(
            harness.rotate.call("enable", &Value::Null).unwrap()["changed"],
            true
        );
        assert_eq!(
            harness.rotate.call("enable", &Value::Null).unwrap()["changed"],
            false
        );
        assert_eq!(
            harness.rotate.call("disable", &Value::Null).unwrap()["changed"],
            true
        );
    }

    #[test]
    fn status_and_explain_are_deterministic_and_side_effect_free() {
        let harness = harness("status-deterministic", enabled("eDP-1", 500));
        harness.now.0.store(1_000, Ordering::SeqCst);
        let first = call(&harness, "status");
        let second = call(&harness, "status");
        assert_eq!(first, second);
        assert_eq!(first["skipped"], "debouncing");
        assert!(calls(&harness).is_empty());

        let first = call(&harness, "explain");
        let second = call(&harness, "explain");
        assert_eq!(first, second);
        assert!(calls(&harness).is_empty());
    }
}
