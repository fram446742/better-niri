//! The `better-companion` daemon: one Unix-socket server over the bounded
//! contexts.
//!
//! One JSON request per line and one response per line. Malformed or unknown
//! requests produce an error response and keep the connection alive; request
//! lines are capped and drained so a client cannot allocate without bound.
//! A failing connection never stops the accept loop.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use companion_core::{Registry, Request, Response};

/// Largest accepted request line. Larger lines are drained and rejected so one
/// client cannot make the daemon allocate without bound.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

pub fn build_registry(socket: &Path) -> io::Result<Registry> {
    Ok(build_registry_with_parking(socket)?.0)
}

/// Builds the registry and returns the parking handle for event tracking.
pub fn build_registry_with_parking(
    socket: &Path,
) -> io::Result<(Registry, companion_parking::Parking)> {
    build_registry_with_activity(socket, companion_idle::ActivityClock::new())
}

/// Builds the registry with a caller-owned idle activity clock, so the daemon
/// can feed niri events into the same clock `idle.*` reads.
pub fn build_registry_with_activity(
    socket: &Path,
    activity: companion_idle::ActivityClock,
) -> io::Result<(Registry, companion_parking::Parking)> {
    let (registry, parking, _adapters) = build_registry_with_adapters(socket, activity)?;
    Ok((registry, parking))
}

/// Builds the registry and returns the adapter engine so the daemon can start
/// its worker and forward niri events to it.
pub fn build_registry_with_adapters(
    socket: &Path,
    activity: companion_idle::ActivityClock,
) -> io::Result<(
    Registry,
    companion_parking::Parking,
    companion_adapters::Adapters,
)> {
    let mut registry = Registry::with_socket(socket);
    let parking = companion_parking::Parking::from_state_dir()?;
    registry.register(parking.clone());
    registry.register(companion_automation::Automation::from_state_dir()?);
    registry.register(companion_hooks::Hooks::from_state_dir()?);
    registry.register(companion_system::System::new(socket));
    registry.register(companion_niri::Parker::new(
        parking.clone(),
        companion_niri::Compositor::from_env(),
    ));
    let windows = companion_niri::Windows::new(companion_niri::Compositor::from_env());
    let stage = companion_niri::Stage::new(parking.clone(), companion_niri::Compositor::from_env());
    let scratchpad =
        companion_niri::Scratchpad::new(parking.clone(), companion_niri::Compositor::from_env());
    registry.register(stage.clone());
    registry.register(scratchpad.clone());
    registry.register(windows.clone());
    registry.register(companion_niri::Nsticky::new(stage, scratchpad, windows));
    registry.register(companion_niri::Compositor::from_env());
    registry.register(companion_rules::Rules::from_state_dir(parking.clone())?);
    registry.register(companion_niri::Swallow::from_state_dir(
        parking.clone(),
        companion_niri::Compositor::from_env(),
    )?);
    registry.register(companion_idle::Idle::with_activity(
        companion_idle::state_path(),
        activity,
    )?);
    registry.register(companion_rotate::Rotate::from_state_dir()?);
    let adapters = companion_adapters::Adapters::from_state_dir()?;
    for adapter in adapters.adapters() {
        registry.register(adapter);
    }
    registry.register(adapters.context());
    registry.set_observer(adapters.observer());
    Ok((registry, parking, adapters))
}

/// Binds the socket, replacing a stale socket file.
pub fn bind(socket: &Path) -> io::Result<UnixListener> {
    if socket.exists() {
        let _ = std::fs::remove_file(socket);
    }
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket)?;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

/// Reads one request line, bounding the amount of memory a single line can
/// allocate.
fn read_request_line(reader: &mut BufReader<UnixStream>, line: &mut String) -> io::Result<usize> {
    line.clear();
    let read = {
        let mut limited = reader.by_ref().take(MAX_REQUEST_BYTES as u64 + 1);
        limited.read_line(line)?
    };
    if read > MAX_REQUEST_BYTES && !line.ends_with('\n') {
        discard_line(reader)?;
    }
    Ok(read)
}

/// Drops the remainder of an oversized line in fixed-size chunks.
fn discard_line(reader: &mut BufReader<UnixStream>) -> io::Result<()> {
    let mut buf = [0u8; 4096];
    loop {
        let read = reader.read(&mut buf)?;
        if read == 0 || buf[..read].contains(&b'\n') {
            return Ok(());
        }
    }
}

/// Serves one connection to completion.
pub fn serve_stream(stream: UnixStream, registry: &Registry) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        let read = read_request_line(&mut reader, &mut line)?;
        if read == 0 {
            return Ok(());
        }
        let response = if read > MAX_REQUEST_BYTES {
            Response::Error {
                message: format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
            }
        } else if line.trim().is_empty() {
            continue;
        } else {
            match serde_json::from_str::<Request>(line.trim()) {
                Ok(request) => registry.handle(request),
                Err(err) => Response::Error {
                    message: format!("invalid request: {err}"),
                },
            }
        };
        let body = serde_json::to_string(&response)
            .unwrap_or_else(|err| format!("{{\"kind\":\"error\",\"message\":\"{err}\"}}"));
        writer.write_all(body.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
}

pub fn socket_default() -> PathBuf {
    companion_core::socket_path()
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-daemon-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("daemon.sock")
    }

    #[test]
    fn serves_list_and_call_over_the_socket() {
        let socket = temp_socket("list-call");
        let listener = bind(&socket).unwrap();
        let registry = build_registry(&socket).unwrap();

        let server_socket = socket.clone();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            serve_stream(stream, &registry).unwrap();
            let _ = server_socket;
        });

        let stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);

        writer.write_all(b"{\"kind\":\"list\"}\n").unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response: Response = serde_json::from_str(line.trim()).unwrap();
        match response {
            Response::Contexts { contexts } => {
                assert!(contexts.contains_key("adapters"));
                assert!(contexts.contains_key("parking"));
                assert!(contexts.contains_key("automation"));
                assert!(contexts.contains_key("hooks"));
                assert!(contexts.contains_key("idle"));
                assert!(contexts.contains_key("rotate"));
                assert!(contexts.contains_key("system"));
                assert!(contexts.contains_key("compositor"));
                assert!(contexts.contains_key("parker"));
                assert!(contexts.contains_key("stage"));
                assert!(contexts.contains_key("scratchpad"));
                assert!(contexts.contains_key("nsticky"));
                assert!(contexts.contains_key("rules"));
                assert!(contexts.contains_key("windows"));
                assert!(contexts.contains_key("session"));
                assert!(contexts.contains_key("monitor"));
                assert!(contexts.contains_key("audio"));
                assert!(contexts.contains_key("wallpaper"));
                assert!(contexts.contains_key("swallow"));
            }
            other => panic!("unexpected: {other:?}"),
        }

        writer
            .write_all(b"{\"kind\":\"call\",\"namespace\":\"system\",\"action\":\"ping\",\"params\":null}\n")
            .unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let response: Response = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            response,
            Response::Value {
                value: serde_json::json!("pong")
            }
        );

        drop(writer);
        drop(reader);
        handle.join().unwrap();
    }

    #[test]
    fn registry_builds_all_contexts() {
        let socket = temp_socket("registry");
        let registry = build_registry(&socket).unwrap();
        let names = registry.namespaces();
        assert_eq!(
            names,
            vec![
                "adapters",
                "audio",
                "automation",
                "compositor",
                "hooks",
                "idle",
                "monitor",
                "nsticky",
                "parker",
                "parking",
                "rotate",
                "rules",
                "scratchpad",
                "session",
                "stage",
                "swallow",
                "system",
                "wallpaper",
                "windows",
            ]
        );
        let _ = std::fs::remove_file(&socket);
    }

    #[test]
    fn default_socket_is_under_runtime_dir_or_temp() {
        let path = socket_default();
        assert!(path.ends_with("better-companion.sock"));
    }

    fn read_response(reader: &mut BufReader<UnixStream>) -> Response {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[test]
    fn malformed_and_unknown_requests_do_not_close_the_connection() {
        let (client, server) = UnixStream::pair().unwrap();
        let registry = build_registry(&temp_socket("bad-lines")).unwrap();
        let handle = std::thread::spawn(move || serve_stream(server, &registry));

        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);

        writer.write_all(b"this is not json\n").unwrap();
        assert!(matches!(read_response(&mut reader), Response::Error { .. }));

        writer
            .write_all(b"{\"kind\":\"no_such_request\"}\n")
            .unwrap();
        assert!(matches!(read_response(&mut reader), Response::Error { .. }));

        // The connection still serves valid requests after both failures.
        writer.write_all(b"{\"kind\":\"list\"}\n").unwrap();
        assert!(matches!(
            read_response(&mut reader),
            Response::Contexts { .. }
        ));

        drop(writer);
        drop(reader);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn oversized_request_is_drained_and_rejected() {
        let (client, server) = UnixStream::pair().unwrap();
        let registry = build_registry(&temp_socket("oversized")).unwrap();
        let handle = std::thread::spawn(move || serve_stream(server, &registry));

        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);

        let oversized = format!("{}\n", "x".repeat(MAX_REQUEST_BYTES + 128));
        writer.write_all(oversized.as_bytes()).unwrap();
        match read_response(&mut reader) {
            Response::Error { message } => assert!(message.contains("exceeds"), "{message}"),
            other => panic!("unexpected: {other:?}"),
        }

        // The rest of the line was discarded, so the next request is clean.
        writer.write_all(b"{\"kind\":\"list\"}\n").unwrap();
        assert!(matches!(
            read_response(&mut reader),
            Response::Contexts { .. }
        ));

        drop(writer);
        drop(reader);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn partial_request_without_a_newline_is_processed() {
        use std::net::Shutdown;

        let (mut client, server) = UnixStream::pair().unwrap();
        let registry = build_registry(&temp_socket("partial")).unwrap();
        let handle = std::thread::spawn(move || serve_stream(server, &registry));

        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(b"{\"kind\":\"status\"}").unwrap();
        client.shutdown(Shutdown::Write).unwrap();

        let mut reader = BufReader::new(client);
        assert!(matches!(
            read_response(&mut reader),
            Response::Status { .. }
        ));
        handle.join().unwrap().unwrap();
    }
}
