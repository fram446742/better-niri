use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use loom_session::profiles::MonitorApply;
use loom_session::{
    AudioProfile, CompanionProfile, Context, DEFAULT_HISTORY_KEEP, DEFAULT_POLL_MS,
    DEFAULT_RETRIES, DEFAULT_WAIT_MS, HistoryStore, ImportMode, LaunchEnv, MonitorProfile,
    ProfileEnv, ProfileHooks, Profiles, RestoreOptions, Store, WallpaperProfile, capture,
    deactivate_context, export_context, export_store, history_dir, import_into, query,
    restore_context, state_path, templates, write_atomic,
};
use serde_json::{Value, json};

fn usage() -> &'static str {
    r#"loom-session [--socket PATH] <command>

Contexts:
  list                          List saved contexts
  show <name>                   Print a context
  capture <name> [options]      Snapshot windows, workspaces and outputs
  save <name> [options]         Alias for capture (keeps existing settings)
  restore <name> [options]      Apply outputs, monitor, workspaces, windows, profiles
  explain <name> [options]      Print the planned restore without applying
  deactivate <name> [--dry-run] Run deactivate hooks (no niri changes)
  rename <old> <new>            Rename a context
  delete <name>                 Delete a context

Capture options:
  --launch app=cmd              Launch command for a missing app
  --hook-pre CMD                Hook before any change
  --hook-activate CMD           Hook after the context is active
  --hook-post CMD               Hook after the restore
  --hook-deactivate CMD         Hook for `deactivate`
  --template NAME [--set K=V]   Shape defaults from a built-in template
  --monitor-profile NAME [--monitor-via auto|ipc|command] [--monitor-command CMD]
  --audio-profile NAME [--audio-config PATH] [--audio-command CMD]
  --wallpaper-scene NAME [--wallpaper-command CMD]
  --companion-rule NAME ...     Record rule names
  --companion-call NS.ACTION    Run via loomctl on restore
  --companion-mark K=V ...      Record marks and export to commands
  --companion-command CMD       Override the companion command
  --profile-activate KIND=CMD   Per-profile activate hook (monitor/audio/wallpaper/companion)
  --profile-deactivate KIND=CMD Per-profile deactivate hook

Restore options:
  --dry-run / --explain         Plan without changing anything
  --wait-ms MS                  Late-window deadline (default 2000)
  --poll-ms MS                  Poll interval (default 50)
  --retry N                     Niri reconnect attempts (default 3)
  --no-preserve-user-changes    Do not re-check steps before applying
  --force                       Continue after a failing pre hook
  --snapshot-before             Record the current state in history first

History:
  history list                  List snapshots (newest first)
  history show <id|name>        Print one snapshot
  history restore <id|name>     Restore a snapshot
  history prune [--keep N] [--max-age-days N]
  snapshot [--auto] [--name NAME] [--interval-ms MS] [--count N] [--keep N]
                                Record a bounded history snapshot (no daemon)

Import/export:
  export [name] [--file PATH]   Whole store, or one context, as JSON
  import [--file PATH] [--merge|--replace] [--name NAME]
                                Validate and import a store or single context

Templates:
  template list                 Built-in Development/Gaming/Work shapes
  template show <name>          Raw template blueprint
  template apply <name> <ctx> [--set K=V]... [--force]

Socket defaults to $NIRI_SOCKET; state to $LOOM_SESSION_STATE or
$XDG_STATE_HOME/loom-session/contexts.json (legacy sessions.json is read).
History defaults to $XDG_STATE_HOME/loom-session/history/ (or
$LOOM_SESSION_HISTORY, or a `history` sibling of --state).
Hooks receive LOOM_SESSION_CONTEXT and LOOM_SESSION_PHASE; profile hooks
also receive LOOM_SESSION_PROFILE and profile commands may receive
LOOM_SESSION_COMPANION_MARKS."#
}

#[derive(Default)]
struct Cli {
    socket: Option<PathBuf>,
    state: Option<PathBuf>,
    launches: BTreeMap<String, String>,
    hook_pre: Vec<String>,
    hook_activate: Vec<String>,
    hook_post: Vec<String>,
    hook_deactivate: Vec<String>,
    profile_activate: Vec<(String, String)>,
    profile_deactivate: Vec<(String, String)>,
    wait_ms: u64,
    poll_ms: u64,
    retries: u32,
    dry_run: bool,
    explain: bool,
    force: bool,
    preserve_user_changes: bool,
    snapshot_before: bool,
    monitor_profile: Option<String>,
    monitor_via: Option<MonitorApply>,
    monitor_command: Option<String>,
    audio_profile: Option<String>,
    audio_config: Option<PathBuf>,
    audio_command: Option<String>,
    wallpaper_scene: Option<String>,
    wallpaper_command: Option<String>,
    companion_rules: Vec<String>,
    companion_marks: BTreeMap<String, String>,
    companion_calls: Vec<String>,
    companion_command: Option<String>,
    companion_socket: Option<PathBuf>,
    template: Option<String>,
    template_sets: Vec<String>,
    keep: Option<usize>,
    max_age_days: Option<u64>,
    auto: bool,
    name: Option<String>,
    interval_ms: Option<u64>,
    count: Option<usize>,
    file: Option<PathBuf>,
    merge: bool,
    replace: bool,
    args: Vec<String>,
}

impl Cli {
    fn parse() -> Result<Self, String> {
        let mut cli = Self {
            socket: std::env::var_os("NIRI_SOCKET").map(PathBuf::from),
            wait_ms: DEFAULT_WAIT_MS,
            poll_ms: DEFAULT_POLL_MS,
            retries: DEFAULT_RETRIES,
            preserve_user_changes: true,
            ..Self::default()
        };
        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            let mut value = |name: &str| -> Result<String, String> {
                iter.next().ok_or_else(|| format!("{name} needs a value"))
            };
            match arg.as_str() {
                "--socket" => cli.socket = Some(PathBuf::from(value("--socket")?)),
                "--state" => cli.state = Some(PathBuf::from(value("--state")?)),
                "--launch" => {
                    let pair = value("--launch")?;
                    let (app, command) = pair
                        .split_once('=')
                        .ok_or_else(|| "--launch needs app_id=command".to_owned())?;
                    cli.launches.insert(app.to_owned(), command.to_owned());
                }
                "--hook-pre" => cli.hook_pre.push(value("--hook-pre")?),
                "--hook-activate" => cli.hook_activate.push(value("--hook-activate")?),
                "--hook-post" => cli.hook_post.push(value("--hook-post")?),
                "--hook-deactivate" => cli.hook_deactivate.push(value("--hook-deactivate")?),
                "--profile-activate" => cli.profile_activate.push(parse_pair(
                    "--profile-activate",
                    value("--profile-activate")?,
                )?),
                "--profile-deactivate" => cli.profile_deactivate.push(parse_pair(
                    "--profile-deactivate",
                    value("--profile-deactivate")?,
                )?),
                "--wait-ms" => cli.wait_ms = parse_number("--wait-ms", value("--wait-ms")?)?,
                "--poll-ms" => cli.poll_ms = parse_number("--poll-ms", value("--poll-ms")?)?,
                "--retry" => cli.retries = parse_number("--retry", value("--retry")?)?,
                "--dry-run" => cli.dry_run = true,
                "--explain" => cli.explain = true,
                "--force" => cli.force = true,
                "--no-preserve-user-changes" => cli.preserve_user_changes = false,
                "--snapshot-before" => cli.snapshot_before = true,
                "--monitor-profile" => cli.monitor_profile = Some(value("--monitor-profile")?),
                "--monitor-via" => {
                    cli.monitor_via = Some(match value("--monitor-via")?.as_str() {
                        "auto" => MonitorApply::Auto,
                        "ipc" => MonitorApply::Ipc,
                        "command" => MonitorApply::Command,
                        other => {
                            return Err(format!(
                                "--monitor-via needs auto, ipc or command, got `{other}`"
                            ));
                        }
                    });
                }
                "--monitor-command" => cli.monitor_command = Some(value("--monitor-command")?),
                "--audio-profile" => cli.audio_profile = Some(value("--audio-profile")?),
                "--audio-config" => {
                    cli.audio_config = Some(PathBuf::from(value("--audio-config")?))
                }
                "--audio-command" => cli.audio_command = Some(value("--audio-command")?),
                "--wallpaper-scene" => cli.wallpaper_scene = Some(value("--wallpaper-scene")?),
                "--wallpaper-command" => {
                    cli.wallpaper_command = Some(value("--wallpaper-command")?)
                }
                "--companion-rule" => cli.companion_rules.push(value("--companion-rule")?),
                "--companion-mark" => {
                    let (key, mark) = parse_pair("--companion-mark", value("--companion-mark")?)?;
                    cli.companion_marks.insert(key, mark);
                }
                "--companion-call" => cli.companion_calls.push(value("--companion-call")?),
                "--companion-command" => {
                    cli.companion_command = Some(value("--companion-command")?)
                }
                "--companion-socket" => {
                    cli.companion_socket = Some(PathBuf::from(value("--companion-socket")?))
                }
                "--template" => cli.template = Some(value("--template")?),
                "--set" => cli.template_sets.push(value("--set")?),
                "--keep" => cli.keep = Some(parse_number("--keep", value("--keep")?)?),
                "--max-age-days" => {
                    cli.max_age_days =
                        Some(parse_number("--max-age-days", value("--max-age-days")?)?)
                }
                "--auto" => cli.auto = true,
                "--name" => cli.name = Some(value("--name")?),
                "--interval-ms" => {
                    cli.interval_ms = Some(parse_number("--interval-ms", value("--interval-ms")?)?)
                }
                "--count" => cli.count = Some(parse_number("--count", value("--count")?)?),
                "--file" => cli.file = Some(PathBuf::from(value("--file")?)),
                "--merge" => cli.merge = true,
                "--replace" => cli.replace = true,
                "--json" => (),
                "--version" | "-V" => {
                    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                "--help" | "-h" => {
                    print!("{}", usage());
                    std::process::exit(0);
                }
                other => cli.args.push(other.to_owned()),
            }
        }
        if cli.merge && cli.replace {
            return Err("--merge and --replace are mutually exclusive".to_owned());
        }
        Ok(cli)
    }

    fn state(&self) -> PathBuf {
        self.state.clone().unwrap_or_else(state_path)
    }

    fn history(&self) -> HistoryStore {
        if let Some(dir) = std::env::var_os("LOOM_SESSION_HISTORY") {
            return HistoryStore::new(PathBuf::from(dir));
        }
        let state = self.state();
        match state.parent() {
            Some(parent) => HistoryStore::new(parent.join("history")),
            None => HistoryStore::new(history_dir()),
        }
    }

    fn keep(&self) -> usize {
        self.keep.unwrap_or(DEFAULT_HISTORY_KEEP)
    }

    fn profile_env(&self) -> ProfileEnv {
        let mut env = ProfileEnv::detect();
        if self.companion_socket.is_some() {
            env.companion_socket = self.companion_socket.clone();
        }
        env
    }

    fn restore_options(&self, dry_run: bool) -> RestoreOptions {
        RestoreOptions {
            dry_run,
            explain: self.explain,
            force: self.force,
            wait_ms: self.wait_ms,
            poll_ms: self.poll_ms,
            retries: self.retries,
            preserve_user_changes: self.preserve_user_changes,
            launch_env: LaunchEnv::detect(),
            profile_env: self.profile_env(),
        }
    }

    fn apply_profile_hooks(&self, profiles: &mut Profiles) -> Result<(), String> {
        for (kind, command) in &self.profile_activate {
            hooks_mut(profiles, kind)
                .ok_or_else(|| profile_missing_error(kind))?
                .activate
                .push(command.clone());
        }
        for (kind, command) in &self.profile_deactivate {
            hooks_mut(profiles, kind)
                .ok_or_else(|| profile_missing_error(kind))?
                .deactivate
                .push(command.clone());
        }
        Ok(())
    }
}

fn parse_pair(flag: &str, value: String) -> Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| format!("{flag} needs key=value, got `{value}`"))?;
    if key.is_empty() {
        return Err(format!("{flag} needs a non-empty key"));
    }
    Ok((key.to_owned(), value.to_owned()))
}

fn parse_number<T: std::str::FromStr>(flag: &str, value: String) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} needs a number, got `{value}`"))
}

fn profile_missing_error(kind: &str) -> String {
    format!(
        "profile `{kind}` is not configured; add --{kind}-profile/--{kind}-scene/--companion-rule first"
    )
}

fn hooks_mut<'a>(profiles: &'a mut Profiles, kind: &str) -> Option<&'a mut ProfileHooks> {
    match kind {
        "monitor" => profiles.monitor.as_mut().map(|profile| &mut profile.hooks),
        "audio" => profiles.audio.as_mut().map(|profile| &mut profile.hooks),
        "wallpaper" => profiles
            .wallpaper
            .as_mut()
            .map(|profile| &mut profile.hooks),
        "companion" => profiles
            .companion
            .as_mut()
            .map(|profile| &mut profile.hooks),
        _ => None,
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("loom-session: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse()?;
    let Some(command) = cli.args.first().cloned() else {
        return Err(format!("missing command\n\n{}", usage()));
    };
    let state = cli.state();
    let mut store = Store::load(&state).map_err(|err| format!("cannot load state: {err}"))?;
    let socket = || {
        cli.socket
            .clone()
            .ok_or_else(|| "NIRI_SOCKET is not set; run inside niri or pass --socket".to_owned())
    };
    let save = |store: &Store| {
        store
            .save(&state)
            .map_err(|err| format!("cannot save state: {err}"))
    };

    match command.as_str() {
        "list" => {
            let names: Vec<_> = store.contexts.keys().cloned().collect();
            println!("{}", serde_json::to_string_pretty(&names).unwrap());
        }
        "show" => {
            let name = positional(&cli.args, 1, "show needs a context name")?;
            let context = store
                .get(name)
                .ok_or_else(|| format!("unknown context `{name}`"))?;
            println!("{}", serde_json::to_string_pretty(context).unwrap());
        }
        "delete" => {
            let name = positional(&cli.args, 1, "delete needs a context name")?;
            if !store.remove(name) {
                return Err(format!("unknown context `{name}`"));
            }
            save(&store)?;
            println!("{}", json!({ "deleted": name }));
        }
        "rename" => {
            let from = positional(&cli.args, 1, "rename needs an old and a new name")?;
            let to = positional(&cli.args, 2, "rename needs an old and a new name")?;
            store.rename(from, to)?;
            save(&store)?;
            println!("{}", json!({ "renamed": from, "to": to }));
        }
        "capture" | "save" => {
            let name = positional(&cli.args, 1, &format!("{command} needs a context name"))?;
            let (workspaces, windows, outputs) = query(&socket()?)?;
            let existing = store.get(name).cloned();
            let mut command_map = existing
                .as_ref()
                .map(|context| context.command_map.clone())
                .unwrap_or_default();
            command_map.extend(cli.launches.clone());
            let mut hooks = existing
                .as_ref()
                .map(|context| context.hooks.clone())
                .unwrap_or_default();
            hooks.pre.extend(cli.hook_pre.clone());
            hooks.activate.extend(cli.hook_activate.clone());
            hooks.post.extend(cli.hook_post.clone());
            hooks.deactivate.extend(cli.hook_deactivate.clone());

            let mut context = capture(&workspaces, &windows, &outputs, command_map, hooks);
            let mut profiles = existing
                .as_ref()
                .map(|context| context.profiles.clone())
                .unwrap_or_default();
            apply_profile_overrides(&cli, &mut profiles);
            cli.apply_profile_hooks(&mut profiles)?;
            context.profiles = profiles;
            context.template = existing
                .as_ref()
                .and_then(|context| context.template.clone());
            let created_at = existing
                .as_ref()
                .map(|existing| existing.created_at)
                .unwrap_or(context.created_at);
            context.created_at = created_at;

            if let Some(template) = &cli.template {
                apply_capture_template(&mut context, template, &cli.template_sets)?;
            }

            store.insert(name.to_owned(), context.clone());
            save(&store)?;
            println!("{}", serde_json::to_string_pretty(&context).unwrap());
        }
        "restore" | "explain" => {
            let name = positional(&cli.args, 1, &format!("{command} needs a context name"))?;
            let context = store
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unknown context `{name}`"))?;
            let explain = cli.explain || command == "explain";
            let dry_run = cli.dry_run || explain;
            if cli.snapshot_before && !dry_run {
                record_snapshot(&cli, &store, name, "pre-restore", &socket()?)?;
            }
            let options = cli.restore_options(dry_run);
            let outcome = restore_context(&socket()?, name, &context, &options)?;
            println!("{}", serde_json::to_string_pretty(&outcome.report).unwrap());
            if let Some(error) = outcome.failure {
                return Err(error);
            }
        }
        "deactivate" => {
            let name = positional(&cli.args, 1, "deactivate needs a context name")?;
            let context = store
                .get(name)
                .cloned()
                .ok_or_else(|| format!("unknown context `{name}`"))?;
            let report = deactivate_context(name, &context, cli.dry_run, &cli.profile_env());
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if !report.ok {
                return Err(report
                    .error
                    .unwrap_or_else(|| "deactivation failed".to_owned()));
            }
        }
        "history" => run_history(&cli, &mut store, &socket)?,
        "snapshot" => run_snapshot(&cli, &store, &socket)?,
        "export" => run_export(&cli, &store)?,
        "import" => run_import(&cli, &mut store, &save)?,
        "template" => run_template(&cli, &mut store, &save)?,
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}

fn positional<'a>(args: &'a [String], index: usize, message: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| message.to_owned())
}

fn apply_profile_overrides(cli: &Cli, profiles: &mut Profiles) {
    if let Some(name) = &cli.monitor_profile {
        let hooks = profiles
            .monitor
            .as_ref()
            .map(|profile| profile.hooks.clone())
            .unwrap_or_default();
        profiles.monitor = Some(MonitorProfile {
            profile: name.clone(),
            apply: cli.monitor_via.unwrap_or_default(),
            command: cli.monitor_command.clone(),
            hooks,
        });
    } else if let Some(monitor) = &mut profiles.monitor
        && (cli.monitor_command.is_some() || cli.monitor_via.is_some())
    {
        if let Some(apply) = cli.monitor_via {
            monitor.apply = apply;
        }
        if cli.monitor_command.is_some() {
            monitor.command = cli.monitor_command.clone();
        }
    }

    if let Some(name) = &cli.audio_profile {
        let hooks = profiles
            .audio
            .as_ref()
            .map(|profile| profile.hooks.clone())
            .unwrap_or_default();
        profiles.audio = Some(AudioProfile {
            profile: name.clone(),
            config: cli.audio_config.clone(),
            command: cli.audio_command.clone(),
            hooks,
        });
    } else if let Some(audio) = &mut profiles.audio
        && (cli.audio_config.is_some() || cli.audio_command.is_some())
    {
        audio.config = cli.audio_config.clone();
        audio.command = cli.audio_command.clone();
    }

    if let Some(scene) = &cli.wallpaper_scene {
        let hooks = profiles
            .wallpaper
            .as_ref()
            .map(|profile| profile.hooks.clone())
            .unwrap_or_default();
        profiles.wallpaper = Some(WallpaperProfile {
            scene: scene.clone(),
            command: cli.wallpaper_command.clone(),
            hooks,
        });
    } else if let Some(wallpaper) = &mut profiles.wallpaper
        && cli.wallpaper_command.is_some()
    {
        wallpaper.command = cli.wallpaper_command.clone();
    }

    if !cli.companion_rules.is_empty()
        || !cli.companion_marks.is_empty()
        || !cli.companion_calls.is_empty()
        || cli.companion_command.is_some()
    {
        let existing = profiles.companion.clone().unwrap_or_default();
        let mut companion = CompanionProfile {
            rules: existing.rules,
            marks: existing.marks,
            calls: existing.calls,
            command: existing.command,
            hooks: existing.hooks,
        };
        companion.rules.extend(cli.companion_rules.clone());
        companion.marks.extend(cli.companion_marks.clone());
        companion.calls.extend(cli.companion_calls.clone());
        if cli.companion_command.is_some() {
            companion.command = cli.companion_command.clone();
        }
        companion.rules.sort();
        companion.rules.dedup();
        companion.calls.sort();
        companion.calls.dedup();
        profiles.companion = Some(companion);
    }
}

fn apply_capture_template(
    context: &mut Context,
    name: &str,
    sets: &[String],
) -> Result<(), String> {
    let template = templates::get(name).ok_or_else(|| format!("unknown template `{name}`"))?;
    let values = templates::values_from_sets(template, sets)?;
    let rendered = templates::render(name, &values)?;
    let app_ids: BTreeSet<&str> = context
        .windows
        .iter()
        .map(|window| window.app_id.as_str())
        .collect();
    for (app_id, command) in &rendered.command_map {
        if app_ids.contains(app_id.as_str()) && !context.command_map.contains_key(app_id) {
            context.command_map.insert(app_id.clone(), command.clone());
        }
    }
    context.template = Some(template.name.to_owned());
    Ok(())
}

fn record_snapshot(
    cli: &Cli,
    store: &Store,
    name: &str,
    trigger: &str,
    socket: &Path,
) -> Result<loom_session::HistoryEntry, String> {
    let (workspaces, windows, outputs) = query(socket)?;
    let existing = store.get(name);
    let mut context = capture(
        &workspaces,
        &windows,
        &outputs,
        existing
            .map(|context| context.command_map.clone())
            .unwrap_or_default(),
        existing
            .map(|context| context.hooks.clone())
            .unwrap_or_default(),
    );
    if let Some(existing) = existing {
        context.profiles = existing.profiles.clone();
        context.template = existing.template.clone();
        context.created_at = existing.created_at;
    }
    let entry = cli
        .history()
        .record(name, trigger, &context, cli.keep())
        .map_err(|err| format!("cannot record history: {err}"))?;
    Ok(entry)
}

fn run_snapshot(
    cli: &Cli,
    store: &Store,
    socket: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<(), String> {
    let name = cli.name.clone().unwrap_or_else(|| "auto".to_owned());
    let trigger = if cli.auto { "auto" } else { "manual" };
    let socket = socket()?;
    let limit = match (cli.interval_ms, cli.count) {
        (None, _) => 1,
        (Some(_), Some(count)) => count.max(1),
        (Some(_), None) => usize::MAX,
    };
    let mut written = 0;
    loop {
        let entry = record_snapshot(cli, store, &name, trigger, &socket)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "id": entry.id,
                "name": entry.name,
                "trigger": entry.trigger,
                "created_at": entry.created_at,
            }))
            .unwrap()
        );
        written += 1;
        if written >= limit {
            break;
        }
        let interval = cli.interval_ms.unwrap_or(0);
        if interval == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(interval));
    }
    Ok(())
}

fn run_history(
    cli: &Cli,
    _store: &mut Store,
    socket: &dyn Fn() -> Result<PathBuf, String>,
) -> Result<(), String> {
    let sub = positional(&cli.args, 1, "history needs list/show/restore/prune")?;
    let history = cli.history();
    match sub {
        "list" => {
            let listing = history
                .load()
                .map_err(|err| format!("cannot read history: {err}"))?;
            for (path, reason) in &listing.skipped {
                eprintln!(
                    "loom-session: skipping history file {}: {reason}",
                    path.display()
                );
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&listing.summaries()).unwrap()
            );
        }
        "show" => {
            let key = positional(&cli.args, 2, "history show needs an id or name")?;
            let entry = history
                .find(key)
                .map_err(|err| format!("cannot read history: {err}"))?
                .ok_or_else(|| format!("unknown history entry `{key}`"))?;
            println!("{}", serde_json::to_string_pretty(&entry).unwrap());
        }
        "restore" => {
            let key = positional(&cli.args, 2, "history restore needs an id or name")?;
            let entry = history
                .find(key)
                .map_err(|err| format!("cannot read history: {err}"))?
                .ok_or_else(|| format!("unknown history entry `{key}`"))?;
            let dry_run = cli.dry_run || cli.explain;
            let options = cli.restore_options(dry_run);
            let outcome = restore_context(&socket()?, &entry.name, &entry.context, &options)?;
            println!("{}", serde_json::to_string_pretty(&outcome.report).unwrap());
            if let Some(error) = outcome.failure {
                return Err(error);
            }
        }
        "prune" => {
            let max_age = cli
                .max_age_days
                .map(|days| days.saturating_mul(24 * 60 * 60));
            let removed = history
                .prune(cli.keep(), max_age)
                .map_err(|err| format!("cannot prune history: {err}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "removed": removed,
                    "keep": cli.keep(),
                }))
                .unwrap()
            );
        }
        other => return Err(format!("unknown history command `{other}`")),
    }
    Ok(())
}

fn read_input(file: &Option<PathBuf>) -> Result<Value, String> {
    let mut bytes = Vec::new();
    match file {
        Some(path) => {
            bytes = std::fs::read(path)
                .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
        }
        None => {
            std::io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|err| format!("cannot read stdin: {err}"))?;
        }
    }
    serde_json::from_slice(&bytes).map_err(|err| format!("invalid JSON: {err}"))
}

fn run_export(cli: &Cli, store: &Store) -> Result<(), String> {
    let value = match cli.args.get(1) {
        Some(name) => {
            let context = store
                .get(name)
                .ok_or_else(|| format!("unknown context `{name}`"))?;
            export_context(name, context)
        }
        None => export_store(store),
    };
    let bytes =
        serde_json::to_vec_pretty(&value).map_err(|err| format!("cannot encode export: {err}"))?;
    match &cli.file {
        Some(path) => {
            write_atomic(path, &bytes)
                .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
            println!("{}", json!({ "exported": path }));
        }
        None => println!("{}", String::from_utf8_lossy(&bytes)),
    }
    Ok(())
}

fn run_import(
    cli: &Cli,
    store: &mut Store,
    save: &dyn Fn(&Store) -> Result<(), String>,
) -> Result<(), String> {
    let value = read_input(&cli.file)?;
    let mode = if cli.replace {
        ImportMode::Replace
    } else {
        ImportMode::Merge
    };
    let report = import_into(store, &value, mode, cli.name.as_deref())?;
    save(store)?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(())
}

fn run_template(
    cli: &Cli,
    store: &mut Store,
    save: &dyn Fn(&Store) -> Result<(), String>,
) -> Result<(), String> {
    let sub = positional(&cli.args, 1, "template needs list/show/apply")?;
    match sub {
        "list" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&templates::describe_all()).unwrap()
            );
        }
        "show" => {
            let name = positional(&cli.args, 2, "template show needs a template name")?;
            let raw = templates::raw(name).ok_or_else(|| format!("unknown template `{name}`"))?;
            println!("{}", serde_json::to_string_pretty(&raw).unwrap());
        }
        "apply" => {
            let template = positional(&cli.args, 2, "template apply needs a template name")?;
            let name = positional(&cli.args, 3, "template apply needs a context name")?;
            let definition =
                templates::get(template).ok_or_else(|| format!("unknown template `{template}`"))?;
            let values = templates::values_from_sets(definition, &cli.template_sets)?;
            let context = templates::render(template, &values)?;
            if store.get(name).is_some() && !cli.force {
                return Err(format!(
                    "context `{name}` already exists (pass --force to replace it)"
                ));
            }
            store.insert(name, context.clone());
            save(store)?;
            println!("{}", serde_json::to_string_pretty(&context).unwrap());
        }
        other => return Err(format!("unknown template command `{other}`")),
    }
    Ok(())
}
