//! `better-audio`: rule-based audio routing for PipeWire (via `pactl`).
//!
//! Rules match an application (case-insensitive substring or `*` wildcard) and
//! name a target sink (exact name, or a unique substring). `apply` moves every
//! matching playback stream whose current sink differs, and optionally sets the
//! default sink. `watch` repeats `apply` on an interval so new streams are
//! routed as they appear.
//!
//! Streams without a matching rule are left alone. The backend is a single
//! external `pactl` executable (`$BETTER_AUDIO_PACTL`), which keeps the tool
//! compositor-agnostic and easy to test with a fake program.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sink {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    pub id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_id: Option<u32>,
    pub app: String,
    #[serde(default)]
    pub media_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Case-insensitive app match; `*` matches anything, otherwise a substring.
    pub app: String,
    /// Target sink name, or a unique substring of it.
    pub sink: String,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Store {
    #[serde(default)]
    pub default_sink: Option<String>,
    #[serde(default)]
    pub rules: Vec<Rule>,
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub moves: Vec<Move>,
    pub default: Option<(String, String)>,
    /// Streams that matched a rule whose sink does not exist.
    pub unresolved: Vec<String>,
}

/// Computes the moves needed to satisfy the rules.
pub fn plan(
    store: &Store,
    sinks: &[Sink],
    streams: &[Stream],
    current_default: Option<&str>,
) -> Plan {
    let mut plan = Plan::default();
    let mut unresolved = BTreeSet::new();
    for stream in streams {
        let Some(rule) = resolve_rule(&store.rules, &stream.app) else {
            continue;
        };
        let Some(sink) = resolve_sink(sinks, &rule.sink) else {
            unresolved.insert(format!("{} -> {}", stream.app, rule.sink));
            continue;
        };
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

#[derive(Debug, Clone)]
pub struct Backend {
    program: String,
}

impl Default for Backend {
    fn default() -> Self {
        Self::new(std::env::var("BETTER_AUDIO_PACTL").unwrap_or_else(|_| "pactl".to_owned()))
    }
}

impl Backend {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        // A just-written script can briefly fail with ETXTBSY; retry it.
        let mut attempts = 0;
        let output = loop {
            match Command::new(&self.program).args(args).output() {
                Ok(output) => break output,
                Err(err) if err.raw_os_error() == Some(26) && attempts < 20 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(err) => return Err(format!("cannot run {}: {err}", self.program)),
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "{} {} failed ({}): {}",
                self.program,
                args.join(" "),
                output.status,
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn sinks(&self) -> Result<Vec<Sink>, String> {
        parse_sinks(&self.run(&["-f", "json", "list", "sinks"])?)
    }

    pub fn streams(&self) -> Result<Vec<Stream>, String> {
        parse_streams(&self.run(&["-f", "json", "list", "sink-inputs"])?)
    }

    pub fn default_sink(&self) -> Result<Option<String>, String> {
        match self.run(&["get-default-sink"]) {
            Ok(name) => {
                let name = name.trim().to_owned();
                Ok((!name.is_empty()).then_some(name))
            }
            // Older pactl builds lack `get-default-sink`.
            Err(_) => Ok(None),
        }
    }

    pub fn move_stream(&self, stream_id: u32, sink: &str) -> Result<(), String> {
        self.run(&["move-sink-input", &stream_id.to_string(), sink])
            .map(|_| ())
    }

    pub fn set_default_sink(&self, sink: &str) -> Result<(), String> {
        self.run(&["set-default-sink", sink]).map(|_| ())
    }
}

pub fn parse_sinks(json: &str) -> Result<Vec<Sink>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("invalid sinks JSON: {err}"))?;
    let array = value
        .as_array()
        .ok_or_else(|| "sinks JSON is not an array".to_owned())?;
    let mut sinks = Vec::new();
    for entry in array {
        let Some(id) = entry.get("index").and_then(Value::as_u64) else {
            continue;
        };
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        sinks.push(Sink {
            id: id as u32,
            name,
            description,
        });
    }
    Ok(sinks)
}

pub fn parse_streams(json: &str) -> Result<Vec<Stream>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("invalid streams JSON: {err}"))?;
    let array = value
        .as_array()
        .ok_or_else(|| "streams JSON is not an array".to_owned())?;
    let mut streams = Vec::new();
    for entry in array {
        let Some(id) = entry.get("index").and_then(Value::as_u64) else {
            continue;
        };
        let sink_id = entry
            .get("sink")
            .and_then(Value::as_u64)
            .map(|id| id as u32);
        let properties = entry.get("properties").cloned().unwrap_or(Value::Null);
        let app = [
            "application.name",
            "application.process.binary",
            "media.name",
        ]
        .iter()
        .find_map(|key| {
            properties
                .get(*key)
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.starts_with("Sink Input"))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("stream {id}"));
        let media_name = properties
            .get("media.name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        streams.push(Stream {
            id: id as u32,
            sink_id,
            app,
            media_name,
        });
    }
    Ok(streams)
}

/// Applies a plan through the backend. With `dry_run`, nothing is executed.
pub fn execute(backend: &Backend, plan: &Plan, dry_run: bool) -> Result<Vec<String>, String> {
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
    if let Some((from, to)) = &plan.default {
        if dry_run {
            log.push(format!("default {from} -> {to}"));
        } else {
            backend.set_default_sink(to)?;
            log.push(format!("default {from} -> {to}"));
        }
    }
    Ok(log)
}

/// Queries the backend and applies the current rules once.
pub fn apply_once(
    backend: &Backend,
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("better-audio-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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
        }
    }

    fn rule(app: &str, sink: &str, priority: i32) -> Rule {
        Rule {
            app: app.to_owned(),
            sink: sink.to_owned(),
            priority,
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
    fn plan_moves_only_mismatched_streams() {
        let store = Store {
            default_sink: Some("headset".to_owned()),
            rules: vec![
                rule("spotify", "headset", 0),
                rule("game", "hdmi", 0),
                rule("missing-app", "nope", 0),
            ],
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
    }

    #[test]
    fn plan_skips_default_when_already_set() {
        let store = Store {
            default_sink: Some("alsa_output.hdmi".to_owned()),
            rules: vec![],
        };
        let plan = plan(&store, &sinks(), &[], Some("alsa_output.hdmi"));
        assert!(plan.moves.is_empty());
        assert!(plan.default.is_none());
    }

    #[test]
    fn parses_pactl_json() {
        let sinks = parse_sinks(
            r#"[{"index":1,"name":"alsa_output.hdmi","description":"HDMI"},
                {"index":2,"name":"bluez","description":"BT"}]"#,
        )
        .unwrap();
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[1].name, "bluez");

        let streams = parse_streams(
            r#"[{"index":5,"sink":2,"properties":{"application.name":"Spotify","media.name":"Song"}},
                {"index":6,"sink":1,"properties":{"application.process.binary":"mpv"}},
                {"index":7,"sink":1}]"#,
        )
        .unwrap();
        assert_eq!(streams[0].app, "Spotify");
        assert_eq!(streams[0].media_name, "Song");
        assert_eq!(streams[1].app, "mpv");
        assert_eq!(streams[2].app, "stream 7");
    }

    #[test]
    fn store_round_trips() {
        let path = temp_dir("store").join("rules.json");
        let store = Store {
            default_sink: Some("alsa_output.usb-headset".to_owned()),
            rules: vec![rule("spotify", "headset", 5)],
        };
        store.save(&path).unwrap();
        let reloaded = Store::load(&path).unwrap();
        assert_eq!(reloaded, store);
    }

    /// Fake `pactl` that answers JSON queries and records mutations.
    fn fake_pactl(dir: &Path, default: &str) -> (String, PathBuf) {
        let log = dir.join("calls.log");
        let program = dir.join("pactl");
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
case "$*" in
  "-f json list sinks")
    printf '%s' '[{{"index":1,"name":"alsa_output.usb-headset","description":"Headset"}},{{"index":2,"name":"alsa_output.hdmi","description":"HDMI"}}]'
    ;;
  "-f json list sink-inputs")
    printf '%s' '[{{"index":10,"sink":2,"properties":{{"application.name":"Spotify"}}}},{{"index":11,"sink":2,"properties":{{"application.name":"Chromium"}}}},{{"index":12,"sink":1,"properties":{{"application.name":"Spotify"}}}}]'
    ;;
  "get-default-sink")
    printf '%s\n' {default}
    ;;
  *)
    printf 'ok\n'
    ;;
esac
"#,
            log = log.display(),
            default = default
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        (program.display().to_string(), log)
    }

    #[test]
    fn apply_moves_streams_and_sets_default() {
        let dir = temp_dir("apply");
        let (program, log) = fake_pactl(&dir, "alsa_output.hdmi");
        let backend = Backend::new(program);
        let store = Store {
            default_sink: Some("headset".to_owned()),
            rules: vec![rule("spotify", "headset", 0)],
        };
        let (plan, executed) = apply_once(&backend, &store, false).unwrap();
        assert_eq!(plan.moves.len(), 1);
        assert_eq!(executed.len(), 2);
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("move-sink-input 10 alsa_output.usb-headset"));
        assert!(calls.contains("set-default-sink alsa_output.usb-headset"));
        // Chromium has no rule and must not be moved.
        assert!(!calls.contains("move-sink-input 11"));
    }

    #[test]
    fn dry_run_queries_but_does_not_mutate() {
        let dir = temp_dir("dry");
        let (program, log) = fake_pactl(&dir, "alsa_output.hdmi");
        let backend = Backend::new(program);
        let store = Store {
            default_sink: Some("headset".to_owned()),
            rules: vec![rule("spotify", "headset", 0)],
        };
        let (_, executed) = apply_once(&backend, &store, true).unwrap();
        assert!(executed.iter().any(|line| line.starts_with("move 10")));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("move-sink-input"));
        assert!(!calls.contains("set-default-sink"));
        assert!(calls.contains("-f json list sinks"));
    }

    #[test]
    fn failed_pactl_surfaces_stderr() {
        let dir = temp_dir("fail");
        let program = dir.join("pactl");
        let mut file = std::fs::File::create(&program).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "echo 'connection refused' >&2").unwrap();
        writeln!(file, "exit 1").unwrap();
        drop(file);
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = Backend::new(program.display().to_string());
        let error = backend.sinks().unwrap_err();
        assert!(error.contains("connection refused"), "{error}");
    }
}
