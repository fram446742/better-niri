//! Persisted adapter configuration.
//!
//! The original Phase C files only had `enabled`, `commands` and `hooks`.
//! Every new field is serde-defaulted, so those files keep loading unchanged.
//! Legacy `hooks` still resolve to one command per event and are treated as
//! subscriptions with adapter-level defaults when the adapter is enabled.

use std::collections::BTreeMap;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_DEDUP_MS: u64 = 1_000;
pub const DEFAULT_MAX_CONCURRENT: usize = 2;

/// What to do after a command fails.
///
/// Accepted spellings: `"ignore"`, `"log"`, `"disable"`,
/// `"disable-after-3"`, or an object such as
/// `{"kind":"disable_after","failures":3}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailurePolicy {
    Ignore,
    #[default]
    Log,
    DisableAfter(u32),
}

impl FailurePolicy {
    pub fn parse(text: &str) -> Result<Self, String> {
        let normalized = text.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "ignore" => Ok(Self::Ignore),
            "log" => Ok(Self::Log),
            "disable" => Ok(Self::DisableAfter(1)),
            other => match other.strip_prefix("disable-after-") {
                Some(count) => {
                    let count: u32 = count
                        .parse()
                        .map_err(|_| format!("invalid failure policy `{text}`"))?;
                    if count == 0 {
                        return Err("disable-after count must be at least 1".to_owned());
                    }
                    Ok(Self::DisableAfter(count))
                }
                None => Err(format!(
                    "unknown failure policy `{text}`; use ignore, log or disable-after-N"
                )),
            },
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(text) => Self::parse(text),
            Value::Object(map) => {
                let kind = map
                    .get("kind")
                    .or_else(|| map.get("policy"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| "failure policy object needs a string `kind`".to_owned())?;
                let failures = map
                    .get("failures")
                    .or_else(|| map.get("after"))
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as u32;
                match kind.trim().to_ascii_lowercase().replace('_', "-").as_str() {
                    "ignore" => Ok(Self::Ignore),
                    "log" => Ok(Self::Log),
                    "disable" => Ok(Self::DisableAfter(failures.max(1))),
                    "disable-after" => {
                        if failures == 0 {
                            return Err("failure policy `failures` must be at least 1".to_owned());
                        }
                        Ok(Self::DisableAfter(failures))
                    }
                    other => Err(format!("unknown failure policy kind `{other}`")),
                }
            }
            other => Err(format!(
                "failure policy must be a string or object, got {other}"
            )),
        }
    }

    pub fn as_json(self) -> Value {
        match self {
            Self::Ignore => json!("ignore"),
            Self::Log => json!("log"),
            Self::DisableAfter(count) => json!(format!("disable-after-{count}")),
        }
    }
}

impl Serialize for FailurePolicy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text = match self {
            Self::Ignore => "ignore".to_owned(),
            Self::Log => "log".to_owned(),
            Self::DisableAfter(count) => format!("disable-after-{count}"),
        };
        serializer.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for FailurePolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(&value).map_err(de::Error::custom)
    }
}

/// One command template attached to one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_policy: Option<FailurePolicy>,
}

impl Subscription {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            label: None,
            timeout_ms: None,
            dedup_ms: None,
            failure_policy: None,
        }
    }

    pub fn from_hook(command: impl Into<String>) -> Self {
        Self {
            label: Some("legacy-hook".to_owned()),
            ..Self::new(command)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub enabled: bool,
    pub commands: BTreeMap<String, String>,
    /// Legacy single command per event. Still written by `set-hook`.
    pub hooks: BTreeMap<String, String>,
    pub subscriptions: BTreeMap<String, Vec<Subscription>>,
    pub timeout_ms: u64,
    pub dedup_ms: u64,
    pub max_concurrent: usize,
    pub failure_policy: FailurePolicy,
}

impl Default for State {
    fn default() -> Self {
        Self {
            enabled: false,
            commands: BTreeMap::new(),
            hooks: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            dedup_ms: DEFAULT_DEDUP_MS,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            failure_policy: FailurePolicy::default(),
        }
    }
}

impl State {
    /// Configured subscriptions plus legacy hooks, with exact duplicates
    /// collapsed. Hook commands run with adapter-level defaults.
    pub fn subscriptions_for(&self, event: &str) -> Vec<Subscription> {
        let mut subscriptions = self.subscriptions.get(event).cloned().unwrap_or_default();
        if let Some(hook) = self.hooks.get(event)
            && !subscriptions
                .iter()
                .any(|subscription| subscription.command == *hook)
        {
            subscriptions.push(Subscription::from_hook(hook.clone()));
        }
        subscriptions
    }
}

/// Event names accept underscores and a few historical aliases.
pub fn normalize_event(event: &str) -> String {
    let event = event.trim().to_ascii_lowercase().replace('_', "-");
    match event.as_str() {
        "workspace-switched" | "workspace-focused" => "workspace-activated".to_owned(),
        "window-focus-changed" | "window-focused-changed" => "window-focused".to_owned(),
        "window-opened-or-changed" | "window-changed" => "window-opened".to_owned(),
        "output-hotplug" | "output-connected" => "output-added".to_owned(),
        "output-disconnected" => "output-removed".to_owned(),
        other => other.to_owned(),
    }
}

/// Every event the adapter engine can deliver.
pub const EVENT_KINDS: &[&str] = &[
    "workspace-activated",
    "workspace-added",
    "workspace-removed",
    "workspace-moved",
    "output-added",
    "output-removed",
    "window-opened",
    "window-closed",
    "window-focused",
    "window-moved",
    "park",
    "restore",
    "stage",
    "unstage",
    "scratchpad-toggle",
    "scratchpad-show",
    "scratchpad-hide",
    "idle-transition",
    "idle-active",
    "rotation-applied",
];

pub fn is_known_event(event: &str) -> bool {
    EVENT_KINDS.contains(&normalize_event(event).as_str())
}
