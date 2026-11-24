//! Terminal swallowing (companion Phase B).
//!
//! When a GUI window opens from a terminal, minimize the terminal while the
//! child lives and restore it when the child closes. Windows are matched
//! through their `/proc` parent chain and the terminal app-id allowlist. This
//! is opt-in (`swallow.enable`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use companion_core::{ActionInfo, Context, load_json, save_json};
use companion_parking::Parking;
use niri_ipc::{Action, Request, Response, Window};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Compositor;

pub const SWALLOW_LOT: &str = "swallow";

const DEFAULT_TERMINALS: &[&str] = &[
    "alacritty",
    "foot",
    "kitty",
    "wezterm",
    "org.wezfurlong.wezterm",
    "ghostty",
    "com.mitchellh.ghostty",
    "konsole",
    "gnome-terminal-server",
    "xterm",
];

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    enabled: bool,
    #[serde(default)]
    terminals: Vec<String>,
}

struct Inner {
    path: PathBuf,
    config: Mutex<Config>,
    parking: Parking,
    compositor: Compositor,
    parent: Arc<dyn Fn(i32) -> Option<i32> + Send + Sync>,
}

#[derive(Clone)]
pub struct Swallow {
    inner: Arc<Inner>,
}

impl Swallow {
    pub fn load(
        path: impl Into<PathBuf>,
        parking: Parking,
        compositor: Compositor,
    ) -> std::io::Result<Self> {
        Self::load_with_parent(path, parking, compositor, Arc::new(proc_parent))
    }

    /// Test hook: builds a swallow tracker with a synthetic parent resolver.
    #[doc(hidden)]
    pub fn load_with_parent(
        path: impl Into<PathBuf>,
        parking: Parking,
        compositor: Compositor,
        parent: Arc<dyn Fn(i32) -> Option<i32> + Send + Sync>,
    ) -> std::io::Result<Self> {
        let path = path.into();
        let mut config: Config = load_json(&path)?;
        if config.terminals.is_empty() {
            config.terminals = DEFAULT_TERMINALS.iter().map(|s| s.to_string()).collect();
        }
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                config: Mutex::new(config),
                parking,
                compositor,
                parent,
            }),
        })
    }

    pub fn from_state_dir(parking: Parking, compositor: Compositor) -> std::io::Result<Self> {
        Self::load(
            companion_core::state_dir().join("swallow.json"),
            parking,
            compositor,
        )
    }

    fn save(&self, config: &Config) -> Result<(), String> {
        save_json(&self.inner.path, config).map_err(|err| format!("cannot save swallow: {err}"))
    }

    fn is_enabled(&self) -> bool {
        self.inner
            .config
            .lock()
            .map(|config| config.enabled)
            .unwrap_or(false)
    }

    fn terminal_allowed(&self, app_id: Option<&str>) -> bool {
        let Ok(config) = self.inner.config.lock() else {
            return false;
        };
        app_id.is_some_and(|app_id| {
            config
                .terminals
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(app_id))
        })
    }

    fn send_action(&self, action: Action) -> Result<(), String> {
        match self.inner.compositor.send(Request::Action(action))? {
            Response::Handled => Ok(()),
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }

    fn entries(&self) -> BTreeMap<String, Value> {
        self.inner.parking.lot(SWALLOW_LOT)
    }

    /// Returns `(window id, pid, app_id, is_minimized)` for auto-tracked windows.
    fn windows(&self) -> Vec<(u64, i32, Option<String>, bool)> {
        self.inner
            .parking
            .lot(crate::AUTO_WINDOWS)
            .into_iter()
            .filter_map(|(id, value)| {
                let id = id.parse().ok()?;
                let pid = value.get("pid").and_then(Value::as_i64)? as i32;
                let app_id = value
                    .get("app_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let minimized = value
                    .get("is_minimized")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Some((id, pid, app_id, minimized))
            })
            .collect()
    }

    pub fn maybe_swallow(&self, window: &Window) -> Result<(), String> {
        if !self.is_enabled() {
            return Ok(());
        }
        let Some(child_pid) = window.pid else {
            return Ok(());
        };
        if self.entries().contains_key(&format!("child:{}", window.id)) {
            return Ok(());
        }

        let windows = self.windows();
        let mut pid = child_pid;
        for _ in 0..16 {
            let Some(parent_pid) = (self.inner.parent)(pid) else {
                break;
            };
            if parent_pid <= 1 {
                break;
            }
            if let Some((parent_id, _, app_id, minimized)) = windows
                .iter()
                .find(|(id, pid, _, _)| *id != window.id && *pid == parent_pid)
            {
                if !self.terminal_allowed(app_id.as_deref()) {
                    return Ok(());
                }
                if !*minimized {
                    self.send_action(Action::MinimizeWindow {
                        id: Some(*parent_id),
                    })?;
                }
                self.inner.parking.call(
                    "park",
                    &json!({
                        "lot": SWALLOW_LOT,
                        "id": format!("child:{}", window.id),
                        "value": {
                            "child": window.id,
                            "child_pid": child_pid,
                            "parent": parent_id,
                            "parent_pid": parent_pid,
                            "parent_app_id": app_id,
                        }
                    }),
                )?;
                return Ok(());
            }
            pid = parent_pid;
        }
        Ok(())
    }

    pub fn on_window_closed(&self, child_id: u64) -> Result<(), String> {
        let key = format!("child:{child_id}");
        if !self.entries().contains_key(&key) {
            return Ok(());
        }
        let entry = self
            .inner
            .parking
            .call("unpark", &json!({ "lot": SWALLOW_LOT, "id": key }))?;
        if let Some(parent) = entry["value"]["parent"].as_u64() {
            let parent_still_open = self.windows().iter().any(|(id, ..)| *id == parent);
            if parent_still_open {
                let _ = self.send_action(Action::UnminimizeWindow { id: Some(parent) });
            }
        }
        Ok(())
    }

    pub fn restore(&self, child_id: u64) -> Result<Value, String> {
        self.on_window_closed(child_id)?;
        Ok(json!({ "child": child_id, "restored": true }))
    }
}

impl Context for Swallow {
    fn namespace(&self) -> &'static str {
        "swallow"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("enable", "Minimize terminals while their children are open"),
            ActionInfo::new("disable", "Stop swallowing new children"),
            ActionInfo::new("status", "Enabled state, terminals and swallowed pairs"),
            ActionInfo::new("list", "List swallowed child → parent pairs"),
            ActionInfo::new("restore", "Restore the parent of swallowed `child`"),
            ActionInfo::new("add-terminal", "Allow an additional `app_id` term"),
            ActionInfo::new("remove-terminal", "Remove an `app_id` term"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        let mut config = self
            .inner
            .config
            .lock()
            .map_err(|_| "swallow config lock poisoned".to_owned())?;
        match action {
            "enable" => {
                config.enabled = true;
                self.save(&config)?;
                Ok(json!({ "enabled": true }))
            }
            "disable" => {
                config.enabled = false;
                self.save(&config)?;
                Ok(json!({ "enabled": false }))
            }
            "status" => Ok(json!({
                "enabled": config.enabled,
                "terminals": config.terminals,
                "swallowed": self.entries(),
            })),
            "list" => Ok(json!({ "swallowed": self.entries() })),
            "restore" => {
                let child = params
                    .get("child")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "swallow.restore needs a `child` window id".to_owned())?;
                self.restore(child)
            }
            "add-terminal" => {
                let app_id = params
                    .get("app_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "swallow.add-terminal needs `app_id`".to_owned())?;
                if !config.terminals.iter().any(|t| t == app_id) {
                    config.terminals.push(app_id.to_owned());
                }
                self.save(&config)?;
                Ok(json!({ "terminals": config.terminals }))
            }
            "remove-terminal" => {
                let app_id = params
                    .get("app_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "swallow.remove-terminal needs `app_id`".to_owned())?;
                config.terminals.retain(|t| t != app_id);
                self.save(&config)?;
                Ok(json!({ "terminals": config.terminals }))
            }
            other => Err(format!("unknown action `swallow.{other}`")),
        }
    }
}

/// Reads the parent pid from `/proc/<pid>/status`.
pub fn proc_parent(pid: i32) -> Option<i32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("PPid:")
            .and_then(|value| value.trim().parse().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-swallow-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn proc_parent_reads_own_process() {
        let pid = std::process::id() as i32;
        let parent = proc_parent(pid).expect("own ppid");
        assert!(parent > 0);
        assert_ne!(parent, pid);
    }

    #[test]
    fn terminal_allowlist_and_config_persist() {
        let dir = temp_dir("config");
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        let swallow = Swallow::load(
            dir.join("swallow.json"),
            parking,
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        assert!(!swallow.is_enabled());
        swallow.call("enable", &Value::Null).unwrap();
        swallow
            .call("add-terminal", &json!({ "app_id": "myterm" }))
            .unwrap();
        let status = swallow.call("status", &Value::Null).unwrap();
        assert_eq!(status["enabled"], true);
        assert!(
            status["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "myterm")
        );

        let reloaded = Swallow::load(
            dir.join("swallow.json"),
            Parking::load(dir.join("parking2.json")).unwrap(),
            Compositor::at(dir.join("unused.sock")),
        )
        .unwrap();
        assert!(reloaded.is_enabled());
        assert!(reloaded.terminal_allowed(Some("myterm")));
        assert!(reloaded.terminal_allowed(Some("ALACRITTY")));
    }
}
