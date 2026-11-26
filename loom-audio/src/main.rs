use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

use loom_audio::backends::{self, BackendConfig, BackendKind, Selection};
use loom_audio::{
    ChangeGate, DeviceKind, DeviceSnapshot, FocusContext, HotplugWatcher, Rule, Store,
    apply_once_with_context, config_path, execute, execute_profile, execute_restore,
    explain_stream, parse_mute, parse_volume, plan_profile, plan_profile_restore, plan_restore,
    plan_signature, plan_with_context, set_mute, set_volume,
};
use serde_json::json;

fn usage() -> &'static str {
    "loom-audio [options] <command>\n\n\
     Options:\n\
       --config PATH              Rules file (default $XDG_CONFIG_HOME/loom-audio/rules.json)\n\
       --backend NAME             auto | native | pactl | wpctl (default auto)\n\
       --pactl PATH               pactl executable (default $LOOM_AUDIO_PACTL or `pactl`)\n\
       --wpctl PATH               wpctl executable (default $LOOM_AUDIO_WPCTL or `wpctl`)\n\
       --pw-dump PATH             pw-dump executable (default $LOOM_AUDIO_PW_DUMP)\n\
       --pw-cli PATH              pw-cli executable (default $LOOM_AUDIO_PW_CLI)\n\
       --focus-command CMD        Command printing focused-window JSON\n\
                                  (default $LOOM_AUDIO_FOCUS_COMMAND)\n\
       --json                     Emit a `{command, ok, data}` envelope\n\
       --dry-run                  Print planned changes without applying them\n\n\
     Commands:\n\
       sinks                      List playback sinks\n\
       sources                    List capture sources (input devices)\n\
       streams                    List playback streams (app + current sink)\n\
       links                      List PipeWire links (diagnostics)\n\
       volume <sink|source> <device> <0..1>   Set a device's linear volume\n\
       mute <sink|source> <device> on|off     Mute or unmute a device\n\
       rules                      List rules\n\
       rules add <app> <sink> [--priority N] [--volume 0..1] [--mute on|off]\n\
       rules remove <index>       Remove the rule at that index\n\
       default <sink|none>        Set (or clear) the default sink\n\
       profile list               List named profiles\n\
       profile show <name>        Print one profile\n\
       profile apply <name>       Apply a sink/source/volume/mute profile\n\
       profile restore            Restore defaults from before the profile\n\
       apply [--dry-run]          Route matching streams once\n\
       restore [--dry-run]        Move remembered streams back to their original sink\n\
       watch [--interval-ms N]    Keep routing new streams (default 2000 ms)\n\
       diagnose                   Backend, devices, links and why rules matched\n\
       status                     Show the active backend, why, and current state\n\n\
     App patterns are case-insensitive substrings or globs (`*`, `?`);\n\
     sink/source references are exact names or unique substrings.\n\
     Rules may set a linear per-stream volume (1.0 == 100%) and mute state.\n\
     Optional `binary`, `title` and `workspace` rule fields match the focused\n\
     window context (from --focus-command); every matching stream is routed."
}

struct Options {
    config: PathBuf,
    backend: BackendKind,
    backend_config: BackendConfig,
    dry_run: bool,
    json: bool,
    priority: i32,
    volume: Option<f32>,
    mute: Option<bool>,
    interval: Duration,
    focus_command: Option<String>,
    args: Vec<String>,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        config: std::env::var_os("LOOM_AUDIO_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(config_path),
        backend: BackendKind::Auto,
        backend_config: BackendConfig::default(),
        dry_run: false,
        json: false,
        priority: 0,
        volume: None,
        mute: None,
        interval: Duration::from_millis(2000),
        focus_command: std::env::var("LOOM_AUDIO_FOCUS_COMMAND")
            .ok()
            .filter(|command| !command.trim().is_empty()),
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
            "--focus-command" => options.focus_command = Some(value("--focus-command")?),
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
            "--json" => options.json = true,
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

fn emit<T: serde::Serialize>(options: &Options, command: &str, data: &T) {
    if options.json {
        println!(
            "{}",
            json!({ "command": command, "ok": true, "data": data })
        );
    } else {
        println!("{}", serde_json::to_string_pretty(data).unwrap());
    }
}

fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("loom-audio {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("loom-audio: {err}");
            ExitCode::FAILURE
        }
    }
}

fn select(options: &Options) -> Result<Selection, String> {
    backends::select(options.backend, &options.backend_config)
}

/// Reads the focused window/workspace JSON from the configured command.
fn focus_context(options: &Options) -> Option<FocusContext> {
    let command = options.focus_command.as_deref()?;
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|err| eprintln!("loom-audio: focus command failed: {err}"))
        .ok()?;
    if !output.status.success() {
        return None;
    }
    FocusContext::from_json(String::from_utf8_lossy(&output.stdout).trim())
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
        "sources" => {
            let selection = select(&options)?;
            let sources = selection.backend.sources()?;
            let default = selection.backend.default_source()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "backend": selection.status.active.as_str(),
                    "default": default,
                    "sources": sources,
                }))
                .unwrap()
            );
        }
        "links" => {
            let selection = select(&options)?;
            let links = selection.backend.links()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "backend": selection.status.active.as_str(),
                    "links": links,
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
                        "binary": stream.binary,
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
        "mute" => {
            let kind = DeviceKind::parse(
                options
                    .args
                    .get(1)
                    .ok_or_else(|| "mute needs <sink|source> <device> on|off".to_owned())?,
            )?;
            let reference = options
                .args
                .get(2)
                .ok_or_else(|| "mute needs a device name".to_owned())?;
            let mute = parse_mute(
                options
                    .args
                    .get(3)
                    .ok_or_else(|| "mute needs on or off".to_owned())?,
            )?;
            let selection = select(&options)?;
            let report = set_mute(
                selection.backend.as_ref(),
                kind,
                reference,
                mute,
                options.dry_run,
            )?;
            emit(&options, "mute", &report);
        }
        "volume" => {
            let kind = DeviceKind::parse(
                options
                    .args
                    .get(1)
                    .ok_or_else(|| "volume needs <sink|source> <device> <0..1>".to_owned())?,
            )?;
            let reference = options
                .args
                .get(2)
                .ok_or_else(|| "volume needs a device name".to_owned())?;
            let volume = parse_volume(
                options
                    .args
                    .get(3)
                    .ok_or_else(|| "volume needs a level in 0..1".to_owned())?,
            )?;
            let selection = select(&options)?;
            let report = set_volume(
                selection.backend.as_ref(),
                kind,
                reference,
                volume,
                options.dry_run,
            )?;
            emit(&options, "volume", &report);
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
                    ..Rule::default()
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
        "default-source" => {
            let source = options
                .args
                .get(1)
                .ok_or_else(|| "default-source needs a source or `none`".to_owned())?;
            let selection = select(&options)?;
            if source == "none" {
                store.default_source = None;
            } else {
                let sources = selection.backend.sources()?;
                let resolved = loom_audio::resolve_source(&sources, source)
                    .ok_or_else(|| format!("source `{source}` does not resolve"))?;
                let name = resolved.name.clone();
                if !options.dry_run {
                    selection.backend.set_default_source(&name)?;
                }
                store.default_source = Some(name);
            }
            if !options.dry_run {
                store
                    .save(&options.config)
                    .map_err(|err| format!("cannot save state: {err}"))?;
            }
            println!("{}", json!({ "default_source": store.default_source }));
        }
        "profile" => {
            let sub = options
                .args
                .get(1)
                .map(String::as_str)
                .ok_or_else(|| "profile needs list/show/apply/restore".to_owned())?;
            match sub {
                "list" => {
                    println!("{}", serde_json::to_string_pretty(&store.profiles).unwrap());
                }
                "show" => {
                    let name = options
                        .args
                        .get(2)
                        .ok_or_else(|| "profile show needs a name".to_owned())?;
                    let profile = store
                        .profiles
                        .get(name)
                        .ok_or_else(|| format!("unknown profile `{name}`"))?;
                    println!("{}", serde_json::to_string_pretty(profile).unwrap());
                }
                "apply" => {
                    let name = options
                        .args
                        .get(2)
                        .ok_or_else(|| "profile apply needs a name".to_owned())?
                        .clone();
                    let selection = select(&options)?;
                    let sinks = selection.backend.sinks()?;
                    let sources = selection.backend.sources()?;
                    let streams = selection.backend.streams()?;
                    let current_sink = selection.backend.default_sink()?;
                    let current_source = selection.backend.default_source()?;
                    let plan = plan_profile(
                        &store,
                        &name,
                        &sinks,
                        &sources,
                        &streams,
                        current_sink.as_deref(),
                        current_source.as_deref(),
                    )?;
                    let executed =
                        execute_profile(selection.backend.as_ref(), &plan, options.dry_run)?;
                    if !options.dry_run {
                        if let Some(remember) = plan.remember.clone() {
                            store.profile_restore = Some(remember);
                        }
                        store.active_profile = Some(name.clone());
                        store
                            .save(&options.config)
                            .map_err(|err| format!("cannot save state: {err}"))?;
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "backend": selection.status.active.as_str(),
                            "dry_run": options.dry_run,
                            "profile": name,
                            "executed": executed,
                            "unresolved": plan.unresolved,
                        }))
                        .unwrap()
                    );
                }
                "restore" => {
                    let selection = select(&options)?;
                    let sinks = selection.backend.sinks()?;
                    let sources = selection.backend.sources()?;
                    let current_sink = selection.backend.default_sink()?;
                    let current_source = selection.backend.default_source()?;
                    let plan = plan_profile_restore(
                        &store,
                        &sinks,
                        &sources,
                        current_sink.as_deref(),
                        current_source.as_deref(),
                    );
                    let executed =
                        execute_profile(selection.backend.as_ref(), &plan, options.dry_run)?;
                    if !options.dry_run {
                        store.profile_restore = None;
                        store.active_profile = None;
                        store
                            .save(&options.config)
                            .map_err(|err| format!("cannot save state: {err}"))?;
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "backend": selection.status.active.as_str(),
                            "dry_run": options.dry_run,
                            "restored": plan.name,
                            "executed": executed,
                            "unresolved": plan.unresolved,
                        }))
                        .unwrap()
                    );
                }
                other => return Err(format!("unknown profile command `{other}`")),
            }
        }
        "diagnose" => {
            let selection = select(&options)?;
            let sinks = selection.backend.sinks()?;
            let sources = selection.backend.sources()?;
            let streams = selection.backend.streams()?;
            let links = selection.backend.links()?;
            let focus = focus_context(&options);
            let explanations: Vec<_> = streams
                .iter()
                .map(|stream| explain_stream(&store, stream, focus.as_ref()))
                .collect();
            let attempted: Vec<_> = selection
                .status
                .attempts
                .iter()
                .map(|attempt| json!({ "backend": attempt.kind.as_str(), "error": attempt.error }))
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "backend": {
                        "active": selection.status.active.as_str(),
                        "description": selection.status.active_description,
                        "degraded": selection.status.degraded,
                        "reason": selection.status.explanation(),
                        "attempts": attempted,
                    },
                    "devices": {
                        "sinks": sinks,
                        "sources": sources,
                        "default_sink": selection.backend.default_sink()?,
                        "default_source": selection.backend.default_source()?,
                    },
                    "links": links,
                    "focus": focus.as_ref().map(|focus| json!({
                        "app": focus.app,
                        "title": focus.title,
                        "workspace": focus.workspace,
                    })),
                    "rules": store.rules,
                    "streams": explanations,
                    "profiles": store.profiles,
                }))
                .unwrap()
            );
        }
        "apply" => {
            let selection = select(&options)?;
            let focus = focus_context(&options);
            let (plan, executed) = apply_once_with_context(
                selection.backend.as_ref(),
                &store,
                focus.as_ref(),
                options.dry_run,
            )?;
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
                    "focus": focus.as_ref().map(|focus| json!({
                        "app": focus.app,
                        "title": focus.title,
                        "workspace": focus.workspace,
                    })),
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
                "loom-audio: {} ({})",
                selection.status.explanation(),
                selection.status.active_description
            );
            let mut hotplug = HotplugWatcher::new();
            let mut gate = ChangeGate::new();
            loop {
                let focus = focus_context(&options);
                match DeviceSnapshot::read(selection.backend.as_ref()) {
                    Ok(devices) => {
                        if hotplug.observe(devices.clone()) {
                            eprintln!(
                                "loom-audio: devices changed ({} sinks, {} sources)",
                                devices.sinks.len(),
                                devices.sources.len()
                            );
                        }
                        match selection.backend.streams() {
                            Ok(streams) => {
                                let mut plan = plan_with_context(
                                    &store,
                                    &devices.sinks,
                                    &streams,
                                    devices.default_sink.as_deref(),
                                    focus.as_ref(),
                                );
                                plan.default_source = loom_audio::plan_default_source(
                                    &store,
                                    &devices.sources,
                                    devices.default_source.as_deref(),
                                );
                                let has_work = !plan.moves.is_empty()
                                    || !plan.profiles.is_empty()
                                    || plan.default.is_some()
                                    || plan.default_source.is_some();
                                if !has_work {
                                    gate.reset();
                                } else if gate.allow(plan_signature(&plan)) {
                                    match execute(
                                        selection.backend.as_ref(),
                                        &plan,
                                        options.dry_run,
                                    ) {
                                        Ok(executed) => {
                                            if !options.dry_run && !plan.remember.is_empty() {
                                                store.remember(plan.remember);
                                                if let Err(err) = store.save(&options.config) {
                                                    eprintln!(
                                                        "loom-audio: cannot save state: {err}"
                                                    );
                                                }
                                            }
                                            for line in executed {
                                                println!("{line}");
                                            }
                                        }
                                        Err(err) => eprintln!("loom-audio: {err}"),
                                    }
                                }
                                for line in &plan.unresolved {
                                    eprintln!("loom-audio: unresolved {line}");
                                }
                            }
                            Err(err) => eprintln!("loom-audio: {err}"),
                        }
                    }
                    Err(err) => eprintln!("loom-audio: {err}"),
                }
                std::thread::sleep(options.interval);
            }
        }
        "status" => {
            let (active, description, degraded, reason, attempts, current_default, sinks, sources) =
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
                            selection.backend.sinks().unwrap_or_default(),
                            selection.backend.sources().unwrap_or_default(),
                        )
                    }
                    Err(err) => (
                        None,
                        None,
                        false,
                        err,
                        Vec::new(),
                        None,
                        Vec::new(),
                        Vec::new(),
                    ),
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
                    "sinks": sinks,
                    "sources": sources,
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
