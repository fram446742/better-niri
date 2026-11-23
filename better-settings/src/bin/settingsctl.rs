use std::path::PathBuf;
use std::process::ExitCode;

use better_settings::{config_path, current, normalize, patch, save, Edit, CATALOG};

fn usage() -> &'static str {
    "better-settingsctl [--config PATH] <command>\n\n\
     Commands:\n\
       list                       Show catalog settings and their current values\n\
       get <path>                 Print the current value of one setting\n\
       set <path> <value> [--dry-run]\n\
                                  Patch one setting in place\n\
       diff                       Print the config with the catalog values applied\n\
                                   (nothing is written)\n\n\
     Config defaults to $NIRI_CONFIG, then\n\
     $XDG_CONFIG_HOME/niri/config.kdl, then ~/.config/niri/config.kdl.\n\
     Writes keep the previous file as <name>.bak-<epoch>."
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-settingsctl: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut config = std::env::var_os("BETTER_SETTINGS_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("NIRI_CONFIG").map(PathBuf::from))
        .unwrap_or_else(config_path);
    let mut dry_run = false;
    let mut args = Vec::new();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => {
                config = PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--config needs a path".to_owned())?,
                )
            }
            "--dry-run" => dry_run = true,
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

    let text = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("cannot read {}: {err}", config.display())),
    };

    match command.as_str() {
        "list" => {
            let mut rows = Vec::new();
            for spec in CATALOG {
                rows.push(serde_json::json!({
                    "path": spec.path,
                    "label": spec.label,
                    "help": spec.help,
                    "value": current(&text, spec.path).map_err(|err| err.to_string())?,
                }));
            }
            println!("{}", serde_json::to_string_pretty(&rows).unwrap());
        }
        "get" => {
            let path = args
                .get(1)
                .ok_or_else(|| "get needs a setting path".to_owned())?;
            match current(&text, path).map_err(|err| err.to_string())? {
                Some(value) => println!("{value}"),
                None => return Err(format!("`{path}` is not set in {}", config.display())),
            }
        }
        "diff" => {
            let mut patched = text.clone();
            for spec in CATALOG {
                let Some(value) = current(&text, spec.path).map_err(|err| err.to_string())? else {
                    continue;
                };
                let edit = Edit {
                    path: spec.path.to_owned(),
                    value,
                    flag: matches!(spec.kind, better_settings::Kind::Flag),
                };
                patched = patch(&patched, &edit).map_err(|err| err.to_string())?;
            }
            print!("{patched}");
        }
        "set" => {
            let path = args
                .get(1)
                .ok_or_else(|| "set needs a setting path".to_owned())?;
            let raw = args.get(2).ok_or_else(|| "set needs a value".to_owned())?;
            let spec = CATALOG.iter().find(|spec| spec.path == path.as_str());
            let (value, flag) = match spec {
                Some(spec) => {
                    let value = normalize(spec.kind, raw).map_err(|err| err.to_string())?;
                    (value, matches!(spec.kind, better_settings::Kind::Flag))
                }
                None => (raw.clone(), false),
            };
            let edit = Edit {
                path: path.clone(),
                value: if flag {
                    normalize(better_settings::Kind::Flag, &value).map_err(|err| err.to_string())?
                } else {
                    value
                },
                flag,
            };
            let patched = patch(&text, &edit).map_err(|err| err.to_string())?;
            if patched == text {
                println!("{}", serde_json::json!({ "changed": false, "path": path }));
                return Ok(());
            }
            let backup = save(&config, &patched, dry_run)
                .map_err(|err| format!("cannot write {}: {err}", config.display()))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "changed": true,
                    "dry_run": dry_run,
                    "path": path,
                    "config": config,
                    "backup": backup,
                    "text": patched,
                }))
                .unwrap()
            );
        }
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}
