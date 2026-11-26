//! Idle policy types: timeouts, actions and inhibitor configuration.
//!
//! A policy is inert until `enabled` is set. Timeouts are sorted by
//! `after_secs` so status, explain and tick output is stable across calls.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// What a timeout does when its deadline passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdleAction {
    /// Lock every logind session.
    Lock,
    /// Power off monitors through niri's DPMS action.
    DpmsOff,
    /// Suspend through logind.
    Suspend,
    /// Run the timeout's explicit `command`.
    Command,
}

impl IdleAction {
    pub fn as_str(self) -> &'static str {
        match self {
            IdleAction::Lock => "lock",
            IdleAction::DpmsOff => "dpms-off",
            IdleAction::Suspend => "suspend",
            IdleAction::Command => "command",
        }
    }

    fn rank(self) -> u8 {
        match self {
            IdleAction::Lock => 0,
            IdleAction::DpmsOff => 1,
            IdleAction::Suspend => 2,
            IdleAction::Command => 3,
        }
    }
}

/// One policy step, reached after `after_secs` of uninterrupted idleness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timeout {
    /// Seconds of idleness before this step runs.
    pub after_secs: u64,
    /// Step to perform.
    pub action: IdleAction,
    /// Shell command for [`IdleAction::Command`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Timeout {
    /// Stable key used to remember that the step already fired.
    pub fn key(&self) -> String {
        format!("{}:{}", self.after_secs, self.action.as_str())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.after_secs == 0 {
            return Err(format!(
                "timeout `{}` needs `after_secs` greater than zero",
                self.action.as_str()
            ));
        }
        match self.action {
            IdleAction::Command => {
                let command = self
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|command| !command.is_empty())
                    .ok_or_else(|| "a `command` timeout needs a non-empty `command`".to_owned())?;
                if command.len() > 4096 {
                    return Err("timeout `command` is longer than 4096 bytes".to_owned());
                }
            }
            _ => {
                if self.command.is_some() {
                    return Err(format!(
                        "timeout `{}` does not take a `command`; use action `command`",
                        self.action.as_str()
                    ));
                }
            }
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

/// Idle configuration. Every command is optional: no shell command runs unless
/// a timeout or an override explicitly configures one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    /// A disabled policy evaluates but never executes or changes monitors.
    #[serde(default)]
    pub enabled: bool,
    /// Evaluate and report, but never execute and never mark steps as fired.
    #[serde(default)]
    pub dry_run: bool,
    /// Timeouts, sorted by `after_secs` after configuration.
    #[serde(default)]
    pub timeouts: Vec<Timeout>,
    /// Block timeouts while a non-corked PulseAudio/PipeWire sink input exists.
    #[serde(default = "default_true")]
    pub audio_inhibitor: bool,
    /// Probe overriding the built-in `pactl` check (exit 0 = inhibited,
    /// 1 = free, other = unavailable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_command: Option<String>,
    /// Block timeouts while a focused window covers an output.
    #[serde(default = "default_true")]
    pub fullscreen_inhibitor: bool,
    /// Probe overriding the niri fullscreen heuristic (same exit codes as
    /// `audio_command`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen_command: Option<String>,
    /// Override for [`IdleAction::Lock`] (defaults to logind through `busctl`,
    /// then `loginctl lock-sessions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_command: Option<String>,
    /// Override for [`IdleAction::Suspend`] (defaults to logind through
    /// `busctl`, then `systemctl suspend`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_command: Option<String>,
    /// Include logind's boolean `IdleHint` in status output. It never resets the
    /// local idle clock by itself.
    #[serde(default)]
    pub logind_idle_hint: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: false,
            timeouts: Vec::new(),
            audio_inhibitor: true,
            audio_command: None,
            fullscreen_inhibitor: true,
            fullscreen_command: None,
            lock_command: None,
            suspend_command: None,
            logind_idle_hint: false,
        }
    }
}

impl Policy {
    /// Validates and sorts timeouts in place.
    ///
    /// Two timeouts with the same deadline and action are ambiguous, so they are
    /// rejected instead of silently collapsing.
    pub fn normalize(&mut self) -> Result<(), String> {
        if let Some(command) = &self.lock_command {
            validate_override("lock_command", command)?;
        }
        if let Some(command) = &self.suspend_command {
            validate_override("suspend_command", command)?;
        }
        if let Some(command) = &self.audio_command {
            validate_override("audio_command", command)?;
        }
        if let Some(command) = &self.fullscreen_command {
            validate_override("fullscreen_command", command)?;
        }

        let mut keys = BTreeSet::new();
        for timeout in &self.timeouts {
            timeout.validate()?;
            if !keys.insert(timeout.key()) {
                return Err(format!(
                    "duplicate timeout: {} after {}s",
                    timeout.action.as_str(),
                    timeout.after_secs
                ));
            }
        }
        self.timeouts.sort_by(|left, right| {
            left.after_secs
                .cmp(&right.after_secs)
                .then_with(|| left.action.rank().cmp(&right.action.rank()))
                .then_with(|| left.command.cmp(&right.command))
        });
        Ok(())
    }
}

fn validate_override(name: &str, command: &str) -> Result<(), String> {
    if command.trim().is_empty() {
        return Err(format!("`{name}` must not be empty"));
    }
    if command.len() > 4096 {
        return Err(format!("`{name}` is longer than 4096 bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_timeouts_and_rejects_duplicates() {
        let mut policy = Policy {
            timeouts: vec![
                Timeout {
                    after_secs: 600,
                    action: IdleAction::Suspend,
                    command: None,
                },
                Timeout {
                    after_secs: 300,
                    action: IdleAction::Lock,
                    command: None,
                },
            ],
            ..Policy::default()
        };
        policy.normalize().unwrap();
        assert_eq!(
            policy
                .timeouts
                .iter()
                .map(|timeout| timeout.after_secs)
                .collect::<Vec<_>>(),
            vec![300, 600]
        );

        policy.timeouts.push(Timeout {
            after_secs: 300,
            action: IdleAction::Lock,
            command: None,
        });
        assert!(policy.normalize().is_err());
    }

    #[test]
    fn command_timeouts_require_a_command() {
        let mut policy = Policy {
            timeouts: vec![Timeout {
                after_secs: 30,
                action: IdleAction::Command,
                command: None,
            }],
            ..Policy::default()
        };
        assert!(policy.normalize().is_err());

        policy.timeouts[0].command = Some("printf ok".into());
        policy.normalize().unwrap();
    }

    #[test]
    fn built_in_actions_reject_a_command() {
        let mut policy = Policy {
            timeouts: vec![Timeout {
                after_secs: 30,
                action: IdleAction::Lock,
                command: Some("not used".into()),
            }],
            ..Policy::default()
        };
        assert!(policy.normalize().is_err());
    }
}
