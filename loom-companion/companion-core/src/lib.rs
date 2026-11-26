//! Shared protocol, registry and filesystem helpers for `loom-companion`.
//!
//! The daemon is one process with bounded-context crates. Every context owns a
//! hierarchical action namespace (`parking.*`, `automation.*`, `hooks.*`,
//! `system.*`, …); [`Registry`] merges them and serves the JSON-line protocol
//! over a Unix socket.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, io};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of the daemon socket protocol. Clients can compare this and
/// [`PROTOCOL_VERSION`] from [`Request::Describe`].
pub const PROTOCOL: &str = "loom-companion";
/// Current version of the daemon socket protocol.
///
/// Version 2 adds request ids, error codes, marks and event subscriptions.
/// Every addition is optional on the wire, so version-1 clients keep working.
pub const PROTOCOL_VERSION: u32 = 2;

/// Event topics accepted by [`Request::Subscribe`]. An empty topic list and
/// `"*"` both subscribe to every topic.
pub const EVENT_TOPICS: &[&str] = &[
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
    "adapter-delivery",
];

/// Returns true when `topic` is a known event topic (or the wildcard).
pub fn is_known_topic(topic: &str) -> bool {
    topic == "*" || EVENT_TOPICS.contains(&topic)
}

/// Auto-tracked parking lot with one entry per open window.
pub const AUTO_WINDOWS: &str = "auto:windows";
/// Auto-tracked parking lot with one entry per workspace.
pub const AUTO_WORKSPACES: &str = "auto:workspaces";

fn default_available() -> bool {
    true
}

/// One argument of an action, for machine-readable discovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParamInfo {
    /// Parameter name as used in the `params` object.
    pub name: String,
    /// JSON kind: `string`, `number`, `bool`, `array` or `object`.
    pub kind: String,
    /// Whether the action fails without this parameter.
    pub required: bool,
    /// Human-readable meaning.
    pub description: String,
}

impl ParamInfo {
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            required,
            description: description.into(),
        }
    }

    pub fn string(name: impl Into<String>, required: bool, description: impl Into<String>) -> Self {
        Self::new(name, "string", required, description)
    }

    pub fn integer(
        name: impl Into<String>,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        Self::new(name, "integer", required, description)
    }

    pub fn object(name: impl Into<String>, required: bool, description: impl Into<String>) -> Self {
        Self::new(name, "object", required, description)
    }

    pub fn array(name: impl Into<String>, required: bool, description: impl Into<String>) -> Self {
        Self::new(name, "array", required, description)
    }

    pub fn boolean(
        name: impl Into<String>,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        Self::new(name, "boolean", required, description)
    }
}

/// One action exposed by a bounded context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionInfo {
    pub name: String,
    pub description: String,
    /// Namespace that owns the action. Filled in by [`Registry::list`].
    #[serde(default)]
    pub owner: String,
    /// Machine-readable argument schema. Empty for actions that take no parameters.
    #[serde(default)]
    pub params: Vec<ParamInfo>,
    /// Capabilities the action needs (`shell` for actions that execute commands, `niri` for
    /// actions that require a compositor connection, ...).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Whether the action is usable in this build. Contexts can mark actions unavailable when a
    /// dependency (compositor feature, audio server, sensor) is missing.
    #[serde(default = "default_available")]
    pub available: bool,
}

impl ActionInfo {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            owner: String::new(),
            params: Vec::new(),
            permissions: Vec::new(),
            available: true,
        }
    }

    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = owner.into();
        self
    }

    pub fn with_params(mut self, params: impl IntoIterator<Item = ParamInfo>) -> Self {
        self.params = params.into_iter().collect();
        self
    }

    pub fn with_permissions(mut self, permissions: &[&str]) -> Self {
        self.permissions = permissions.iter().map(|p| (*p).to_owned()).collect();
        self
    }

    pub fn unavailable(mut self) -> Self {
        self.available = false;
        self
    }

    /// Marks the action unavailable when its host dependency is missing.
    pub fn with_availability(mut self, available: bool) -> Self {
        self.available = available;
        self
    }
}

/// A request on the daemon socket. One request per line, JSON-encoded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// List every context and its actions.
    List,
    /// Call one action in one namespace.
    Call {
        namespace: String,
        action: String,
        #[serde(default)]
        params: Value,
        /// Optional client-chosen request id, echoed on the response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    /// Daemon status.
    Status,
    /// Describe the protocol, its version and every action with owner and argument schema.
    Describe,
    /// Turn this connection into an event stream until it closes.
    ///
    /// Empty `events` (or `"*"`) subscribes to every topic. Events are written
    /// as `{"kind":"event",...}` JSON lines. There is no in-band unsubscribe:
    /// close the connection, which ends the stream and removes the subscriber.
    Subscribe {
        #[serde(default)]
        events: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
}

/// A response on the daemon socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Contexts {
        contexts: BTreeMap<String, Vec<ActionInfo>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    Value {
        value: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    Status {
        version: String,
        pid: u32,
        uptime_secs: u64,
        socket: String,
        contexts: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    Error {
        message: String,
        /// Stable machine-readable error code; optional so old responses keep
        /// deserializing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    /// Protocol description and full action registry.
    Description {
        protocol: String,
        version: u32,
        actions: BTreeMap<String, Vec<ActionInfo>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    /// Acknowledgement of a [`Request::Subscribe`]; event lines follow.
    Subscribed {
        events: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
    /// One event on a subscribed connection.
    Event {
        event: String,
        #[serde(default)]
        payload: Value,
        /// Monotonic event sequence number across the daemon.
        #[serde(default)]
        seq: u64,
        timestamp_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
    },
}

impl Response {
    /// Attaches a request id to the response that supports one.
    pub fn with_id(self, request_id: Option<u64>) -> Self {
        let Some(id) = request_id else {
            return self;
        };
        match self {
            Response::Contexts { contexts, .. } => Response::Contexts {
                contexts,
                id: Some(id),
            },
            Response::Value { value, .. } => Response::Value {
                value,
                id: Some(id),
            },
            Response::Status {
                version,
                pid,
                uptime_secs,
                socket,
                contexts,
                ..
            } => Response::Status {
                version,
                pid,
                uptime_secs,
                socket,
                contexts,
                id: Some(id),
            },
            Response::Error { message, code, .. } => Response::Error {
                message,
                code,
                id: Some(id),
            },
            Response::Description {
                protocol,
                version,
                actions,
                ..
            } => Response::Description {
                protocol,
                version,
                actions,
                id: Some(id),
            },
            Response::Subscribed { events, .. } => Response::Subscribed {
                events,
                id: Some(id),
            },
            Response::Event {
                event,
                payload,
                seq,
                timestamp_ms,
                ..
            } => Response::Event {
                event,
                payload,
                seq,
                timestamp_ms,
                id: Some(id),
            },
        }
    }

    /// Builds an [`Response::Error`] with a stable code.
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
            code: Some(code.into()),
            id: None,
        }
    }
}

/// One event published on the daemon's event bus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub event: String,
    #[serde(default)]
    pub payload: Value,
    pub seq: u64,
    pub timestamp_ms: u64,
}

impl Event {
    pub fn new(event: impl Into<String>, payload: Value) -> Self {
        Self {
            event: event.into(),
            payload,
            seq: 0,
            timestamp_ms: now_millis(),
        }
    }
}

/// Current wall-clock time in milliseconds since the Unix epoch.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Result of a timed subscription receive.
#[derive(Debug, Clone, PartialEq)]
pub enum EventRecv {
    Event(Event),
    Timeout,
    Closed,
}

/// One received event subscription. Dropping it unsubscribes.
pub struct EventSubscription {
    receiver: Receiver<Event>,
    slow: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    bus: Option<Arc<Mutex<BusInner>>>,
    id: u64,
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        if let Some(bus) = &self.bus
            && let Ok(mut inner) = bus.lock()
        {
            inner.subscribers.remove(&self.id);
        }
    }
}

impl EventSubscription {
    /// Waits for the next event. `None` means the bus was dropped or the
    /// subscriber was removed (for example because it fell too far behind).
    pub fn recv(&self) -> Option<Event> {
        self.receiver.recv().ok()
    }

    /// Returns a queued event without blocking.
    pub fn try_recv(&self) -> Option<Event> {
        self.receiver.try_recv().ok()
    }

    /// Waits up to `timeout` for one event.
    pub fn recv_timeout(&self, timeout: Duration) -> EventRecv {
        match self.receiver.recv_timeout(timeout) {
            Ok(event) => EventRecv::Event(event),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => EventRecv::Timeout,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => EventRecv::Closed,
        }
    }

    /// True when the subscription was closed for being too slow.
    pub fn is_slow(&self) -> bool {
        self.slow.load(Ordering::Relaxed)
    }

    /// Number of events dropped because this subscriber's queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

struct Subscriber {
    topics: BTreeSet<String>,
    sender: SyncSender<Event>,
    slow: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
}

#[derive(Default)]
struct BusInner {
    subscribers: BTreeMap<u64, Subscriber>,
    next_subscriber: u64,
    seq: u64,
}

/// Bounded, non-blocking fan-out bus used by [`Request::Subscribe`].
///
/// Each subscriber has its own bounded queue. Publishing never blocks: a full
/// queue marks the subscriber as slow and removes it, so one stalled client
/// cannot stall the daemon or other clients.
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a subscriber for `topics` with a queue of `capacity` events.
    pub fn subscribe(
        &self,
        topics: impl IntoIterator<Item = String>,
        capacity: usize,
    ) -> EventSubscription {
        let topics: BTreeSet<String> = topics.into_iter().collect();
        let (sender, receiver) = sync_channel(capacity.max(1));
        let slow = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let mut id = 0;
        if let Ok(mut inner) = self.inner.lock() {
            inner.next_subscriber += 1;
            id = inner.next_subscriber;
            inner.subscribers.insert(
                id,
                Subscriber {
                    topics,
                    sender,
                    slow: slow.clone(),
                    dropped: dropped.clone(),
                },
            );
        }
        EventSubscription {
            receiver,
            slow,
            dropped,
            bus: Some(self.inner.clone()),
            id,
        }
    }

    /// Publishes one event to every matching subscriber.
    ///
    /// Returns the number of subscribers that received it. Slow subscribers
    /// are removed and counted as not receiving the event.
    pub fn publish(&self, event: Event) -> usize {
        let Ok(mut inner) = self.inner.lock() else {
            return 0;
        };
        inner.seq += 1;
        let seq = inner.seq;
        let mut delivered = 0;
        let mut slow = Vec::new();
        for (id, subscriber) in &inner.subscribers {
            if !subscriber_matches(subscriber, &event.event) {
                continue;
            }
            let event = Event {
                seq,
                ..event.clone()
            };
            match subscriber.sender.try_send(event) {
                Ok(()) => delivered += 1,
                Err(TrySendError::Full(_)) => {
                    subscriber.slow.store(true, Ordering::Relaxed);
                    subscriber.dropped.fetch_add(1, Ordering::Relaxed);
                    slow.push(*id);
                }
                Err(TrySendError::Disconnected(_)) => slow.push(*id),
            }
        }
        for id in slow {
            inner.subscribers.remove(&id);
        }
        delivered
    }

    /// Number of live subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.subscribers.len())
            .unwrap_or_default()
    }
}

fn subscriber_matches(subscriber: &Subscriber, event: &str) -> bool {
    subscriber.topics.is_empty()
        || subscriber.topics.contains("*")
        || subscriber.topics.contains(event)
}

/// A single step of an automation sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Step {
    pub namespace: String,
    pub action: String,
    #[serde(default)]
    pub params: Value,
}

/// One bounded context of the daemon.
pub trait Context: Send + Sync {
    fn namespace(&self) -> &'static str;
    fn actions(&self) -> Vec<ActionInfo>;
    fn call(&self, action: &str, params: &Value) -> Result<Value, String>;

    /// Optional named sequence, used by the registry's `automation.run` support.
    fn sequence(&self, _name: &str) -> Option<Vec<Step>> {
        None
    }

    /// Optional rule evaluation for `rules.run`; returns the steps to execute
    /// for `window` (or every tracked window when `None`).
    fn matched_steps(&self, _window: Option<u64>) -> Option<Result<Vec<Step>, String>> {
        None
    }

    /// Optional workspace-event evaluation for `rules.run`/`rules.explain`.
    /// `workspace` is an explicit workspace state object, or `None` for every
    /// tracked workspace.
    fn event_steps(
        &self,
        _event: &str,
        _workspace: Option<&Value>,
    ) -> Option<Result<Vec<Step>, String>> {
        None
    }
}

/// One completed rule or automation run, for the bounded history log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRecord {
    pub timestamp_ms: u64,
    /// `automation.run`, `rules.run` or another executor.
    pub source: String,
    /// Sequence name, `rules`, or another run label.
    pub name: String,
    pub steps: Vec<Step>,
    /// `{"ok":true,"results":[...]}` or `{"ok":false,"error":"..."}`.
    pub result: Value,
}

/// Receives completed runs. Implementations must not block the caller.
pub trait RunRecorder: Send + Sync {
    fn record(&self, record: RunRecord);
}

/// Notified after every action call delegated to a context.
///
/// Observers turn already-completed companion actions into events for optional
/// consumers such as the companion adapters crate. They must not call back into
/// the registry: the registry invokes them after the action has finished, and
/// the daemon's observer only enqueues work.
pub trait CallObserver: Send + Sync {
    fn after_call(
        &self,
        namespace: &str,
        action: &str,
        params: &Value,
        result: &Result<Value, String>,
    );
}

type ContextResult = Result<Value, String>;

impl<T: Context + ?Sized> Context for Arc<T> {
    fn namespace(&self) -> &'static str {
        (**self).namespace()
    }

    fn actions(&self) -> Vec<ActionInfo> {
        (**self).actions()
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        (**self).call(action, params)
    }

    fn sequence(&self, name: &str) -> Option<Vec<Step>> {
        (**self).sequence(name)
    }

    fn matched_steps(&self, window: Option<u64>) -> Option<Result<Vec<Step>, String>> {
        (**self).matched_steps(window)
    }

    fn event_steps(
        &self,
        event: &str,
        workspace: Option<&Value>,
    ) -> Option<Result<Vec<Step>, String>> {
        (**self).event_steps(event, workspace)
    }
}

/// The merged action registry.
pub struct Registry {
    contexts: BTreeMap<String, Box<dyn Context>>,
    observer: Option<Arc<dyn CallObserver>>,
    recorder: Option<Arc<dyn RunRecorder>>,
    started: Instant,
    socket: Option<PathBuf>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            contexts: BTreeMap::new(),
            observer: None,
            recorder: None,
            started: Instant::now(),
            socket: None,
        }
    }

    /// Registry whose status reports `socket` rather than the default path.
    pub fn with_socket(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: Some(socket.into()),
            ..Self::new()
        }
    }

    pub fn register(&mut self, context: impl Context + 'static) {
        let namespace = context.namespace().to_owned();
        self.contexts.insert(namespace, Box::new(context));
    }

    /// Observes completed action calls without changing the action protocol.
    ///
    /// The observer runs on the calling thread after the context returns. It is
    /// used to translate successful companion actions into adapter events.
    pub fn set_observer(&mut self, observer: Arc<dyn CallObserver>) {
        self.observer = Some(observer);
    }

    /// Records completed rule/automation runs into a history log.
    pub fn set_recorder(&mut self, recorder: Arc<dyn RunRecorder>) {
        self.recorder = Some(recorder);
    }

    fn record_run(&self, source: &str, name: &str, steps: &[Step], result: Value) {
        if let Some(recorder) = &self.recorder {
            recorder.record(RunRecord {
                timestamp_ms: now_millis(),
                source: source.to_owned(),
                name: name.to_owned(),
                steps: steps.to_vec(),
                result,
            });
        }
    }

    pub fn namespaces(&self) -> Vec<String> {
        self.contexts.keys().cloned().collect()
    }

    pub fn list(&self) -> BTreeMap<String, Vec<ActionInfo>> {
        self.contexts
            .iter()
            .map(|(name, ctx)| {
                let mut actions = ctx.actions();
                for action in &mut actions {
                    if action.owner.is_empty() {
                        action.owner.clone_from(name);
                    }
                }
                (name.clone(), actions)
            })
            .collect()
    }

    pub fn handle(&self, request: Request) -> Response {
        match request {
            Request::List => Response::Contexts {
                contexts: self.list(),
                id: None,
            },
            Request::Call {
                namespace,
                action,
                params,
                id,
            } => {
                let response = match self.call(&namespace, &action, &params) {
                    Ok(value) => Response::Value { value, id: None },
                    Err(message) => {
                        let code = error_code(&message);
                        Response::error(code, message)
                    }
                };
                response.with_id(id)
            }
            Request::Describe => Response::Description {
                protocol: PROTOCOL.to_owned(),
                version: PROTOCOL_VERSION,
                actions: self.list(),
                id: None,
            },
            Request::Status => Response::Status {
                version: VERSION.to_owned(),
                pid: std::process::id(),
                uptime_secs: self.started.elapsed().as_secs(),
                socket: self
                    .socket
                    .clone()
                    .unwrap_or_else(socket_path)
                    .display()
                    .to_string(),
                contexts: self.namespaces(),
                id: None,
            },
            Request::Subscribe { id, .. } => Response::error(
                "stream_required",
                "subscribe needs a streaming connection; use the daemon socket",
            )
            .with_id(id),
        }
    }

    fn call(&self, namespace: &str, action: &str, params: &Value) -> ContextResult {
        // Automation sequences dispatch other actions through the same registry,
        // so `automation.run` is the one cross-context builtin.
        if namespace == "automation" && action == "run" {
            return self.run_automation(params);
        }
        if namespace == "rules" && action == "run" {
            return self.run_rules(params);
        }

        let context = self
            .contexts
            .get(namespace)
            .ok_or_else(|| format!("unknown namespace `{namespace}`"))?;
        let result = context.call(action, params);
        if let Some(observer) = &self.observer {
            observer.after_call(namespace, action, params, &result);
        }
        result
    }

    fn run_automation(&self, params: &Value) -> ContextResult {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "automation.run needs a string `name`".to_owned())?;
        let context = self
            .contexts
            .get("automation")
            .ok_or_else(|| "automation context is not registered".to_owned())?;
        let steps = context
            .sequence(name)
            .ok_or_else(|| format!("unknown automation sequence `{name}`"))?;
        if params.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return Ok(serde_json::json!({
                "name": name,
                "dry_run": true,
                "steps": steps,
            }));
        }

        let record_steps = steps.clone();
        let outcome = self
            .dispatch_steps(steps, "automation.run")
            .map(|results| serde_json::json!({ "name": name, "results": results }));
        self.record_run("automation.run", name, &record_steps, run_result(&outcome));
        outcome
    }

    fn run_rules(&self, params: &Value) -> ContextResult {
        let event = params
            .get("event")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let window = params.get("window").and_then(Value::as_u64);
        let workspace = params.get("workspace_state");
        let context = self
            .contexts
            .get("rules")
            .ok_or_else(|| "rules context is not registered".to_owned())?;
        let steps = match &event {
            Some(event) => context
                .event_steps(event, workspace)
                .ok_or_else(|| "rules context did not provide workspace evaluation".to_owned())??,
            None => context
                .matched_steps(window)
                .ok_or_else(|| "rules context did not provide rule evaluation".to_owned())??,
        };
        if params.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return Ok(match &event {
                Some(event) => serde_json::json!({
                    "event": event,
                    "workspace": workspace,
                    "dry_run": true,
                    "steps": steps,
                }),
                None => serde_json::json!({
                    "window": window,
                    "dry_run": true,
                    "steps": steps,
                }),
            });
        }
        let record_steps = steps.clone();
        let name = match &event {
            Some(event) => format!("rules:{event}"),
            None => "rules".to_owned(),
        };
        let outcome = self
            .dispatch_steps(steps, "rules.run")
            .map(|results| match &event {
                Some(event) => serde_json::json!({
                    "event": event,
                    "workspace": workspace,
                    "results": results,
                }),
                None => serde_json::json!({
                    "window": window,
                    "results": results,
                }),
            });
        self.record_run("rules.run", &name, &record_steps, run_result(&outcome));
        outcome
    }

    fn dispatch_steps(&self, steps: Vec<Step>, caller: &str) -> Result<Vec<Value>, String> {
        let mut results = Vec::with_capacity(steps.len());
        for step in steps {
            if (step.namespace == "automation" && step.action == "run")
                || (step.namespace == "rules" && step.action == "run")
            {
                return Err(format!("{caller} cannot call itself"));
            }
            let value = self.call(&step.namespace, &step.action, &step.params)?;
            results.push(serde_json::json!({
                "namespace": step.namespace,
                "action": step.action,
                "value": value,
            }));
        }
        Ok(results)
    }
}

fn error_code(message: &str) -> &'static str {
    if message.starts_with("unknown namespace") {
        "unknown_namespace"
    } else if message.starts_with("unknown action") {
        "unknown_action"
    } else if message.contains("cannot call itself") {
        "recursive_run"
    } else {
        "action_failed"
    }
}

fn run_result(outcome: &ContextResult) -> Value {
    match outcome {
        Ok(value) => serde_json::json!({ "ok": true, "value": value }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    }
}

/// Default user configuration file:
/// `$XDG_CONFIG_HOME/loom-companion/config.json` or
/// `~/.config/loom-companion/config.json`.
pub fn config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("loom-companion/config.json");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config/loom-companion/config.json")
}

/// The daemon socket path: `$XDG_RUNTIME_DIR/loom-companion.sock`.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("loom-companion.sock")
}

/// Persistent state directory: `$XDG_STATE_HOME/loom-companion` or
/// `~/.local/state/loom-companion`.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("loom-companion");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/state/loom-companion")
}

/// Loads JSON state, returning `T::default()` when the file is missing.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(err),
    }
}

/// Writes JSON state atomically (temporary file + rename).
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

/// Renders a duration as whole seconds, saturating at `u64::MAX`.
pub fn secs(duration: Duration) -> u64 {
    duration.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Context for Echo {
        fn namespace(&self) -> &'static str {
            "echo"
        }

        fn actions(&self) -> Vec<ActionInfo> {
            vec![
                ActionInfo::new("say", "Returns its `text` parameter")
                    .with_params([ParamInfo::string("text", true, "text to echo")])
                    .with_permissions(&["shell"]),
            ]
        }

        fn call(&self, action: &str, params: &Value) -> ContextResult {
            match action {
                "say" => Ok(params.get("text").cloned().unwrap_or(Value::Null)),
                other => Err(format!("unknown action `echo.{other}`")),
            }
        }
    }

    #[test]
    fn registry_lists_and_calls_contexts() {
        let mut registry = Registry::new();
        registry.register(Echo);

        let listed = match registry.handle(Request::List) {
            Response::Contexts { contexts, .. } => contexts,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(listed["echo"][0].name, "say");

        let response = registry.handle(Request::Call {
            namespace: "echo".into(),
            action: "say".into(),
            params: serde_json::json!({ "text": "hi" }),
            id: None,
        });
        assert_eq!(
            response,
            Response::Value {
                value: "hi".into(),
                id: None
            }
        );
    }

    #[test]
    fn unknown_namespace_and_action_are_errors() {
        let mut registry = Registry::new();
        registry.register(Echo);

        let missing_ns = registry.handle(Request::Call {
            namespace: "nope".into(),
            action: "x".into(),
            params: Value::Null,
            id: None,
        });
        assert!(matches!(missing_ns, Response::Error { .. }));

        let missing_action = registry.handle(Request::Call {
            namespace: "echo".into(),
            action: "nope".into(),
            params: Value::Null,
            id: None,
        });
        assert!(matches!(missing_action, Response::Error { .. }));
    }

    #[test]
    fn describe_reports_protocol_owner_and_argument_schema() {
        let mut registry = Registry::new();
        registry.register(Echo);

        let Response::Description {
            protocol,
            version,
            actions,
            ..
        } = registry.handle(Request::Describe)
        else {
            panic!("unexpected response");
        };
        assert_eq!(protocol, PROTOCOL);
        assert_eq!(version, PROTOCOL_VERSION);

        let say = &actions["echo"][0];
        assert_eq!(say.owner, "echo");
        assert!(say.available);
        assert_eq!(say.params.len(), 1);
        assert_eq!(say.params[0].name, "text");
        assert!(say.params[0].required);
        assert_eq!(say.permissions, vec!["shell".to_owned()]);
    }

    #[test]
    fn action_info_defaults_keep_old_metadata_working() {
        // A context that does not annotate metadata still reports a usable action: the registry
        // fills the owner and defaults availability to true.
        let mut registry = Registry::new();
        registry.register(Plain);
        let actions = registry.list();
        let plain = &actions["plain"][0];
        assert_eq!(plain.owner, "plain");
        assert!(plain.available);
        assert!(plain.params.is_empty());
        assert!(plain.permissions.is_empty());

        // Wire format stays compatible with older clients: missing metadata deserializes.
        let back: ActionInfo = serde_json::from_str(r#"{"name":"x","description":"y"}"#).unwrap();
        assert!(back.available);
        assert_eq!(back.owner, "");

        // Contexts mark compositor-dependent actions unavailable without niri.
        let unavailable = ActionInfo::new("x", "y").with_availability(false);
        assert!(!unavailable.available);
        assert!(ActionInfo::new("x", "y").with_availability(true).available);
    }

    struct Plain;

    impl Context for Plain {
        fn namespace(&self) -> &'static str {
            "plain"
        }
        fn actions(&self) -> Vec<ActionInfo> {
            vec![ActionInfo::new("noop", "Does nothing")]
        }
        fn call(&self, _action: &str, _params: &Value) -> ContextResult {
            Ok(Value::Null)
        }
    }

    #[test]
    fn request_and_response_round_trip() {
        let request = Request::Call {
            namespace: "parking".into(),
            action: "list".into(),
            params: serde_json::json!({ "lot": "main" }),
            id: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(request, back);

        let response = Response::Value {
            value: serde_json::json!({ "ok": true }),
            id: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(response, back);
    }

    #[test]
    fn legacy_json_without_new_fields_still_deserializes() {
        let call: Request =
            serde_json::from_str(r#"{"kind":"call","namespace":"echo","action":"say"}"#).unwrap();
        assert_eq!(
            call,
            Request::Call {
                namespace: "echo".into(),
                action: "say".into(),
                params: Value::Null,
                id: None,
            }
        );
        let response: Response = serde_json::from_str(r#"{"kind":"error","message":"x"}"#).unwrap();
        assert_eq!(
            response,
            Response::Error {
                message: "x".into(),
                code: None,
                id: None,
            }
        );

        // New optional fields stay out of the wire format when unused.
        let value = Response::Value {
            value: Value::Null,
            id: None,
        };
        assert_eq!(
            serde_json::to_value(&value).unwrap(),
            serde_json::json!({"kind":"value","value":null})
        );
    }

    #[test]
    fn request_ids_are_echoed_and_errors_carry_codes() {
        let mut registry = Registry::new();
        registry.register(Echo);

        let response = registry.handle(Request::Call {
            namespace: "echo".into(),
            action: "say".into(),
            params: serde_json::json!({ "text": "hi" }),
            id: Some(42),
        });
        assert_eq!(
            response,
            Response::Value {
                value: "hi".into(),
                id: Some(42)
            }
        );

        match registry.handle(Request::Call {
            namespace: "nope".into(),
            action: "x".into(),
            params: Value::Null,
            id: Some(7),
        }) {
            Response::Error { code, id, message } => {
                assert_eq!(code.as_deref(), Some("unknown_namespace"));
                assert_eq!(id, Some(7));
                assert!(message.contains("nope"));
            }
            other => panic!("unexpected: {other:?}"),
        }

        match registry.handle(Request::Call {
            namespace: "echo".into(),
            action: "nope".into(),
            params: Value::Null,
            id: None,
        }) {
            Response::Error { code, .. } => {
                assert_eq!(code.as_deref(), Some("unknown_action"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn subscribe_requests_are_rejected_off_the_stream() {
        match Registry::new().handle(Request::Subscribe {
            events: vec!["park".into()],
            id: Some(3),
        }) {
            Response::Error { code, id, .. } => {
                assert_eq!(code.as_deref(), Some("stream_required"));
                assert_eq!(id, Some(3));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn event_bus_filters_topics_sequences_and_drops_slow_subscribers() {
        let bus = EventBus::new();
        let only_park = bus.subscribe(["park".to_owned()], 8);
        assert_eq!(bus.subscriber_count(), 1);

        assert_eq!(
            bus.publish(Event::new("park", serde_json::json!({ "window": 1 }))),
            1
        );
        assert_eq!(bus.publish(Event::new("restore", Value::Null)), 0);
        let event = only_park.recv().unwrap();
        assert_eq!(event.event, "park");
        assert_eq!(event.seq, 1);
        assert_eq!(event.payload["window"], 1);
        // The restore event does not match `park`.
        assert!(only_park.try_recv().is_none());
        assert_eq!(bus.subscriber_count(), 1);

        // A queue of one drops the subscriber on the second matching event.
        let slow = bus.subscribe(["*".to_owned()], 1);
        bus.publish(Event::new("park", Value::Null));
        bus.publish(Event::new("park", Value::Null));
        assert!(slow.is_slow());
        assert_eq!(slow.dropped(), 1);
        // only_park is still there; the slow wildcard subscriber was removed.
        assert_eq!(bus.subscriber_count(), 1);
        drop(slow);
    }

    #[test]
    fn dropping_a_subscription_removes_it_immediately() {
        let bus = EventBus::new();
        let subscription = bus.subscribe(["park".to_owned()], 4);
        assert_eq!(bus.subscriber_count(), 1);
        drop(subscription);
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn event_bus_empty_topics_subscribe_to_everything() {
        let bus = EventBus::new();
        let all = bus.subscribe(Vec::<String>::new(), 4);
        bus.publish(Event::new(
            "idle-active",
            serde_json::json!({ "phase": "reset" }),
        ));
        assert_eq!(all.recv().unwrap().event, "idle-active");
        assert!(is_known_topic("park"));
        assert!(is_known_topic("*"));
        assert!(!is_known_topic("not-a-real-topic"));
    }

    #[test]
    fn dry_run_returns_steps_without_dispatching_or_recording() {
        use std::sync::Mutex as StdMutex;

        struct Recorder(Arc<StdMutex<Vec<RunRecord>>>);
        impl RunRecorder for Recorder {
            fn record(&self, record: RunRecord) {
                self.0.lock().unwrap().push(record);
            }
        }

        struct Sequence;
        impl Context for Sequence {
            fn namespace(&self) -> &'static str {
                "automation"
            }
            fn actions(&self) -> Vec<ActionInfo> {
                vec![]
            }
            fn call(&self, _action: &str, params: &Value) -> ContextResult {
                panic!("dry run must not dispatch: {params}");
            }
            fn sequence(&self, _name: &str) -> Option<Vec<Step>> {
                Some(vec![Step {
                    namespace: "echo".into(),
                    action: "say".into(),
                    params: serde_json::json!({"text": "hi"}),
                }])
            }
        }

        let records = Arc::new(StdMutex::new(Vec::new()));
        let mut registry = Registry::new();
        registry.register(Sequence);
        registry.set_recorder(Arc::new(Recorder(records.clone())));
        let response = registry.handle(Request::Call {
            namespace: "automation".into(),
            action: "run".into(),
            params: serde_json::json!({"name": "anything", "dry_run": true}),
            id: None,
        });
        match response {
            Response::Value { value, .. } => {
                assert_eq!(value["dry_run"], true);
                assert_eq!(value["steps"][0]["action"], "say");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(records.lock().unwrap().is_empty());
    }

    #[test]
    fn recorder_observes_completed_runs() {
        use std::sync::Mutex as StdMutex;

        struct Recorder(Arc<StdMutex<Vec<RunRecord>>>);
        impl RunRecorder for Recorder {
            fn record(&self, record: RunRecord) {
                self.0.lock().unwrap().push(record);
            }
        }

        struct Sequence;
        impl Context for Sequence {
            fn namespace(&self) -> &'static str {
                "automation"
            }
            fn actions(&self) -> Vec<ActionInfo> {
                vec![]
            }
            fn call(&self, _action: &str, _params: &Value) -> ContextResult {
                Ok(Value::Null)
            }
            fn sequence(&self, name: &str) -> Option<Vec<Step>> {
                (name == "greet").then(|| {
                    vec![Step {
                        namespace: "echo".into(),
                        action: "say".into(),
                        params: serde_json::json!({"text": "hi"}),
                    }]
                })
            }
        }

        let records = Arc::new(StdMutex::new(Vec::new()));
        let mut registry = Registry::new();
        registry.register(Echo);
        registry.register(Sequence);
        registry.set_recorder(Arc::new(Recorder(records.clone())));

        let response = registry.handle(Request::Call {
            namespace: "automation".into(),
            action: "run".into(),
            params: serde_json::json!({"name": "greet"}),
            id: None,
        });
        assert!(matches!(response, Response::Value { .. }), "{response:?}");
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, "automation.run");
        assert_eq!(records[0].name, "greet");
        assert_eq!(records[0].steps.len(), 1);
        assert_eq!(records[0].result["ok"], true);
    }
}
