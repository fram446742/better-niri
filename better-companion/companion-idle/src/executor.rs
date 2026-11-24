//! System actions used by idle timeouts.
//!
//! Lock and suspend go through logind. When the `busctl` shell-out is
//! unavailable or fails, the executor falls back to `loginctl lock-sessions`
//! and `systemctl suspend`. This keeps the crate free of a D-Bus dependency
//! (the cached `dbus` crate needs libdbus at build time). DPMS uses niri's
//! `PowerOffMonitors`/`PowerOnMonitors` actions, so an idle timeout never
//! depends on a display power tool being installed.
//!
//! Every action is explicit: the default policy is disabled and has no
//! timeouts, and no command runs unless a timeout or override names it.

use std::process::Command;

use companion_niri::Compositor;
use niri_ipc::{Action, Request, Response};

/// Pluggable executor, replaced by a recorder in tests.
pub trait IdleExecutor: Send + Sync {
    /// Locks all sessions. `Some` overrides the built-in logind call.
    fn lock(&self, command: Option<&str>) -> Result<String, String>;

    /// Suspends the machine. `Some` overrides the built-in logind call.
    fn suspend(&self, command: Option<&str>) -> Result<String, String>;

    /// Powers monitors off (`on = false`) or on through niri.
    fn dpms(&self, on: bool) -> Result<String, String>;

    /// Runs one explicitly configured shell command.
    fn shell(&self, command: &str) -> Result<String, String>;
}

/// Host executor: shell-outs to logind tools and niri IPC.
pub struct HostExecutor {
    compositor: Compositor,
}

impl Default for HostExecutor {
    fn default() -> Self {
        Self::from_env()
    }
}

impl HostExecutor {
    pub fn from_env() -> Self {
        Self {
            compositor: Compositor::from_env(),
        }
    }

    pub fn with_compositor(compositor: Compositor) -> Self {
        Self { compositor }
    }
}

impl IdleExecutor for HostExecutor {
    fn lock(&self, command: Option<&str>) -> Result<String, String> {
        if let Some(command) = command {
            return self
                .shell(command)
                .map(|detail| format!("lock command: {detail}"));
        }
        run_with_fallback(
            &[
                (
                    "busctl",
                    &[
                        "call",
                        "org.freedesktop.login1",
                        "/org/freedesktop/login1",
                        "org.freedesktop.login1.Manager",
                        "LockSessions",
                    ],
                ),
                ("loginctl", &["lock-sessions"]),
            ],
            "lock sessions",
        )
    }

    fn suspend(&self, command: Option<&str>) -> Result<String, String> {
        if let Some(command) = command {
            return self
                .shell(command)
                .map(|detail| format!("suspend command: {detail}"));
        }
        run_with_fallback(
            &[
                (
                    "busctl",
                    &[
                        "call",
                        "org.freedesktop.login1",
                        "/org/freedesktop/login1",
                        "org.freedesktop.login1.Manager",
                        "Suspend",
                        "b",
                        "false",
                    ],
                ),
                ("systemctl", &["suspend"]),
            ],
            "suspend",
        )
    }

    fn dpms(&self, on: bool) -> Result<String, String> {
        let (action, description) = if on {
            (Action::PowerOnMonitors {}, "PowerOnMonitors")
        } else {
            (
                Action::PowerOffMonitors {
                    skip_isolated: false,
                },
                "PowerOffMonitors",
            )
        };
        match self.compositor.request(Request::Action(action))? {
            Response::Handled => Ok(format!("niri {description}")),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn shell(&self, command: &str) -> Result<String, String> {
        run_shell(command)
    }
}

fn run_shell(command: &str) -> Result<String, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|err| format!("cannot run command: {err}"))?;
    if output.status.success() {
        Ok(format!("command exited with {}", output.status))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let suffix = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        };
        Err(format!("command failed with {}{suffix}", output.status))
    }
}

fn run_with_fallback(candidates: &[(&str, &[&str])], description: &str) -> Result<String, String> {
    let mut errors = Vec::new();
    for (program, args) in candidates {
        match Command::new(program).args(*args).output() {
            Ok(output) if output.status.success() => {
                return Ok(format!("{description} via {program}"));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                errors.push(format!(
                    "{program} exited with {}: {}",
                    output.status,
                    stderr.trim()
                ));
            }
            Err(err) => errors.push(format!("cannot run {program}: {err}")),
        }
    }
    Err(format!(
        "{description} failed; tried {}: {}",
        candidates
            .iter()
            .map(|(program, _)| *program)
            .collect::<Vec<_>>()
            .join(", "),
        errors.join("; ")
    ))
}

/// Convenience descriptions for documentation and status limitations.
pub fn executor_notes() -> &'static [&'static str] {
    &[
        "lock and suspend shell out to busctl, then loginctl/systemctl",
        "DPMS uses niri PowerOffMonitors/PowerOnMonitors",
        "configure lock_command/suspend_command to replace the logind calls",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_reports_success_and_failure() {
        let executor = HostExecutor::from_env();
        assert!(executor.shell("printf ok").is_ok());
        let error = executor.shell("exit 3").unwrap_err();
        assert!(error.contains("command failed"));
    }

    #[test]
    fn explicit_overrides_run_instead_of_logind() {
        let executor = HostExecutor::from_env();
        let detail = executor.lock(Some("printf lock-override")).unwrap();
        assert!(detail.contains("lock command"));
        let detail = executor.suspend(Some("printf suspend-override")).unwrap();
        assert!(detail.contains("suspend command"));
    }

    #[test]
    fn dpms_without_a_compositor_is_an_error_not_a_panic() {
        let executor = HostExecutor::from_env();
        // $NIRI_SOCKET points nowhere in the test environment.
        assert!(executor.dpms(false).is_err());
    }
}
