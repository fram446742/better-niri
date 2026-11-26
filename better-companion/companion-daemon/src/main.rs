use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use companion_core::{Registry, Request, Response};
use companion_daemon::{bind, build_runtime, serve_stream_with_events, socket_default};

const USAGE: &str = "better-companion [--socket PATH] [--config PATH] [--once] [--no-track] [--help]\n\n\
     One daemon serving parking, automation, hooks, system, windows, idle, rotate and optional adapters over a Unix socket.\n\
     --socket PATH  Override the socket path (default: $XDG_RUNTIME_DIR/better-companion.sock)\n\
     --config PATH  Config file (default: $XDG_CONFIG_HOME/better-companion/config.json)\n\
     --once         Serve a single connection and exit (useful for tests)\n\
     --no-track     Do not subscribe to the niri event stream\n";

fn usage() -> &'static str {
    USAGE
}

fn main() -> ExitCode {
    let mut socket: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut once = false;
    let mut track = true;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => match args.next() {
                Some(path) => socket = Some(PathBuf::from(path)),
                None => {
                    eprintln!("--socket needs a path");
                    return ExitCode::FAILURE;
                }
            },
            "--config" => match args.next() {
                Some(path) => config = Some(PathBuf::from(path)),
                None => {
                    eprintln!("--config needs a path");
                    return ExitCode::FAILURE;
                }
            },
            "--once" => once = true,
            "--no-track" => track = false,
            "--version" | "-V" => {
                println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unexpected argument `{other}`\n\n{}", usage());
                return ExitCode::FAILURE;
            }
        }
    }

    let socket = socket.unwrap_or_else(socket_default);
    let config = config.unwrap_or_else(companion_core::config_path);
    match run(&socket, &config, once, track) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-companion: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    socket: &std::path::Path,
    config_path: &std::path::Path,
    once: bool,
    track: bool,
) -> std::io::Result<()> {
    let activity = companion_idle::ActivityClock::new();
    let mut runtime = build_runtime(socket, activity.clone())?;
    runtime
        .apply_config(config_path)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let companion_daemon::Runtime {
        registry,
        parking,
        adapters,
        bus,
        ..
    } = runtime;
    adapters.start_worker();
    if track && let Some(niri_socket) = companion_niri::socket_from_env() {
        let parking = parking.clone();
        let swallow = companion_niri::Swallow::from_state_dir(
            parking.clone(),
            companion_niri::Compositor::from_env(),
        )?;
        let tracker_socket = niri_socket.clone();
        let tracker_adapters = adapters.clone();
        std::thread::spawn(move || {
            companion_niri::run_tracker_with(
                tracker_socket,
                parking,
                swallow,
                || tracker_adapters.reset_niri(),
                |event| tracker_adapters.niri_event(event),
            );
        });
        // A second, read-only subscription marks focus/window/workspace events
        // as idle activity. niri serves each connection independently.
        std::thread::spawn(move || companion_idle::run_activity_stream(niri_socket, activity));
    }
    let registry = std::sync::Arc::new(registry);
    let listener = bind(socket)?;
    eprintln!(
        "better-companion {} listening on {}",
        companion_core::VERSION,
        socket.display()
    );

    if once {
        let (stream, _) = listener.accept()?;
        return serve_stream_with_events(stream, &registry, Some(&bus));
    }

    // Timer loops make idle timeouts and auto-rotation autonomous. Both
    // contexts are inert until configured; a disabled policy returns before
    // probing, so these loops stay cheap.
    spawn_ticker(registry.clone(), "idle", "tick", Duration::from_secs(5));
    spawn_ticker(registry.clone(), "rotate", "tick", Duration::from_secs(1));
    spawn_ticker(
        registry.clone(),
        "screentime",
        "tick",
        Duration::from_secs(1),
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let registry = registry.clone();
                let bus = bus.clone();
                std::thread::spawn(move || {
                    if let Err(err) = serve_stream_with_events(stream, &registry, Some(&bus)) {
                        eprintln!("better-companion: connection error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("better-companion: accept error: {err}"),
        }
    }
    Ok(())
}

/// Calls one namespace action on an interval, logging only errors.
fn spawn_ticker(
    registry: Arc<Registry>,
    namespace: &'static str,
    action: &'static str,
    interval: Duration,
) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(interval);
            let response = registry.handle(Request::Call {
                namespace: namespace.to_owned(),
                action: action.to_owned(),
                params: serde_json::json!({}),
                id: None,
            });
            if let Response::Error { message, .. } = response {
                eprintln!("better-companion: {namespace}.{action}: {message}");
            }
        }
    });
}
