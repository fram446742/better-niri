use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("better-settingsctl {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let output = better_settings::cli::run(std::env::args().skip(1).collect());
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    if output.code == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
