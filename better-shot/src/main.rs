use std::path::PathBuf;
use std::process::ExitCode;

use better_shot::{run, Mode, Options};

fn usage() -> &'static str {
    "better-shot [--socket PATH] <ui|screen|window> [options]\n\n\
     Options:\n\
       --path PATH       Save here (default: Screenshots/better-shot-<epoch>.png)\n\
       --id ID           Window id for `window` (default: focused)\n\
       --no-pointer      Hide the pointer\n\
       --copy            Pipe the saved PNG to wl-copy\n\
       --annotate        Open the saved PNG in satty\n\
       --dry-run         Print the planned action without capturing\n\n\
     Socket defaults to $NIRI_SOCKET."
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-shot: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> Result<(), String> {
    let mut socket = std::env::var_os("NIRI_SOCKET").map(PathBuf::from);
    let mut options = Options::default();
    let mut mode = None;
    let mut args = Vec::new();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--socket needs a path".to_owned())?,
                ))
            }
            "--path" => {
                options.path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--path needs a value".to_owned())?,
                ))
            }
            "--id" => {
                options.window_id = Some(
                    iter.next()
                        .ok_or_else(|| "--id needs a number".to_owned())?
                        .parse()
                        .map_err(|_| "--id needs a number".to_owned())?,
                )
            }
            "--no-pointer" => options.show_pointer = false,
            "--pointer" => options.show_pointer = true,
            "--copy" => options.copy = true,
            "--annotate" => options.annotate = true,
            "--dry-run" => options.dry_run = true,
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            "ui" | "screen" | "window" => mode = Some(arg),
            _ => args.push(arg),
        }
    }
    if let Some(extra) = args.first() {
        return Err(format!("unexpected argument `{extra}`\n\n{}", usage()));
    }
    options.mode = match mode.as_deref() {
        Some("screen") => Mode::Screen,
        Some("window") => Mode::Window,
        Some("ui") | None => Mode::Ui,
        _ => unreachable!(),
    };
    if options.mode == Mode::Ui && (options.copy || options.annotate) {
        return Err("--copy/--annotate are not supported with the interactive UI mode".to_owned());
    }

    let socket = socket
        .ok_or_else(|| "NIRI_SOCKET is not set; run inside niri or pass --socket".to_owned())?;
    let outcome = run(&socket, &options)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "path": outcome.path,
            "copied": outcome.copied,
            "annotated": outcome.annotated,
            "action": outcome.action,
            "dry_run": options.dry_run,
        }))
        .unwrap()
    );
    Ok(())
}
