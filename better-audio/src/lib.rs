//! `better-audio`: rule-based audio routing for PipeWire.
//!
//! Rules match an application (case-insensitive substring or `*`/`?` glob) and
//! name a target sink (exact name, or a unique substring), plus an optional
//! per-stream profile (volume and mute). `apply` moves every matching playback
//! stream whose current sink differs and applies the profile; `restore` puts
//! streams back where they were before the first rule moved them.
//!
//! Three interchangeable backends are available (see [`backends`]):
//! native `libpipewire-0.3`, `pactl`, and a CLI fallback built on `pw-dump`,
//! `pw-cli` and `wpctl`. [`backends::select`] probes them in order and degrades
//! gracefully when one is unavailable.
//!
//! Streams without a matching rule are left alone.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod backends;

pub use backends::{AudioBackend, BackendConfig, BackendKind, BackendStatus, Selection};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sink {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    pub id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_id: Option<u32>,
    pub app: String,
    #[serde(default)]
    pub media_name: String,
    /// Current linear volume (1.0 == 100%), when the backend reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Case-insensitive app match; `*` matches anything, otherwise a substring.
    pub app: String,
    /// Target sink name, or a unique substring of it.
    pub sink: String,
    #[serde(default)]
    pub priority: i32,
    /// Optional linear volume (1.0 == 100%).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
}

/// Where a stream was before a rule moved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreEntry {
    pub app: String,
    #[serde(default)]
    pub media_name: String,
    pub sink: String,
}

impl RestoreEntry {
    pub fn key(&self) -> String {
        restore_key(&self.app, &self.media_name)
    }
}

/// Streams are matched across runs by app name and media name; node ids change.
pub fn restore_key(app: &str, media_name: &str) -> String {
    format!("{app}\u{1f}{media_name}")
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Store {
    #[serde(default)]
    pub default_sink: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Original sinks remembered by the first successful move.
    #[serde(default)]
    pub restore: Vec<RestoreEntry>,
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_default();
    base.join("better-audio/rules.json")
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

    /// Adds restore entries that are not already remembered.
    pub fn remember(&mut self, entries: impl IntoIterator<Item = RestoreEntry>) {
        let mut known: BTreeSet<String> = self.restore.iter().map(RestoreEntry::key).collect();
        for entry in entries {
            if known.insert(entry.key()) {
                self.restore.push(entry);
            }
        }
    }

    /// Drops restore entries by key.
    pub fn forget(&mut self, keys: &BTreeSet<String>) {
        self.restore.retain(|entry| !keys.contains(&entry.key()));
    }

    pub fn restore_entry(&self, app: &str, media_name: &str) -> Option<&RestoreEntry> {
        let key = restore_key(app, media_name);
        self.restore.iter().find(|entry| entry.key() == key)
    }
}

/// Case-insensitive glob match supporting `*` and `?`, or a substring match
/// when the pattern contains no wildcard.
pub fn pattern_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    let text = text.to_lowercase();
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return text.contains(&pattern);
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut p = 0;
    let mut t = 0;
    let mut star = None;
    let mut resume = 0;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Picks the winning rule: highest priority first, then declaration order.
pub fn resolve_rule<'a>(rules: &'a [Rule], app: &str) -> Option<&'a Rule> {
    let mut candidates: Vec<(usize, &Rule)> = rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| pattern_matches(&rule.app, app))
        .collect();
    candidates.sort_by(|(index_a, a), (index_b, b)| {
        b.priority.cmp(&a.priority).then(index_a.cmp(index_b))
    });
    candidates.first().map(|(_, rule)| *rule)
}

/// Resolves a sink reference: exact name first, then a unique substring.
pub fn resolve_sink<'a>(sinks: &'a [Sink], reference: &str) -> Option<&'a Sink> {
    if let Some(exact) = sinks.iter().find(|sink| sink.name == reference) {
        return Some(exact);
    }
    let needle = reference.to_lowercase();
    let matches: Vec<&Sink> = sinks
        .iter()
        .filter(|sink| sink.name.to_lowercase().contains(&needle))
        .collect();
    if matches.len() == 1 {
        matches.first().copied()
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub stream_id: u32,
    pub app: String,
    pub from: Option<u32>,
    pub to: u32,
    pub sink: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub stream_id: u32,
    pub app: String,
    pub volume: Option<f32>,
    pub mute: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    pub moves: Vec<Move>,
    pub profiles: Vec<Profile>,
    pub default: Option<(String, String)>,
    /// Streams that matched a rule whose sink does not exist.
    pub unresolved: Vec<String>,
    /// Original sinks to remember for `restore` once the moves succeed.
    pub remember: Vec<RestoreEntry>,
}

const VOLUME_EPSILON: f32 = 0.005;

fn volume_differs(current: Option<f32>, target: f32) -> bool {
    current.is_none_or(|current| (current - target).abs() > VOLUME_EPSILON)
}

/// Computes the moves and profile changes needed to satisfy the rules.
pub fn plan(
    store: &Store,
    sinks: &[Sink],
    streams: &[Stream],
    current_default: Option<&str>,
) -> Plan {
    let mut plan = Plan::default();
    let mut unresolved = BTreeSet::new();
    let mut remembered: BTreeSet<String> = store.restore.iter().map(RestoreEntry::key).collect();

    for stream in streams {
        let Some(rule) = resolve_rule(&store.rules, &stream.app) else {
            continue;
        };
        let Some(sink) = resolve_sink(sinks, &rule.sink) else {
            unresolved.insert(format!("{} -> {}", stream.app, rule.sink));
            continue;
        };

        if stream.sink_id != Some(sink.id) {
            if !remembered.contains(&restore_key(&stream.app, &stream.media_name))
                && let Some(from) = stream
                    .sink_id
                    .and_then(|id| sinks.iter().find(|sink| sink.id == id))
                && from.name != sink.name
            {
                let entry = RestoreEntry {
                    app: stream.app.clone(),
                    media_name: stream.media_name.clone(),
                    sink: from.name.clone(),
                };
                remembered.insert(entry.key());
                plan.remember.push(entry);
            }
            plan.moves.push(Move {
                stream_id: stream.id,
                app: stream.app.clone(),
                from: stream.sink_id,
                to: sink.id,
                sink: sink.name.clone(),
            });
        }

        let volume = rule
            .volume
            .filter(|target| volume_differs(stream.volume, *target));
        let mute = rule.mute.filter(|target| stream.mute != Some(*target));
        if volume.is_some() || mute.is_some() {
            plan.profiles.push(Profile {
                stream_id: stream.id,
                app: stream.app.clone(),
                volume,
                mute,
            });
        }
    }
    plan.unresolved = unresolved.into_iter().collect();

    if let Some(wanted) = store.default_sink.as_deref()
        && let Some(sink) = resolve_sink(sinks, wanted)
        && current_default != Some(sink.name.as_str())
    {
        plan.default = Some((
            current_default.unwrap_or_default().to_owned(),
            sink.name.clone(),
        ));
    }
    plan
}

/// A restore operation derived from [`Store::restore`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub moves: Vec<Move>,
    pub unresolved: Vec<String>,
    /// Restore entries that can be dropped afterwards (restored or stale).
    pub done: BTreeSet<String>,
}

impl RestorePlan {
    pub fn into_plan(self) -> Plan {
        Plan {
            moves: self.moves,
            ..Plan::default()
        }
    }
}

/// Plans moving remembered streams back to their original sinks.
///
/// Entries whose stream is gone are reported and dropped; entries whose sink is
/// missing are reported and kept for a later attempt.
pub fn plan_restore(store: &Store, sinks: &[Sink], streams: &[Stream]) -> RestorePlan {
    let mut plan = RestorePlan::default();
    for entry in &store.restore {
        let key = entry.key();
        let matching: Vec<&Stream> = streams
            .iter()
            .filter(|stream| restore_key(&stream.app, &stream.media_name) == key)
            .collect();
        if matching.is_empty() {
            plan.unresolved
                .push(format!("{} ({}) is gone", entry.app, entry.media_name));
            plan.done.insert(key);
            continue;
        }
        let Some(sink) = resolve_sink(sinks, &entry.sink) else {
            plan.unresolved
                .push(format!("{} -> {} (missing)", entry.app, entry.sink));
            continue;
        };
        plan.done.insert(key);
        for stream in matching {
            if stream.sink_id == Some(sink.id) {
                continue;
            }
            plan.moves.push(Move {
                stream_id: stream.id,
                app: stream.app.clone(),
                from: stream.sink_id,
                to: sink.id,
                sink: sink.name.clone(),
            });
        }
    }
    plan
}

/// Applies a plan through the backend. With `dry_run`, nothing is executed.
pub fn execute(
    backend: &dyn AudioBackend,
    plan: &Plan,
    dry_run: bool,
) -> Result<Vec<String>, String> {
    let mut log = Vec::new();
    for movement in &plan.moves {
        if dry_run {
            log.push(format!(
                "move {} ({}) -> {} [{}]",
                movement.stream_id, movement.app, movement.sink, movement.to
            ));
        } else {
            backend.move_stream(movement.stream_id, &movement.sink)?;
            log.push(format!(
                "moved {} ({}) -> {}",
                movement.stream_id, movement.app, movement.sink
            ));
        }
    }
    for profile in &plan.profiles {
        if let Some(volume) = profile.volume {
            if dry_run {
                log.push(format!(
                    "volume {} ({}) -> {:.0}%",
                    profile.stream_id,
                    profile.app,
                    volume * 100.0
                ));
            } else {
                backend.set_stream_volume(profile.stream_id, volume)?;
                log.push(format!(
                    "volume {} ({}) -> {:.0}%",
                    profile.stream_id,
                    profile.app,
                    volume * 100.0
                ));
            }
        }
        if let Some(mute) = profile.mute {
            if dry_run {
                log.push(format!(
                    "mute {} ({}) -> {}",
                    profile.stream_id,
                    profile.app,
                    if mute { "on" } else { "off" }
                ));
            } else {
                backend.set_stream_mute(profile.stream_id, mute)?;
                log.push(format!(
                    "mute {} ({}) -> {}",
                    profile.stream_id,
                    profile.app,
                    if mute { "on" } else { "off" }
                ));
            }
        }
    }
    if let Some((from, to)) = &plan.default {
        if !dry_run {
            backend.set_default_sink(to)?;
        }
        log.push(format!("default {from} -> {to}"));
    }
    Ok(log)
}

/// Queries the backend and applies the current rules once.
pub fn apply_once(
    backend: &dyn AudioBackend,
    store: &Store,
    dry_run: bool,
) -> Result<(Plan, Vec<String>), String> {
    let sinks = backend.sinks()?;
    let streams = backend.streams()?;
    let default = backend.default_sink()?;
    let plan = plan(store, &sinks, &streams, default.as_deref());
    let log = execute(backend, &plan, dry_run)?;
    Ok((plan, log))
}

/// Executes a restore plan (moves only) and returns its log.
pub fn execute_restore(
    backend: &dyn AudioBackend,
    restore: &RestorePlan,
    dry_run: bool,
) -> Result<Vec<String>, String> {
    execute(backend, &restore.clone().into_plan(), dry_run)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use self::backend_fakes::FakeBackend;
    use super::*;

    fn sinks() -> Vec<Sink> {
        vec![
            Sink {
                id: 1,
                name: "alsa_output.usb-headset".to_owned(),
                description: "Headset".to_owned(),
            },
            Sink {
                id: 2,
                name: "alsa_output.hdmi".to_owned(),
                description: "HDMI".to_owned(),
            },
            Sink {
                id: 3,
                name: "bluez_output.headphones".to_owned(),
                description: "BT".to_owned(),
            },
        ]
    }

    fn stream(id: u32, app: &str, sink: u32) -> Stream {
        Stream {
            id,
            sink_id: Some(sink),
            app: app.to_owned(),
            media_name: String::new(),
            volume: None,
            mute: None,
        }
    }

    fn rule(app: &str, sink: &str, priority: i32) -> Rule {
        Rule {
            app: app.to_owned(),
            sink: sink.to_owned(),
            priority,
            volume: None,
            mute: None,
        }
    }

    #[test]
    fn wildcard_and_substring_matching() {
        assert!(pattern_matches("spotify", "Spotify"));
        assert!(pattern_matches("fire*", "Firefox"));
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("fire?ox", "Firefox"));
        assert!(!pattern_matches("fire*", "Chromium"));
        assert!(!pattern_matches("", "anything"));
    }

    #[test]
    fn highest_priority_rule_wins_then_declaration_order() {
        let rules = vec![
            rule("*", "hdmi", 0),
            rule("spotify", "headset", 10),
            rule("spot*", "hdmi", 10),
        ];
        assert_eq!(resolve_rule(&rules, "Spotify").unwrap().sink, "headset");
        assert_eq!(resolve_rule(&rules, "Chromium").unwrap().sink, "hdmi");
        assert!(resolve_rule(&[], "Spotify").is_none());
    }

    #[test]
    fn sinks_resolve_exactly_then_by_unique_substring() {
        let sinks = sinks();
        assert_eq!(resolve_sink(&sinks, "alsa_output.hdmi").unwrap().id, 2);
        assert_eq!(resolve_sink(&sinks, "bluez").unwrap().id, 3);
        assert!(resolve_sink(&sinks, "alsa_output").is_none());
        assert!(resolve_sink(&sinks, "missing").is_none());
    }

    #[test]
    fn plan_moves_only_mismatched_streams_and_remembers_origin() {
        let store = Store {
            default_sink: Some("headset".to_owned()),
            rules: vec![
                rule("spotify", "headset", 0),
                rule("game", "hdmi", 0),
                rule("missing-app", "nope", 0),
            ],
            restore: vec![],
        };
        let streams = vec![
            stream(10, "Spotify", 2),
            stream(11, "Game", 2),
            stream(12, "Chromium", 1),
            stream(13, "missing-app", 1),
        ];
        let plan = plan(&store, &sinks(), &streams, Some("alsa_output.hdmi"));
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.moves[0].stream_id, 10);
        assert_eq!(plan.moves[0].to, 1);
        assert_eq!(plan.moves[0].from, Some(2));
        assert_eq!(plan.default.as_ref().unwrap().1, "alsa_output.usb-headset");
        assert_eq!(plan.unresolved, ["missing-app -> nope"]);
        // The first move records where Spotify came from.
        assert_eq!(plan.remember.len(), 1);
        assert_eq!(plan.remember[0].sink, "alsa_output.hdmi");
    }

    #[test]
    fn plan_does_not_overwrite_remembered_sink() {
        let store = Store {
            default_sink: None,
            rules: vec![rule("spotify", "headset", 0)],
            restore: vec![RestoreEntry {
                app: "Spotify".to_owned(),
                media_name: String::new(),
                sink: "bluez_output.headphones".to_owned(),
            }],
        };
        let plan = plan(&store, &sinks(), &[stream(10, "Spotify", 2)], None);
        assert_eq!(plan.moves.len(), 1);
        assert!(plan.remember.is_empty());
    }

    #[test]
    fn plan_is_idempotent_when_stream_already_at_target() {
        let store = Store {
            default_sink: None,
            rules: vec![rule("spotify", "headset", 0)],
            restore: vec![],
        };
        let plan = plan(&store, &sinks(), &[stream(10, "Spotify", 1)], None);
        assert!(plan.moves.is_empty());
        assert!(plan.profiles.is_empty());
        assert!(plan.remember.is_empty());
    }

    #[test]
    fn plan_applies_profile_and_skips_equal_values() {
        let mut profile_rule = rule("spotify", "headset", 0);
        profile_rule.volume = Some(0.5);
        profile_rule.mute = Some(true);
        let store = Store {
            default_sink: None,
            rules: vec![profile_rule],
            restore: vec![],
        };
        let mut playing = stream(10, "Spotify", 1);
        playing.volume = Some(0.8);
        let result = plan(&store, &sinks(), &[playing], None);
        assert!(result.moves.is_empty());
        assert_eq!(result.profiles.len(), 1);
        assert_eq!(result.profiles[0].volume, Some(0.5));
        assert_eq!(result.profiles[0].mute, Some(true));

        let mut equal = stream(10, "Spotify", 1);
        equal.volume = Some(0.5);
        equal.mute = Some(true);
        let result = plan(&store, &sinks(), &[equal], None);
        assert!(result.profiles.is_empty());
    }

    #[test]
    fn plan_skips_default_when_already_set() {
        let store = Store {
            default_sink: Some("alsa_output.hdmi".to_owned()),
            rules: vec![],
            restore: vec![],
        };
        let plan = plan(&store, &sinks(), &[], Some("alsa_output.hdmi"));
        assert!(plan.moves.is_empty());
        assert!(plan.default.is_none());
    }

    #[test]
    fn restore_plan_moves_back_and_marks_done() {
        let store = Store {
            default_sink: None,
            rules: vec![],
            restore: vec![
                RestoreEntry {
                    app: "Spotify".to_owned(),
                    media_name: "Song".to_owned(),
                    sink: "alsa_output.usb-headset".to_owned(),
                },
                RestoreEntry {
                    app: "Gone".to_owned(),
                    media_name: String::new(),
                    sink: "alsa_output.hdmi".to_owned(),
                },
            ],
        };
        let mut playing = stream(10, "Spotify", 2);
        playing.media_name = "Song".to_owned();
        let restore = plan_restore(&store, &sinks(), &[playing]);
        assert_eq!(restore.moves.len(), 1);
        assert_eq!(restore.moves[0].to, 1);
        assert_eq!(restore.done.len(), 2);
        assert!(restore.unresolved.iter().any(|line| line.contains("Gone")));

        let mut store = store;
        store.forget(&restore.done);
        assert!(store.restore.is_empty());
    }

    #[test]
    fn restore_skips_streams_already_home() {
        let store = Store {
            default_sink: None,
            rules: vec![],
            restore: vec![RestoreEntry {
                app: "Spotify".to_owned(),
                media_name: String::new(),
                sink: "alsa_output.usb-headset".to_owned(),
            }],
        };
        let restore = plan_restore(&store, &sinks(), &[stream(10, "Spotify", 1)]);
        assert!(restore.moves.is_empty());
        assert_eq!(restore.done.len(), 1);
    }

    #[test]
    fn store_round_trips() {
        let path = std::env::temp_dir()
            .join(format!("better-audio-store-{}", std::process::id()))
            .join("rules.json");
        let store = Store {
            default_sink: Some("alsa_output.usb-headset".to_owned()),
            rules: vec![Rule {
                app: "spotify".to_owned(),
                sink: "headset".to_owned(),
                priority: 5,
                volume: Some(0.5),
                mute: Some(true),
            }],
            restore: vec![RestoreEntry {
                app: "Spotify".to_owned(),
                media_name: "Song".to_owned(),
                sink: "alsa_output.hdmi".to_owned(),
            }],
        };
        store.save(&path).unwrap();
        let reloaded = Store::load(&path).unwrap();
        assert_eq!(reloaded, store);
    }

    #[test]
    fn store_accepts_legacy_rules_without_profile_or_restore() {
        let store: Store = serde_json::from_value(json!({
            "default_sink": "headset",
            "rules": [{ "app": "spotify", "sink": "headset" }]
        }))
        .unwrap();
        assert_eq!(store.rules[0].volume, None);
        assert!(store.restore.is_empty());
    }

    #[test]
    fn fake_backend_executes_moves_profiles_and_default() {
        let backend = FakeBackend::new(BackendKind::Pactl, Vec::new(), Vec::new(), None);
        let store = Store {
            default_sink: Some("headset".to_owned()),
            rules: vec![Rule {
                app: "spotify".to_owned(),
                sink: "headset".to_owned(),
                priority: 0,
                volume: Some(0.5),
                mute: Some(true),
            }],
            restore: vec![],
        };
        backend.set_state(sinks(), vec![stream(10, "Spotify", 2)], Some("hdmi"));
        let (plan, log) = apply_once(&backend, &store, false).unwrap();
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(plan.profiles.len(), 1);
        assert_eq!(log.len(), 4);
        let calls = backend.calls();
        assert!(calls.contains(&"move 10 alsa_output.usb-headset".to_owned()));
        assert!(calls.contains(&"volume 10 0.5".to_owned()));
        assert!(calls.contains(&"mute 10 true".to_owned()));
        assert!(calls.contains(&"default alsa_output.usb-headset".to_owned()));
    }

    #[test]
    fn dry_run_executes_nothing() {
        let backend = FakeBackend::new(BackendKind::Wpctl, Vec::new(), Vec::new(), None);
        backend.set_state(sinks(), vec![stream(10, "Spotify", 2)], Some("hdmi"));
        let store = Store {
            default_sink: Some("headset".to_owned()),
            rules: vec![rule("spotify", "headset", 0)],
            restore: vec![],
        };
        let (_, log) = apply_once(&backend, &store, true).unwrap();
        assert!(log.iter().any(|line| line.starts_with("move 10")));
        assert!(backend.calls().is_empty());
    }

    #[test]
    fn backend_selection_auto_prefers_native() {
        let selection = backends::select_backend(BackendKind::Auto, |kind| {
            Ok(Box::new(FakeBackend::new(
                kind,
                Vec::new(),
                Vec::new(),
                None,
            )))
        })
        .unwrap();
        assert_eq!(selection.status.active, BackendKind::Native);
        assert!(!selection.status.degraded);
        assert_eq!(selection.status.attempts.len(), 1);
    }

    #[test]
    fn backend_selection_degrades_when_native_is_unavailable() {
        let selection = backends::select_backend(BackendKind::Auto, |kind| match kind {
            BackendKind::Native => Err("cannot connect to PipeWire".to_owned()),
            other => Ok(Box::new(FakeBackend::new(
                other,
                Vec::new(),
                Vec::new(),
                None,
            ))),
        })
        .unwrap();
        assert_eq!(selection.status.active, BackendKind::Pactl);
        assert!(!selection.status.degraded);
        assert!(selection.status.attempts[0].error.is_some());
        assert!(
            selection
                .status
                .explanation()
                .contains("native unavailable")
        );
    }

    #[test]
    fn backend_selection_marks_explicit_fallback_degraded() {
        let selection = backends::select_backend(BackendKind::Pactl, |kind| match kind {
            BackendKind::Pactl => Err("pactl: command not found".to_owned()),
            other => Ok(Box::new(FakeBackend::new(
                other,
                Vec::new(),
                Vec::new(),
                None,
            ))),
        })
        .unwrap();
        assert_eq!(selection.status.active, BackendKind::Native);
        assert!(selection.status.degraded);
        assert!(selection.status.explanation().contains("pactl unavailable"));
    }

    #[test]
    fn backend_selection_reports_every_failure() {
        let result = backends::select_backend(BackendKind::Auto, |kind| {
            Err(format!("{} is missing", kind.as_str()))
        });
        let Err(error) = result else {
            panic!("selection unexpectedly succeeded");
        };
        assert!(error.contains("native is missing"), "{error}");
        assert!(error.contains("pactl is missing"), "{error}");
        assert!(error.contains("wpctl is missing"), "{error}");
    }

    /// Fake backend used by routing and selection tests. It records every
    /// mutation so tests can assert on dry-run and idempotency.
    pub mod backend_fakes {
        use std::cell::RefCell;

        use crate::backends::AudioBackend;
        use crate::{BackendKind, Sink, Stream};

        pub struct FakeBackend {
            kind: BackendKind,
            sinks: RefCell<Vec<Sink>>,
            streams: RefCell<Vec<Stream>>,
            default: RefCell<Option<String>>,
            calls: RefCell<Vec<String>>,
        }

        impl FakeBackend {
            pub fn new(
                kind: BackendKind,
                sinks: Vec<Sink>,
                streams: Vec<Stream>,
                default: Option<String>,
            ) -> Self {
                Self {
                    kind,
                    sinks: RefCell::new(sinks),
                    streams: RefCell::new(streams),
                    default: RefCell::new(default),
                    calls: RefCell::new(Vec::new()),
                }
            }

            pub fn set_state(&self, sinks: Vec<Sink>, streams: Vec<Stream>, default: Option<&str>) {
                *self.sinks.borrow_mut() = sinks;
                *self.streams.borrow_mut() = streams;
                *self.default.borrow_mut() = default.map(str::to_owned);
            }

            pub fn calls(&self) -> Vec<String> {
                self.calls.borrow().clone()
            }
        }

        impl AudioBackend for FakeBackend {
            fn kind(&self) -> BackendKind {
                self.kind
            }

            fn description(&self) -> String {
                format!("fake {}", self.kind.as_str())
            }

            fn sinks(&self) -> Result<Vec<Sink>, String> {
                Ok(self.sinks.borrow().clone())
            }

            fn streams(&self) -> Result<Vec<Stream>, String> {
                Ok(self.streams.borrow().clone())
            }

            fn default_sink(&self) -> Result<Option<String>, String> {
                Ok(self.default.borrow().clone())
            }

            fn move_stream(&self, stream_id: u32, sink: &str) -> Result<(), String> {
                self.calls
                    .borrow_mut()
                    .push(format!("move {stream_id} {sink}"));
                Ok(())
            }

            fn set_default_sink(&self, sink: &str) -> Result<(), String> {
                self.calls.borrow_mut().push(format!("default {sink}"));
                Ok(())
            }

            fn set_stream_volume(&self, stream_id: u32, volume: f32) -> Result<(), String> {
                self.calls
                    .borrow_mut()
                    .push(format!("volume {stream_id} {volume}"));
                Ok(())
            }

            fn set_stream_mute(&self, stream_id: u32, mute: bool) -> Result<(), String> {
                self.calls
                    .borrow_mut()
                    .push(format!("mute {stream_id} {mute}"));
                Ok(())
            }
        }
    }
}
