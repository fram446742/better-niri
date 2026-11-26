//! The `loom-companion` daemon: one Unix-socket server over the bounded
//! contexts.
//!
//! One JSON request per line and one response per line. Malformed or unknown
//! requests produce an error response and keep the connection alive; request
//! lines are capped and drained so a client cannot allocate without bound.
//! A failing connection never stops the accept loop.
//!
//! `subscribe` turns a connection into a bounded, non-blocking event stream;
//! a slow client is dropped instead of stalling the daemon. Completed
//! rule/automation runs are recorded in the `history` context, persistent
//! window marks are shared by `marks`, `windows` and `rules`, and the optional
//! user config file is applied at startup and by `config.reload`.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use companion_config::{ConfigTarget, ReloadReport};
use companion_core::{EventBus, Registry, Request, Response, Step, is_known_topic};

/// Largest accepted request line. Larger lines are drained and rejected so one
/// client cannot make the daemon allocate without bound.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// Per-subscriber event queue. A client that falls this far behind is
/// disconnected rather than allowed to stall the daemon or grow memory.
const SUBSCRIPTION_QUEUE: usize = 128;

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
    let (registry, parking, adapters, _bus) = build_registry_with_bus(socket, activity)?;
    Ok((registry, parking, adapters))
}

/// Builds the registry and returns the event bus that serves subscriptions.
///
/// Every event-producing path (niri translation, completed companion actions
/// and adapter deliveries) publishes on this bus.
pub fn build_registry_with_bus(
    socket: &Path,
    activity: companion_idle::ActivityClock,
) -> io::Result<(
    Registry,
    companion_parking::Parking,
    companion_adapters::Adapters,
    EventBus,
)> {
    let runtime = build_runtime(socket, activity)?;
    Ok((
        runtime.registry,
        runtime.parking,
        runtime.adapters,
        runtime.bus,
    ))
}

/// Handles to the live contexts, so the daemon can register `config.*` after a
/// startup config pass and still apply hot reloads to the same instances.
pub struct Runtime {
    pub registry: Registry,
    pub parking: companion_parking::Parking,
    pub adapters: companion_adapters::Adapters,
    pub bus: EventBus,
    pub history: Arc<companion_history::History>,
    pub marks: companion_niri::Marks,
    pub rules: Arc<companion_rules::Rules>,
    pub hooks: Arc<companion_hooks::Hooks>,
    pub automation: Arc<companion_automation::Automation>,
}

impl Runtime {
    /// Config target backed by this runtime's live contexts.
    pub fn config_target(&self) -> Arc<DaemonConfigTarget> {
        Arc::new(DaemonConfigTarget {
            rules: self.rules.clone(),
            hooks: self.hooks.clone(),
            automation: self.automation.clone(),
            adapters: self.adapters.clone(),
        })
    }

    /// Applies the optional user config file and registers `config.*`.
    ///
    /// Missing files are a no-op. Validation happens before any context is
    /// changed, so an invalid file leaves the daemon untouched.
    pub fn apply_config(&mut self, path: &Path) -> Result<ReloadReport, String> {
        let config = companion_config::Config::new(path, Box::new(self.config_target()));
        let report = config.apply()?;
        self.registry.register(config);
        Ok(report)
    }
}

/// Applies config-file sections to the daemon's live contexts.
pub struct DaemonConfigTarget {
    rules: Arc<companion_rules::Rules>,
    hooks: Arc<companion_hooks::Hooks>,
    automation: Arc<companion_automation::Automation>,
    adapters: companion_adapters::Adapters,
}

impl ConfigTarget for DaemonConfigTarget {
    fn validate_adapter(&self, namespace: &str, patch: &serde_json::Value) -> Result<(), String> {
        self.adapters.validate_config(namespace, patch)
    }

    fn apply_rules(
        &self,
        rules: &BTreeMap<String, companion_rules::Rule>,
    ) -> Result<usize, String> {
        self.rules.apply_config(rules)
    }

    fn apply_hooks(&self, hooks: &BTreeMap<String, Vec<String>>) -> Result<usize, String> {
        self.hooks.apply_config(hooks)
    }

    fn apply_automation(&self, sequences: &BTreeMap<String, Vec<Step>>) -> Result<usize, String> {
        self.automation.apply_config(sequences)
    }

    fn apply_adapter(&self, namespace: &str, patch: &serde_json::Value) -> Result<(), String> {
        self.adapters.apply_config(namespace, patch)
    }
}

/// Builds every context and the event/history plumbing.
pub fn build_runtime(
    socket: &Path,
    activity: companion_idle::ActivityClock,
) -> io::Result<Runtime> {
    let mut registry = Registry::with_socket(socket);
    let parking = companion_parking::Parking::from_state_dir()?;
    registry.register(parking.clone());

    let automation = Arc::new(companion_automation::Automation::from_state_dir()?);
    let hooks = Arc::new(companion_hooks::Hooks::from_state_dir()?);
    let history = Arc::new(companion_history::History::from_state_dir()?);
    let marks = companion_niri::Marks::from_state_dir(companion_niri::Compositor::from_env())?;
    let rules = Arc::new(
        companion_rules::Rules::from_state_dir(parking.clone())?.with_marks(marks.clone()),
    );

    registry.register(automation.clone());
    registry.register(hooks.clone());
    registry.register(history.clone());
    registry.register(marks.clone());
    registry.set_recorder(history.clone());
    registry.register(companion_system::System::new(socket));
    registry.register(companion_niri::Parker::new(
        parking.clone(),
        companion_niri::Compositor::from_env(),
    ));
    let windows = companion_niri::Windows::new(companion_niri::Compositor::from_env())
        .with_marks(marks.clone());
    let stage = companion_niri::Stage::new(parking.clone(), companion_niri::Compositor::from_env());
    let scratchpad =
        companion_niri::Scratchpad::new(parking.clone(), companion_niri::Compositor::from_env());
    registry.register(stage.clone());
    registry.register(scratchpad.clone());
    registry.register(windows.clone());
    registry.register(companion_niri::Nsticky::new(stage, scratchpad, windows));
    registry.register(companion_niri::Compositor::from_env());
    registry.register(rules.clone());
    registry.register(companion_niri::Swallow::from_state_dir(
        parking.clone(),
        companion_niri::Compositor::from_env(),
    )?);
    registry.register(companion_idle::Idle::with_activity(
        companion_idle::state_path(),
        activity,
    )?);
    registry.register(companion_rotate::Rotate::from_state_dir()?);
    registry.register(companion_niri::ScreenTime::from_state_dir(
        companion_niri::Compositor::from_env(),
    )?);
    let adapters = companion_adapters::Adapters::from_state_dir()?;
    let bus = EventBus::new();
    adapters.set_event_bus(bus.clone());
    for adapter in adapters.adapters() {
        registry.register(adapter);
    }
    registry.register(adapters.context());
    registry.set_observer(adapters.observer());
    Ok(Runtime {
        registry,
        parking,
        adapters,
        bus,
        history,
        marks,
        rules,
        hooks,
        automation,
    })
}

/// Startup config pass. A missing file is fine; invalid content is an error.
pub fn apply_config_startup(config: &companion_config::Config) -> Result<ReloadReport, String> {
    config.apply()
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
    serve_stream_with_events(stream, registry, None)
}

/// Serves one connection, optionally with the daemon event bus.
///
/// Without a bus, `subscribe` is rejected with `stream_required`. With a bus,
/// a subscribed connection becomes a read-only event stream until it closes.
pub fn serve_stream_with_events(
    stream: UnixStream,
    registry: &Registry,
    bus: Option<&EventBus>,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        let read = read_request_line(&mut reader, &mut line)?;
        if read == 0 {
            return Ok(());
        }
        let request = if read > MAX_REQUEST_BYTES {
            write_response(
                &mut writer,
                &Response::error(
                    "request_too_large",
                    format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
                ),
            )?;
            continue;
        } else if line.trim().is_empty() {
            continue;
        } else {
            match serde_json::from_str::<Request>(line.trim()) {
                Ok(request) => request,
                Err(err) => {
                    write_response(
                        &mut writer,
                        &Response::error("invalid_request", format!("invalid request: {err}")),
                    )?;
                    continue;
                }
            }
        };

        if let Request::Subscribe { events, id } = request {
            let Some(bus) = bus else {
                write_response(
                    &mut writer,
                    &Response::error(
                        "stream_required",
                        "this connection is not serving event subscriptions",
                    )
                    .with_id(id),
                )?;
                continue;
            };
            if let Some(topic) = events.iter().find(|topic| !is_known_topic(topic)) {
                let mut known: Vec<&str> = companion_core::EVENT_TOPICS.to_vec();
                known.push("*");
                write_response(
                    &mut writer,
                    &Response::error(
                        "unknown_topic",
                        format!(
                            "unknown event topic `{topic}`; known topics: {}",
                            known.join(", ")
                        ),
                    )
                    .with_id(id),
                )?;
                continue;
            }
            let subscription = bus.subscribe(events.clone(), SUBSCRIPTION_QUEUE);
            write_response(&mut writer, &Response::Subscribed { events, id })?;
            return stream_events(&mut writer, reader, &subscription, id);
        }

        let response = registry.handle(request);
        write_response(&mut writer, &response)?;
    }
}

/// Writes queued events until the client disconnects or falls too far behind.
fn stream_events(
    writer: &mut UnixStream,
    mut reader: BufReader<UnixStream>,
    subscription: &companion_core::EventSubscription,
    id: Option<u64>,
) -> io::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};

    // A subscribed connection is read-only; the watcher exists only to notice
    // EOF/half-close so an idle subscription does not leak its server thread.
    let disconnected = Arc::new(AtomicBool::new(false));
    let watcher_disconnected = disconnected.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 1];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        watcher_disconnected.store(true, Ordering::Relaxed);
    });

    loop {
        if disconnected.load(Ordering::Relaxed) {
            break;
        }
        match subscription.recv_timeout(std::time::Duration::from_millis(100)) {
            companion_core::EventRecv::Event(event) => {
                let response = Response::Event {
                    event: event.event,
                    payload: event.payload,
                    seq: event.seq,
                    timestamp_ms: event.timestamp_ms,
                    id,
                };
                write_response(writer, &response)?;
            }
            companion_core::EventRecv::Timeout => {}
            companion_core::EventRecv::Closed => break,
        }
    }
    if subscription.is_slow() {
        write_response(
            writer,
            &Response::error(
                "slow_subscriber",
                format!(
                    "subscription closed: client fell behind; {} event(s) dropped",
                    subscription.dropped()
                ),
            )
            .with_id(id),
        )?;
    }
    Ok(())
}

fn write_response(writer: &mut UnixStream, response: &Response) -> io::Result<()> {
    let body = serde_json::to_string(response)
        .unwrap_or_else(|err| format!("{{\"kind\":\"error\",\"message\":\"{err}\"}}"));
    writer.write_all(body.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
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
            "loom-companion-daemon-{}-{name}",
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
            Response::Contexts { contexts, .. } => {
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
                value: serde_json::json!("pong"),
                id: None,
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
                "history",
                "hooks",
                "idle",
                "marks",
                "monitor",
                "nsticky",
                "parker",
                "parking",
                "rotate",
                "rules",
                "scratchpad",
                "screentime",
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
        assert!(path.ends_with("loom-companion.sock"));
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
            Response::Error { message, .. } => assert!(message.contains("exceeds"), "{message}"),
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
    fn describe_reports_mark_history_and_parameter_metadata() {
        let registry = build_registry(&temp_socket("describe")).unwrap();
        let Response::Description {
            protocol,
            version,
            actions,
            ..
        } = registry.handle(Request::Describe)
        else {
            panic!("unexpected response");
        };
        assert_eq!(protocol, companion_core::PROTOCOL);
        assert_eq!(version, 2);

        let marks_set = &actions["marks"][0];
        assert_eq!(marks_set.name, "set");
        assert_eq!(marks_set.owner, "marks");
        assert!(
            marks_set
                .params
                .iter()
                .any(|param| param.name == "mark" && param.required)
        );

        let find = actions["windows"]
            .iter()
            .find(|action| action.name == "find")
            .unwrap();
        assert!(find.params.iter().any(|param| param.name == "mark"));
        assert!(
            find.permissions
                .iter()
                .any(|permission| permission == "niri")
        );

        let park = actions["parker"]
            .iter()
            .find(|action| action.name == "park")
            .unwrap();
        assert!(park.params.iter().any(|param| param.name == "window"));

        let list = actions["history"]
            .iter()
            .find(|action| action.name == "list")
            .unwrap();
        assert!(list.params.iter().any(|param| param.name == "limit"));

        let subscribe = actions["swallow"]
            .iter()
            .find(|action| action.name == "add-rule")
            .unwrap();
        assert!(subscribe.params.iter().any(|param| param.name == "name"));
    }

    #[test]
    fn config_context_is_registered_and_works_without_a_file() {
        let socket = temp_socket("config");
        let mut runtime = build_runtime(&socket, companion_idle::ActivityClock::new()).unwrap();
        let path = socket.with_file_name("missing-config.json");
        let report = runtime.apply_config(&path).unwrap();
        assert!(!report.reloaded);

        let Response::Contexts { contexts, .. } = runtime.registry.handle(Request::List) else {
            panic!("unexpected response");
        };
        assert!(contexts.contains_key("config"));

        let Response::Value { value, .. } = runtime.registry.handle(Request::Call {
            namespace: "config".into(),
            action: "show".into(),
            params: serde_json::json!({}),
            id: None,
        }) else {
            panic!("unexpected response");
        };
        assert_eq!(value["exists"], false);
        assert_eq!(value["path"], path.display().to_string());
        let _ = std::fs::remove_file(&socket);
    }

    #[test]
    fn subscription_streams_matching_events_and_stops_on_disconnect() {
        let (client, server) = UnixStream::pair().unwrap();
        let registry = build_registry(&temp_socket("subscribe")).unwrap();
        let bus = EventBus::new();
        let server_bus = bus.clone();
        let handle = std::thread::spawn(move || {
            serve_stream_with_events(server, &registry, Some(&server_bus))
        });

        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);

        writer
            .write_all(b"{\"kind\":\"subscribe\",\"events\":[\"park\"],\"id\":5}\n")
            .unwrap();
        let ack: Response = {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str(line.trim()).unwrap()
        };
        assert_eq!(
            ack,
            Response::Subscribed {
                events: vec!["park".into()],
                id: Some(5),
            }
        );

        // A non-matching event is not forwarded.
        bus.publish(companion_core::Event::new(
            "restore",
            serde_json::json!({"window": 1}),
        ));
        // The matching event is forwarded with the request id.
        bus.publish(companion_core::Event::new(
            "park",
            serde_json::json!({"window": 2}),
        ));
        let event: Response = {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str(line.trim()).unwrap()
        };
        match event {
            Response::Event {
                event,
                payload,
                seq,
                id,
                ..
            } => {
                assert_eq!(event, "park");
                assert_eq!(payload["window"], 2);
                assert!(seq >= 1);
                assert_eq!(id, Some(5));
            }
            other => panic!("unexpected: {other:?}"),
        }

        drop(writer);
        drop(reader);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn slow_subscriber_is_closed_with_a_clear_final_error() {
        let (client, server) = UnixStream::pair().unwrap();
        let bus = EventBus::new();
        let subscription = bus.subscribe(["park".to_owned()], 1);
        bus.publish(companion_core::Event::new(
            "park",
            serde_json::json!({"window": 1}),
        ));
        // The second event overflows the queue and removes the subscriber.
        bus.publish(companion_core::Event::new(
            "park",
            serde_json::json!({"window": 2}),
        ));
        assert!(subscription.is_slow());

        let reader = BufReader::new(server.try_clone().unwrap());
        let mut writer = server;
        let handle =
            std::thread::spawn(move || stream_events(&mut writer, reader, &subscription, Some(3)));

        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(client);
        let event: Response = {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str(line.trim()).unwrap()
        };
        assert!(matches!(event, Response::Event { .. }));
        let error: Response = {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str(line.trim()).unwrap()
        };
        match error {
            Response::Error { code, message, id } => {
                assert_eq!(code.as_deref(), Some("slow_subscriber"));
                assert_eq!(id, Some(3));
                assert!(message.contains("dropped"), "{message}");
            }
            other => panic!("unexpected: {other:?}"),
        }
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn subscribe_rejects_unknown_topics_and_requires_a_bus() {
        let (client, server) = UnixStream::pair().unwrap();
        let registry = build_registry(&temp_socket("subscribe-bad")).unwrap();
        let bus = EventBus::new();
        let handle =
            std::thread::spawn(move || serve_stream_with_events(server, &registry, Some(&bus)));

        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        writer
            .write_all(b"{\"kind\":\"subscribe\",\"events\":[\"not-a-topic\"]}\n")
            .unwrap();
        match read_response(&mut reader) {
            Response::Error { code, message, .. } => {
                assert_eq!(code.as_deref(), Some("unknown_topic"));
                assert!(message.contains("not-a-topic"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        drop(writer);
        drop(reader);
        handle.join().unwrap().unwrap();

        // Without a bus the same request is rejected instead of hanging.
        let (client, server) = UnixStream::pair().unwrap();
        let registry = build_registry(&temp_socket("subscribe-nobus")).unwrap();
        let handle = std::thread::spawn(move || serve_stream(server, &registry));
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        writer
            .write_all(b"{\"kind\":\"subscribe\",\"events\":[\"park\"]}\n")
            .unwrap();
        match read_response(&mut reader) {
            Response::Error { code, .. } => {
                assert_eq!(code.as_deref(), Some("stream_required"));
            }
            other => panic!("unexpected: {other:?}"),
        }
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
