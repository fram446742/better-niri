use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use better_audio::{apply_once, config_path, Backend, Rule, Store};

fn usage() -> &'static str {
    "better-audio [--config PATH] [--pactl PATH] <command>\n\n\
     Commands:\n\
       sinks                      List playback sinks\n\
       streams                    List playback streams (app + current sink)\n\
       rules                      List rules\n\
       rules add <app> <sink> [--priority N]\n\
       rules remove <index>       Remove the rule at that index\n\
       default <sink|none>        Set (or clear) the default sink\n\
       apply [--dry-run]          Route matching streams once\n\
       watch [--interval-ms N]    Keep routing new streams (default 2000 ms)\n\
       status                     Show the store and current defaults\n\n\
     App patterns are case-insensitive substrings or globs (`*`, `?`);\n\
     sink references are exact names or unique substrings.\n\
     Config defaults to $XDG_CONFIG_HOME/better-audio/rules.json;\n\
     pactl defaults to $BETTER_AUDIO_PACTL or `pactl`."
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-audio: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut config = std::env::var_os("BETTER_AUDIO_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(config_path);
    let mut pactl = std::env::var("BETTER_AUDIO_PACTL").unwrap_or_else(|_| "pactl".to_owned());
    let mut dry_run = false;
    let mut priority = 0;
    let mut interval = Duration::from_millis(2000);
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
            "--pactl" => {
                pactl = iter
                    .next()
                    .ok_or_else(|| "--pactl needs a path".to_owned())?
            }
            "--priority" => {
                priority = iter
                    .next()
                    .ok_or_else(|| "--priority needs a number".to_owned())?
                    .parse()
                    .map_err(|_| "--priority needs a number".to_owned())?
            }
            "--interval-ms" => {
                let millis: u64 = iter
                    .next()
                    .ok_or_else(|| "--interval-ms needs a number".to_owned())?
                    .parse()
                    .map_err(|_| "--interval-ms needs a number".to_owned())?;
                interval = Duration::from_millis(millis.max(100));
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

    let backend = Backend::new(pactl);
    let mut store = Store::load(&config).map_err(|err| format!("cannot load state: {err}"))?;

    match command.as_str() {
        "sinks" => {
            let sinks = backend.sinks()?;
            let default = backend.default_sink()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "default": default,
                    "sinks": sinks,
                }))
                .unwrap()
            );
        }
        "streams" => {
            let sinks = backend.sinks()?;
            let streams = backend.streams()?;
            let rows: Vec<_> = streams
                .iter()
                .map(|stream| {
                    let sink = stream.sink_id.and_then(|id| {
                        sinks
                            .iter()
                            .find(|sink| sink.id == id)
                            .map(|sink| &sink.name)
                    });
                    serde_json::json!({
                        "id": stream.id,
                        "app": stream.app,
                        "media_name": stream.media_name,
                        "sink": sink,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows).unwrap());
        }
        "rules" => match args.get(1).map(String::as_str) {
            None | Some("list") => {
                println!("{}", serde_json::to_string_pretty(&store.rules).unwrap());
            }
            Some("add") => {
                let app = args
                    .get(2)
                    .ok_or_else(|| "rules add needs an app pattern".to_owned())?;
                let sink = args
                    .get(3)
                    .ok_or_else(|| "rules add needs a sink reference".to_owned())?;
                store.rules.push(Rule {
                    app: app.clone(),
                    sink: sink.clone(),
                    priority,
                });
                store
                    .save(&config)
                    .map_err(|err| format!("cannot save state: {err}"))?;
                println!(
                    "{}",
                    serde_json::json!({ "rules": store.rules.len(), "app": app, "sink": sink })
                );
            }
            Some("remove") => {
                let index: usize = args
                    .get(2)
                    .ok_or_else(|| "rules remove needs an index".to_owned())?
                    .parse()
                    .map_err(|_| "rules remove needs an index".to_owned())?;
                if index >= store.rules.len() {
                    return Err(format!("rule {index} does not exist"));
                }
                let removed = store.rules.remove(index);
                store
                    .save(&config)
                    .map_err(|err| format!("cannot save state: {err}"))?;
                println!("{}", serde_json::to_string_pretty(&removed).unwrap());
            }
            Some(other) => return Err(format!("unknown rules command `{other}`")),
        },
        "default" => {
            let sink = args
                .get(1)
                .ok_or_else(|| "default needs a sink or `none`".to_owned())?;
            store.default_sink = if sink == "none" {
                None
            } else {
                Some(sink.clone())
            };
            store
                .save(&config)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!(
                "{}",
                serde_json::json!({ "default_sink": store.default_sink })
            );
        }
        "apply" => {
            let (plan, executed) = apply_once(&backend, &store, dry_run)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": dry_run,
                    "executed": executed,
                    "unresolved": plan.unresolved,
                }))
                .unwrap()
            );
        }
        "watch" => loop {
            match apply_once(&backend, &store, dry_run) {
                Ok((_, executed)) => {
                    for line in executed {
                        println!("{line}");
                    }
                }
                Err(err) => eprintln!("better-audio: {err}"),
            }
            println!("---");
            std::thread::sleep(interval);
        },
        "status" => {
            let default = backend.default_sink().ok().flatten();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "config": config,
                    "current_default": default,
                    "configured_default": store.default_sink,
                    "rules": store.rules,
                }))
                .unwrap()
            );
        }
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}
