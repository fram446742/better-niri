use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use better_audio::backends::{self, BackendConfig, BackendKind, Selection};
use better_audio::{Rule, Store, apply_once, config_path, execute_restore, plan_restore};
use serde_json::json;

fn usage() -> &'static str {
    "better-audio [options] <command>\n\n\
     Options:\n\
       --config PATH              Rules file (default $XDG_CONFIG_HOME/better-audio/rules.json)\n\
       --backend NAME             auto | native | pactl | wpctl (default auto)\n\
       --pactl PATH               pactl executable (default $BETTER_AUDIO_PACTL or `pactl`)\n\
       --wpctl PATH               wpctl executable (default $BETTER_AUDIO_WPCTL or `wpctl`)\n\
       --pw-dump PATH             pw-dump executable (default $BETTER_AUDIO_PW_DUMP)\n\
       --pw-cli PATH              pw-cli executable (default $BETTER_AUDIO_PW_CLI)\n\
       --dry-run                  Print planned changes without applying them\n\n\
     Commands:\n\
       sinks                      List playback sinks\n\
       streams                    List playback streams (app + current sink)\n\
       rules                      List rules\n\
       rules add <app> <sink> [--priority N] [--volume 0..1] [--mute on|off]\n\
       rules remove <index>       Remove the rule at that index\n\
       default <sink|none>        Set (or clear) the default sink\n\
       apply [--dry-run]          Route matching streams once\n\
       restore [--dry-run]        Move remembered streams back to their original sink\n\
       watch [--interval-ms N]    Keep routing new streams (default 2000 ms)\n\
       status                     Show the active backend, why, and current state\n\n\
     App patterns are case-insensitive substrings or globs (`*`, `?`);\n\
     sink references are exact names or unique substrings.\n\
     Rules may set a linear per-stream volume (1.0 == 100%) and mute state."
}

struct Options {
    config: PathBuf,
    backend: BackendKind,
    backend_config: BackendConfig,
    dry_run: bool,
    priority: i32,
    volume: Option<f32>,
    mute: Option<bool>,
    interval: Duration,
    args: Vec<String>,
}

fn parse_mute(value: &str) -> Result<bool, String> {
    match value.to_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Ok(true),
        "0" | "false" | "off" | "no" => Ok(false),
        other => Err(format!("invalid mute value `{other}` (expected on or off)")),
    }
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        config: std::env::var_os("BETTER_AUDIO_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(config_path),
        backend: BackendKind::Auto,
        backend_config: BackendConfig::default(),
        dry_run: false,
        priority: 0,
        volume: None,
        mute: None,
        interval: Duration::from_millis(2000),
        args: Vec::new(),
    };

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| iter.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--config" => options.config = PathBuf::from(value("--config")?),
            "--backend" => options.backend = BackendKind::parse(&value("--backend")?)?,
            "--pactl" => options.backend_config.pactl = value("--pactl")?,
            "--wpctl" => options.backend_config.wpctl = value("--wpctl")?,
            "--pw-dump" => options.backend_config.pw_dump = value("--pw-dump")?,
            "--pw-cli" => options.backend_config.pw_cli = value("--pw-cli")?,
            "--priority" => {
                options.priority = value("--priority")?
                    .parse()
                    .map_err(|_| "--priority needs a number".to_owned())?;
            }
            "--volume" => {
                let volume: f32 = value("--volume")?
                    .parse()
                    .map_err(|_| "--volume needs a number (1.0 == 100%)".to_owned())?;
                if volume < 0.0 {
                    return Err("--volume must not be negative".to_owned());
                }
                options.volume = Some(volume);
            }
            "--mute" => options.mute = Some(parse_mute(&value("--mute")?)?),
            "--interval-ms" => {
                let millis: u64 = value("--interval-ms")?
                    .parse()
                    .map_err(|_| "--interval-ms needs a number".to_owned())?;
                options.interval = Duration::from_millis(millis.max(100));
            }
            "--dry-run" => options.dry_run = true,
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`\n\n{}", usage()));
            }
            _ => options.args.push(arg),
        }
    }
    Ok(options)
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

fn select(options: &Options) -> Result<Selection, String> {
    backends::select(options.backend, &options.backend_config)
}

fn run() -> Result<(), String> {
    let options = parse_options()?;
    let Some(command) = options.args.first().cloned() else {
        return Err(format!("missing command\n\n{}", usage()));
    };

    let mut store =
        Store::load(&options.config).map_err(|err| format!("cannot load state: {err}"))?;

    match command.as_str() {
        "sinks" => {
            let selection = select(&options)?;
            let sinks = selection.backend.sinks()?;
            let default = selection.backend.default_sink()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "backend": selection.status.active.as_str(),
                    "default": default,
                    "sinks": sinks,
                }))
                .unwrap()
            );
        }
        "streams" => {
            let selection = select(&options)?;
            let sinks = selection.backend.sinks()?;
            let streams = selection.backend.streams()?;
            let rows: Vec<_> = streams
                .iter()
                .map(|stream| {
                    let sink = stream.sink_id.and_then(|id| {
                        sinks
                            .iter()
                            .find(|sink| sink.id == id)
                            .map(|sink| &sink.name)
                    });
                    json!({
                        "id": stream.id,
                        "app": stream.app,
                        "media_name": stream.media_name,
                        "sink": sink,
                        "volume": stream.volume,
                        "mute": stream.mute,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "backend": selection.status.active.as_str(),
                    "streams": rows,
                }))
                .unwrap()
            );
        }
        "rules" => match options.args.get(1).map(String::as_str) {
            None | Some("list") => {
                println!("{}", serde_json::to_string_pretty(&store.rules).unwrap());
            }
            Some("add") => {
                let app = options
                    .args
                    .get(2)
                    .ok_or_else(|| "rules add needs an app pattern".to_owned())?;
                let sink = options
                    .args
                    .get(3)
                    .ok_or_else(|| "rules add needs a sink reference".to_owned())?;
                let rule = Rule {
                    app: app.clone(),
                    sink: sink.clone(),
                    priority: options.priority,
                    volume: options.volume,
                    mute: options.mute,
                };
                store.rules.push(rule.clone());
                store
                    .save(&options.config)
                    .map_err(|err| format!("cannot save state: {err}"))?;
                println!("{}", serde_json::to_string_pretty(&rule).unwrap());
            }
            Some("remove") => {
                let index: usize = options
                    .args
                    .get(2)
                    .ok_or_else(|| "rules remove needs an index".to_owned())?
                    .parse()
                    .map_err(|_| "rules remove needs an index".to_owned())?;
                if index >= store.rules.len() {
                    return Err(format!("rule {index} does not exist"));
                }
                let removed = store.rules.remove(index);
                store
                    .save(&options.config)
                    .map_err(|err| format!("cannot save state: {err}"))?;
                println!("{}", serde_json::to_string_pretty(&removed).unwrap());
            }
            Some(other) => return Err(format!("unknown rules command `{other}`")),
        },
        "default" => {
            let sink = options
                .args
                .get(1)
                .ok_or_else(|| "default needs a sink or `none`".to_owned())?;
            store.default_sink = if sink == "none" {
                None
            } else {
                Some(sink.clone())
            };
            store
                .save(&options.config)
                .map_err(|err| format!("cannot save state: {err}"))?;
            println!("{}", json!({ "default_sink": store.default_sink }));
        }
        "apply" => {
            let selection = select(&options)?;
            let (plan, executed) = apply_once(selection.backend.as_ref(), &store, options.dry_run)?;
            let remembered = plan.remember.len();
            if !options.dry_run && remembered > 0 {
                store.remember(plan.remember.clone());
                store
                    .save(&options.config)
                    .map_err(|err| format!("cannot save state: {err}"))?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "backend": selection.status.active.as_str(),
                    "dry_run": options.dry_run,
                    "executed": executed,
                    "unresolved": plan.unresolved,
                    "remembered": remembered,
                }))
                .unwrap()
            );
        }
        "restore" => {
            let selection = select(&options)?;
            let sinks = selection.backend.sinks()?;
            let streams = selection.backend.streams()?;
            let restore = plan_restore(&store, &sinks, &streams);
            let executed = execute_restore(selection.backend.as_ref(), &restore, options.dry_run)?;
            if !options.dry_run {
                store.forget(&restore.done);
                store
                    .save(&options.config)
                    .map_err(|err| format!("cannot save state: {err}"))?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "backend": selection.status.active.as_str(),
                    "dry_run": options.dry_run,
                    "executed": executed,
                    "unresolved": restore.unresolved,
                    "forgotten": restore.done.len(),
                }))
                .unwrap()
            );
        }
        "watch" => {
            let selection = select(&options)?;
            eprintln!(
                "better-audio: {} ({})",
                selection.status.explanation(),
                selection.status.active_description
            );
            loop {
                match apply_once(selection.backend.as_ref(), &store, options.dry_run) {
                    Ok((plan, executed)) => {
                        if !options.dry_run && !plan.remember.is_empty() {
                            store.remember(plan.remember);
                            if let Err(err) = store.save(&options.config) {
                                eprintln!("better-audio: cannot save state: {err}");
                            }
                        }
                        for line in executed {
                            println!("{line}");
                        }
                    }
                    Err(err) => eprintln!("better-audio: {err}"),
                }
                println!("---");
                std::thread::sleep(options.interval);
            }
        }
        "status" => {
            let (active, description, degraded, reason, attempts, current_default) =
                match select(&options) {
                    Ok(selection) => {
                        let current = selection.backend.default_sink().ok().flatten();
                        let attempts: Vec<_> = selection
                            .status
                            .attempts
                            .iter()
                            .map(|attempt| {
                                json!({
                                    "backend": attempt.kind.as_str(),
                                    "error": attempt.error,
                                })
                            })
                            .collect();
                        (
                            Some(selection.status.active.as_str()),
                            Some(selection.status.active_description.clone()),
                            selection.status.degraded,
                            selection.status.explanation(),
                            attempts,
                            current,
                        )
                    }
                    Err(err) => (None, None, false, err, Vec::new(), None),
                };
            let restore_count = store.restore.len();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "requested_backend": options.backend.as_str(),
                    "active_backend": active,
                    "active_backend_description": description,
                    "degraded": degraded,
                    "reason": reason,
                    "attempts": attempts,
                    "config": options.config.display().to_string(),
                    "current_default": current_default,
                    "configured_default": store.default_sink,
                    "rules": store.rules,
                    "restore_entries": store.restore,
                    "restore_count": restore_count,
                }))
                .unwrap()
            );
        }
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}
