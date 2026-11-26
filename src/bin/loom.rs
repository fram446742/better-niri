//! One entry point for the Loom tools.
//!
//! Each companion keeps its own crate and binary; this command only selects one
//! by name, so the individual CLIs can still be installed and used on their own.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, exit};

struct Tool {
    name: &'static str,
    binary: &'static str,
    manifest: &'static str,
    summary: &'static str,
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "companion",
        binary: "loomctl",
        manifest: "loom-companion/Cargo.toml",
        summary: "Automation, staging, scratchpads, rules and window marks",
    },
    Tool {
        name: "companion-daemon",
        binary: "loom-companion",
        manifest: "loom-companion/Cargo.toml",
        summary: "Run the companion automation daemon in the foreground",
    },
    Tool {
        name: "portal",
        binary: "loom-portal",
        manifest: "loom-portal/Cargo.toml",
        summary: "Desktop portal backend",
    },
    Tool {
        name: "portal-picker",
        binary: "loom-portal-picker",
        manifest: "loom-portal/Cargo.toml",
        summary: "Portal selection UI",
    },
    Tool {
        name: "monitor",
        binary: "loom-monitor",
        manifest: "loom-monitor/Cargo.toml",
        summary: "Output profiles, rules and safe apply/rollback",
    },
    Tool {
        name: "session",
        binary: "loom-session",
        manifest: "loom-session/Cargo.toml",
        summary: "Save and restore work contexts",
    },
    Tool {
        name: "shot",
        binary: "loom-shot",
        manifest: "loom-shot/Cargo.toml",
        summary: "Capture, recording, annotations, OCR and pins",
    },
    Tool {
        name: "wallpaper",
        binary: "loom-wallpaper",
        manifest: "loom-wallpaper/Cargo.toml",
        summary: "Scenes and per-workspace wallpapers",
    },
    Tool {
        name: "settings",
        binary: "loom-settings",
        manifest: "loom-settings/Cargo.toml",
        summary: "Graphical settings",
    },
    Tool {
        name: "settingsctl",
        binary: "loom-settingsctl",
        manifest: "loom-settings/Cargo.toml",
        summary: "Settings inspection and KDL edits",
    },
    Tool {
        name: "audio",
        binary: "loom-audio",
        manifest: "loom-audio/Cargo.toml",
        summary: "PipeWire routing by application and window",
    },
    Tool {
        name: "sidepanels",
        binary: "loom-sidepanels",
        manifest: "loom-sidepanels/Cargo.toml",
        summary: "Layer-shell panels and floating side stacks",
    },
    Tool {
        name: "tui",
        binary: "loom-tui",
        manifest: "loom-tui/Cargo.toml",
        summary: "Dashboard for outputs, workspaces and windows",
    },
];

fn find_tool(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

fn usage() -> String {
    let mut text = String::from(
        "Loom tools\n\n\
         Usage: loom <tool> [args...]\n\
         \x20      loom --list\n\
         \x20      loom --version\n\n\
         Tools:\n",
    );
    let width = TOOLS.iter().map(|tool| tool.name.len()).max().unwrap_or(0);
    for tool in TOOLS {
        text.push_str(&format!(
            "  {:<width$}  {}\n",
            tool.name,
            tool.summary,
            width = width
        ));
    }
    text.push_str("\nRun `loom <tool> --help` for that tool's own options.\n");
    text
}

fn run(tool: &Tool, args: &[OsString]) -> ! {
    match Command::new(tool.binary).args(args).status() {
        Ok(status) => exit(status.code().unwrap_or(1)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            eprintln!("loom: cannot run {}: {err}", tool.binary);
            exit(1);
        }
    }

    // Source checkouts do not put the companion workspaces on PATH, so fall
    // back to their manifests. Installed packages always take the branch above.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join(tool.manifest);
    if !manifest.is_file() {
        eprintln!(
            "loom: {} is not installed and {} is unavailable",
            tool.binary, tool.manifest
        );
        exit(127);
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    match Command::new(cargo)
        .args(["run", "--quiet", "--manifest-path"])
        .arg(&manifest)
        .arg("--bin")
        .arg(tool.binary)
        .arg("--")
        .args(args)
        .status()
    {
        Ok(status) => exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("loom: cannot start cargo for {}: {err}", tool.binary);
            exit(1);
        }
    }
}

fn main() {
    let mut args = std::env::args_os();
    let _program = args.next();
    let Some(command) = args.next() else {
        print!("{}", usage());
        return;
    };

    if command == "--help" || command == "-h" || command == "help" {
        print!("{}", usage());
        return;
    }
    if command == "--version" || command == "-V" || command == "version" {
        println!("loom {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if command == "--list" || command == "list" {
        for tool in TOOLS {
            println!("{}", tool.name);
        }
        return;
    }

    let name = command.to_string_lossy();
    let Some(tool) = find_tool(&name) else {
        eprintln!("loom: unknown tool `{name}`\n");
        eprint!("{}", usage());
        exit(2);
    };

    let rest: Vec<OsString> = args.collect();
    run(tool, &rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_visible_tool_resolves_to_its_binary() {
        assert_eq!(find_tool("shot").unwrap().binary, "loom-shot");
        assert_eq!(find_tool("companion").unwrap().binary, "loomctl");
        assert_eq!(find_tool("monitor").unwrap().binary, "loom-monitor");
        assert!(find_tool("niri").is_none());
    }

    #[test]
    fn usage_lists_every_tool() {
        let text = usage();
        for tool in TOOLS {
            assert!(text.contains(tool.name), "missing {}", tool.name);
        }
    }
}
