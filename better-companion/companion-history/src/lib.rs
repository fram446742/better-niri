//! Bounded history of completed rule and automation runs.
//!
//! The registry records `automation.run` and `rules.run` through
//! [`companion_core::RunRecorder`]. Entries keep timestamp, source, sequence or
//! rule label, the expanded steps and the outcome. The log is newest-first,
//! bounded (256 by default) and persisted to
//! `$XDG_STATE_HOME/better-companion/history.json` so it survives a restart.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use companion_core::{
    ActionInfo, Context, ParamInfo, RunRecord, RunRecorder, load_json, save_json,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Default number of retained runs.
pub const DEFAULT_CAPACITY: usize = 256;
/// Largest `limit` accepted by `history.list`.
pub const MAX_LIST_LIMIT: usize = 1000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    records: Vec<RunRecord>,
}

pub struct History {
    path: Option<PathBuf>,
    capacity: usize,
    records: Mutex<VecDeque<RunRecord>>,
}

impl History {
    /// In-memory history (used by tests and embedders).
    pub fn new(capacity: usize) -> Self {
        Self {
            path: None,
            capacity: capacity.max(1),
            records: Mutex::new(VecDeque::new()),
        }
    }

    /// Loads persisted history from `path`.
    pub fn load(path: impl Into<PathBuf>, capacity: usize) -> std::io::Result<Self> {
        let path = path.into();
        let state: State = load_json(&path)?;
        let mut records: VecDeque<RunRecord> = state.records.into();
        while records.len() > capacity.max(1) {
            records.pop_back();
        }
        Ok(Self {
            path: Some(path),
            capacity: capacity.max(1),
            records: Mutex::new(records),
        })
    }

    pub fn from_state_dir() -> std::io::Result<Self> {
        Self::load(
            companion_core::state_dir().join("history.json"),
            DEFAULT_CAPACITY,
        )
    }

    fn save(&self, records: &VecDeque<RunRecord>) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let state = State {
            records: records.iter().cloned().collect(),
        };
        save_json(path, &state).map_err(|err| format!("cannot save history: {err}"))
    }

    /// Adds one record, evicting the oldest when at capacity.
    pub fn push(&self, record: RunRecord) {
        if let Ok(mut records) = self.records.lock() {
            records.push_front(record);
            while records.len() > self.capacity {
                records.pop_back();
            }
            let _ = self.save(&records);
        }
    }

    /// Newest-first records, at most `limit`.
    pub fn list(&self, limit: usize) -> Vec<RunRecord> {
        let limit = limit.min(MAX_LIST_LIMIT);
        self.records
            .lock()
            .map(|records| records.iter().take(limit).cloned().collect())
            .unwrap_or_default()
    }

    /// Number of retained records.
    pub fn len(&self) -> usize {
        self.records
            .lock()
            .map(|records| records.len())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut records) = self.records.lock() {
            records.clear();
            let _ = self.save(&records);
        }
    }
}

impl RunRecorder for History {
    fn record(&self, record: RunRecord) {
        self.push(record);
    }
}

impl Context for History {
    fn namespace(&self) -> &'static str {
        "history"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new(
                "list",
                "Recent rule/automation runs, newest first, each with timestamp, source, steps and result",
            )
            .with_params([ParamInfo::integer(
                "limit",
                false,
                "maximum entries to return (default 32, max 1000)",
            )]),
            ActionInfo::new("clear", "Remove every history entry"),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "list" => {
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|limit| usize::try_from(limit).unwrap_or(MAX_LIST_LIMIT))
                    .unwrap_or(32);
                let records = self.list(limit);
                Ok(json!({
                    "records": records,
                    "count": records.len(),
                    "total": self.len(),
                    "capacity": self.capacity,
                }))
            }
            "clear" => {
                let cleared = self.len();
                self.clear();
                Ok(json!({ "cleared": cleared }))
            }
            other => Err(format!("unknown action `history.{other}`")),
        }
    }
}

/// Path used by the daemon for persistent history.
pub fn default_path() -> PathBuf {
    companion_core::state_dir().join("history.json")
}

/// Loads a history from an explicit path.
pub fn load_at(path: &Path) -> std::io::Result<History> {
    History::load(path, DEFAULT_CAPACITY)
}

#[cfg(test)]
mod tests {
    use companion_core::Step;
    use serde_json::json;

    use super::*;

    fn record(name: &str, ok: bool) -> RunRecord {
        RunRecord {
            timestamp_ms: 1_700_000_000_000,
            source: "automation.run".into(),
            name: name.into(),
            steps: vec![Step {
                namespace: "system".into(),
                action: "ping".into(),
                params: Value::Null,
            }],
            result: if ok {
                json!({ "ok": true, "value": null })
            } else {
                json!({ "ok": false, "error": "boom" })
            },
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-history-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history.json")
    }

    #[test]
    fn keeps_newest_first_and_bounds_the_log() {
        let history = History::new(2);
        history.push(record("one", true));
        history.push(record("two", true));
        history.push(record("three", false));
        assert_eq!(history.len(), 2);
        let listed = history.call("list", &Value::Null).unwrap();
        assert_eq!(listed["count"], 2);
        assert_eq!(listed["records"][0]["name"], "three");
        assert_eq!(listed["records"][1]["name"], "two");
        assert_eq!(listed["records"][0]["result"]["ok"], false);
        assert_eq!(listed["capacity"], 2);
    }

    #[test]
    fn list_limit_and_clear_work() {
        let history = History::new(16);
        for index in 0..5 {
            history.push(record(&format!("s{index}"), true));
        }
        let listed = history.call("list", &json!({ "limit": 2 })).unwrap();
        assert_eq!(listed["count"], 2);
        assert_eq!(listed["total"], 5);

        let cleared = history.call("clear", &Value::Null).unwrap();
        assert_eq!(cleared["cleared"], 5);
        assert!(history.is_empty());
    }

    #[test]
    fn persists_across_reload_and_ignores_over_capacity_records() {
        let path = temp_path("reload");
        let history = History::load(&path, 3).unwrap();
        for index in 0..5 {
            history.push(record(&format!("s{index}"), true));
        }

        let reloaded = load_at(&path).unwrap();
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded.list(10)[0].name, "s4");
        assert_eq!(reloaded.list(10)[2].name, "s2");
    }

    #[test]
    fn recorder_impl_feeds_the_context() {
        let history = History::new(4);
        RunRecorder::record(&history, record("direct", true));
        assert_eq!(history.list(1)[0].name, "direct");
    }
}
