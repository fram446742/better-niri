use std::path::PathBuf;
use std::process::ExitCode;

use companion_daemon::{bind, build_registry_with_parking, serve_stream, socket_default};

fn usage() -> &'static str {
    "better-companion [--socket PATH] [--once] [--help]\n\n\
     One daemon serving the parking, automation, hooks and system namespaces over a Unix socket.\n\
     --socket PATH  Override the socket path (default: $XDG_RUNTIME_DIR/better-companion.sock)\n\
     --once         Serve a single connection and exit (useful for tests)\n\
     --no-track     Do not subscribe to the niri event stream\n"
}

fn main() -> ExitCode {
    let mut socket: Option<PathBuf> = None;
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
            "--once" => once = true,
            "--no-track" => track = false,
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
    match run(&socket, once, track) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-companion: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(socket: &std::path::Path, once: bool, track: bool) -> std::io::Result<()> {
    let (registry, parking) = build_registry_with_parking(socket)?;
    if track {
        if let Some(niri_socket) = companion_niri::socket_from_env() {
            let parking = parking.clone();
            let swallow = companion_niri::Swallow::from_state_dir(
                parking.clone(),
                companion_niri::Compositor::from_env(),
            )?;
            std::thread::spawn(move || companion_niri::run_tracker(niri_socket, parking, swallow));
        }
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
        return serve_stream(stream, &registry);
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let registry = registry.clone();
                std::thread::spawn(move || {
                    if let Err(err) = serve_stream(stream, &registry) {
                        eprintln!("better-companion: connection error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("better-companion: accept error: {err}"),
        }
    }
    Ok(())
}
