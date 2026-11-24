use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use better_session::{
    Context, DEFAULT_WAIT_MS, RestoreOptions, Store, capture, query, restore_context, state_path,
};

fn usage() -> &'static str {
    r#"better-session [--socket PATH] <command>

Contexts:
  list                          List saved contexts
  show <name>                   Print a context
  capture <name> [--launch app=cmd]... [--hook-pre CMD]... [--hook-post CMD]...
                                Snapshot windows, workspaces and outputs
  save <name> ...               Alias for capture
  restore <name> [--dry-run] [--explain] [--wait-ms MS] [--force]
                                Apply outputs, then workspaces, then windows
  explain <name> [--wait-ms MS] Print the planned restore without applying
  rename <old> <new>            Rename a context
  delete <name>                 Delete a context

Socket defaults to $NIRI_SOCKET; state to $BETTER_SESSION_STATE or
$XDG_STATE_HOME/better-session/contexts.json (legacy sessions.json is read).
Hooks receive BETTER_SESSION_CONTEXT and BETTER_SESSION_PHASE."#
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
    let mut hook_pre = Vec::new();
    let mut hook_post = Vec::new();
    let mut wait_ms = DEFAULT_WAIT_MS;
    let mut dry_run = false;
    let mut explain = false;
    let mut force = false;

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
            "--hook-pre" => hook_pre.push(
                iter.next()
                    .ok_or_else(|| "--hook-pre needs a command".to_owned())?,
            ),
            "--hook-post" => hook_post.push(
                iter.next()
                    .ok_or_else(|| "--hook-post needs a command".to_owned())?,
            ),
            "--wait-ms" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--wait-ms needs a number".to_owned())?;
                wait_ms = value
                    .parse()
                    .map_err(|_| format!("--wait-ms needs a number, got `{value}`"))?;
            }
            "--dry-run" => dry_run = true,
            "--explain" => explain = true,
            "--force" => force = true,
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
            let names: Vec<_> = store.contexts.keys().cloned().collect();
            println!("{}", serde_json::to_string_pretty(&names).unwrap());
        }
        "show" => {
            let name = args
                .get(1)
                .ok_or_else(|| "show needs a context name".to_owned())?;
            let context = store
                .get(name)
                .ok_or_else(|| format!("unknown context `{name}`"))?;
            println!("{}", serde_json::to_string_pretty(context).unwrap());
        }
        "delete" => {
            let name = args
                .get(1)
                .ok_or_else(|| "delete needs a context name".to_owned())?;
            if !store.remove(name) {
                return Err(format!("unknown context `{name}`"));
            }
            store
                .save(&state)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", serde_json::json!({ "deleted": name }));
        }
        "rename" => {
            let from = args
                .get(1)
                .ok_or_else(|| "rename needs an old and a new name".to_owned())?;
            let to = args
                .get(2)
                .ok_or_else(|| "rename needs an old and a new name".to_owned())?;
            store.rename(from, to)?;
            store
                .save(&state)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", serde_json::json!({ "renamed": from, "to": to }));
        }
        "capture" | "save" => {
            let name = args
                .get(1)
                .ok_or_else(|| format!("{command} needs a context name"))?;
            let (workspaces, windows, outputs) = query(&socket()?)?;

            let existing = store.get(name).cloned();
            let mut command_map = existing
                .as_ref()
                .map(|context| context.command_map.clone())
                .unwrap_or_default();
            command_map.extend(launches.clone());
            let mut hooks = existing
                .as_ref()
                .map(|context| context.hooks.clone())
                .unwrap_or_default();
            hooks.pre.extend(hook_pre.clone());
            hooks.post.extend(hook_post.clone());

            let context = capture(&workspaces, &windows, &outputs, command_map, hooks);
            let created_at = existing
                .as_ref()
                .map(|existing| existing.created_at)
                .unwrap_or(context.created_at);
            let context = Context {
                created_at,
                ..context
            };
            store.insert(name.clone(), context.clone());
            store
                .save(&state)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", serde_json::to_string_pretty(&context).unwrap());
        }
        "restore" | "explain" => {
            let name = args
                .get(1)
                .ok_or_else(|| format!("{command} needs a context name"))?;
            let context = store
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unknown context `{name}`"))?;
            let explain = explain || command == "explain";
            let options = RestoreOptions {
                dry_run: dry_run || explain,
                explain,
                force,
                wait_ms,
                ..RestoreOptions::default()
            };
            let outcome = restore_context(&socket()?, name, &context, &options)?;
            println!("{}", serde_json::to_string_pretty(&outcome.report).unwrap());
            if let Some(error) = outcome.failure {
                return Err(error);
            }
        }
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}
