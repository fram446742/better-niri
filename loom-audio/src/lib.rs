//! `loom-audio`: rule-based audio routing for PipeWire.
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod backends;
pub mod mixer;

pub use backends::{AudioBackend, BackendConfig, BackendKind, BackendStatus, Selection};
pub use mixer::{DeviceKind, MixerReport, parse_mute, parse_volume, set_mute, set_volume};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sink {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Current linear volume (1.0 == 100%), when the backend reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
}

/// A capture device ("source" in PulseAudio terms).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Current linear volume (1.0 == 100%), when the backend reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
}

/// A PipeWire link between two nodes, for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkInfo {
    pub id: u32,
    pub output_node: u32,
    pub input_node: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    pub id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_id: Option<u32>,
    pub app: String,
    /// `application.process.binary`, when the server reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    #[serde(default)]
    pub media_name: String,
    /// Current linear volume (1.0 == 100%), when the backend reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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
    /// Optional `application.process.binary` match (a stream property).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Optional focused-window title match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional focused-workspace name match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Focused window information used by context-aware rules.
///
/// It is filled from an external command (for example niri's
/// `focused-window` JSON) so this crate does not need a compositor dependency.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusContext {
    pub app: Option<String>,
    pub title: Option<String>,
    pub workspace: Option<String>,
}

impl FocusContext {
    /// Parses a JSON object with `app`/`app_id`, `title` and
    /// `workspace`/`workspace_name` fields.
    pub fn from_json(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let get = |keys: &[&str]| {
            keys.iter().find_map(|key| {
                value
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
            })
        };
        let focus = Self {
            app: get(&["app", "app_id", "application"]),
            title: get(&["title", "window_title"]),
            workspace: get(&["workspace", "workspace_name"]),
        };
        (focus.app.is_some() || focus.title.is_some() || focus.workspace.is_some()).then_some(focus)
    }
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

/// A named set of defaults and a global output profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioProfile {
    /// Default sink name (exact or unique substring).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink: Option<String>,
    /// Default source (input device) name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Linear volume applied to every playback stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
}

/// Defaults remembered before a named profile was applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileRestore {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sink: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Store {
    #[serde(default)]
    pub default_sink: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_source: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Original sinks remembered by the first successful move.
    #[serde(default)]
    pub restore: Vec<RestoreEntry>,
    /// Named sink/source/volume/mute profiles.
    #[serde(default)]
    pub profiles: BTreeMap<String, AudioProfile>,
    /// Profile last applied, for `profile restore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    /// Defaults before the active profile was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_restore: Option<ProfileRestore>,
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_default();
    base.join("loom-audio/rules.json")
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

/// Whether one rule matches a stream and optional focused-window context.
pub fn rule_matches(
    rule: &Rule,
    app: &str,
    binary: Option<&str>,
    media_name: &str,
    focus: Option<&FocusContext>,
) -> bool {
    let focus_app = focus.and_then(|focus| focus.app.as_deref());
    let focus_title = focus.and_then(|focus| focus.title.as_deref());
    let focus_workspace = focus.and_then(|focus| focus.workspace.as_deref());
    let app_matches = pattern_matches(&rule.app, app)
        || focus_app.is_some_and(|focused| pattern_matches(&rule.app, focused));
    if !app_matches {
        return false;
    }
    if let Some(pattern) = rule.binary.as_deref() {
        let matched = binary.is_some_and(|value| pattern_matches(pattern, value))
            || focus_app.is_some_and(|focused| pattern_matches(pattern, focused));
        if !matched {
            return false;
        }
    }
    if let Some(pattern) = rule.title.as_deref() {
        let matched = (!media_name.is_empty() && pattern_matches(pattern, media_name))
            || focus_title.is_some_and(|title| pattern_matches(pattern, title));
        if !matched {
            return false;
        }
    }
    if let Some(pattern) = rule.workspace.as_deref()
        && !focus_workspace.is_some_and(|workspace| pattern_matches(pattern, workspace))
    {
        return false;
    }
    true
}

/// Picks the winning rule for one stream, including focused-window context.
///
/// `app`/`binary` may match either the stream properties or the focused
/// application. `title` and `workspace` are context-only (a stream does not
/// know the focused workspace). Every matching stream is routed
/// independently, so an app with several streams gets each one handled.
pub fn resolve_rule_with_context<'a>(
    rules: &'a [Rule],
    app: &str,
    binary: Option<&str>,
    media_name: &str,
    focus: Option<&FocusContext>,
) -> Option<&'a Rule> {
    let mut candidates: Vec<(usize, &Rule)> = rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| rule_matches(rule, app, binary, media_name, focus))
        .collect();
    candidates.sort_by(|(index_a, a), (index_b, b)| {
        b.priority.cmp(&a.priority).then(index_a.cmp(index_b))
    });
    candidates.first().map(|(_, rule)| *rule)
}

/// Picks the winning rule for an app name (no focused-window context). Kept
/// for compatibility with rule listing and simple app-only rules.
pub fn resolve_rule<'a>(rules: &'a [Rule], app: &str) -> Option<&'a Rule> {
    resolve_rule_with_context(rules, app, None, "", None)
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
pub struct StreamProfile {
    pub stream_id: u32,
    pub app: String,
    pub volume: Option<f32>,
    pub mute: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    pub moves: Vec<Move>,
    pub profiles: Vec<StreamProfile>,
    pub default: Option<(String, String)>,
    /// Default input device change `(from, to)`, when configured.
    pub default_source: Option<(String, String)>,
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
    plan_with_context(store, sinks, streams, current_default, None)
}

/// Context-aware variant of [`plan`]. `focus` upgrades rules that mention the
/// focused window title or workspace; a rule still applies per stream.
pub fn plan_with_context(
    store: &Store,
    sinks: &[Sink],
    streams: &[Stream],
    current_default: Option<&str>,
    focus: Option<&FocusContext>,
) -> Plan {
    let mut plan = Plan::default();
    let mut unresolved = BTreeSet::new();
    let mut remembered: BTreeSet<String> = store.restore.iter().map(RestoreEntry::key).collect();

    for stream in streams {
        let Some(rule) = resolve_rule_with_context(
            &store.rules,
            &stream.app,
            stream.binary.as_deref(),
            &stream.media_name,
            focus,
        ) else {
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
            plan.profiles.push(StreamProfile {
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
    if let Some((from, to)) = &plan.default_source {
        if !dry_run {
            backend.set_default_source(to)?;
        }
        log.push(format!("default source {from} -> {to}"));
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
    let mut plan = plan(store, &sinks, &streams, default.as_deref());
    let sources = backend.sources()?;
    plan.default_source =
        plan_default_source(store, &sources, backend.default_source()?.as_deref());
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

/// Resolves a source reference: exact name first, then a unique substring.
pub fn resolve_source<'a>(sources: &'a [Source], reference: &str) -> Option<&'a Source> {
    if let Some(exact) = sources.iter().find(|source| source.name == reference) {
        return Some(exact);
    }
    let needle = reference.to_lowercase();
    let matches: Vec<&Source> = sources
        .iter()
        .filter(|source| source.name.to_lowercase().contains(&needle))
        .collect();
    if matches.len() == 1 {
        matches.first().copied()
    } else {
        None
    }
}

/// Planned default-source change from `Store::default_source`.
pub fn plan_default_source(
    store: &Store,
    sources: &[Source],
    current: Option<&str>,
) -> Option<(String, String)> {
    let wanted = store.default_source.as_deref()?;
    let source = resolve_source(sources, wanted)?;
    (current != Some(source.name.as_str()))
        .then(|| (current.unwrap_or_default().to_owned(), source.name.clone()))
}

/// A snapshot of every device the backend knows about, used for hotplug
/// watching and diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    pub sinks: Vec<Sink>,
    pub sources: Vec<Source>,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
}

impl DeviceSnapshot {
    pub fn read(backend: &dyn AudioBackend) -> Result<Self, String> {
        Ok(Self {
            sinks: backend.sinks()?,
            sources: backend.sources()?,
            default_sink: backend.default_sink()?,
            default_source: backend.default_source()?,
        })
    }

    pub fn sink_names(&self) -> Vec<&str> {
        self.sinks.iter().map(|sink| sink.name.as_str()).collect()
    }

    pub fn source_names(&self) -> Vec<&str> {
        self.sources
            .iter()
            .map(|source| source.name.as_str())
            .collect()
    }
}

/// Poll-based hotplug watcher: reports a change only when the device set
/// differs from the previous observation.
#[derive(Debug, Default, Clone)]
pub struct HotplugWatcher {
    last: Option<DeviceSnapshot>,
}

impl HotplugWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observes a snapshot. The first observation counts as a change (startup
    /// should route existing streams), later identical snapshots do not.
    pub fn observe(&mut self, snapshot: DeviceSnapshot) -> bool {
        if self.last.as_ref() == Some(&snapshot) {
            return false;
        }
        self.last = Some(snapshot);
        true
    }

    pub fn last(&self) -> Option<&DeviceSnapshot> {
        self.last.as_ref()
    }
}

/// Suppresses repeated work for an unchanged plan ("only switch on change").
#[derive(Debug, Default, Clone)]
pub struct ChangeGate {
    last: Option<String>,
}

impl ChangeGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(&mut self, signature: impl Into<String>) -> bool {
        let signature = signature.into();
        if self.last.as_deref() == Some(signature.as_str()) {
            return false;
        }
        self.last = Some(signature);
        true
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// Stable signature of a routing plan, used by [`ChangeGate`].
pub fn plan_signature(plan: &Plan) -> String {
    let moves = plan
        .moves
        .iter()
        .map(|movement| format!("{}:{}:{}", movement.stream_id, movement.to, movement.sink))
        .collect::<Vec<_>>()
        .join(",");
    let profiles = plan
        .profiles
        .iter()
        .map(|profile| {
            format!(
                "{}:{:?}:{:?}",
                profile.stream_id, profile.volume, profile.mute
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{moves}|{profiles}|{:?}|{:?}",
        plan.default, plan.default_source
    )
}

/// What a profile application changes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProfilePlan {
    pub name: String,
    pub default_sink: Option<(String, String)>,
    pub default_source: Option<(String, String)>,
    pub streams: Vec<StreamProfile>,
    pub unresolved: Vec<String>,
    /// Defaults to remember before the first profile application.
    pub remember: Option<ProfileRestore>,
}

impl ProfilePlan {
    pub fn is_empty(&self) -> bool {
        self.default_sink.is_none() && self.default_source.is_none() && self.streams.is_empty()
    }
}

/// Plans a named profile: default sink/source plus a volume/mute set applied to
/// every playback stream whose value differs.
pub fn plan_profile(
    store: &Store,
    name: &str,
    sinks: &[Sink],
    sources: &[Source],
    streams: &[Stream],
    current_sink: Option<&str>,
    current_source: Option<&str>,
) -> Result<ProfilePlan, String> {
    let profile = store
        .profiles
        .get(name)
        .ok_or_else(|| format!("unknown profile `{name}`"))?;
    let mut plan = ProfilePlan {
        name: name.to_owned(),
        ..ProfilePlan::default()
    };
    if let Some(reference) = profile.sink.as_deref() {
        match resolve_sink(sinks, reference) {
            Some(sink) if current_sink != Some(sink.name.as_str()) => {
                plan.default_sink = Some((
                    current_sink.unwrap_or_default().to_owned(),
                    sink.name.clone(),
                ));
            }
            Some(_) => {}
            None => plan
                .unresolved
                .push(format!("sink `{reference}` is missing")),
        }
    }
    if let Some(reference) = profile.source.as_deref() {
        match resolve_source(sources, reference) {
            Some(source) if current_source != Some(source.name.as_str()) => {
                plan.default_source = Some((
                    current_source.unwrap_or_default().to_owned(),
                    source.name.clone(),
                ));
            }
            Some(_) => {}
            None => plan
                .unresolved
                .push(format!("source `{reference}` is missing")),
        }
    }
    for stream in streams {
        let volume = profile
            .volume
            .filter(|target| volume_differs(stream.volume, *target));
        let mute = profile.mute.filter(|target| stream.mute != Some(*target));
        if volume.is_some() || mute.is_some() {
            plan.streams.push(StreamProfile {
                stream_id: stream.id,
                app: stream.app.clone(),
                volume,
                mute,
            });
        }
    }
    if !plan.is_empty() && store.profile_restore.is_none() {
        plan.remember = Some(ProfileRestore {
            sink: current_sink.map(str::to_owned),
            source: current_source.map(str::to_owned),
        });
    }
    Ok(plan)
}

/// Plans restoring the defaults remembered before a profile was applied.
pub fn plan_profile_restore(
    store: &Store,
    sinks: &[Sink],
    sources: &[Source],
    current_sink: Option<&str>,
    current_source: Option<&str>,
) -> ProfilePlan {
    let mut plan = ProfilePlan {
        name: store.active_profile.clone().unwrap_or_default(),
        ..ProfilePlan::default()
    };
    let Some(restore) = store.profile_restore.as_ref() else {
        return plan;
    };
    if let Some(reference) = restore.sink.as_deref() {
        match resolve_sink(sinks, reference) {
            Some(sink) if current_sink != Some(sink.name.as_str()) => {
                plan.default_sink = Some((
                    current_sink.unwrap_or_default().to_owned(),
                    sink.name.clone(),
                ));
            }
            Some(_) => {}
            None => plan
                .unresolved
                .push(format!("sink `{reference}` is missing")),
        }
    }
    if let Some(reference) = restore.source.as_deref() {
        match resolve_source(sources, reference) {
            Some(source) if current_source != Some(source.name.as_str()) => {
                plan.default_source = Some((
                    current_source.unwrap_or_default().to_owned(),
                    source.name.clone(),
                ));
            }
            Some(_) => {}
            None => plan
                .unresolved
                .push(format!("source `{reference}` is missing")),
        }
    }
    plan
}

/// Executes a [`ProfilePlan`] through the backend.
pub fn execute_profile(
    backend: &dyn AudioBackend,
    plan: &ProfilePlan,
    dry_run: bool,
) -> Result<Vec<String>, String> {
    let mut log = Vec::new();
    if let Some((from, to)) = &plan.default_sink {
        if !dry_run {
            backend.set_default_sink(to)?;
        }
        log.push(format!("default sink {from} -> {to}"));
    }
    if let Some((from, to)) = &plan.default_source {
        if !dry_run {
            backend.set_default_source(to)?;
        }
        log.push(format!("default source {from} -> {to}"));
    }
    for profile in &plan.streams {
        if let Some(volume) = profile.volume {
            if !dry_run {
                backend.set_stream_volume(profile.stream_id, volume)?;
            }
            log.push(format!(
                "volume {} ({}) -> {:.0}%",
                profile.stream_id,
                profile.app,
                volume * 100.0
            ));
        }
        if let Some(mute) = profile.mute {
            if !dry_run {
                backend.set_stream_mute(profile.stream_id, mute)?;
            }
            log.push(format!(
                "mute {} ({}) -> {}",
                profile.stream_id,
                profile.app,
                if mute { "on" } else { "off" }
            ));
        }
    }
    Ok(log)
}

/// Why a rule did or did not match one stream, for `diagnose`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Explanation {
    pub stream_id: u32,
    pub app: String,
    pub binary: Option<String>,
    pub media_name: String,
    /// Indices of every rule whose conditions matched.
    pub matched_rules: Vec<usize>,
    /// Index of the winning rule.
    pub winner: Option<usize>,
    pub reason: String,
}

/// Explains rule resolution for one stream.
pub fn explain_stream(store: &Store, stream: &Stream, focus: Option<&FocusContext>) -> Explanation {
    let matched_rules: Vec<usize> = store
        .rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            rule_matches(
                rule,
                &stream.app,
                stream.binary.as_deref(),
                &stream.media_name,
                focus,
            )
        })
        .map(|(index, _)| index)
        .collect();
    let winner = resolve_rule_with_context(
        &store.rules,
        &stream.app,
        stream.binary.as_deref(),
        &stream.media_name,
        focus,
    )
    .and_then(|rule| store.rules.iter().position(|candidate| candidate == rule));
    let reason = match winner.and_then(|index| store.rules.get(index).map(|rule| (index, rule))) {
        Some((index, rule)) => format!(
            "rule {index} matched app `{}` -> `{}` (priority {})",
            rule.app, rule.sink, rule.priority
        ),
        None => format!("no rule matched app `{}`", stream.app),
    };
    Explanation {
        stream_id: stream.id,
        app: stream.app.clone(),
        binary: stream.binary.clone(),
        media_name: stream.media_name.clone(),
        matched_rules,
        winner,
        reason,
    }
}

/// Applies rules once with a focused-window context.
pub fn apply_once_with_context(
    backend: &dyn AudioBackend,
    store: &Store,
    focus: Option<&FocusContext>,
    dry_run: bool,
) -> Result<(Plan, Vec<String>), String> {
    let sinks = backend.sinks()?;
    let streams = backend.streams()?;
    let default = backend.default_sink()?;
    let mut plan = plan_with_context(store, &sinks, &streams, default.as_deref(), focus);
    let sources = backend.sources()?;
    plan.default_source =
        plan_default_source(store, &sources, backend.default_source()?.as_deref());
    let log = execute(backend, &plan, dry_run)?;
    Ok((plan, log))
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
                volume: None,
                mute: None,
            },
            Sink {
                id: 2,
                name: "alsa_output.hdmi".to_owned(),
                description: "HDMI".to_owned(),
                volume: None,
                mute: None,
            },
            Sink {
                id: 3,
                name: "bluez_output.headphones".to_owned(),
                description: "BT".to_owned(),
                volume: None,
                mute: None,
            },
        ]
    }

    fn stream(id: u32, app: &str, sink: u32) -> Stream {
        Stream {
            id,
            sink_id: Some(sink),
            app: app.to_owned(),
            binary: None,
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
            ..Rule::default()
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
            ..Store::default()
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
            ..Store::default()
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
            ..Store::default()
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
            ..Store::default()
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
            ..Store::default()
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
            ..Store::default()
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
            ..Store::default()
        };
        let restore = plan_restore(&store, &sinks(), &[stream(10, "Spotify", 1)]);
        assert!(restore.moves.is_empty());
        assert_eq!(restore.done.len(), 1);
    }

    #[test]
    fn store_round_trips() {
        let path = std::env::temp_dir()
            .join(format!("loom-audio-store-{}", std::process::id()))
            .join("rules.json");
        let store = Store {
            default_sink: Some("alsa_output.usb-headset".to_owned()),
            rules: vec![Rule {
                app: "spotify".to_owned(),
                sink: "headset".to_owned(),
                priority: 5,
                volume: Some(0.5),
                mute: Some(true),
                ..Rule::default()
            }],
            restore: vec![RestoreEntry {
                app: "Spotify".to_owned(),
                media_name: "Song".to_owned(),
                sink: "alsa_output.hdmi".to_owned(),
            }],
            ..Store::default()
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
                ..Rule::default()
            }],
            restore: vec![],
            ..Store::default()
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
            ..Store::default()
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

    #[test]
    fn context_rules_handle_several_streams_of_one_app() {
        let mut store = Store {
            rules: vec![Rule {
                app: "firefox".to_owned(),
                sink: "headset".to_owned(),
                title: Some("Meeting*".to_owned()),
                ..Rule::default()
            }],
            ..Store::default()
        };
        store.rules[0].workspace = Some("work".to_owned());
        let streams = vec![
            Stream {
                id: 10,
                sink_id: Some(2),
                app: "Firefox".to_owned(),
                binary: Some("firefox".to_owned()),
                media_name: "Meeting room".to_owned(),
                volume: None,
                mute: None,
            },
            Stream {
                id: 11,
                sink_id: Some(2),
                app: "Firefox".to_owned(),
                binary: Some("firefox".to_owned()),
                media_name: "Music".to_owned(),
                volume: None,
                mute: None,
            },
        ];
        // Without focus the workspace/title rule does not match.
        assert!(plan(&store, &sinks(), &streams, None).moves.is_empty());

        let focus = FocusContext {
            app: Some("firefox".to_owned()),
            title: Some("Meeting notes".to_owned()),
            workspace: Some("work".to_owned()),
        };
        let plan = plan_with_context(&store, &sinks(), &streams, None, Some(&focus));
        // Both streams of the same app are routed: routing is per stream, not
        // per application.
        assert_eq!(plan.moves.len(), 2);
        assert_eq!(
            plan.moves.iter().map(|m| m.stream_id).collect::<Vec<_>>(),
            [10, 11]
        );
        assert!(plan.moves.iter().all(|movement| movement.to == 1));
    }

    #[test]
    fn focus_context_parses_niri_like_json() {
        let focus = FocusContext::from_json(
            r#"{"id":1,"title":"Docs","app_id":"firefox","workspace":"web"}"#,
        )
        .unwrap();
        assert_eq!(focus.app.as_deref(), Some("firefox"));
        assert_eq!(focus.title.as_deref(), Some("Docs"));
        assert_eq!(focus.workspace.as_deref(), Some("web"));
        assert!(FocusContext::from_json(r#"{"id":1}"#).is_none());
        assert!(FocusContext::from_json("not json").is_none());
    }

    #[test]
    fn profiles_plan_and_apply_defaults_and_stream_values() {
        let backend = FakeBackend::new(BackendKind::Pactl, Vec::new(), Vec::new(), None);
        let sources = vec![Source {
            id: 4,
            name: "alsa_input.usb-mic".to_owned(),
            description: "Mic".to_owned(),
            volume: None,
            mute: None,
        }];
        backend.set_devices(
            sinks(),
            sources.clone(),
            Some("alsa_output.hdmi"),
            Some("alsa_input.usb-mic"),
        );
        let mut playing = stream(10, "Spotify", 2);
        playing.volume = Some(1.0);
        backend.set_state(sinks(), vec![playing.clone()], Some("alsa_output.hdmi"));
        let store = Store {
            profiles: BTreeMap::from([(
                "headset".to_owned(),
                AudioProfile {
                    sink: Some("headset".to_owned()),
                    source: Some("usb-mic".to_owned()),
                    volume: Some(0.5),
                    mute: Some(false),
                },
            )]),
            ..Store::default()
        };
        let plan = plan_profile(
            &store,
            "headset",
            &sinks(),
            &sources,
            &[playing],
            Some("alsa_output.hdmi"),
            Some("alsa_input.usb-mic"),
        )
        .unwrap();
        assert_eq!(
            plan.default_sink,
            Some((
                "alsa_output.hdmi".to_owned(),
                "alsa_output.usb-headset".to_owned()
            ))
        );
        // The source is already the default, so there is nothing to change.
        assert!(plan.default_source.is_none());
        assert_eq!(plan.streams.len(), 1);
        assert_eq!(plan.streams[0].volume, Some(0.5));
        assert_eq!(
            plan.remember.as_ref().unwrap().sink.as_deref(),
            Some("alsa_output.hdmi")
        );

        let log = execute_profile(&backend, &plan, false).unwrap();
        assert!(
            log.contains(&"default sink alsa_output.hdmi -> alsa_output.usb-headset".to_owned())
        );
        let calls = backend.calls();
        assert!(calls.contains(&"default alsa_output.usb-headset".to_owned()));
        assert!(calls.contains(&"volume 10 0.5".to_owned()));
        assert!(plan_profile(&store, "missing", &sinks(), &sources, &[], None, None).is_err());
    }

    #[test]
    fn profile_restore_returns_to_the_previous_defaults() {
        let store = Store {
            active_profile: Some("headset".to_owned()),
            profile_restore: Some(ProfileRestore {
                sink: Some("alsa_output.hdmi".to_owned()),
                source: Some("alsa_input.usb-mic".to_owned()),
            }),
            ..Store::default()
        };
        let sources = vec![Source {
            id: 4,
            name: "alsa_input.usb-mic".to_owned(),
            description: String::new(),
            volume: None,
            mute: None,
        }];
        let plan = plan_profile_restore(
            &store,
            &sinks(),
            &sources,
            Some("alsa_output.usb-headset"),
            Some("other-mic"),
        );
        assert_eq!(
            plan.default_sink,
            Some((
                "alsa_output.usb-headset".to_owned(),
                "alsa_output.hdmi".to_owned()
            ))
        );
        assert_eq!(
            plan.default_source,
            Some(("other-mic".to_owned(), "alsa_input.usb-mic".to_owned()))
        );
    }

    #[test]
    fn hotplug_watcher_reports_only_changes() {
        let mut watcher = HotplugWatcher::new();
        let snapshot = DeviceSnapshot {
            sinks: sinks(),
            sources: Vec::new(),
            default_sink: Some("alsa_output.hdmi".to_owned()),
            default_source: None,
        };
        assert!(watcher.observe(snapshot.clone()));
        assert!(!watcher.observe(snapshot.clone()));
        let mut changed = snapshot;
        changed.sinks.pop();
        assert!(watcher.observe(changed.clone()));
        assert_eq!(watcher.last(), Some(&changed));
    }

    #[test]
    fn change_gate_skips_identical_plans() {
        let plan = Plan {
            moves: vec![Move {
                stream_id: 10,
                app: "Spotify".to_owned(),
                from: Some(2),
                to: 1,
                sink: "alsa_output.usb-headset".to_owned(),
            }],
            ..Plan::default()
        };
        let signature = plan_signature(&plan);
        let mut gate = ChangeGate::new();
        assert!(gate.allow(signature.clone()));
        assert!(!gate.allow(signature.clone()));
        assert!(gate.allow("something else"));
        gate.reset();
        assert!(gate.allow(signature));
    }

    #[test]
    fn diagnostics_explain_why_a_rule_matched() {
        let store = Store {
            rules: vec![
                rule("spotify", "headset", 0),
                Rule {
                    app: "spot".to_owned(),
                    sink: "hdmi".to_owned(),
                    priority: 5,
                    ..Rule::default()
                },
            ],
            ..Store::default()
        };
        let spotify = stream(10, "Spotify", 2);
        let explanation = explain_stream(&store, &spotify, None);
        assert_eq!(explanation.matched_rules, vec![0, 1]);
        assert_eq!(explanation.winner, Some(1));
        assert!(explanation.reason.contains("priority 5"), "{explanation:?}");

        let unmatched = explain_stream(&store, &stream(11, "mpv", 2), None);
        assert!(unmatched.matched_rules.is_empty());
        assert_eq!(unmatched.winner, None);
        assert!(unmatched.reason.contains("no rule matched"));
    }

    #[test]
    fn resolves_sources_exactly_then_by_unique_substring() {
        let sources = vec![
            Source {
                id: 4,
                name: "alsa_input.usb-mic".to_owned(),
                description: "Mic".to_owned(),
                volume: None,
                mute: None,
            },
            Source {
                id: 5,
                name: "alsa_input.pci".to_owned(),
                description: "PCI".to_owned(),
                volume: None,
                mute: None,
            },
        ];
        assert_eq!(
            resolve_source(&sources, "alsa_input.usb-mic").unwrap().id,
            4
        );
        assert_eq!(resolve_source(&sources, "usb").unwrap().id, 4);
        assert!(resolve_source(&sources, "alsa_input").is_none());
    }

    #[test]
    fn apply_sets_the_configured_default_source() {
        let backend = FakeBackend::new(BackendKind::Pactl, Vec::new(), Vec::new(), None);
        let sources = vec![Source {
            id: 4,
            name: "alsa_input.usb-mic".to_owned(),
            description: String::new(),
            volume: None,
            mute: None,
        }];
        backend.set_devices(
            sinks(),
            sources,
            Some("alsa_output.hdmi"),
            Some("alsa_input.pci"),
        );
        let store = Store {
            default_source: Some("usb-mic".to_owned()),
            ..Store::default()
        };
        let (plan, log) = apply_once(&backend, &store, false).unwrap();
        assert_eq!(
            plan.default_source,
            Some(("alsa_input.pci".to_owned(), "alsa_input.usb-mic".to_owned()))
        );
        assert!(log.iter().any(|line| line.contains("default source")));
        assert!(
            backend
                .calls()
                .contains(&"default-source alsa_input.usb-mic".to_owned())
        );

        // Already at the wanted source: no write.
        backend.set_devices(
            sinks(),
            vec![Source {
                id: 4,
                name: "alsa_input.usb-mic".to_owned(),
                description: String::new(),
                volume: None,
                mute: None,
            }],
            Some("alsa_output.hdmi"),
            Some("alsa_input.usb-mic"),
        );
        let (plan, _) = apply_once(&backend, &store, false).unwrap();
        assert!(plan.default_source.is_none());
    }

    /// Fake backend used by routing and selection tests. It records every
    /// mutation so tests can assert on dry-run and idempotency.
    pub mod backend_fakes {
        use std::cell::RefCell;

        use crate::backends::AudioBackend;
        use crate::{BackendKind, LinkInfo, Sink, Source, Stream};

        pub struct FakeBackend {
            kind: BackendKind,
            sinks: RefCell<Vec<Sink>>,
            sources: RefCell<Vec<Source>>,
            streams: RefCell<Vec<Stream>>,
            default: RefCell<Option<String>>,
            default_source: RefCell<Option<String>>,
            links: RefCell<Vec<LinkInfo>>,
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
                    sources: RefCell::new(Vec::new()),
                    streams: RefCell::new(streams),
                    default: RefCell::new(default),
                    default_source: RefCell::new(None),
                    links: RefCell::new(Vec::new()),
                    calls: RefCell::new(Vec::new()),
                }
            }

            pub fn set_state(&self, sinks: Vec<Sink>, streams: Vec<Stream>, default: Option<&str>) {
                *self.sinks.borrow_mut() = sinks;
                *self.streams.borrow_mut() = streams;
                *self.default.borrow_mut() = default.map(str::to_owned);
            }

            pub fn set_devices(
                &self,
                sinks: Vec<Sink>,
                sources: Vec<Source>,
                default: Option<&str>,
                default_source: Option<&str>,
            ) {
                *self.sinks.borrow_mut() = sinks;
                *self.sources.borrow_mut() = sources;
                *self.default.borrow_mut() = default.map(str::to_owned);
                *self.default_source.borrow_mut() = default_source.map(str::to_owned);
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

            fn sources(&self) -> Result<Vec<Source>, String> {
                Ok(self.sources.borrow().clone())
            }

            fn links(&self) -> Result<Vec<LinkInfo>, String> {
                Ok(self.links.borrow().clone())
            }

            fn default_sink(&self) -> Result<Option<String>, String> {
                Ok(self.default.borrow().clone())
            }

            fn default_source(&self) -> Result<Option<String>, String> {
                Ok(self.default_source.borrow().clone())
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

            fn set_default_source(&self, source: &str) -> Result<(), String> {
                self.calls
                    .borrow_mut()
                    .push(format!("default-source {source}"));
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
