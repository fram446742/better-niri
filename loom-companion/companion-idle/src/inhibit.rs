//! Inhibitors that keep an idle policy from firing.
//!
//! Three sources exist:
//!
//! - user-requested (persisted by `idle.inhibit`, evaluated by the context),
//! - audio: a non-corked PulseAudio/PipeWire sink input,
//! - fullscreen: a focused window that covers an output.
//!
//! Audio and fullscreen accept an explicit probe command. Their built-in
//! probes are read-only (`pactl`, niri IPC) and never act on the session. A
//! probe that cannot run is reported as `unavailable` and does **not** block
//! timeouts, so a missing tool cannot silently disable idle policy.

use std::collections::HashMap;
use std::process::Command;

use companion_niri::Compositor;
use niri_ipc::{Output, Request, Response, Transform, Window};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::policy::Policy;

/// Result of one inhibitor probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum InhibitState {
    /// The inhibitor is turned off in policy.
    Disabled,
    /// The inhibitor is enabled and the condition is absent.
    Inactive,
    /// The condition is present; execution is blocked.
    Active {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// The probe could not run. This does not block execution.
    Unavailable { detail: String },
}

impl InhibitState {
    pub fn is_active(&self) -> bool {
        matches!(self, InhibitState::Active { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            InhibitState::Disabled => "disabled",
            InhibitState::Inactive => "inactive",
            InhibitState::Active { .. } => "active",
            InhibitState::Unavailable { .. } => "unavailable",
        }
    }
}

/// One pluggable inhibitor.
pub trait Inhibitor: Send + Sync {
    fn check(&self, policy: &Policy) -> InhibitState;
}

/// Built-in audio probe: a sink input that is not corked means audio plays.
///
/// Exit codes are the interface: 0 = active, 1 = inactive, anything else =
/// unavailable. The `pactl` probe uses 2 when `pactl` is missing.
pub const AUDIO_PROBE: &str = "command -v pactl >/dev/null 2>&1 || exit 2; \
     pactl list sink-inputs 2>/dev/null | grep -q \"Corked: no\"";

/// Audio inhibitor.
#[derive(Debug, Default)]
pub struct AudioInhibitor;

impl Inhibitor for AudioInhibitor {
    fn check(&self, policy: &Policy) -> InhibitState {
        if !policy.audio_inhibitor {
            return InhibitState::Disabled;
        }
        let command = policy.audio_command.as_deref().unwrap_or(AUDIO_PROBE);
        probe_command("audio", command)
    }
}

/// Fullscreen inhibitor.
///
/// niri's IPC has no `is_fullscreen` flag, so the built-in heuristic treats a
/// focused window whose tile covers an output's logical size as fullscreen
/// (four logical pixels of tolerance, both orientations).
#[derive(Default)]
pub struct FullscreenInhibitor {
    compositor: Option<Compositor>,
}

impl FullscreenInhibitor {
    pub fn from_env() -> Self {
        Self {
            compositor: Some(Compositor::from_env()),
        }
    }

    pub fn with_compositor(compositor: Compositor) -> Self {
        Self {
            compositor: Some(compositor),
        }
    }
}

impl Inhibitor for FullscreenInhibitor {
    fn check(&self, policy: &Policy) -> InhibitState {
        if !policy.fullscreen_inhibitor {
            return InhibitState::Disabled;
        }
        if let Some(command) = &policy.fullscreen_command {
            return probe_command("fullscreen", command);
        }
        let Some(compositor) = &self.compositor else {
            return InhibitState::Unavailable {
                detail: "no niri IPC client configured".to_owned(),
            };
        };
        match focused_fullscreen(compositor) {
            Ok(Some(detail)) => InhibitState::Active {
                detail: Some(detail),
            },
            Ok(None) => InhibitState::Inactive,
            Err(err) => InhibitState::Unavailable { detail: err },
        }
    }
}

fn focused_fullscreen(compositor: &Compositor) -> Result<Option<String>, String> {
    let windows = match compositor.request(Request::Windows) {
        Ok(Response::Windows(windows)) => windows,
        Ok(other) => return Err(format!("unexpected niri response: {other:?}")),
        Err(err) => return Err(err),
    };
    let focused: Vec<&Window> = windows
        .iter()
        .filter(|window| window.is_focused && !window.is_minimized)
        .collect();
    if focused.is_empty() {
        return Ok(None);
    }
    let outputs = match compositor.request(Request::Outputs) {
        Ok(Response::Outputs(outputs)) => outputs,
        Ok(other) => return Err(format!("unexpected niri response: {other:?}")),
        Err(err) => return Err(err),
    };
    for window in focused {
        if let Some(output) = covering_output(window, &outputs) {
            return Ok(Some(format!(
                "focused window {} covers output {output}",
                window.id
            )));
        }
    }
    Ok(None)
}

/// Output covered by a window, if any.
///
/// Pure so the heuristic is testable without a compositor.
pub fn covering_output(window: &Window, outputs: &HashMap<String, Output>) -> Option<String> {
    if window.is_minimized {
        return None;
    }
    let tolerance = 4.0;
    for (name, output) in outputs {
        let Some(logical) = output.logical else {
            continue;
        };
        let width = f64::from(logical.width);
        let height = f64::from(logical.height);
        let (tile_width, tile_height) = window.layout.tile_size;
        let (window_width, window_height) = (
            f64::from(window.layout.window_size.0),
            f64::from(window.layout.window_size.1),
        );
        let portrait = matches!(logical.transform, Transform::_90 | Transform::_270);
        let (first_width, first_height) = if portrait {
            (height, width)
        } else {
            (width, height)
        };
        for (width, height) in [(first_width, first_height), (width, height)] {
            if covers(tile_width, tile_height, width, height, tolerance)
                || covers(window_width, window_height, width, height, tolerance)
            {
                return Some(name.clone());
            }
        }
    }
    None
}

fn covers(width: f64, height: f64, output_width: f64, output_height: f64, tolerance: f64) -> bool {
    (width - output_width).abs() <= tolerance && (height - output_height).abs() <= tolerance
}

fn probe_command(kind: &str, command: &str) -> InhibitState {
    match Command::new("sh").arg("-c").arg(command).output() {
        Ok(output) if output.status.success() => InhibitState::Active {
            detail: Some(format!("{kind} probe reported active")),
        },
        Ok(output) => match output.status.code() {
            Some(1) => InhibitState::Inactive,
            code => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                let suffix = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                };
                InhibitState::Unavailable {
                    detail: format!(
                        "{kind} probe exited with {}{suffix}",
                        code.map_or_else(|| "a signal".to_owned(), |code| code.to_string())
                    ),
                }
            }
        },
        Err(err) => InhibitState::Unavailable {
            detail: format!("cannot run {kind} probe: {err}"),
        },
    }
}

/// Renders an inhibitor state for status output.
pub fn state_json(state: &InhibitState) -> Value {
    serde_json::to_value(state).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn window(tile: (f64, f64), window_size: (i32, i32), focused: bool) -> Window {
        serde_json::from_value(json!({
            "id": 7,
            "title": "test",
            "app_id": "test",
            "pid": 1,
            "workspace_id": 1,
            "is_focused": focused,
            "is_floating": false,
            "is_sticky": false,
            "is_urgent": false,
            "is_minimized": false,
            "layout": {
                "pos_in_scrolling_layout": [1, 1],
                "tile_size": [tile.0, tile.1],
                "window_size": [window_size.0, window_size.1],
                "tile_pos_in_workspace_view": [0.0, 0.0],
                "window_offset_in_tile": [0.0, 0.0]
            },
            "focus_timestamp": null
        }))
        .unwrap()
    }

    fn output(width: u32, height: u32, transform: Transform) -> Output {
        serde_json::from_value(json!({
            "name": "eDP-1",
            "make": "test",
            "model": "test",
            "serial": null,
            "physical_size": null,
            "modes": [],
            "current_mode": null,
            "is_custom_mode": false,
            "vrr_supported": false,
            "vrr_enabled": false,
            "logical": {
                "x": 0,
                "y": 0,
                "width": width,
                "height": height,
                "scale": 1.0,
                "transform": transform
            },
            "max_bpc": null
        }))
        .unwrap()
    }

    fn outputs(output: Output) -> HashMap<String, Output> {
        HashMap::from([("eDP-1".to_owned(), output)])
    }

    #[test]
    fn detects_a_window_that_covers_an_output() {
        let outputs = outputs(output(1920, 1080, Transform::Normal));
        let full = window((1920.0, 1080.0), (1920, 1080), true);
        assert_eq!(covering_output(&full, &outputs).as_deref(), Some("eDP-1"));

        let half = window((960.0, 1080.0), (960, 1080), true);
        assert_eq!(covering_output(&half, &outputs), None);
    }

    #[test]
    fn accepts_a_rotated_output_and_window_geometry() {
        let outputs = outputs(output(1920, 1080, Transform::_90));
        let rotated = window((1080.0, 1920.0), (100, 100), true);
        assert_eq!(
            covering_output(&rotated, &outputs).as_deref(),
            Some("eDP-1")
        );

        let native = window((1.0, 1.0), (1920, 1080), true);
        assert_eq!(covering_output(&native, &outputs).as_deref(), Some("eDP-1"));
    }

    #[test]
    fn ignores_minimized_windows() {
        let outputs = outputs(output(1920, 1080, Transform::Normal));
        let mut minimized = window((1920.0, 1080.0), (1920, 1080), true);
        minimized.is_minimized = true;
        assert_eq!(covering_output(&minimized, &outputs), None);
    }

    #[test]
    fn audio_inhibitor_obeys_policy_and_exit_codes() {
        let inhibitor = AudioInhibitor;
        let mut policy = Policy {
            audio_command: Some("exit 0".into()),
            ..Policy::default()
        };
        assert!(inhibitor.check(&policy).is_active());

        policy.audio_command = Some("exit 1".into());
        assert_eq!(inhibitor.check(&policy), InhibitState::Inactive);

        policy.audio_command = Some("exit 2".into());
        assert_eq!(inhibitor.check(&policy).label(), "unavailable");

        policy.audio_inhibitor = false;
        assert_eq!(inhibitor.check(&policy), InhibitState::Disabled);
    }

    #[test]
    fn fullscreen_inhibitor_uses_an_explicit_command() {
        let policy = Policy {
            fullscreen_command: Some("exit 0".into()),
            ..Policy::default()
        };
        assert!(FullscreenInhibitor::default().check(&policy).is_active());
    }

    #[test]
    fn missing_probe_tool_is_unavailable_not_active() {
        let policy = Policy {
            audio_command: Some("definitely-not-a-command-loom-companion".into()),
            ..Policy::default()
        };
        let state = AudioInhibitor.check(&policy);
        assert_eq!(state.label(), "unavailable");
        assert!(!state.is_active());
    }
}
