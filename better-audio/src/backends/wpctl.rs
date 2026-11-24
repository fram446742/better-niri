//! CLI fallback backend: `pw-dump` for snapshots, `pw-cli` for relinking and
//! `wpctl` for defaults, volume and mute.
//!
//! This backend exists for builds without the native `pipewire` crate. It does
//! not depend on the PulseAudio protocol, so it also works when `pactl` is not
//! installed.

use std::process::Command;

use super::dump::{Dump, DumpPort, parse_dump};
use super::{AudioBackend, BackendKind};
use crate::{Sink, Stream, resolve_sink};

#[derive(Debug, Clone)]
pub struct WpctlBackend {
    dump_program: String,
    cli_program: String,
    wpctl_program: String,
}

impl WpctlBackend {
    pub fn new(
        dump_program: impl Into<String>,
        cli_program: impl Into<String>,
        wpctl_program: impl Into<String>,
    ) -> Self {
        Self {
            dump_program: dump_program.into(),
            cli_program: cli_program.into(),
            wpctl_program: wpctl_program.into(),
        }
    }

    fn run(program: &str, args: &[&str]) -> Result<String, String> {
        let mut attempts = 0;
        let output = loop {
            match Command::new(program).args(args).output() {
                Ok(output) => break output,
                Err(err) if err.raw_os_error() == Some(26) && attempts < 20 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(err) => return Err(format!("cannot run {program}: {err}")),
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "{program} {} failed ({}): {}",
                args.join(" "),
                output.status,
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn dump(&self) -> Result<Dump, String> {
        parse_dump(&Self::run(&self.dump_program, &[])?)
    }

    fn wpctl(&self, args: &[&str]) -> Result<String, String> {
        Self::run(&self.wpctl_program, args)
    }

    fn pw_cli(&self, args: &[&str]) -> Result<String, String> {
        Self::run(&self.cli_program, args)
    }
}

impl AudioBackend for WpctlBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Wpctl
    }

    fn description(&self) -> String {
        format!(
            "wpctl CLI ({}; {}; {})",
            self.dump_program, self.cli_program, self.wpctl_program
        )
    }

    fn sinks(&self) -> Result<Vec<Sink>, String> {
        Ok(self.dump()?.sinks)
    }

    fn streams(&self) -> Result<Vec<Stream>, String> {
        Ok(self.dump()?.streams)
    }

    fn default_sink(&self) -> Result<Option<String>, String> {
        match self.wpctl(&["inspect", "@DEFAULT_AUDIO_SINK@"]) {
            Ok(text) => Ok(parse_inspect_node_name(&text)),
            // WirePlumber may be absent; a missing default is not fatal.
            Err(_) => Ok(None),
        }
    }

    fn move_stream(&self, stream_id: u32, sink: &str) -> Result<(), String> {
        let dump = self.dump()?;
        let sink = resolve_sink(&dump.sinks, sink)
            .ok_or_else(|| format!("sink `{sink}` does not resolve"))?;
        if !dump.streams.iter().any(|stream| stream.id == stream_id) {
            return Err(format!("stream {stream_id} is not present"));
        }

        let outputs: Vec<&DumpPort> = dump
            .ports
            .iter()
            .filter(|port| port.node_id == stream_id && port.direction == "out")
            .collect();
        if outputs.is_empty() {
            return Err(format!("stream {stream_id} has no output ports"));
        }
        let inputs: Vec<&DumpPort> = dump
            .ports
            .iter()
            .filter(|port| port.node_id == sink.id && port.direction == "in")
            .collect();
        if inputs.is_empty() {
            return Err(format!("sink `{}` has no input ports", sink.name));
        }

        let existing_target_ports: Vec<u32> = dump
            .links
            .iter()
            .filter(|link| link.output_node == stream_id && link.input_node == sink.id)
            .map(|link| link.output_port)
            .collect();

        let mut planned = Vec::new();
        let mut used_inputs = Vec::new();
        for (index, output) in outputs.iter().enumerate() {
            let input = inputs
                .iter()
                .find(|input| {
                    !used_inputs.contains(&input.id)
                        && !output.channel.is_empty()
                        && input.channel == output.channel
                })
                .or_else(|| {
                    inputs
                        .iter()
                        .find(|input| !used_inputs.contains(&input.id) && index < inputs.len())
                })
                .or_else(|| inputs.get(index))
                .ok_or_else(|| format!("sink `{}` has too few input ports", sink.name))?;
            used_inputs.push(input.id);
            if !existing_target_ports.contains(&output.id) {
                planned.push((output, input));
            }
        }

        // Create the new links before destroying the old ones so a failure
        // leaves audio flowing.
        // `pw-cli` creates the link as a remote object owned by the short-lived
        // helper process; `object.linger` keeps it alive after that process
        // exits (the same thing `pw-link` does by default).
        for (output, input) in &planned {
            self.pw_cli(&[
                "create-link",
                &stream_id.to_string(),
                &output.id.to_string(),
                &sink.id.to_string(),
                &input.id.to_string(),
                "{ \"object.linger\": true }",
            ])?;
        }

        for link in dump
            .links
            .iter()
            .filter(|link| link.output_node == stream_id && link.input_node != sink.id)
        {
            self.pw_cli(&["destroy", &link.id.to_string()])?;
        }
        Ok(())
    }

    fn set_default_sink(&self, sink: &str) -> Result<(), String> {
        self.wpctl(&["set-default", sink]).map(|_| ())
    }

    fn set_stream_volume(&self, stream_id: u32, volume: f32) -> Result<(), String> {
        self.set_stream_props(stream_id, Some(volume), None)
    }

    fn set_stream_mute(&self, stream_id: u32, mute: bool) -> Result<(), String> {
        self.set_stream_props(stream_id, None, Some(mute))
    }

    fn probe(&self) -> Result<(), String> {
        self.dump()?;
        self.wpctl(&["status"]).map(|_| ())
    }
}

impl WpctlBackend {
    /// Writes linear `Props` through `pw-cli`. `wpctl set-volume` converts its
    /// percentage argument to a cubic channel volume, which would make the
    /// same rule value mean different things per backend.
    fn set_stream_props(
        &self,
        stream_id: u32,
        volume: Option<f32>,
        mute: Option<bool>,
    ) -> Result<(), String> {
        let dump = self.dump()?;
        let channels = dump
            .ports
            .iter()
            .filter(|port| port.node_id == stream_id && port.direction == "out")
            .count()
            .max(1);
        let mut props = serde_json::Map::new();
        if let Some(volume) = volume {
            let values: Vec<f32> = std::iter::repeat_n(volume.max(0.0), channels).collect();
            props.insert("channelVolumes".to_owned(), serde_json::json!(values));
        }
        if let Some(mute) = mute {
            props.insert("mute".to_owned(), serde_json::json!(mute));
        }
        if props.is_empty() {
            return Ok(());
        }
        let value = serde_json::Value::Object(props).to_string();
        self.pw_cli(&["set-param", &stream_id.to_string(), "Props", &value])
            .map(|_| ())
    }
}

/// Extracts `node.name` from `wpctl inspect @DEFAULT_AUDIO_SINK@` output.
pub fn parse_inspect_node_name(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim().trim_start_matches('*').trim();
        if let Some(value) = line.strip_prefix("node.name =") {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::*;

    const DUMP: &str = include_str!("../../tests/fixtures/pw-dump.json");

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("better-audio-wpctl-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    fn fake_backend(dir: &Path, dump: &str) -> (WpctlBackend, PathBuf) {
        let dump_path = dir.join("dump.json");
        std::fs::write(&dump_path, dump).unwrap();
        let log = dir.join("calls.log");
        let dump_program = script(
            dir,
            "pw-dump",
            &format!("#!/bin/sh\ncat '{}'\n", dump_path.display()),
        );
        let cli_program = script(
            dir,
            "pw-cli",
            &format!(
                "#!/bin/sh\nprintf 'pw-cli %s\\n' \"$*\" >> '{}'\n",
                log.display()
            ),
        );
        let wpctl_program = script(
            dir,
            "wpctl",
            &format!(
                "#!/bin/sh\nprintf 'wpctl %s\\n' \"$*\" >> '{log}'\n\
                 if [ \"$1\" = inspect ]; then\n\
                   printf '  * node.name = \"alsa_output.hdmi\"\\n'\n\
                 fi\n",
                log = log.display()
            ),
        );
        (
            WpctlBackend::new(dump_program, cli_program, wpctl_program),
            log,
        )
    }

    #[test]
    fn moves_stream_with_pw_cli_ids() {
        let dir = temp_dir("move");
        let (backend, log) = fake_backend(&dir, DUMP);
        backend.move_stream(146, "hdmi").unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("create-link 146 134 59 93 { \"object.linger\": true }"),
            "{calls}"
        );
        assert!(
            calls.contains("create-link 146 133 59 94 { \"object.linger\": true }"),
            "{calls}"
        );
        assert!(calls.contains("destroy 77"), "{calls}");
        assert!(calls.contains("destroy 128"), "{calls}");
        // The Zen stream is already linked to 52: moving it there is a no-op.
        backend.move_stream(165, "easyeffects_sink").unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("create-link 165"), "{calls}");
        assert!(!calls.contains("destroy 117"), "{calls}");
    }

    #[test]
    fn sets_default_and_profile_through_wpctl() {
        let dir = temp_dir("profile");
        let (backend, log) = fake_backend(&dir, DUMP);
        assert_eq!(
            backend.default_sink().unwrap().as_deref(),
            Some("alsa_output.hdmi")
        );
        backend.set_default_sink("alsa_output.usb-headset").unwrap();
        backend.set_stream_volume(165, 0.25).unwrap();
        backend.set_stream_mute(165, true).unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("wpctl set-default alsa_output.usb-headset"),
            "{calls}"
        );
        assert!(
            calls.contains(r#"pw-cli set-param 165 Props {"channelVolumes":[0.25,0.25]}"#),
            "{calls}"
        );
        assert!(
            calls.contains(r#"pw-cli set-param 165 Props {"mute":true}"#),
            "{calls}"
        );
        assert!(!calls.contains("wpctl set-volume"), "{calls}");
    }

    #[test]
    fn rejects_missing_stream_and_sink() {
        let dir = temp_dir("missing");
        let (backend, _) = fake_backend(&dir, DUMP);
        assert!(
            backend
                .move_stream(999, "hdmi")
                .unwrap_err()
                .contains("not present")
        );
        assert!(
            backend
                .move_stream(146, "nope")
                .unwrap_err()
                .contains("does not resolve")
        );
    }

    #[test]
    fn parses_wpctl_inspect_name() {
        let text = "  * node.name = \"alsa_output.hdmi\"\n  * node.description = \"HDMI\"\n";
        assert_eq!(
            parse_inspect_node_name(text).as_deref(),
            Some("alsa_output.hdmi")
        );
        assert_eq!(parse_inspect_node_name("no name"), None);
    }
}
