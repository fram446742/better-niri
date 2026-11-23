use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use better_session::{execute, plan, query, snapshot, state_path, Session, Store};

fn usage() -> &'static str {
    "better-session [--socket PATH] <command>\n\n\
     Commands:\n\
       list                          List saved sessions\n\
       show <name>                   Print a session\n\
       save <name> [--launch app=cmd]...\n\
                                     Snapshot workspaces/windows; keep launch commands\n\
       restore <name> [--dry-run]    Move existing windows, spawn configured apps\n\
       delete <name>                 Delete a session\n\n\
     Socket defaults to $NIRI_SOCKET; state to $BETTER_SESSION_STATE or\n\
     $XDG_STATE_HOME/better-session/sessions.json."
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-session: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut socket = std::env::var_os("NIRI_SOCKET").map(PathBuf::from);
    let mut args = Vec::new();
    let mut launches = BTreeMap::new();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--socket needs a path".to_owned())?,
                ))
            }
            "--launch" => {
                let pair = iter
                    .next()
                    .ok_or_else(|| "--launch needs app_id=command".to_owned())?;
                let (app, command) = pair
                    .split_once('=')
                    .ok_or_else(|| "--launch needs app_id=command".to_owned())?;
                launches.insert(app.to_owned(), command.to_owned());
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

    let state = std::env::var_os("BETTER_SESSION_STATE")
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
            let names: Vec<_> = store.sessions.keys().cloned().collect();
            println!("{}", serde_json::to_string_pretty(&names).unwrap());
        }
        "show" => {
            let name = args
                .get(1)
                .ok_or_else(|| "show needs a session name".to_owned())?;
            let session = store
                .sessions
                .get(name)
                .ok_or_else(|| format!("unknown session `{name}`"))?;
            println!("{}", serde_json::to_string_pretty(session).unwrap());
        }
        "delete" => {
            let name = args
                .get(1)
                .ok_or_else(|| "delete needs a session name".to_owned())?;
            if store.sessions.remove(name).is_none() {
                return Err(format!("unknown session `{name}`"));
            }
            store
                .save(&state)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", serde_json::json!({ "deleted": name }));
        }
        "save" => {
            let name = args
                .get(1)
                .ok_or_else(|| "save needs a session name".to_owned())?;
            let (workspaces, windows, _) = query(&socket()?)?;
            let mut commands = store
                .sessions
                .get(name)
                .map(|session| session.command_map.clone())
                .unwrap_or_default();
            commands.extend(launches);
            let session = snapshot(&workspaces, &windows, commands);
            store.sessions.insert(name.clone(), session.clone());
            store
                .save(&state)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", serde_json::to_string_pretty(&session).unwrap());
        }
        "restore" => {
            let name = args
                .get(1)
                .ok_or_else(|| "restore needs a session name".to_owned())?;
            let session: Session = store
                .sessions
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unknown session `{name}`"))?;
            let dry_run = args.iter().any(|arg| arg == "--dry-run");
            let socket = socket()?;
            let (workspaces, windows, _) = query(&socket)?;
            let (actions, missing) = plan(&session, &workspaces, &windows);
            let results = execute(&socket, &actions, dry_run)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "session": name,
                    "dry_run": dry_run,
                    "actions": results,
                    "missing": missing,
                }))
                .unwrap()
            );
        }
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}
