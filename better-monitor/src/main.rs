use std::path::PathBuf;
use std::process::ExitCode;

use better_monitor::{Store, apply, query_outputs, restore, snapshot, state_path};

fn usage() -> &'static str {
    "better-monitor [--socket PATH] <command>\n\n\
     Commands:\n\
       list                         List saved profiles\n\
       show <name>                  Print a profile\n\
       save <name> [output...]      Snapshot current outputs into <name>\n\
       apply <name> [--dry-run]     Apply <name> (stores __previous first)\n\
       restore [--dry-run]          Re-apply __previous\n\
       delete <name>                Delete a profile\n\n\
     Socket defaults to $NIRI_SOCKET; state to $BETTER_MONITOR_STATE or\n\
     $XDG_STATE_HOME/better-monitor/profiles.json."
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-monitor: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut socket = std::env::var_os("NIRI_SOCKET").map(PathBuf::from);
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
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            _ => args.push(arg),
        }
    }
    let Some(command) = args.first().cloned() else {
        return Err(format!("missing command\n\n{}", usage()));
    };

    let state = std::env::var_os("BETTER_MONITOR_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(state_path);
    let mut store = Store::load(&state).map_err(|err| format!("cannot load state: {err}"))?;

    let socket = || {
        socket
            .clone()
            .ok_or_else(|| "NIRI_SOCKET is not set; run inside niri or pass --socket".to_owned())
    };

    match command.as_str() {
        "list" => {
            let names: Vec<_> = store.profiles.keys().cloned().collect();
            println!("{}", serde_json::to_string_pretty(&names).unwrap());
        }
        "show" => {
            let name = args
                .get(1)
                .ok_or_else(|| "show needs a profile name".to_owned())?;
            let profile = store
                .get(name)
                .ok_or_else(|| format!("unknown profile `{name}`"))?;
            println!("{}", serde_json::to_string_pretty(profile).unwrap());
        }
        "delete" => {
            let name = args
                .get(1)
                .ok_or_else(|| "delete needs a profile name".to_owned())?;
            if !store.remove(name) {
                return Err(format!("unknown profile `{name}`"));
            }
            store
                .save(&state)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", serde_json::json!({ "deleted": name }));
        }
        "save" => {
            let name = args
                .get(1)
                .ok_or_else(|| "save needs a profile name".to_owned())?;
            let outputs = query_outputs(&socket()?)?;
            let mut profile = snapshot(&outputs);
            let only: Vec<String> = args.iter().skip(2).cloned().collect();
            if !only.is_empty() {
                profile.outputs.retain(|name, _| only.contains(name));
            }
            if profile.outputs.is_empty() {
                return Err("no matching outputs to save".to_owned());
            }
            store.insert(name.clone(), profile.clone());
            store
                .save(&state)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", serde_json::to_string_pretty(&profile).unwrap());
        }
        "apply" => {
            let name = args
                .get(1)
                .ok_or_else(|| "apply needs a profile name".to_owned())?;
            let dry_run = args.iter().any(|arg| arg == "--dry-run");
            let planned = apply(&socket()?, &mut store, name, &state, dry_run)?;
            println!("{}", serde_json::to_string_pretty(&planned).unwrap());
        }
        "restore" => {
            let dry_run = args.iter().any(|arg| arg == "--dry-run");
            let planned = restore(&socket()?, &mut store, &state, dry_run)?;
            println!("{}", serde_json::to_string_pretty(&planned).unwrap());
        }
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}
