//! Integration tests for `better-portal-picker`.
//!
//! Window mode is exercised end-to-end against a fake niri IPC socket. Output
//! and region modes need a real layer-shell compositor, so those only cover the
//! failure paths (missing compositor / missing IPC) here; their selection
//! parsing is covered by the library unit tests.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::JoinHandle;

fn picker() -> &'static str {
    env!("CARGO_BIN_EXE_better-portal-picker")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "better-portal-picker-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serves exactly one niri IPC PickWindow request and replies with `window`.
fn fake_niri(path: &Path, window: Option<u64>) -> JoinHandle<()> {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: niri_ipc::Request = serde_json::from_str(line.trim()).unwrap();
        assert!(matches!(request, niri_ipc::Request::PickWindow));

        let picked = window.map(|id| {
            serde_json::json!({
                "id": id,
                "title": "picked",
                "app_id": "app",
                "pid": 1,
                "workspace_id": 1,
                "is_focused": true,
                "is_floating": false,
                "is_sticky": false,
                "is_urgent": false,
                "is_minimized": false,
                "layout": {
                    "pos_in_scrolling_layout": [1, 1],
                    "tile_size": [100.0, 100.0],
                    "window_size": [100, 100],
                    "tile_pos_in_workspace_view": [0.0, 0.0],
                    "window_offset_in_tile": [0.0, 0.0]
                },
                "focus_timestamp": null
            })
        });
        let response = serde_json::json!({ "Ok": { "PickedWindow": picked } });
        let mut stream = stream;
        stream
            .write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
    })
}

fn run_picker(args: &[&str], socket: Option<&Path>) -> std::process::Output {
    let mut command = Command::new(picker());
    command
        .args(args)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET");
    match socket {
        Some(socket) => {
            command.env("NIRI_SOCKET", socket);
        }
        None => {
            command.env_remove("NIRI_SOCKET");
        }
    }
    command.output().expect("run better-portal-picker")
}

#[test]
fn window_picker_prints_the_picked_window_and_exits_zero() {
    let dir = temp_dir("window");
    let socket = dir.join("niri.sock");
    let server = fake_niri(&socket, Some(42));

    let output = run_picker(&["--screencast-window"], Some(&socket));
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json, serde_json::json!({ "window": 42 }));
    server.join().unwrap();
}

#[test]
fn window_picker_cancellation_exits_two() {
    let dir = temp_dir("window-cancel");
    let socket = dir.join("niri.sock");
    let server = fake_niri(&socket, None);

    let output = run_picker(&["--screencast-window"], Some(&socket));
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    server.join().unwrap();
}

#[test]
fn window_picker_requires_a_niri_socket() {
    let output = run_picker(&["--screencast-window"], None);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("NIRI_SOCKET"), "stderr: {stderr}");
}

#[test]
fn output_mode_without_a_compositor_exits_non_zero() {
    let output = run_picker(&["--screencast"], None);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}
