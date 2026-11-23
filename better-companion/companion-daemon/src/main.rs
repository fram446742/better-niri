use std::path::PathBuf;
use std::process::ExitCode;

use companion_daemon::{bind, build_registry, serve_stream, socket_default};

fn usage() -> &'static str {
    "better-companion [--socket PATH] [--once] [--help]\n\n\
     One daemon serving the parking, automation, hooks and system namespaces over a Unix socket.\n\
     --socket PATH  Override the socket path (default: $XDG_RUNTIME_DIR/better-companion.sock)\n\
     --once         Serve a single connection and exit (useful for tests)\n"
}

fn main() -> ExitCode {
    let mut socket: Option<PathBuf> = None;
    let mut once = false;

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
    match run(&socket, once) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-companion: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(socket: &std::path::Path, once: bool) -> std::io::Result<()> {
    let registry = std::sync::Arc::new(build_registry(socket)?);
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
