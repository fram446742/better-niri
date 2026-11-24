//! Daemon introspection and environment helpers.

use std::path::PathBuf;
use std::time::Instant;

use companion_core::{ActionInfo, Context};
use serde_json::{Value, json};

pub struct System {
    socket: PathBuf,
    started: Instant,
}

impl System {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            started: Instant::now(),
        }
    }
}

impl Context for System {
    fn namespace(&self) -> &'static str {
        "system"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("ping", "Returns `pong`"),
            ActionInfo::new("status", "Daemon version, pid, uptime and paths"),
            ActionInfo::new("paths", "Socket and state directory paths"),
        ]
    }

    fn call(&self, action: &str, _params: &Value) -> Result<Value, String> {
        match action {
            "ping" => Ok(json!("pong")),
            "status" => Ok(json!({
                "version": companion_core::VERSION,
                "pid": std::process::id(),
                "uptime_secs": self.started.elapsed().as_secs(),
                "socket": self.socket.display().to_string(),
                "namespace": self.namespace(),
            })),
            "paths" => Ok(json!({
                "socket": self.socket.display().to_string(),
                "state_dir": companion_core::state_dir().display().to_string(),
            })),
            other => Err(format!("unknown action `system.{other}`")),
        }
    }
}
