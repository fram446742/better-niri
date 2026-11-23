//! The `better-companion` daemon: one Unix-socket server over the bounded
//! contexts.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use companion_core::{Registry, Request, Response};

pub fn build_registry(socket: &Path) -> io::Result<Registry> {
    Ok(build_registry_with_parking(socket)?.0)
}

/// Builds the registry and returns the parking handle for event tracking.
pub fn build_registry_with_parking(
    socket: &Path,
) -> io::Result<(Registry, companion_parking::Parking)> {
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
    registry.register(companion_niri::Windows::new(
        companion_niri::Compositor::from_env(),
    ));
    registry.register(companion_niri::Compositor::from_env());
    registry.register(companion_rules::Rules::from_state_dir(parking.clone())?);
    Ok((registry, parking))
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

/// Serves one connection to completion.
pub fn serve_stream(stream: UnixStream, registry: &Registry) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => registry.handle(request),
            Err(err) => Response::Error {
                message: format!("invalid request: {err}"),
            },
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
                assert!(contexts.contains_key("parking"));
                assert!(contexts.contains_key("automation"));
                assert!(contexts.contains_key("hooks"));
                assert!(contexts.contains_key("system"));
                assert!(contexts.contains_key("compositor"));
                assert!(contexts.contains_key("parker"));
                assert!(contexts.contains_key("rules"));
                assert!(contexts.contains_key("windows"));
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
                "automation",
                "compositor",
                "hooks",
                "parker",
                "parking",
                "rules",
                "system",
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
}
