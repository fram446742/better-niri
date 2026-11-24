//! `pactl` backend (PipeWire's PulseAudio compatibility layer).

use std::process::Command;

use serde_json::Value;

use super::{AudioBackend, BackendKind};
use crate::{Sink, Stream};

#[derive(Debug, Clone)]
pub struct PactlBackend {
    program: String,
}

impl PactlBackend {
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
}

impl AudioBackend for PactlBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Pactl
    }

    fn description(&self) -> String {
        format!("pactl ({})", self.program)
    }

    fn sinks(&self) -> Result<Vec<Sink>, String> {
        parse_sinks(&self.run(&["-f", "json", "list", "sinks"])?)
    }

    fn streams(&self) -> Result<Vec<Stream>, String> {
        parse_streams(&self.run(&["-f", "json", "list", "sink-inputs"])?)
    }

    fn default_sink(&self) -> Result<Option<String>, String> {
        match self.run(&["get-default-sink"]) {
            Ok(name) => {
                let name = name.trim().to_owned();
                Ok((!name.is_empty()).then_some(name))
            }
            // Older pactl builds lack `get-default-sink`.
            Err(_) => Ok(None),
        }
    }

    fn move_stream(&self, stream_id: u32, sink: &str) -> Result<(), String> {
        self.run(&["move-sink-input", &stream_id.to_string(), sink])
            .map(|_| ())
    }

    fn set_default_sink(&self, sink: &str) -> Result<(), String> {
        self.run(&["set-default-sink", sink]).map(|_| ())
    }

    fn set_stream_volume(&self, stream_id: u32, volume: f32) -> Result<(), String> {
        self.run(&[
            "set-sink-input-volume",
            &stream_id.to_string(),
            &linear_to_percent(volume),
        ])
        .map(|_| ())
    }

    fn set_stream_mute(&self, stream_id: u32, mute: bool) -> Result<(), String> {
        self.run(&[
            "set-sink-input-mute",
            &stream_id.to_string(),
            if mute { "1" } else { "0" },
        ])
        .map(|_| ())
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
            volume: parse_volume(entry),
            mute: entry.get("mute").and_then(Value::as_bool),
        });
    }
    Ok(streams)
}

/// Converts a linear PipeWire volume into pactl's cubic percentage.
fn linear_to_percent(volume: f32) -> String {
    let percent = (volume.max(0.0) as f64).cbrt() * 100.0;
    format!("{percent:.2}%")
}

/// Reads a sink-input volume as a linear fraction (1.0 == 100%).
fn parse_volume(entry: &Value) -> Option<f32> {
    let channels = entry.get("volume")?.as_object()?;
    let channel = channels.values().next()?;
    if let Some(percent) = channel.get("value_percent").and_then(Value::as_str) {
        let percent = percent.trim().trim_end_matches('%');
        if let Ok(percent) = percent.parse::<f32>() {
            // pactl reports a cubic percentage; PipeWire Props are linear.
            return Some((percent / 100.0).powi(3));
        }
    }
    channel
        .get("value")
        .and_then(Value::as_f64)
        .map(|value| value as f32 / 65536.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINKS: &str = include_str!("../../tests/fixtures/pactl-sinks.json");
    const STREAMS: &str = include_str!("../../tests/fixtures/pactl-sink-inputs.json");

    #[test]
    fn parses_pactl_fixtures() {
        let sinks = parse_sinks(SINKS).unwrap();
        assert_eq!(sinks.len(), 3);
        assert_eq!(sinks[1].name, "alsa_output.hdmi");
        assert_eq!(sinks[2].description, "BT");

        let streams = parse_streams(STREAMS).unwrap();
        assert_eq!(streams[0].app, "Spotify");
        assert_eq!(streams[0].media_name, "Song");
        assert_eq!(streams[0].sink_id, Some(2));
        assert_eq!(streams[0].volume, Some(0.125));
        assert_eq!(streams[0].mute, Some(false));
        assert_eq!(streams[1].app, "mpv");
        assert_eq!(streams[1].mute, Some(true));
        assert_eq!(streams[2].app, "stream 12");
        assert_eq!(streams[2].volume, None);
    }

    #[test]
    fn converts_between_linear_volume_and_pactl_percent() {
        assert_eq!(linear_to_percent(1.0), "100.00%");
        assert_eq!(linear_to_percent(0.4), "73.68%");
        assert_eq!(linear_to_percent(0.0), "0.00%");
        let percent = linear_to_percent(0.4);
        assert!(
            (parse_volume(&serde_json::json!({
                "volume": { "front-left": { "value_percent": percent } }
            }))
            .unwrap()
                - 0.4)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn rejects_non_array_json() {
        let error = parse_sinks("{}").unwrap_err();
        assert!(error.contains("not an array"), "{error}");
        let error = parse_streams("[] trailing").unwrap_err();
        assert!(error.contains("invalid streams JSON"), "{error}");
    }
}
