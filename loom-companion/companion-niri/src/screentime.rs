//! Per-application usage time, sampled from the focused niri window.
//!
//! The daemon calls `screentime.tick` once a second. Time is charged to the
//! focused window's app id; when focus moves, the previous app's slice is
//! written before the new one starts. Totals persist as seconds so the data
//! survives restarts and remains cheap to read.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use companion_core::{ActionInfo, Context, ParamInfo, load_json, save_json};
use niri_ipc::{Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Compositor;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    seconds: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Active {
    app: String,
    since_ms: u64,
}

pub struct ScreenTime {
    compositor: Compositor,
    path: PathBuf,
    state: Mutex<State>,
    active: Mutex<Option<Active>>,
}

impl ScreenTime {
    pub fn new(compositor: Compositor, path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let state = load_json(&path)?;
        Ok(Self {
            compositor,
            path,
            state: Mutex::new(state),
            active: Mutex::new(None),
        })
    }

    pub fn from_state_dir(compositor: Compositor) -> io::Result<Self> {
        Self::new(
            compositor,
            companion_core::state_dir().join("screentime.json"),
        )
    }

    fn focused_app(&self) -> Option<String> {
        match self.compositor.send(Request::FocusedWindow).ok()? {
            Response::FocusedWindow(Some(window)) => {
                Some(window.app_id.unwrap_or_else(|| "unknown".to_owned()))
            }
            _ => None,
        }
    }

    /// Charges the elapsed time to the last focused app and starts a new slice.
    fn advance(&self, app: Option<&str>, now_ms: u64) -> Result<(), String> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| "screentime state lock poisoned".to_owned())?;
        if let Some(previous) = active.take() {
            let elapsed = now_ms.saturating_sub(previous.since_ms);
            if elapsed > 0 {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "screentime state lock poisoned".to_owned())?;
                *state.seconds.entry(previous.app).or_default() += elapsed / 1000;
            }
        }
        if let Some(app) = app {
            *active = Some(Active {
                app: app.to_owned(),
                since_ms: now_ms,
            });
        }
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "screentime state lock poisoned".to_owned())?;
        save_json(&self.path, &*state).map_err(|err| format!("cannot save screentime: {err}"))
    }

    fn status_json(&self) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "screentime state lock poisoned".to_owned())?;
        let total: u64 = state.seconds.values().sum();
        let active = self
            .active
            .lock()
            .map_err(|_| "screentime state lock poisoned".to_owned())?
            .as_ref()
            .map(|active| active.app.clone());
        Ok(json!({
            "apps": state.seconds,
            "total_seconds": total,
            "active_app": active,
            "path": self.path,
        }))
    }

    fn top(&self, limit: usize) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "screentime state lock poisoned".to_owned())?;
        let mut entries: Vec<(&String, &u64)> = state.seconds.iter().collect();
        entries.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        entries.truncate(limit.min(100));
        Ok(json!({
            "apps": entries
                .into_iter()
                .map(|(app, seconds)| json!({ "app": app, "seconds": seconds }))
                .collect::<Vec<_>>(),
        }))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

impl Context for ScreenTime {
    fn namespace(&self) -> &'static str {
        "screentime"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new("tick", "Charge the current second to the focused app")
                .with_permissions(&["niri"]),
            ActionInfo::new("status", "Per-app totals, total time and the active app")
                .with_permissions(&["niri"]),
            ActionInfo::new("top", "Apps sorted by usage; optional `limit` (default 10)")
                .with_params([ParamInfo::integer("limit", false, "maximum entries")])
                .with_permissions(&["niri"]),
            ActionInfo::new("reset", "Clear all recorded usage").with_permissions(&["niri"]),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "tick" => {
                let app = self.focused_app();
                self.advance(app.as_deref(), now_ms())?;
                self.save()?;
                Ok(json!({ "ok": true, "active_app": app }))
            }
            "status" => self.status_json(),
            "top" => {
                let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
                self.top(limit)
            }
            "reset" => {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "screentime state lock poisoned".to_owned())?;
                state.seconds.clear();
                drop(state);
                if let Ok(mut active) = self.active.lock() {
                    *active = None;
                }
                self.save()?;
                Ok(json!({ "ok": true, "cleared": true }))
            }
            other => Err(format!("unknown action `screentime.{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-screentime-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("screentime.json")
    }

    fn empty() -> ScreenTime {
        ScreenTime {
            compositor: Compositor::at("/nonexistent"),
            path: temp_path("main"),
            state: Mutex::new(State::default()),
            active: Mutex::new(None),
        }
    }

    #[test]
    fn charges_time_to_the_active_app_and_switches_on_focus() {
        let time = empty();
        time.advance(Some("Alacritty"), 1_000).unwrap();
        time.advance(Some("Alacritty"), 6_000).unwrap();
        time.advance(Some("firefox"), 9_000).unwrap();
        time.advance(Some("firefox"), 12_000).unwrap();
        time.advance(None, 15_000).unwrap();
        let state = time.state.lock().unwrap();
        assert_eq!(state.seconds.get("Alacritty"), Some(&8));
        assert_eq!(state.seconds.get("firefox"), Some(&6));
    }

    #[test]
    fn status_and_top_read_persisted_totals() {
        let time = empty();
        time.advance(Some("a"), 0).unwrap();
        time.advance(Some("b"), 3_000).unwrap();
        time.advance(None, 8_000).unwrap();
        let status = time.status_json().unwrap();
        assert_eq!(status["total_seconds"], 8);
        let top = time.top(1).unwrap();
        assert_eq!(top["apps"][0]["app"], "b");
        assert_eq!(top["apps"][0]["seconds"], 5);
    }
}
