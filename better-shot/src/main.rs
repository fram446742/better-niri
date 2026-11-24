use std::path::PathBuf;
use std::process::ExitCode;

use better_shot::cli::{self, CliAction};
use better_shot::{record, run};

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
    let parsed = match cli::parse(std::env::args().skip(1))? {
        CliAction::Help => {
            print!("{}", cli::USAGE);
            print!("\n{}", record::USAGE);
            return Ok(());
        }
        CliAction::Run(parsed) => parsed,
        CliAction::Record(options) => {
            let outcome = record::run(&record::Env::from_process(), &options)?;
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&outcome.to_json()).unwrap()
                );
            } else {
                println!("{}", outcome.summary());
            }
            return Ok(());
        }
    };
    let socket = parsed
        .socket
        .or_else(|| std::env::var_os("NIRI_SOCKET").map(PathBuf::from))
        .ok_or_else(|| "NIRI_SOCKET is not set; run inside niri or pass --socket".to_owned())?;
    let outcome = run(&socket, &parsed.options)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "path": outcome.path,
            "format": outcome.format.name(),
            "copied": outcome.copied,
            "annotated": outcome.annotated,
            "metadata_path": outcome.metadata_path,
            "width": outcome.width,
            "height": outcome.height,
            "source_color": outcome.source_color,
            "hdr": outcome.hdr.to_json(),
            "action": outcome.action,
            "dry_run": parsed.options.dry_run,
        }))
        .unwrap()
    );
    Ok(())
}
