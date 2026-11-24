//! Shared protocol, registry and filesystem helpers for `better-companion`.
//!
//! The daemon is one process with bounded-context crates. Every context owns a
//! hierarchical action namespace (`parking.*`, `automation.*`, `hooks.*`,
//! `system.*`, …); [`Registry`] merges them and serves the JSON-line protocol
//! over a Unix socket.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fs, io};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of the daemon socket protocol. Clients can compare this and
/// [`PROTOCOL_VERSION`] from [`Request::Describe`].
pub const PROTOCOL: &str = "better-companion";
/// Current version of the daemon socket protocol.
pub const PROTOCOL_VERSION: u32 = 1;

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
    },
    /// Daemon status.
    Status,
    /// Describe the protocol, its version and every action with owner and argument schema.
    Describe,
}

/// A response on the daemon socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Contexts {
        contexts: BTreeMap<String, Vec<ActionInfo>>,
    },
    Value {
        value: Value,
    },
    Status {
        version: String,
        pid: u32,
        uptime_secs: u64,
        socket: String,
        contexts: Vec<String>,
    },
    Error {
        message: String,
    },
    /// Protocol description and full action registry.
    Description {
        protocol: String,
        version: u32,
        actions: BTreeMap<String, Vec<ActionInfo>>,
    },
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

/// The merged action registry.
pub struct Registry {
    contexts: BTreeMap<String, Box<dyn Context>>,
    observer: Option<Arc<dyn CallObserver>>,
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
            },
            Request::Call {
                namespace,
                action,
                params,
            } => match self.call(&namespace, &action, &params) {
                Ok(value) => Response::Value { value },
                Err(message) => Response::Error { message },
            },
            Request::Describe => Response::Description {
                protocol: PROTOCOL.to_owned(),
                version: PROTOCOL_VERSION,
                actions: self.list(),
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
            },
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

        let results = self.dispatch_steps(steps, "automation.run")?;
        Ok(serde_json::json!({ "name": name, "results": results }))
    }

    fn run_rules(&self, params: &Value) -> ContextResult {
        let window = params.get("window").and_then(Value::as_u64);
        let context = self
            .contexts
            .get("rules")
            .ok_or_else(|| "rules context is not registered".to_owned())?;
        let steps = context
            .matched_steps(window)
            .ok_or_else(|| "rules context did not provide rule evaluation".to_owned())??;
        let results = self.dispatch_steps(steps, "rules.run")?;
        Ok(serde_json::json!({
            "window": window,
            "results": results,
        }))
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

/// The daemon socket path: `$XDG_RUNTIME_DIR/better-companion.sock`.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("better-companion.sock")
}

/// Persistent state directory: `$XDG_STATE_HOME/better-companion` or
/// `~/.local/state/better-companion`.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("better-companion");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".local/state/better-companion")
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
            Response::Contexts { contexts } => contexts,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(listed["echo"][0].name, "say");

        let response = registry.handle(Request::Call {
            namespace: "echo".into(),
            action: "say".into(),
            params: serde_json::json!({ "text": "hi" }),
        });
        assert_eq!(response, Response::Value { value: "hi".into() });
    }

    #[test]
    fn unknown_namespace_and_action_are_errors() {
        let mut registry = Registry::new();
        registry.register(Echo);

        let missing_ns = registry.handle(Request::Call {
            namespace: "nope".into(),
            action: "x".into(),
            params: Value::Null,
        });
        assert!(matches!(missing_ns, Response::Error { .. }));

        let missing_action = registry.handle(Request::Call {
            namespace: "echo".into(),
            action: "nope".into(),
            params: Value::Null,
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
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(request, back);

        let response = Response::Value {
            value: serde_json::json!({ "ok": true }),
        };
        let json = serde_json::to_string(&response).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(response, back);
    }
}
