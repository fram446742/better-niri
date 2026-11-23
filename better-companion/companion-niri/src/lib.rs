//! Compositor bridge: drives and inspects niri over its IPC socket.
//!
//! Actions that the companion wants to perform on the compositor go through
//! `compositor.action`; queries map to the matching niri IPC requests. This is
//! the first compositor-side integration of the companion daemon.

use std::path::PathBuf;

use companion_core::{ActionInfo, Context};
use niri_ipc::socket::Socket;
use niri_ipc::{Action, Reply, Request, Response};
use serde_json::{json, Value};

pub struct Compositor {
    socket: Option<PathBuf>,
}

impl Default for Compositor {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Compositor {
    /// Uses `$NIRI_SOCKET` at call time.
    pub fn from_env() -> Self {
        Self { socket: None }
    }

    /// Uses an explicit socket path (useful for tests and nested sessions).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            socket: Some(path.into()),
        }
    }

    fn connect(&self) -> Result<Socket, String> {
        match &self.socket {
            Some(path) => Socket::connect_to(path),
            None => Socket::connect(),
        }
        .map_err(|err| format!("cannot connect to niri IPC: {err}"))
    }

    fn send(&self, request: Request) -> Result<Response, String> {
        let reply: Reply = self
            .connect()?
            .send(request)
            .map_err(|err| format!("niri IPC error: {err}"))?;
        reply.map_err(|err| format!("niri returned an error: {err}"))
    }

    fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
        serde_json::to_value(value).map_err(|err| format!("cannot encode niri response: {err}"))
    }
}

fn parse_action(params: &Value) -> Result<Action, String> {
    let action = params
        .get("action")
        .ok_or_else(|| "compositor.action needs an `action` object".to_owned())?;
    serde_json::from_value(action.clone()).map_err(|err| format!("invalid niri action JSON: {err}"))
}

impl Context for Compositor {
    fn namespace(&self) -> &'static str {
        "compositor"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("action", "Perform a niri `Action` given as `action` JSON"),
            ActionInfo::new("outputs", "List connected outputs"),
            ActionInfo::new("workspaces", "List workspaces"),
            ActionInfo::new("windows", "List windows"),
            ActionInfo::new("focused-window", "Focused window, if any"),
            ActionInfo::new("version", "Running niri version"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "action" => {
                let action = parse_action(params)?;
                match self.send(Request::Action(action))? {
                    Response::Handled => Ok(json!({ "handled": true })),
                    other => Self::to_value(other),
                }
            }
            "outputs" => match self.send(Request::Outputs)? {
                Response::Outputs(outputs) => Self::to_value(outputs),
                other => Self::to_value(other),
            },
            "workspaces" => match self.send(Request::Workspaces)? {
                Response::Workspaces(workspaces) => Self::to_value(workspaces),
                other => Self::to_value(other),
            },
            "windows" => match self.send(Request::Windows)? {
                Response::Windows(windows) => Self::to_value(windows),
                other => Self::to_value(other),
            },
            "focused-window" => match self.send(Request::FocusedWindow)? {
                Response::FocusedWindow(window) => Self::to_value(window),
                other => Self::to_value(other),
            },
            "version" => match self.send(Request::Version)? {
                Response::Version(version) => Ok(json!(version)),
                other => Self::to_value(other),
            },
            other => Err(format!("unknown action `compositor.{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::mpsc;

    use niri_ipc::Request;
    use serde_json::json;

    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-niri-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("niri.sock")
    }

    /// Starts a fake niri that answers one request with `reply`.
    fn fake_niri(
        path: &PathBuf,
        reply: &'static str,
    ) -> (
        std::thread::JoinHandle<Request>,
        std::sync::mpsc::Receiver<()>,
    ) {
        let listener = UnixListener::bind(path).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            let mut stream = stream;
            stream.write_all(reply.as_bytes()).unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
            request
        });
        (handle, ready_rx)
    }

    #[test]
    fn parses_action_json() {
        let action = parse_action(&json!({ "action": { "FocusColumnRight": {} } })).unwrap();
        assert!(matches!(action, Action::FocusColumnRight {}));
        assert!(parse_action(&json!({})).is_err());
        assert!(parse_action(&json!({ "action": "nope" })).is_err());
    }

    #[test]
    fn sends_action_and_reads_version() {
        let path = temp_socket("version");
        let (handle, ready) = fake_niri(&path, r#"{"Ok":{"Version":"26.4-test"}}"#);
        ready.recv().unwrap();
        let compositor = Compositor::at(&path);
        let value = compositor.call("version", &Value::Null).unwrap();
        assert_eq!(value, json!("26.4-test"));
        let request = handle.join().unwrap();
        assert!(matches!(request, Request::Version));
    }

    #[test]
    fn sends_compositor_action_over_ipc() {
        let path = temp_socket("action");
        let (handle, ready) = fake_niri(&path, r#"{"Ok":"Handled"}"#);
        ready.recv().unwrap();
        let compositor = Compositor::at(&path);
        let value = compositor
            .call("action", &json!({ "action": { "FocusColumnRight": {} } }))
            .unwrap();
        assert_eq!(value["handled"], true);
        let request = handle.join().unwrap();
        assert!(matches!(
            request,
            Request::Action(Action::FocusColumnRight {})
        ));
    }

    #[test]
    fn reports_niri_errors() {
        let path = temp_socket("error");
        let (handle, ready) = fake_niri(&path, r#"{"Err":"boom"}"#);
        ready.recv().unwrap();
        let compositor = Compositor::at(&path);
        let err = compositor.call("version", &Value::Null).unwrap_err();
        assert!(err.contains("boom"), "{err}");
        handle.join().unwrap();
    }
}
