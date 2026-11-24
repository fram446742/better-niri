//! End-to-end CLI tests with a fake niri IPC socket and an isolated XDG tree.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use niri_ipc::{Reply, Request, Response};
use serde_json::Value;

struct FakeNiri {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for FakeNiri {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Fixture {
    root: PathBuf,
    _niri: Option<FakeNiri>,
}

impl Fixture {
    fn new(name: &str, windows: Vec<Value>) -> Self {
        let root =
            std::env::temp_dir().join(format!("better-session-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let niri = if windows.is_empty() && name != "empty" {
            None
        } else {
            Some(start_fake(&root, windows))
        };
        Self { root, _niri: niri }
    }

    fn socket(&self) -> PathBuf {
        self.root.join("niri.sock")
    }

    fn state(&self) -> PathBuf {
        self.root.join("state/contexts.json")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_better-session"));
        command
            .args(args)
            .env("NIRI_SOCKET", self.socket())
            .env("BETTER_SESSION_STATE", self.state())
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_DATA_DIRS", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("BETTER_SESSION_HISTORY", self.root.join("state/history"))
            .env("HOME", self.root.join("home"))
            .env_remove("BETTER_MONITOR_STATE")
            .env_remove("BETTER_AUDIO_CONFIG");
        command
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "command {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.run(args)).unwrap()
    }
}

fn start_fake(root: &Path, windows: Vec<Value>) -> FakeNiri {
    let path = root.join("niri.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::spawn({
        let stop = Arc::clone(&stop);
        move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => handle(stream, &windows),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        }
    });
    FakeNiri {
        stop,
        thread: Some(thread),
    }
}

fn handle(mut stream: UnixStream, windows: &[Value]) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Ok(request) = serde_json::from_str::<Request>(line.trim()) else {
        return;
    };
    let response = match request {
        Request::Outputs => Response::Outputs(std::collections::HashMap::new()),
        Request::Workspaces => Response::Workspaces(
            serde_json::from_value(serde_json::json!([{
                "id": 1,
                "idx": 1,
                "name": "work",
                "output": "eDP-1",
                "is_urgent": false,
                "is_active": true,
                "is_focused": true,
                "active_window_id": null,
                "is_hidden": false
            }]))
            .unwrap(),
        ),
        Request::Windows => Response::Windows(
            windows
                .iter()
                .cloned()
                .map(|window| serde_json::from_value(window).unwrap())
                .collect(),
        ),
        _ => Response::Handled,
    };
    let reply: Reply = Ok(response);
    stream
        .write_all(serde_json::to_string(&reply).unwrap().as_bytes())
        .unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
}

fn window(app_id: &str) -> Value {
    serde_json::json!({
        "id": 10,
        "title": "",
        "app_id": app_id,
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
}

#[test]
fn cli_capture_template_snapshot_history_and_export() {
    let fixture = Fixture::new("flow", vec![window("my-editor")]);
    let args = [
        "capture",
        "work",
        "--template",
        "development",
        "--set",
        "code_app=my-editor",
        "--set",
        "code_cmd=my-editor",
        "--set",
        "browser_app=my-browser",
        "--set",
        "browser_cmd=my-browser",
        "--set",
        "terminal_app=my-terminal",
        "--set",
        "terminal_cmd=my-terminal",
    ];
    let captured = fixture.json(&args);
    assert_eq!(captured["template"], "development");
    assert_eq!(captured["command_map"]["my-editor"], "my-editor");

    let first = fixture.json(&["snapshot", "--auto", "--name", "work", "--keep", "2"]);
    let second = fixture.json(&["snapshot", "--auto", "--name", "work", "--keep", "2"]);
    assert_eq!(first["trigger"], "auto");
    assert_ne!(first["id"], second["id"]);

    fixture.run(&[
        "snapshot",
        "--auto",
        "--name",
        "work",
        "--interval-ms",
        "10",
        "--count",
        "2",
        "--keep",
        "5",
    ]);
    let listed = fixture.json(&["history", "list"]);
    assert_eq!(listed.as_array().unwrap().len(), 4);
    assert_eq!(listed[0]["trigger"], "auto");

    let shown = fixture.json(&["history", "show", second["id"].as_str().unwrap()]);
    assert_eq!(shown["context"]["windows"].as_array().unwrap().len(), 1);
    let report = fixture.json(&[
        "history",
        "restore",
        second["id"].as_str().unwrap(),
        "--dry-run",
    ]);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["ok"], true);

    let pruned = fixture.json(&["history", "prune", "--keep", "1"]);
    assert_eq!(pruned["removed"].as_array().unwrap().len(), 3);
    assert_eq!(
        fixture.json(&["history", "list"]).as_array().unwrap().len(),
        1
    );

    let single_path = fixture.root.join("single.json");
    fixture.run(&["export", "work", "--file", single_path.to_str().unwrap()]);
    let single: Value = serde_json::from_slice(&std::fs::read(&single_path).unwrap()).unwrap();
    assert_eq!(single["name"], "work");
    assert_eq!(single["version"], 1);

    let all_path = fixture.root.join("all.json");
    fixture.run(&["export", "--file", all_path.to_str().unwrap()]);
    let imported = fixture.json(&["import", "--file", all_path.to_str().unwrap(), "--merge"]);
    assert_eq!(imported["updated"], serde_json::json!(["work"]));

    let deactivated = fixture.json(&["deactivate", "work", "--dry-run"]);
    assert_eq!(deactivated["dry_run"], true);
    assert_eq!(deactivated["ok"], true);
}

#[test]
fn cli_template_apply_and_import_replace() {
    let fixture = Fixture::new("templates", Vec::new());
    let dev_args = [
        "template",
        "apply",
        "development",
        "dev",
        "--set",
        "code_app=editor",
        "--set",
        "code_cmd=editor",
        "--set",
        "browser_app=browser",
        "--set",
        "browser_cmd=browser",
        "--set",
        "terminal_app=terminal",
        "--set",
        "terminal_cmd=terminal",
    ];
    let dev = fixture.json(&dev_args);
    assert_eq!(dev["template"], "development");
    assert_eq!(dev["workspaces"].as_array().unwrap().len(), 3);

    fixture.json(&[
        "template",
        "apply",
        "gaming",
        "game",
        "--set",
        "game_app=game",
        "--set",
        "game_cmd=game",
        "--set",
        "chat_app=chat",
        "--set",
        "chat_cmd=chat",
    ]);
    assert_eq!(fixture.json(&["list"]), serde_json::json!(["dev", "game"]));

    let single_path = fixture.root.join("dev.json");
    fixture.run(&["export", "dev", "--file", single_path.to_str().unwrap()]);
    let report = fixture.json(&[
        "import",
        "--file",
        single_path.to_str().unwrap(),
        "--replace",
    ]);
    assert_eq!(report["replaced"], true);
    assert_eq!(report["removed"], serde_json::json!(["game"]));
    assert_eq!(fixture.json(&["list"]), serde_json::json!(["dev"]));

    let output = Command::new(env!("CARGO_BIN_EXE_better-session"))
        .args(["template", "apply", "work", "broken"])
        .env("BETTER_SESSION_STATE", fixture.state())
        .env("HOME", fixture.root.join("home"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("needs values for"));
}

#[test]
fn cli_reports_precise_usage_errors() {
    let fixture = Fixture::new("usage", Vec::new());
    let output = fixture.command(&["restore"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("restore needs a context name"));

    let output = fixture.command(&["import"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid JSON"));
}
