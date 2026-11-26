//! Command templates, environment variables, process execution and the
//! per-adapter concurrency gate.
//!
//! Event values come from the compositor and from window titles, so template
//! substitution always shell-quotes them. A title containing `; rm -rf` stays a
//! single argument instead of becoming shell syntax.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub command: String,
    pub unresolved: Vec<String>,
}

/// Expands `{{field}}` placeholders from `payload`.
pub fn expand_template(template: &str, payload: &Value) -> Expansion {
    let mut command = String::with_capacity(template.len());
    let mut unresolved = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        command.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            unresolved.push(after.trim().to_owned());
            command.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let token = after[..end].trim();
        if token.is_empty() {
            command.push_str("{{}}");
        } else {
            match payload.get(token) {
                Some(value) => command.push_str(&shell_quote(&scalar_text(value))),
                None => {
                    unresolved.push(token.to_owned());
                    command.push_str("''");
                }
            }
        }
        rest = &after[end + 2..];
    }
    command.push_str(rest);
    Expansion {
        command,
        unresolved,
    }
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(_) | Value::Number(_) => value.to_string(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| String::new())
        }
    }
}

/// POSIX single-quote escaping; every value becomes one shell word.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Renders the payload as `COMPANION_*` environment variables.
pub fn environment(adapter: &str, event: &str, payload: &Value) -> Vec<(String, String)> {
    let mut envs = BTreeMap::new();
    envs.insert("COMPANION_ADAPTER".to_owned(), adapter.to_owned());
    envs.insert("COMPANION_EVENT".to_owned(), event.to_owned());
    envs.insert(
        "COMPANION_EVENT_JSON".to_owned(),
        serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned()),
    );
    if let Some(object) = payload.as_object() {
        for (key, value) in object {
            let name = format!("COMPANION_{}", env_name(key));
            let text = match value {
                Value::Null => String::new(),
                other => scalar_text(other),
            };
            envs.insert(name, text);
        }
    }
    envs.into_iter().collect()
}

fn env_name(key: &str) -> String {
    let mut name = String::with_capacity(key.len());
    for character in key.chars() {
        if character.is_ascii_alphanumeric() {
            name.push(character.to_ascii_uppercase());
        } else {
            name.push('_');
        }
    }
    name
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration_ms: u64,
}

/// Runs one command through `sh -c`, optionally killing it after `timeout_ms`.
pub fn run_command(command: &str, timeout_ms: u64, envs: &[(String, String)]) -> CommandOutcome {
    let started = Instant::now();
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .envs(envs.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return CommandOutcome {
                success: false,
                code: None,
                stdout: String::new(),
                stderr: format!("cannot spawn sh: {err}"),
                timed_out: false,
                duration_ms: started.elapsed().as_millis() as u64,
            };
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_pipe(stdout));
    let stderr_reader = std::thread::spawn(move || read_pipe(stderr));

    let timeout = Duration::from_millis(timeout_ms.max(1));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                let _ = child.kill();
                break child.wait().ok();
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    CommandOutcome {
        success: status.is_some_and(|status| status.success()) && !timed_out,
        code: status.and_then(|status| status.code()),
        stdout,
        stderr,
        timed_out,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

fn read_pipe(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Bounded concurrency for one adapter's command executions.
pub struct Gate {
    limit: AtomicUsize,
    active: Mutex<usize>,
    changed: Condvar,
    peak: AtomicUsize,
}

impl Gate {
    pub fn new(limit: usize) -> Self {
        Self {
            limit: AtomicUsize::new(limit.max(1)),
            active: Mutex::new(0),
            changed: Condvar::new(),
            peak: AtomicUsize::new(0),
        }
    }

    pub fn set_limit(&self, limit: usize) {
        self.limit.store(limit.max(1), Ordering::Relaxed);
        self.changed.notify_all();
    }

    pub fn acquire(&self) {
        let limit = self.limit.load(Ordering::Relaxed).max(1);
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        while *active >= limit {
            active = match self.changed.wait(active) {
                Ok(active) => active,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
        *active += 1;
        let current = *active;
        self.peak.fetch_max(current, Ordering::Relaxed);
    }

    pub fn release(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = active.saturating_sub(1);
            self.changed.notify_all();
        }
    }

    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn expansion_quotes_values_and_reports_missing_fields() {
        let expansion = expand_template(
            "echo {{workspace_name}} {{workspace_index}} {{focused}} {{missing}}",
            &json!({
                "workspace_name": "dev",
                "workspace_index": 2,
                "focused": true
            }),
        );
        assert_eq!(expansion.command, "echo 'dev' '2' 'true' ''");
        assert_eq!(expansion.unresolved, ["missing"]);
    }

    #[test]
    fn shell_quotes_stay_one_word() {
        let payload = json!({ "title": "x'; touch /tmp/pwned; echo '" });
        let expansion = expand_template("printf %s {{title}}", &payload);
        assert!(expansion.command.contains("'\\''"));
        assert_eq!(expansion.unresolved, Vec::<String>::new());
    }

    #[test]
    fn environment_is_derived_from_payload_fields() {
        let envs = environment(
            "audio",
            "window-focused",
            &json!({ "app_id": "firefox", "focused": true, "window_id": 4 }),
        );
        let envs: BTreeMap<_, _> = envs.into_iter().collect();
        assert_eq!(envs["COMPANION_ADAPTER"], "audio");
        assert_eq!(envs["COMPANION_EVENT"], "window-focused");
        assert_eq!(envs["COMPANION_APP_ID"], "firefox");
        assert_eq!(envs["COMPANION_FOCUSED"], "true");
        assert_eq!(envs["COMPANION_WINDOW_ID"], "4");
    }

    #[test]
    fn timeout_kills_a_long_command() {
        let started = Instant::now();
        let outcome = run_command("sleep 2", 50, &[]);
        assert!(outcome.timed_out);
        assert!(!outcome.success);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn gate_serializes_at_limit_one_and_tracks_peak() {
        let gate = std::sync::Arc::new(Gate::new(1));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let gate = gate.clone();
            handles.push(std::thread::spawn(move || {
                gate.acquire();
                std::thread::sleep(Duration::from_millis(20));
                gate.release();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(gate.peak(), 1);
    }
}
