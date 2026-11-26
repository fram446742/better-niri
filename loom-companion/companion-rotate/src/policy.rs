//! Rotation policy.
//!
//! The policy is inert until `enabled` is set. `hysteresis_ms` debounces sensor
//! noise: an orientation must stay stable for that long before auto-apply.
//! Manual `rotate.set` bypasses `enabled`/`lock`, but never bypasses `dry_run`.

use serde::{Deserialize, Serialize};

fn default_hysteresis_ms() -> u64 {
    1000
}

/// Auto-rotation configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    /// Auto-apply orientation changes.
    #[serde(default)]
    pub enabled: bool,
    /// Report what would happen without touching the compositor.
    #[serde(default)]
    pub dry_run: bool,
    /// Keep auto-rotation paused. Manual `rotate.set` still works.
    #[serde(default)]
    pub lock: bool,
    /// Output to rotate (for example `eDP-1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Command printing an orientation; when unset, iio-sensor-proxy is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_command: Option<String>,
    /// Stable time required before an orientation is applied.
    #[serde(default = "default_hysteresis_ms")]
    pub hysteresis_ms: u64,
    /// Optional logical position applied with the transform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: false,
            lock: false,
            output: None,
            sensor_command: None,
            hysteresis_ms: default_hysteresis_ms(),
            position: None,
        }
    }
}

impl Policy {
    pub fn normalize(&mut self) -> Result<(), String> {
        if self.hysteresis_ms > 60_000 {
            return Err("`hysteresis_ms` must be at most 60000".to_owned());
        }
        if let Some(output) = &self.output {
            if output.trim().is_empty() {
                return Err("`output` must not be empty".to_owned());
            }
            if output.len() > 128 {
                return Err("`output` is longer than 128 bytes".to_owned());
            }
        }
        if let Some(command) = &self.sensor_command {
            if command.trim().is_empty() {
                return Err("`sensor_command` must not be empty; use null to clear it".to_owned());
            }
            if command.len() > 4096 {
                return Err("`sensor_command` is longer than 4096 bytes".to_owned());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_inert_with_one_second_hysteresis() {
        let policy = Policy::default();
        assert!(!policy.enabled);
        assert!(!policy.dry_run);
        assert!(!policy.lock);
        assert_eq!(policy.hysteresis_ms, 1000);
        assert!(policy.output.is_none());
    }

    #[test]
    fn validates_bounds_and_empty_values() {
        let mut policy = Policy {
            hysteresis_ms: 60_001,
            ..Policy::default()
        };
        assert!(policy.normalize().is_err());

        policy.hysteresis_ms = 100;
        policy.output = Some("  ".into());
        assert!(policy.normalize().is_err());

        policy.output = Some("eDP-1".into());
        policy.sensor_command = Some(" ".into());
        assert!(policy.normalize().is_err());
        policy.sensor_command = None;
        policy.normalize().unwrap();
    }
}
