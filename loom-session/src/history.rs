//! Bounded session history: timestamped context snapshots under
//! `$XDG_STATE_HOME/loom-session/history/`.
//!
//! Every entry is one JSON file, written atomically. Listing skips malformed
//! files and reports them instead of failing the whole command, so a single
//! truncated file cannot hide the rest of the history.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Context, now_secs, state_path, write_atomic};

/// Default number of history entries kept after a snapshot.
pub const DEFAULT_HISTORY_KEEP: usize = 50;

/// Version of the history entry schema.
pub const HISTORY_VERSION: u32 = 1;

/// Default history directory, next to `contexts.json`.
pub fn history_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LOOM_SESSION_HISTORY") {
        return PathBuf::from(dir);
    }
    state_path()
        .parent()
        .map(|parent| parent.join("history"))
        .unwrap_or_else(|| PathBuf::from("history"))
}

/// One recorded context snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    #[serde(default = "default_version")]
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub trigger: String,
    pub context: Context,
}

fn default_version() -> u32 {
    HISTORY_VERSION
}

/// Compact listing row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HistorySummary {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub trigger: String,
    pub windows: usize,
    pub workspaces: usize,
}

/// Result of listing a history directory.
#[derive(Debug, Default)]
pub struct HistoryListing {
    pub entries: Vec<HistoryEntry>,
    /// Files that could not be read or parsed, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
}

impl HistoryListing {
    pub fn summaries(&self) -> Vec<HistorySummary> {
        self.entries
            .iter()
            .map(|entry| HistorySummary {
                id: entry.id.clone(),
                name: entry.name.clone(),
                created_at: entry.created_at,
                trigger: entry.trigger.clone(),
                windows: entry.context.windows.len(),
                workspaces: entry.context.workspaces.len(),
            })
            .collect()
    }
}

/// A history directory.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    dir: PathBuf,
}

impl Default for HistoryStore {
    fn default() -> Self {
        Self::new(history_dir())
    }
}

impl HistoryStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Returns a timestamp/sequence pair that sorts after every existing entry.
    ///
    /// This keeps ids monotonic even when pruning removes the lowest sequence
    /// for the current millisecond, or when the clock moves backwards.
    fn next_id(&self) -> (u128, u32) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return (crate::now_millis(), 0);
        };
        let mut max_millis = 0u128;
        let mut max_sequence = 0u32;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some((prefix, suffix)) = stem.split_once('-') else {
                continue;
            };
            let (Ok(prefix), Ok(suffix)) = (prefix.parse::<u128>(), suffix.parse::<u32>()) else {
                continue;
            };
            if prefix > max_millis {
                max_millis = prefix;
                max_sequence = suffix;
            } else if prefix == max_millis {
                max_sequence = max_sequence.max(suffix);
            }
        }
        let now = crate::now_millis();
        if max_millis >= now {
            (max_millis, max_sequence.saturating_add(1))
        } else {
            (now, 0)
        }
    }

    /// All entries, newest first. Malformed files are skipped and reported.
    pub fn load(&self) -> io::Result<HistoryListing> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(HistoryListing::default());
            }
            Err(err) => return Err(err),
        };
        let mut listing = HistoryListing::default();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    listing
                        .skipped
                        .push((self.dir.clone(), format!("cannot read dir entry: {err}")));
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    listing.skipped.push((path, format!("read: {err}")));
                    continue;
                }
            };
            match serde_json::from_slice::<HistoryEntry>(&bytes) {
                Ok(entry) if entry.version <= HISTORY_VERSION => {
                    listing.entries.push(entry);
                }
                Ok(entry) => listing.skipped.push((
                    path,
                    format!(
                        "unsupported history schema version {} (supports {HISTORY_VERSION})",
                        entry.version
                    ),
                )),
                Err(err) => listing.skipped.push((path, format!("parse: {err}"))),
            }
        }
        listing.entries.sort_by(|left, right| {
            right
                .id
                .cmp(&left.id)
                .then(right.created_at.cmp(&left.created_at))
        });
        Ok(listing)
    }

    /// Writes one snapshot and prunes the store to `keep` entries.
    pub fn record(
        &self,
        name: &str,
        trigger: &str,
        context: &Context,
        keep: usize,
    ) -> io::Result<HistoryEntry> {
        std::fs::create_dir_all(&self.dir)?;
        let created_at = now_secs();
        let (millis, mut sequence) = self.next_id();
        let id = loop {
            let candidate = format!("{millis:013}-{sequence:04}");
            let path = self.entry_path(&candidate);
            if !path.exists() {
                break candidate;
            }
            sequence = sequence.saturating_add(1);
            if sequence == u32::MAX {
                return Err(io::Error::other("cannot allocate a history id"));
            }
        };
        let entry = HistoryEntry {
            version: HISTORY_VERSION,
            id: id.clone(),
            name: name.to_owned(),
            created_at,
            trigger: trigger.to_owned(),
            context: context.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&entry)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        write_atomic(&self.entry_path(&id), &bytes)?;
        self.prune(keep.max(1), None)?;
        Ok(entry)
    }

    /// Finds an entry by exact id, else the newest entry for `name`.
    pub fn find(&self, key: &str) -> io::Result<Option<HistoryEntry>> {
        let listing = self.load()?;
        if let Some(entry) = listing.entries.iter().find(|entry| entry.id == key) {
            return Ok(Some(entry.clone()));
        }
        Ok(listing.entries.into_iter().find(|entry| entry.name == key))
    }

    /// Removes entries beyond `keep` and optionally older than `max_age_secs`.
    ///
    /// Returns the removed ids, oldest first. Missing directories are fine.
    pub fn prune(&self, keep: usize, max_age_secs: Option<u64>) -> io::Result<Vec<String>> {
        let listing = self.load()?;
        let now = now_secs();
        let mut removed = Vec::new();
        for (index, entry) in listing.entries.iter().enumerate() {
            let too_many = index >= keep;
            let too_old =
                max_age_secs.is_some_and(|max_age| now.saturating_sub(entry.created_at) > max_age);
            if too_many || too_old {
                match std::fs::remove_file(self.entry_path(&entry.id)) {
                    Ok(()) => removed.push(entry.id.clone()),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        removed.push(entry.id.clone());
                    }
                    Err(err) => {
                        return Err(io::Error::new(
                            err.kind(),
                            format!("cannot prune history entry {}: {err}", entry.id),
                        ));
                    }
                }
            }
        }
        removed.sort();
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Hooks, WorkspaceSnapshot};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-session-history-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn context(marker: u64) -> Context {
        Context {
            created_at: marker,
            workspaces: vec![WorkspaceSnapshot {
                id: marker,
                name: Some(format!("ws-{marker}")),
                idx: 1,
                ..WorkspaceSnapshot::default()
            }],
            hooks: Hooks::default(),
            ..Context::default()
        }
    }

    #[test]
    fn records_lists_and_finds_by_id_and_name() {
        let store = HistoryStore::new(temp_dir("record"));
        let first = store.record("work", "manual", &context(1), 10).unwrap();
        let second = store.record("work", "auto", &context(2), 10).unwrap();
        assert_ne!(first.id, second.id);

        let listing = store.load().unwrap();
        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].id, second.id, "newest first");
        assert!(listing.skipped.is_empty());

        assert_eq!(
            store.find(&first.id).unwrap().unwrap().context.created_at,
            1
        );
        assert_eq!(store.find("work").unwrap().unwrap().id, second.id);
        assert!(store.find("missing").unwrap().is_none());
    }

    #[test]
    fn record_prunes_to_bounded_retention() {
        let store = HistoryStore::new(temp_dir("prune"));
        for index in 0..6 {
            store.record("work", "auto", &context(index), 3).unwrap();
        }
        let listing = store.load().unwrap();
        assert_eq!(listing.entries.len(), 3);
        assert_eq!(listing.entries[0].context.created_at, 5);
        assert_eq!(listing.entries[2].context.created_at, 3);
    }

    #[test]
    fn malformed_entries_are_skipped_not_fatal() {
        let dir = temp_dir("malformed");
        let store = HistoryStore::new(&dir);
        store.record("work", "manual", &context(1), 10).unwrap();
        std::fs::write(dir.join("broken.json"), b"{not json").unwrap();
        std::fs::write(dir.join("ignored.txt"), b"text").unwrap();
        let listing = store.load().unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.skipped.len(), 1);
    }

    #[test]
    fn prune_can_remove_by_age() {
        let dir = temp_dir("age");
        let store = HistoryStore::new(&dir);
        let entry = store.record("old", "manual", &context(1), 10).unwrap();
        let path = dir.join(format!("{}.json", entry.id));
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["created_at"] = serde_json::json!(now_secs().saturating_sub(10));
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let removed = store.prune(10, Some(0)).unwrap();
        assert_eq!(removed, vec![entry.id]);
        assert!(store.load().unwrap().entries.is_empty());
    }
}
