//! `better-monitor`: output profiles with safe apply and rollback.
//!
//! Profiles are JSON snapshots of output settings (mode, scale, transform,
//! position, VRR and on/off). `apply` stores the current state as `__previous`
//! first, so `restore` can undo a bad profile. `--dry-run` prints the planned
//! niri output actions without sending them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use niri_ipc::{
    ConfiguredMode, ConfiguredPosition, ModeToSet, Output, OutputAction, PositionToSet, ScaleToSet,
    Transform, VrrToSet,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutputProfile {
    #[serde(default)]
    pub off: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<(u16, u16, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vrr: Option<bool>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputProfile>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

/// State path: `$XDG_STATE_HOME/better-monitor/profiles.json` or
/// `~/.local/state/better-monitor/profiles.json`.
pub fn state_path() -> PathBuf {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".local/state")
        });
    dir.join("better-monitor/profiles.json")
}

impl Store {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, path)
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn insert(&mut self, name: impl Into<String>, profile: Profile) {
        self.profiles.insert(name.into(), profile);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.profiles.remove(name).is_some()
    }
}

/// Captures the currently applied output settings.
pub fn snapshot(outputs: &BTreeMap<String, Output>) -> Profile {
    let mut profile = Profile::default();
    for (name, output) in outputs {
        let mut entry = OutputProfile::default();
        if let Some(logical) = output.logical {
            entry.off = false;
            entry.scale = Some(logical.scale);
            entry.transform = Some(logical.transform);
            entry.position = Some((logical.x, logical.y));
            if let Some(index) = output.current_mode {
                if let Some(mode) = output.modes.get(index) {
                    entry.mode = Some((mode.width, mode.height, mode.refresh_rate));
                }
            }
            entry.vrr = Some(output.vrr_enabled);
        } else {
            entry.off = true;
        }
        profile.outputs.insert(name.clone(), entry);
    }
    profile
}

/// Builds the output actions that apply `profile`, in a safe order.
pub fn plan(profile: &Profile) -> Vec<(String, OutputAction)> {
    let mut actions = Vec::new();
    for (name, output) in &profile.outputs {
        actions.push((
            name.clone(),
            if output.off {
                OutputAction::Off
            } else {
                OutputAction::On
            },
        ));
        if output.off {
            continue;
        }
        if let Some((width, height, refresh_rate)) = output.mode {
            actions.push((
                name.clone(),
                OutputAction::Mode {
                    mode: ModeToSet::Specific(ConfiguredMode {
                        width,
                        height,
                        refresh: Some(refresh_rate as f64 / 1000.),
                    }),
                },
            ));
        }
        if let Some(scale) = output.scale {
            actions.push((
                name.clone(),
                OutputAction::Scale {
                    scale: ScaleToSet::Specific(scale),
                },
            ));
        }
        if let Some(transform) = output.transform {
            actions.push((name.clone(), OutputAction::Transform { transform }));
        }
        if let Some((x, y)) = output.position {
            actions.push((
                name.clone(),
                OutputAction::Position {
                    position: PositionToSet::Specific(ConfiguredPosition { x, y }),
                },
            ));
        }
        if let Some(vrr) = output.vrr {
            actions.push((
                name.clone(),
                OutputAction::Vrr {
                    vrr: VrrToSet {
                        vrr,
                        on_demand: false,
                    },
                },
            ));
        }
    }
    actions
}

/// Sends one output action to niri.
pub fn send_action(socket: &Path, output: &str, action: OutputAction) -> Result<(), String> {
    use niri_ipc::socket::Socket;
    use niri_ipc::{Reply, Request, Response};

    let mut socket =
        Socket::connect_to(socket).map_err(|err| format!("cannot connect to niri IPC: {err}"))?;
    let reply: Reply = socket
        .send(Request::Output {
            output: output.to_owned(),
            action,
        })
        .map_err(|err| format!("niri output request: {err}"))?;
    match reply.map_err(|err| format!("niri returned an error: {err}"))? {
        Response::OutputConfigChanged(_) => Ok(()),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

/// Queries the current outputs.
pub fn query_outputs(socket: &Path) -> Result<BTreeMap<String, Output>, String> {
    use niri_ipc::socket::Socket;
    use niri_ipc::{Reply, Request, Response};

    let mut socket =
        Socket::connect_to(socket).map_err(|err| format!("cannot connect to niri IPC: {err}"))?;
    let reply: Reply = socket
        .send(Request::Outputs)
        .map_err(|err| format!("niri outputs request: {err}"))?;
    match reply.map_err(|err| format!("niri returned an error: {err}"))? {
        Response::Outputs(outputs) => Ok(outputs.into_iter().collect()),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

/// Applies a profile, storing the current state under `__previous` first.
pub fn apply(
    socket: &Path,
    store: &mut Store,
    name: &str,
    path: &Path,
    dry_run: bool,
) -> Result<Vec<(String, Value)>, String> {
    let profile = store
        .get(name)
        .cloned()
        .ok_or_else(|| format!("unknown profile `{name}`"))?;

    let current = query_outputs(socket)?;
    let previous = snapshot(&current);
    let actions = plan(&profile);

    let planned: Vec<(String, Value)> = actions
        .iter()
        .map(|(output, action)| {
            (
                output.clone(),
                serde_json::to_value(action).unwrap_or(Value::Null),
            )
        })
        .collect();

    if dry_run {
        return Ok(planned);
    }

    store.insert("__previous", previous);
    store
        .save(path)
        .map_err(|err| format!("cannot save profiles: {err}"))?;

    for (output, action) in actions {
        send_action(socket, &output, action)?;
    }
    Ok(planned)
}

/// Restores the `__previous` profile.
pub fn restore(
    socket: &Path,
    store: &mut Store,
    path: &Path,
    dry_run: bool,
) -> Result<Vec<(String, Value)>, String> {
    if store.get("__previous").is_none() {
        return Err("no previous profile; apply one first".to_owned());
    }
    apply(socket, store, "__previous", path, dry_run)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use niri_ipc::{LogicalOutput, Mode, Output};

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("better-monitor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("profiles.json")
    }

    fn output(name: &str, scale: f64) -> Output {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "make": "test",
            "model": "test",
            "serial": null,
            "physical_size": null,
            "modes": [
                { "width": 1920, "height": 1080, "refresh_rate": 60000, "is_preferred": true }
            ],
            "current_mode": 0,
            "is_custom_mode": false,
            "vrr_supported": true,
            "vrr_enabled": true,
            "logical": {
                "x": 0,
                "y": 0,
                "width": 1920,
                "height": 1080,
                "scale": scale,
                "transform": "Normal"
            },
            "max_bpc": null
        }))
        .unwrap()
    }

    #[test]
    fn snapshots_outputs_and_plans_actions() {
        let outputs: BTreeMap<_, _> = [("eDP-1".to_owned(), output("eDP-1", 1.5))]
            .into_iter()
            .collect();
        let profile = snapshot(&outputs);
        let entry = &profile.outputs["eDP-1"];
        assert_eq!(entry.scale, Some(1.5));
        assert_eq!(entry.mode, Some((1920, 1080, 60000)));
        assert_eq!(entry.vrr, Some(true));

        let actions = plan(&profile);
        assert_eq!(actions.len(), 6);
        assert!(matches!(actions[0].1, OutputAction::On));
        assert!(matches!(
            actions[1].1,
            OutputAction::Mode {
                mode: ModeToSet::Specific(ConfiguredMode {
                    width: 1920,
                    height: 1080,
                    ..
                })
            }
        ));
        assert!(matches!(actions[5].1, OutputAction::Vrr { .. }));
    }

    #[test]
    fn off_outputs_only_emit_off() {
        let mut profile = Profile::default();
        profile.outputs.insert(
            "HDMI-1".to_owned(),
            OutputProfile {
                off: true,
                scale: Some(2.),
                ..OutputProfile::default()
            },
        );
        let actions = plan(&profile);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0].1, OutputAction::Off));
    }

    #[test]
    fn store_round_trips_and_removes() {
        let path = temp_path("store");
        let mut store = Store::default();
        store.insert("desk", Profile::default());
        store.save(&path).unwrap();
        let mut reloaded = Store::load(&path).unwrap();
        assert!(reloaded.get("desk").is_some());
        assert!(reloaded.remove("desk"));
        reloaded.save(&path).unwrap();
        assert!(Store::load(&path).unwrap().get("desk").is_none());
    }

    #[test]
    fn snapshots_disabled_output() {
        let mut out = output("DP-1", 1.);
        out.logical = None;
        let outputs: BTreeMap<_, _> = [("DP-1".to_owned(), out)].into_iter().collect();
        let profile = snapshot(&outputs);
        assert!(profile.outputs["DP-1"].off);
    }

    #[test]
    fn logical_output_type_is_public() {
        // Keep the import used and document the shape we rely on.
        let logical = LogicalOutput {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
            scale: 1.,
            transform: Transform::Normal,
        };
        assert_eq!(logical.x, 1);
        let mode = Mode {
            width: 1920,
            height: 1080,
            refresh_rate: 60000,
            is_preferred: true,
        };
        assert_eq!(mode.height, 1080);
    }
}
