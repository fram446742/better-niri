//! Forward to a companion workspace binary so the tools are runnable from the
//! repository root with `cargo run --bin <tool>`.
//!
//! The companion apps intentionally keep their own Cargo workspaces and
//! lockfiles (they are independently packageable/extractable), so the root
//! binary is a tiny shim that invokes the tool's own `cargo run`.

use std::process::Command;

pub fn run(tool: &str, manifest: &str) -> ! {
    let root = env!("CARGO_MANIFEST_DIR");
    let manifest = format!("{root}/{manifest}");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let status = Command::new(cargo)
        .arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--bin")
        .arg(tool)
        .arg("--")
        .args(std::env::args_os().skip(1))
        .status();

    match status {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("{tool}: cannot run its workspace manifest {manifest}: {err}");
            std::process::exit(1);
        }
    }
}
