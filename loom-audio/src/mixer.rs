//! Device-level mixer controls for sinks and sources.
//!
//! Rule-based stream profiles and these commands share one convention: a
//! volume is linear, where `1.0` is 100%. Backends that expose percentages
//! convert at their boundary (`pactl` uses a cubic percentage; PipeWire
//! `Props` are linear), so the same number means the same loudness everywhere.
//!
//! Every operation first resolves the reference against the live device list,
//! compares the current level and is a no-op when it already matches. That
//! keeps repeated GUI applies idempotent and avoids needless server writes.

use serde::Serialize;

use crate::{AudioBackend, resolve_sink, resolve_source};

const VOLUME_EPSILON: f32 = 0.005;

/// Which device list a mixer command addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Sink,
    Source,
}

impl DeviceKind {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "sink" | "sinks" | "output" | "outputs" => Ok(Self::Sink),
            "source" | "sources" | "input" | "inputs" => Ok(Self::Source),
            other => Err(format!(
                "unknown device kind `{other}` (expected sink or source)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sink => "sink",
            Self::Source => "source",
        }
    }
}

/// Result of a volume or mute command.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MixerReport {
    pub backend: String,
    pub kind: DeviceKind,
    /// Reference as the user wrote it.
    pub target: String,
    /// Resolved device name.
    pub name: String,
    /// Volume after the operation (when the command sets volume).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    /// Mute state after the operation (when the command sets mute).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    /// Whether anything was (or would be) sent to the backend.
    pub changed: bool,
    pub dry_run: bool,
}

/// Parses a linear volume in `0..=1`.
pub fn parse_volume(value: &str) -> Result<f32, String> {
    let volume: f32 = value
        .trim()
        .parse()
        .map_err(|_| format!("volume `{value}` is not a number (1.0 == 100%)"))?;
    if !volume.is_finite() {
        return Err(format!("volume `{value}` is not finite"));
    }
    if !(0.0..=1.0).contains(&volume) {
        return Err(format!("volume {volume} is outside 0..1 (1.0 == 100%)"));
    }
    Ok(volume)
}

/// Parses a mute argument (`on`/`off`, plus the usual true/false aliases).
pub fn parse_mute(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Ok(true),
        "0" | "false" | "off" | "no" => Ok(false),
        other => Err(format!("invalid mute value `{other}` (expected on or off)")),
    }
}

fn volume_differs(current: Option<f32>, target: f32) -> bool {
    current.is_none_or(|current| (current - target).abs() > VOLUME_EPSILON)
}

struct Device {
    name: String,
    volume: Option<f32>,
    mute: Option<bool>,
}

fn resolve_device(
    backend: &dyn AudioBackend,
    kind: DeviceKind,
    reference: &str,
) -> Result<Device, String> {
    match kind {
        DeviceKind::Sink => {
            let sinks = backend.sinks()?;
            let sink = resolve_sink(&sinks, reference)
                .ok_or_else(|| format!("sink `{reference}` does not resolve to one device"))?;
            Ok(Device {
                name: sink.name.clone(),
                volume: sink.volume,
                mute: sink.mute,
            })
        }
        DeviceKind::Source => {
            let sources = backend.sources()?;
            let source = resolve_source(&sources, reference)
                .ok_or_else(|| format!("source `{reference}` does not resolve to one device"))?;
            Ok(Device {
                name: source.name.clone(),
                volume: source.volume,
                mute: source.mute,
            })
        }
    }
}

/// Sets a sink or source volume. Already-matching levels are a no-op.
pub fn set_volume(
    backend: &dyn AudioBackend,
    kind: DeviceKind,
    reference: &str,
    volume: f32,
    dry_run: bool,
) -> Result<MixerReport, String> {
    if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
        return Err(format!("volume {volume} is outside 0..1 (1.0 == 100%)"));
    }
    let device = resolve_device(backend, kind, reference)?;
    let changed = volume_differs(device.volume, volume);
    if changed && !dry_run {
        match kind {
            DeviceKind::Sink => backend.set_sink_volume(&device.name, volume)?,
            DeviceKind::Source => backend.set_source_volume(&device.name, volume)?,
        }
    }
    Ok(MixerReport {
        backend: backend.kind().as_str().to_owned(),
        kind,
        target: reference.to_owned(),
        name: device.name,
        volume: Some(volume),
        mute: None,
        changed,
        dry_run,
    })
}

/// Sets a sink or source mute flag. Already-matching flags are a no-op.
pub fn set_mute(
    backend: &dyn AudioBackend,
    kind: DeviceKind,
    reference: &str,
    mute: bool,
    dry_run: bool,
) -> Result<MixerReport, String> {
    let device = resolve_device(backend, kind, reference)?;
    let changed = device.mute != Some(mute);
    if changed && !dry_run {
        match kind {
            DeviceKind::Sink => backend.set_sink_mute(&device.name, mute)?,
            DeviceKind::Source => backend.set_source_mute(&device.name, mute)?,
        }
    }
    Ok(MixerReport {
        backend: backend.kind().as_str().to_owned(),
        kind,
        target: reference.to_owned(),
        name: device.name,
        volume: None,
        mute: Some(mute),
        changed,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::backends::BackendKind;
    use crate::{LinkInfo, Sink, Source, Stream};

    #[derive(Default)]
    struct FakeBackend {
        sinks: RefCell<Vec<Sink>>,
        sources: RefCell<Vec<Source>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeBackend {
        fn with_devices() -> Self {
            Self {
                sinks: RefCell::new(vec![Sink {
                    id: 1,
                    name: "alsa_output.speakers".to_owned(),
                    description: "Speakers".to_owned(),
                    volume: Some(1.0),
                    mute: Some(false),
                }]),
                sources: RefCell::new(vec![Source {
                    id: 2,
                    name: "alsa_input.usb-mic".to_owned(),
                    description: "USB Mic".to_owned(),
                    volume: Some(0.5),
                    mute: Some(false),
                }]),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl AudioBackend for FakeBackend {
        fn kind(&self) -> BackendKind {
            BackendKind::Pactl
        }

        fn description(&self) -> String {
            "fake".to_owned()
        }

        fn sinks(&self) -> Result<Vec<Sink>, String> {
            Ok(self.sinks.borrow().clone())
        }

        fn streams(&self) -> Result<Vec<Stream>, String> {
            Ok(Vec::new())
        }

        fn default_sink(&self) -> Result<Option<String>, String> {
            Ok(self.sinks.borrow().first().map(|sink| sink.name.clone()))
        }

        fn sources(&self) -> Result<Vec<Source>, String> {
            Ok(self.sources.borrow().clone())
        }

        fn links(&self) -> Result<Vec<LinkInfo>, String> {
            Ok(Vec::new())
        }

        fn move_stream(&self, _stream_id: u32, _sink: &str) -> Result<(), String> {
            Ok(())
        }

        fn set_default_sink(&self, _sink: &str) -> Result<(), String> {
            Ok(())
        }

        fn set_stream_volume(&self, _stream_id: u32, _volume: f32) -> Result<(), String> {
            Ok(())
        }

        fn set_stream_mute(&self, _stream_id: u32, _mute: bool) -> Result<(), String> {
            Ok(())
        }

        fn set_sink_volume(&self, sink: &str, volume: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_sink_volume {sink} {volume}"));
            if let Some(entry) = self
                .sinks
                .borrow_mut()
                .iter_mut()
                .find(|entry| entry.name == sink)
            {
                entry.volume = Some(volume);
            }
            Ok(())
        }

        fn set_sink_mute(&self, sink: &str, mute: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_sink_mute {sink} {mute}"));
            if let Some(entry) = self
                .sinks
                .borrow_mut()
                .iter_mut()
                .find(|entry| entry.name == sink)
            {
                entry.mute = Some(mute);
            }
            Ok(())
        }

        fn set_source_volume(&self, source: &str, volume: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_source_volume {source} {volume}"));
            if let Some(entry) = self
                .sources
                .borrow_mut()
                .iter_mut()
                .find(|entry| entry.name == source)
            {
                entry.volume = Some(volume);
            }
            Ok(())
        }

        fn set_source_mute(&self, source: &str, mute: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("set_source_mute {source} {mute}"));
            if let Some(entry) = self
                .sources
                .borrow_mut()
                .iter_mut()
                .find(|entry| entry.name == source)
            {
                entry.mute = Some(mute);
            }
            Ok(())
        }

        fn probe(&self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn volume_maps_to_the_sink_setter_and_is_idempotent() {
        let backend = FakeBackend::with_devices();
        let report = set_volume(&backend, DeviceKind::Sink, "speakers", 0.25, false).unwrap();
        assert_eq!(report.name, "alsa_output.speakers");
        assert_eq!(report.volume, Some(0.25));
        assert!(report.changed);
        assert_eq!(report.backend, "pactl");
        assert_eq!(
            backend.calls(),
            ["set_sink_volume alsa_output.speakers 0.25"]
        );

        let again = set_volume(&backend, DeviceKind::Sink, "speakers", 0.25, false).unwrap();
        assert!(!again.changed);
        assert_eq!(backend.calls().len(), 1, "a no-op must not write");
    }

    #[test]
    fn volume_and_mute_resolve_sources_separately_from_sinks() {
        let backend = FakeBackend::with_devices();
        let report = set_volume(&backend, DeviceKind::Source, "usb", 0.75, false).unwrap();
        assert_eq!(report.name, "alsa_input.usb-mic");
        let report = set_mute(&backend, DeviceKind::Source, "usb", true, false).unwrap();
        assert_eq!(report.mute, Some(true));
        assert_eq!(
            backend.calls(),
            [
                "set_source_volume alsa_input.usb-mic 0.75",
                "set_source_mute alsa_input.usb-mic true"
            ]
        );
        // A sink command never resolves against the source list.
        let error = set_volume(&backend, DeviceKind::Sink, "usb", 0.5, false).unwrap_err();
        assert!(error.contains("sink `usb`"), "{error}");
    }

    #[test]
    fn mute_maps_and_toggles_idempotently() {
        let backend = FakeBackend::with_devices();
        let report = set_mute(
            &backend,
            DeviceKind::Sink,
            "alsa_output.speakers",
            true,
            false,
        )
        .unwrap();
        assert_eq!(report.mute, Some(true));
        assert!(report.changed);
        let again = set_mute(
            &backend,
            DeviceKind::Sink,
            "alsa_output.speakers",
            true,
            false,
        )
        .unwrap();
        assert!(!again.changed);
        assert_eq!(backend.calls(), ["set_sink_mute alsa_output.speakers true"]);
    }

    #[test]
    fn dry_run_reports_intent_without_calling_the_backend() {
        let backend = FakeBackend::with_devices();
        let report = set_volume(&backend, DeviceKind::Sink, "speakers", 0.4, true).unwrap();
        assert!(report.changed);
        assert!(report.dry_run);
        assert!(backend.calls().is_empty());
    }

    #[test]
    fn invalid_volumes_and_mute_values_are_refused() {
        for value in ["-0.1", "1.1", "nan", "inf", "loud"] {
            assert!(parse_volume(value).is_err(), "{value} was accepted");
        }
        assert_eq!(parse_volume("0").unwrap(), 0.0);
        assert_eq!(parse_volume("1").unwrap(), 1.0);
        assert_eq!(parse_volume("0.5").unwrap(), 0.5);
        for value in ["on", "TRUE", "1", "yes"] {
            assert!(parse_mute(value).unwrap());
        }
        for value in ["off", "false", "0", "no"] {
            assert!(!parse_mute(value).unwrap());
        }
        assert!(parse_mute("maybe").is_err());
        assert!(DeviceKind::parse("monitor").is_err());
        assert_eq!(DeviceKind::parse("Source").unwrap(), DeviceKind::Source);
    }

    #[test]
    fn report_serializes_to_stable_json() {
        let backend = FakeBackend::with_devices();
        let report = set_volume(&backend, DeviceKind::Sink, "speakers", 0.5, true).unwrap();
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["backend"], "pactl");
        assert_eq!(json["kind"], "sink");
        assert_eq!(json["target"], "speakers");
        assert_eq!(json["name"], "alsa_output.speakers");
        assert_eq!(json["volume"], 0.5);
        assert_eq!(json["changed"], true);
        assert_eq!(json["dry_run"], true);
        assert!(json.get("mute").is_none());

        let mute = set_mute(&backend, DeviceKind::Source, "usb", false, false).unwrap();
        let json = serde_json::to_value(&mute).unwrap();
        assert_eq!(json["kind"], "source");
        assert_eq!(json["mute"], false);
        assert!(json.get("volume").is_none());
    }
}
