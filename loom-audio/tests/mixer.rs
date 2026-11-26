//! Command-level mixer tests through a fake `pactl` executable.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const SINKS: &str = include_str!("fixtures/pactl-sinks.json");
const SOURCES: &str = include_str!("fixtures/pactl-sources.json");

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_loom-audio")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("loom-audio-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn script(dir: &Path, logs: &Path) -> String {
    let path = dir.join("pactl");
    let body = format!(
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> '{log}'\n\
         case \"$1 $2 $3 $4\" in\n\
           '-f json list sinks') cat >/dev/null; printf '%s' '{sinks}' ;;\n\
           '-f json list sources') printf '%s' '{sources}' ;;\n\
           'get-default-sink '*) printf '%s\\n' 'alsa_output.speakers' ;;\n\
           'get-default-source '*) printf '%s\\n' 'alsa_input.usb-mic' ;;\n\
         esac\n\
         exit 0\n",
        log = logs.display(),
        sinks = SINKS.replace('\'', "'\\''"),
        sources = SOURCES.replace('\'', "'\\''"),
    );
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

fn run(args: &[&str], pactl: &str, config: &Path) -> std::process::Output {
    Command::new(binary())
        .args([
            "--backend",
            "pactl",
            "--pactl",
            pactl,
            "--config",
            &config.display().to_string(),
        ])
        .args(args)
        .env_remove("LOOM_AUDIO_CONFIG")
        .env_remove("LOOM_AUDIO_PACTL")
        .output()
        .unwrap()
}

fn stdout_json(output: &std::process::Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON: {err}\nstdout:\n{text}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn volume_json_maps_a_linear_level_to_pactl_percent() {
    let dir = temp_dir("volume");
    let log = dir.join("calls.log");
    let pactl = script(&dir, &log);
    let config = dir.join("rules.json");

    let output = run(
        &["--json", "volume", "sink", "hdmi", "0.25"],
        &pactl,
        &config,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = stdout_json(&output);
    assert_eq!(json["command"], "volume");
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["backend"], "pactl");
    assert_eq!(json["data"]["kind"], "sink");
    assert_eq!(json["data"]["target"], "hdmi");
    assert_eq!(json["data"]["name"], "alsa_output.hdmi");
    assert_eq!(json["data"]["volume"], 0.25);
    assert_eq!(json["data"]["changed"], true);
    assert_eq!(json["data"]["dry_run"], false);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("set-sink-volume alsa_output.hdmi 63.00%"),
        "{calls}"
    );
}

#[test]
fn source_volume_and_mute_resolve_against_sources() {
    let dir = temp_dir("source");
    let log = dir.join("calls.log");
    let pactl = script(&dir, &log);
    let config = dir.join("rules.json");

    let output = run(
        &["--json", "volume", "source", "usb-mic", "0.5"],
        &pactl,
        &config,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = stdout_json(&output);
    assert_eq!(json["data"]["kind"], "source");
    assert_eq!(json["data"]["name"], "alsa_input.usb-mic");
    assert_eq!(json["data"]["volume"], 0.5);

    let output = run(
        &["--json", "mute", "source", "usb-mic", "on"],
        &pactl,
        &config,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = stdout_json(&output);
    assert_eq!(json["command"], "mute");
    assert_eq!(json["data"]["mute"], true);
    assert_eq!(json["data"]["changed"], true);

    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("set-source-volume alsa_input.usb-mic 79.37%"),
        "{calls}"
    );
    assert!(
        calls.contains("set-source-mute alsa_input.usb-mic 1"),
        "{calls}"
    );
}

#[test]
fn invalid_volume_range_fails_before_touching_the_backend() {
    let dir = temp_dir("range");
    let log = dir.join("calls.log");
    let pactl = script(&dir, &log);
    let config = dir.join("rules.json");

    let output = run(
        &["--json", "volume", "sink", "hdmi", "1.5"],
        &pactl,
        &config,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("outside 0..1"), "{stderr}");
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(calls.is_empty(), "{calls}");

    let output = run(&["mute", "sink", "hdmi", "maybe"], &pactl, &config);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expected on or off"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dry_run_reports_without_calling_the_mutator() {
    let dir = temp_dir("dry-run");
    let log = dir.join("calls.log");
    let pactl = script(&dir, &log);
    let config = dir.join("rules.json");

    let output = run(
        &["--json", "volume", "sink", "hdmi", "0.5", "--dry-run"],
        &pactl,
        &config,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = stdout_json(&output);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["changed"], true);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.contains("set-sink-volume"), "{calls}");
}
