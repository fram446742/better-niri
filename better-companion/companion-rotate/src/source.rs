//! Pluggable orientation source.
//!
//! Resolution order:
//!
//! 1. `sensor_command` from the policy, if configured (shell command printing an orientation or
//!    `90`/`180`/`270`),
//! 2. `iio-sensor-proxy` over D-Bus, read with `busctl`,
//! 3. unavailable.
//!
//! The D-Bus path is a shell-out because the cached `dbus` crate requires
//! libdbus at build time; this keeps the daemon's build unchanged. A missing
//! proxy, `busctl` or command is reported as a source error and never panics.

use std::process::Command;

use crate::Orientation;
use crate::policy::Policy;

/// One orientation source, replaceable in tests and by embedders.
pub trait OrientationSource: Send + Sync {
    fn read(&self, policy: &Policy) -> Result<Orientation, String>;
    fn describe(&self, policy: &Policy) -> String;
}

/// Built-in source: configured command, then iio-sensor-proxy.
#[derive(Debug, Default)]
pub struct AutoSource;

impl OrientationSource for AutoSource {
    fn read(&self, policy: &Policy) -> Result<Orientation, String> {
        if let Some(command) = &policy.sensor_command {
            return read_command(command);
        }
        read_iio_sensor_proxy()
    }

    fn describe(&self, policy: &Policy) -> String {
        match &policy.sensor_command {
            Some(_) => "sensor_command".to_owned(),
            None => "iio-sensor-proxy via busctl".to_owned(),
        }
    }
}

fn read_command(command: &str) -> Result<Orientation, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|err| format!("cannot run sensor_command: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "sensor_command exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value = stdout.trim();
    if value.is_empty() {
        return Err("sensor_command printed no orientation".to_owned());
    }
    Orientation::parse(value)
}

fn read_iio_sensor_proxy() -> Result<Orientation, String> {
    let output = Command::new("busctl")
        .args([
            "--system",
            "get-property",
            "net.hadess.SensorProxy",
            "/net/hadess/SensorProxy",
            "net.hadess.SensorProxy",
            "AccelerometerOrientation",
        ])
        .output()
        .map_err(|err| {
            format!(
                "no orientation source: configure sensor_command or install iio-sensor-proxy ({err})"
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "iio-sensor-proxy is unavailable (busctl exited with {}): {}",
            output.status,
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_busctl_string(&stdout)
        .ok_or_else(|| format!("unexpected iio-sensor-proxy reply: {}", stdout.trim()))
}

/// Extracts the string from a `busctl get-property` reply such as
/// `s "left-up"`.
pub fn parse_busctl_string(reply: &str) -> Option<Orientation> {
    let reply = reply.trim();
    let value = if let Some(rest) = reply.strip_prefix("s ") {
        rest.trim()
    } else {
        reply
            .rsplit_once(' ')
            .map_or(reply, |(_, value)| value.trim())
    };
    let value = value.trim_matches('"').trim();
    Orientation::parse(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_busctl_string_replies() {
        assert_eq!(
            parse_busctl_string("s \"normal\""),
            Some(Orientation::Normal)
        );
        assert_eq!(
            parse_busctl_string("s \"left-up\"\n"),
            Some(Orientation::LeftUp)
        );
        assert_eq!(parse_busctl_string("s \"nope\""), None);
    }

    #[test]
    fn configured_command_is_used_and_parsed() {
        let policy = Policy {
            sensor_command: Some("printf 'left-up\\n'".into()),
            ..Policy::default()
        };
        // `printf` in sh receives the escaped newline; the source trims it.
        let source = AutoSource;
        assert_eq!(source.read(&policy), Ok(Orientation::LeftUp));
        assert_eq!(source.describe(&policy), "sensor_command");
    }

    #[test]
    fn failing_command_reports_an_error() {
        let policy = Policy {
            sensor_command: Some("exit 4".into()),
            ..Policy::default()
        };
        let error = AutoSource.read(&policy).unwrap_err();
        assert!(error.contains("sensor_command exited"));
    }

    #[test]
    fn missing_tool_degrades_to_an_error() {
        let policy = Policy {
            sensor_command: Some("definitely-not-a-command-better-companion".into()),
            ..Policy::default()
        };
        assert!(AutoSource.read(&policy).is_err());
    }
}
