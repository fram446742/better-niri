//! Optional associations between a context and the other Loom apps.
//!
//! Nothing here is a hard dependency. Every association is opt-in, is applied
//! after the core niri restore, and reports one of `applied`, `skipped`,
//! `missing` or `failed`. A missing or failing app never aborts a restore.
//!
//! - `monitor`: a `loom-monitor` profile. Applied over niri IPC by reading `profiles.json`, or by
//!   running `loom-monitor apply` when the profile cannot be read (or when `apply = "command"` is
//!   requested).
//! - `audio`: a named `loom-audio` rules file, applied with `loom-audio --config <file> apply`.
//! - `wallpaper`: a `loom-wallpaper` scene, selected and applied.
//! - `companion`: `loomctl` calls (for example `rules.run`), plus optional rule names and marks
//!   recorded on the context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use niri_ipc::{Output, Request, Response};
use serde::{Deserialize, Serialize};

use crate::{
    Context, HookStatus, OutputSnapshot, Outputs, execute_steps_guarded, plan_outputs,
    run_hooks_env, send_with_retry,
};

/// Applied/skipped/missing/failed status values.
pub const STATUS_APPLIED: &str = "applied";
pub const STATUS_SKIPPED: &str = "skipped";
pub const STATUS_MISSING: &str = "missing";
pub const STATUS_FAILED: &str = "failed";

/// Hooks that belong to one optional app profile.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProfileHooks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activate: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deactivate: Vec<String>,
}

impl ProfileHooks {
    pub fn is_empty(&self) -> bool {
        self.activate.is_empty() && self.deactivate.is_empty()
    }
}

/// Optional profiles associated with a context.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<MonitorProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallpaper: Option<WallpaperProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<CompanionProfile>,
}

impl Profiles {
    pub fn is_empty(&self) -> bool {
        self.monitor.is_none()
            && self.audio.is_none()
            && self.wallpaper.is_none()
            && self.companion.is_none()
    }
}

/// How a `loom-monitor` profile is applied.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MonitorApply {
    /// Use niri IPC when the profile can be read, else the command.
    #[default]
    Auto,
    /// Only niri `Request::Output` changes from the profile store.
    Ipc,
    /// Only `loom-monitor apply <profile>`.
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MonitorProfile {
    pub profile: String,
    #[serde(default)]
    pub apply: MonitorApply,
    /// Shell command override; when set it is used instead of the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "ProfileHooks::is_empty")]
    pub hooks: ProfileHooks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioProfile {
    pub profile: String,
    /// Explicit rules file. Default:
    /// `$XDG_CONFIG_HOME/loom-audio/profiles/<profile>.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "ProfileHooks::is_empty")]
    pub hooks: ProfileHooks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WallpaperProfile {
    pub scene: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "ProfileHooks::is_empty")]
    pub hooks: ProfileHooks,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompanionProfile {
    /// Rule names recorded as intent. `loomctl` can only run all matching
    /// rules, so this list is metadata unless `calls`/`command` is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    /// Marks recorded on the context and exported to commands as
    /// `LOOM_SESSION_COMPANION_MARKS`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<String, String>,
    /// `loomctl` calls, written as `namespace.action`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "ProfileHooks::is_empty")]
    pub hooks: ProfileHooks,
}

/// One app profile's outcome.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProfileStatus {
    /// `monitor`, `audio`, `wallpaper` or `companion`.
    pub profile: String,
    /// Profile/scene/call name.
    pub name: String,
    /// `applied`, `skipped`, `missing` or `failed`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activate: Option<HookStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deactivate: Option<HookStatus>,
}

/// Locates optional apps and their profile files.
#[derive(Debug, Clone)]
pub struct ProfileEnv {
    /// PATH used for lookups and child processes. `None` inherits the process
    /// PATH. Injecting it keeps tests independent from the host.
    pub path_dirs: Option<Vec<PathBuf>>,
    /// `loom-monitor` profile store.
    pub monitor_state: PathBuf,
    /// `loom-audio` configuration directory.
    pub audio_config_dir: PathBuf,
    /// Optional `loomctl --socket` argument.
    pub companion_socket: Option<PathBuf>,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn state_home() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".local/state"))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"))
}

impl Default for ProfileEnv {
    fn default() -> Self {
        Self::detect()
    }
}

impl ProfileEnv {
    pub fn detect() -> Self {
        Self {
            path_dirs: std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()),
            monitor_state: std::env::var_os("LOOM_MONITOR_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| state_home().join("loom-monitor/profiles.json")),
            audio_config_dir: std::env::var_os("LOOM_AUDIO_CONFIG")
                .map(PathBuf::from)
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| config_home().join("loom-audio")),
            companion_socket: None,
        }
    }

    fn available(&self, program: &str) -> bool {
        if program.contains('/') {
            return is_executable(Path::new(program));
        }
        match &self.path_dirs {
            Some(dirs) => dirs.iter().any(|dir| is_executable(&dir.join(program))),
            None => std::env::var_os("PATH")
                .map(|path| {
                    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program)))
                })
                .unwrap_or(false),
        }
    }

    fn child(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        if let Some(dirs) = &self.path_dirs {
            command.env("PATH", std::env::join_paths(dirs).unwrap_or_default());
        }
        command
    }

    fn child_shell(&self, script: &str) -> Command {
        let mut command = self.child("sh");
        command.arg("-c").arg(script);
        command
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Options for applying optional profiles.
#[derive(Debug, Clone)]
pub struct ProfileApplyOptions {
    pub dry_run: bool,
    pub preserve_user_changes: bool,
    pub retries: u32,
}

impl Default for ProfileApplyOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            preserve_user_changes: true,
            retries: crate::DEFAULT_RETRIES,
        }
    }
}

fn status(
    profile: &str,
    name: impl Into<String>,
    status: &str,
    detail: Option<String>,
) -> ProfileStatus {
    ProfileStatus {
        profile: profile.to_owned(),
        name: name.into(),
        status: status.to_owned(),
        detail,
        error: None,
        activate: None,
        deactivate: None,
    }
}

fn failed(profile: &str, name: impl Into<String>, error: String) -> ProfileStatus {
    let mut status = status(profile, name, STATUS_FAILED, None);
    status.error = Some(error);
    status
}

fn run_profile_hooks(
    context_name: &str,
    profile: &str,
    phase: &str,
    commands: &[String],
    extra: &BTreeMap<String, String>,
) -> HookStatus {
    if commands.is_empty() {
        return HookStatus::none();
    }
    run_hooks_env(context_name, phase, commands, Some(profile), extra)
}

fn mark_env(
    profile: &str,
    name: &str,
    marks: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("LOOM_SESSION_PROFILE".to_owned(), profile.to_owned());
    env.insert("LOOM_SESSION_PROFILE_NAME".to_owned(), name.to_owned());
    if !marks.is_empty() {
        env.insert(
            "LOOM_SESSION_COMPANION_MARKS".to_owned(),
            serde_json::to_string(marks).unwrap_or_else(|_| "{}".to_owned()),
        );
    }
    env
}

/// Applies every configured profile, in a fixed order:
/// monitor, audio, wallpaper, companion.
pub fn apply_profiles(
    socket: &Path,
    context_name: &str,
    context: &Context,
    options: &ProfileApplyOptions,
    env: &ProfileEnv,
) -> Vec<ProfileStatus> {
    let mut statuses = Vec::new();
    if let Some(monitor) = &context.profiles.monitor {
        statuses.push(apply_monitor(socket, context_name, monitor, options, env));
    }
    statuses.extend(apply_other_profiles(
        socket,
        context_name,
        context,
        options,
        env,
    ));
    statuses
}

/// Applies the monitor profile only. Restore runs this before workspaces.
pub fn apply_monitor_profile(
    socket: &Path,
    context_name: &str,
    monitor: &MonitorProfile,
    options: &ProfileApplyOptions,
    env: &ProfileEnv,
) -> ProfileStatus {
    apply_monitor(socket, context_name, monitor, options, env)
}

/// Applies audio, wallpaper and companion profiles, in that order.
pub fn apply_other_profiles(
    socket: &Path,
    context_name: &str,
    context: &Context,
    options: &ProfileApplyOptions,
    env: &ProfileEnv,
) -> Vec<ProfileStatus> {
    let mut statuses = Vec::new();
    if let Some(audio) = &context.profiles.audio {
        statuses.push(apply_audio(context_name, audio, options, env));
    }
    if let Some(wallpaper) = &context.profiles.wallpaper {
        statuses.push(apply_wallpaper(
            socket,
            context_name,
            wallpaper,
            options,
            env,
        ));
    }
    if let Some(companion) = &context.profiles.companion {
        statuses.push(apply_companion(context_name, companion, options, env));
    }
    statuses
}

// --- monitor ---------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct MonitorStore {
    #[serde(default)]
    profiles: BTreeMap<String, MonitorProfileData>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct MonitorProfileData {
    #[serde(rename = "match", default)]
    matcher: Option<MonitorMatch>,
    #[serde(default)]
    outputs: BTreeMap<String, OutputSnapshot>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct MonitorMatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    make: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    serial: Option<String>,
}

impl MonitorMatch {
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.make.is_none() && self.model.is_none() && self.serial.is_none()
    }

    fn matches(&self, output: &Output) -> bool {
        if self.is_empty() {
            return true;
        }
        self.name
            .as_deref()
            .is_none_or(|pattern| wildcard_match(pattern, &output.name))
            && self
                .make
                .as_deref()
                .is_none_or(|pattern| wildcard_match(pattern, &output.make))
            && self
                .model
                .as_deref()
                .is_none_or(|pattern| wildcard_match(pattern, &output.model))
            && self.serial.as_deref().is_none_or(|pattern| {
                wildcard_match(pattern, output.serial.as_deref().unwrap_or(""))
            })
    }
}

/// `*` wildcard, case-insensitive, like `loom-monitor`.
fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let value: Vec<char> = value.to_lowercase().chars().collect();
    let (mut p, mut v) = (0, 0);
    let mut star: Option<usize> = None;
    let mut star_value = 0;
    while v < value.len() {
        if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            star_value = v;
            p += 1;
        } else if let Some(star_p) = star {
            p = star_p + 1;
            star_value += 1;
            v = star_value;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn load_monitor_profile(env: &ProfileEnv, name: &str) -> Option<MonitorProfileData> {
    let bytes = std::fs::read(&env.monitor_state).ok()?;
    let store: MonitorStore = serde_json::from_slice(&bytes).ok()?;
    store.profiles.get(name).cloned()
}

fn monitor_outputs(data: &MonitorProfileData, live: &Outputs) -> BTreeMap<String, OutputSnapshot> {
    let mut desired = BTreeMap::new();
    if let Some(matcher) = &data.matcher {
        for (name, output) in live {
            if !matcher.matches(output) {
                continue;
            }
            let snapshot = data
                .outputs
                .get(name)
                .or_else(|| {
                    (data.outputs.len() == 1).then(|| data.outputs.values().next().unwrap())
                })
                .cloned();
            if let Some(snapshot) = snapshot {
                desired.insert(name.clone(), snapshot);
            }
        }
    } else {
        desired = data.outputs.clone();
    }
    desired
}

fn apply_monitor(
    socket: &Path,
    context_name: &str,
    profile: &MonitorProfile,
    options: &ProfileApplyOptions,
    env: &ProfileEnv,
) -> ProfileStatus {
    if profile.profile.trim().is_empty() {
        return failed(
            "monitor",
            &profile.profile,
            "profile name is empty".to_owned(),
        );
    }
    let use_command = profile.apply == MonitorApply::Command
        || (profile.command.is_some() && profile.apply != MonitorApply::Ipc);
    let data = if profile.apply == MonitorApply::Ipc || !use_command {
        load_monitor_profile(env, &profile.profile)
    } else {
        None
    };
    let mut result = if let Some(data) = data {
        apply_monitor_ipc(socket, &profile.profile, &data, options)
    } else if use_command {
        if let Some(command) = &profile.command {
            run_command_status(
                "monitor",
                &profile.profile,
                command,
                env,
                options,
                context_name,
            )
        } else if env.available("loom-monitor") {
            let command = format!(
                "loom-monitor --state {} apply {}",
                shell_quote(&env.monitor_state.display().to_string()),
                shell_quote(&profile.profile)
            );
            run_command_status(
                "monitor",
                &profile.profile,
                &command,
                env,
                options,
                context_name,
            )
        } else {
            status(
                "monitor",
                &profile.profile,
                STATUS_MISSING,
                Some(format!(
                    "no loom-monitor profile in {} and `loom-monitor` is not installed",
                    env.monitor_state.display()
                )),
            )
        }
    } else if profile.apply == MonitorApply::Ipc {
        status(
            "monitor",
            &profile.profile,
            STATUS_MISSING,
            Some(format!(
                "profile not found in {}",
                env.monitor_state.display()
            )),
        )
    } else if env.available("loom-monitor") {
        let command = format!(
            "loom-monitor --state {} apply {}",
            shell_quote(&env.monitor_state.display().to_string()),
            shell_quote(&profile.profile)
        );
        run_command_status(
            "monitor",
            &profile.profile,
            &command,
            env,
            options,
            context_name,
        )
    } else {
        status(
            "monitor",
            &profile.profile,
            STATUS_MISSING,
            Some(format!(
                "no loom-monitor profile in {} and `loom-monitor` is not installed",
                env.monitor_state.display()
            )),
        )
    };
    finish_with_hooks(
        context_name,
        "monitor",
        &mut result,
        &profile.hooks,
        options,
    );
    result
}

fn apply_monitor_ipc(
    socket: &Path,
    name: &str,
    data: &MonitorProfileData,
    options: &ProfileApplyOptions,
) -> ProfileStatus {
    let live = match send_with_retry(socket, Request::Outputs, options.retries) {
        Ok(Response::Outputs(outputs)) => outputs.into_iter().collect::<Outputs>(),
        Ok(other) => {
            return failed(
                "monitor",
                name,
                format!("unexpected niri response: {other:?}"),
            );
        }
        Err(error) => return failed("monitor", name, error),
    };
    let desired = monitor_outputs(data, &live);
    if desired.is_empty() {
        return status(
            "monitor",
            name,
            STATUS_MISSING,
            Some("no connected output matches this profile".to_owned()),
        );
    }
    let synthetic = Context {
        outputs: desired,
        ..Context::default()
    };
    let (steps, skipped) = plan_outputs(&synthetic, &live);
    if steps.is_empty() {
        if skipped
            .iter()
            .any(|skipped| skipped.reason == "output not connected")
        {
            return status(
                "monitor",
                name,
                STATUS_MISSING,
                Some("profile outputs are not connected".to_owned()),
            );
        }
        return status(
            "monitor",
            name,
            STATUS_SKIPPED,
            Some("already applied".to_owned()),
        );
    }
    if options.dry_run {
        return status(
            "monitor",
            name,
            STATUS_SKIPPED,
            Some(format!(
                "dry-run: would send {} output requests",
                steps.len()
            )),
        );
    }
    match execute_steps_guarded(
        socket,
        &steps,
        false,
        options.retries,
        options.preserve_user_changes,
    ) {
        Ok(outcome) if outcome.skipped.is_empty() => status(
            "monitor",
            name,
            STATUS_APPLIED,
            Some(format!("{} niri output requests", outcome.applied.len())),
        ),
        Ok(outcome) => status(
            "monitor",
            name,
            STATUS_SKIPPED,
            Some(format!(
                "applied {} requests; {} targets changed or disappeared",
                outcome.applied.len(),
                outcome.skipped.len()
            )),
        ),
        Err(error) => failed("monitor", name, error),
    }
}

// --- audio -----------------------------------------------------------------

fn apply_audio(
    context_name: &str,
    profile: &AudioProfile,
    options: &ProfileApplyOptions,
    env: &ProfileEnv,
) -> ProfileStatus {
    if profile.profile.trim().is_empty() {
        return failed(
            "audio",
            &profile.profile,
            "profile name is empty".to_owned(),
        );
    }
    let mut result = if let Some(command) = &profile.command {
        run_command_status(
            "audio",
            &profile.profile,
            command,
            env,
            options,
            context_name,
        )
    } else {
        let config = profile.config.clone().unwrap_or_else(|| {
            let candidate = env
                .audio_config_dir
                .join("profiles")
                .join(format!("{}.json", profile.profile));
            if candidate.exists() || profile.profile != "default" {
                candidate
            } else {
                env.audio_config_dir.join("rules.json")
            }
        });
        if !config.is_file() {
            status(
                "audio",
                &profile.profile,
                STATUS_MISSING,
                Some(format!("audio rules file not found: {}", config.display())),
            )
        } else if !env.available("loom-audio") {
            status(
                "audio",
                &profile.profile,
                STATUS_MISSING,
                Some("`loom-audio` is not installed".to_owned()),
            )
        } else {
            let command = format!(
                "loom-audio --config {} apply",
                shell_quote(&config.display().to_string())
            );
            run_command_status(
                "audio",
                &profile.profile,
                &command,
                env,
                options,
                context_name,
            )
        }
    };
    finish_with_hooks(context_name, "audio", &mut result, &profile.hooks, options);
    result
}

// --- wallpaper -------------------------------------------------------------

fn apply_wallpaper(
    socket: &Path,
    context_name: &str,
    profile: &WallpaperProfile,
    options: &ProfileApplyOptions,
    env: &ProfileEnv,
) -> ProfileStatus {
    if profile.scene.trim().is_empty() {
        return failed(
            "wallpaper",
            &profile.scene,
            "scene name is empty".to_owned(),
        );
    }
    let mut result = if let Some(command) = &profile.command {
        run_command_status(
            "wallpaper",
            &profile.scene,
            command,
            env,
            options,
            context_name,
        )
    } else if !env.available("loom-wallpaper") {
        status(
            "wallpaper",
            &profile.scene,
            STATUS_MISSING,
            Some("`loom-wallpaper` is not installed".to_owned()),
        )
    } else if options.dry_run {
        status(
            "wallpaper",
            &profile.scene,
            STATUS_SKIPPED,
            Some(format!(
                "dry-run: would run `loom-wallpaper scene use {}` then `apply`",
                profile.scene
            )),
        )
    } else {
        let socket_arg = format!("--socket {}", shell_quote(&socket.display().to_string()));
        let select = format!(
            "loom-wallpaper {socket_arg} scene use {}",
            shell_quote(&profile.scene)
        );
        match run_shell(
            &select,
            env,
            None,
            context_name,
            "wallpaper",
            &profile.scene,
        ) {
            Ok(()) => {
                let apply = format!("loom-wallpaper {socket_arg} apply");
                match run_shell(&apply, env, None, context_name, "wallpaper", &profile.scene) {
                    Ok(()) => status(
                        "wallpaper",
                        &profile.scene,
                        STATUS_APPLIED,
                        Some(format!("scene `{}` applied", profile.scene)),
                    ),
                    Err(error) => failed("wallpaper", &profile.scene, error),
                }
            }
            Err(error) => failed("wallpaper", &profile.scene, error),
        }
    };
    finish_with_hooks(
        context_name,
        "wallpaper",
        &mut result,
        &profile.hooks,
        options,
    );
    result
}

// --- companion -------------------------------------------------------------

fn apply_companion(
    context_name: &str,
    profile: &CompanionProfile,
    options: &ProfileApplyOptions,
    env: &ProfileEnv,
) -> ProfileStatus {
    let name = if profile.calls.is_empty() {
        profile
            .rules
            .first()
            .cloned()
            .unwrap_or_else(|| "companion".to_owned())
    } else {
        profile.calls.join(",")
    };
    let marks = mark_env("companion", &name, &profile.marks);
    let mut result = if let Some(command) = &profile.command {
        run_command_status_with_env(
            "companion",
            &name,
            command,
            env,
            options,
            context_name,
            &marks,
        )
    } else if !profile.calls.is_empty() {
        if !env.available("loomctl") {
            status(
                "companion",
                &name,
                STATUS_MISSING,
                Some("`loomctl` is not installed".to_owned()),
            )
        } else if options.dry_run {
            status(
                "companion",
                &name,
                STATUS_SKIPPED,
                Some(format!("dry-run: would call {:?}", profile.calls)),
            )
        } else {
            let mut failure = None;
            for call in &profile.calls {
                let Some((namespace, action)) = call.split_once('.') else {
                    failure = Some(format!("companion call `{call}` is not namespace.action"));
                    break;
                };
                let mut command = env.child("loomctl");
                if let Some(socket) = &env.companion_socket {
                    command.arg("--socket").arg(socket);
                }
                command
                    .arg("call")
                    .arg(namespace)
                    .arg(action)
                    .envs(&marks)
                    .env("LOOM_SESSION_CONTEXT", context_name);
                if let Err(error) = run_child(&mut command, call) {
                    failure = Some(error);
                    break;
                }
            }
            match failure {
                None => status(
                    "companion",
                    &name,
                    STATUS_APPLIED,
                    Some(format!("{} companion calls", profile.calls.len())),
                ),
                Some(error) => failed("companion", &name, error),
            }
        }
    } else if !profile.rules.is_empty() {
        if !env.available("loomctl") {
            status(
                "companion",
                &name,
                STATUS_MISSING,
                Some("`loomctl` is not installed".to_owned()),
            )
        } else if options.dry_run {
            status(
                "companion",
                &name,
                STATUS_SKIPPED,
                Some("dry-run: would run companion rules".to_owned()),
            )
        } else {
            let socket_arg = env
                .companion_socket
                .as_ref()
                .map(|socket| format!(" --socket {}", shell_quote(&socket.display().to_string())))
                .unwrap_or_default();
            let command = format!("loomctl{socket_arg} call rules run");
            run_command_status_with_env(
                "companion",
                &name,
                &command,
                env,
                options,
                context_name,
                &marks,
            )
        }
    } else if !profile.marks.is_empty() {
        status(
            "companion",
            &name,
            STATUS_SKIPPED,
            Some(
                "marks recorded; set companion.command or companion.calls to apply them".to_owned(),
            ),
        )
    } else {
        status(
            "companion",
            &name,
            STATUS_SKIPPED,
            Some("no rules, calls or marks configured".to_owned()),
        )
    };
    finish_with_hooks(
        context_name,
        "companion",
        &mut result,
        &profile.hooks,
        options,
    );
    result
}

// --- shared ----------------------------------------------------------------

fn run_command_status(
    profile: &str,
    name: &str,
    command: &str,
    env: &ProfileEnv,
    options: &ProfileApplyOptions,
    context_name: &str,
) -> ProfileStatus {
    run_command_status_with_env(
        profile,
        name,
        command,
        env,
        options,
        context_name,
        &BTreeMap::new(),
    )
}

fn run_command_status_with_env(
    profile: &str,
    name: &str,
    command: &str,
    env: &ProfileEnv,
    options: &ProfileApplyOptions,
    context_name: &str,
    extra: &BTreeMap<String, String>,
) -> ProfileStatus {
    if options.dry_run {
        return status(
            profile,
            name,
            STATUS_SKIPPED,
            Some(format!("dry-run: would run `{command}`")),
        );
    }
    let program = command.split_whitespace().next().unwrap_or_default();
    if (program.starts_with("loom-") || program == "loomctl") && !env.available(program) {
        return status(
            profile,
            name,
            STATUS_MISSING,
            Some(format!("`{program}` is not installed")),
        );
    }
    match run_shell(command, env, Some(extra), context_name, profile, name) {
        Ok(()) => status(profile, name, STATUS_APPLIED, None),
        Err(error) => failed(profile, name, error),
    }
}

fn run_shell(
    command: &str,
    env: &ProfileEnv,
    extra: Option<&BTreeMap<String, String>>,
    context_name: &str,
    profile: &str,
    name: &str,
) -> Result<(), String> {
    let mut child = env.child_shell(command);
    child
        .env("LOOM_SESSION_CONTEXT", context_name)
        .env("LOOM_SESSION_PROFILE", profile)
        .env("LOOM_SESSION_PROFILE_NAME", name);
    if let Some(extra) = extra {
        child.envs(extra);
    }
    run_child(&mut child, command)
}

fn run_child(command: &mut Command, label: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|err| format!("cannot run `{label}`: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        Err(format!("`{label}` exited with {}", output.status))
    } else {
        Err(format!("`{label}` exited with {}: {stderr}", output.status))
    }
}

fn finish_with_hooks(
    context_name: &str,
    profile: &str,
    result: &mut ProfileStatus,
    hooks: &ProfileHooks,
    options: &ProfileApplyOptions,
) {
    if hooks.activate.is_empty() {
        return;
    }
    if options.dry_run {
        result.activate = Some(HookStatus::skipped(&hooks.activate, "dry-run"));
        return;
    }
    if result.status != STATUS_APPLIED {
        return;
    }
    let marks = BTreeMap::new();
    let hook = run_profile_hooks(context_name, profile, "activate", &hooks.activate, &marks);
    let failed_hook = hook.status == "failed";
    let error = hook.error.clone();
    result.activate = Some(hook);
    if failed_hook {
        result.status = STATUS_FAILED.to_owned();
        result.error = error;
    }
}

/// Runs deactivate hooks for every configured profile.
pub fn deactivate_profiles(
    context_name: &str,
    context: &Context,
    dry_run: bool,
    _env: &ProfileEnv,
) -> Vec<ProfileStatus> {
    let mut statuses = Vec::new();
    let entries: Vec<(&str, &str, &ProfileHooks)> = [
        context
            .profiles
            .monitor
            .as_ref()
            .map(|profile| ("monitor", profile.profile.as_str(), &profile.hooks)),
        context
            .profiles
            .audio
            .as_ref()
            .map(|profile| ("audio", profile.profile.as_str(), &profile.hooks)),
        context
            .profiles
            .wallpaper
            .as_ref()
            .map(|profile| ("wallpaper", profile.scene.as_str(), &profile.hooks)),
        context
            .profiles
            .companion
            .as_ref()
            .map(|profile| ("companion", "companion", &profile.hooks)),
    ]
    .into_iter()
    .flatten()
    .collect();

    for (profile, name, hooks) in entries {
        let mut entry = if hooks.deactivate.is_empty() {
            status(
                profile,
                name,
                STATUS_SKIPPED,
                Some("no deactivate hooks".to_owned()),
            )
        } else if dry_run {
            let mut entry = status(
                profile,
                name,
                STATUS_SKIPPED,
                Some(format!(
                    "dry-run: would run {} deactivate hook(s)",
                    hooks.deactivate.len()
                )),
            );
            entry.deactivate = Some(HookStatus::skipped(&hooks.deactivate, "dry-run"));
            entry
        } else {
            let hook = run_profile_hooks(
                context_name,
                profile,
                "deactivate",
                &hooks.deactivate,
                &BTreeMap::new(),
            );
            let mut entry = if hook.status == "failed" {
                let mut entry = failed(
                    profile,
                    name,
                    hook.error
                        .clone()
                        .unwrap_or_else(|| "deactivate hook failed".to_owned()),
                );
                entry.deactivate = Some(hook);
                entry
            } else {
                status(
                    profile,
                    name,
                    STATUS_APPLIED,
                    Some("deactivate hooks ran".to_owned()),
                )
            };
            if entry.deactivate.is_none() {
                entry.deactivate = Some(HookStatus::ok(&hooks.deactivate));
            }
            entry
        };
        entry.deactivate.get_or_insert_with(HookStatus::none);
        statuses.push(entry);
    }
    statuses
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '@'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wildcard_matching_is_case_insensitive() {
        assert!(wildcard_match("eDP-*", "eDP-1"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("DELL *", "dell u2720q"));
        assert!(!wildcard_match("DP-*", "eDP-1"));
    }

    #[test]
    fn monitor_profile_maps_match_to_live_outputs() {
        let output: Output = serde_json::from_value(json!({
            "name": "eDP-1", "make": "Dell", "model": "X", "serial": null,
            "physical_size": null, "modes": [], "current_mode": null,
            "is_custom_mode": false, "vrr_supported": false, "vrr_enabled": false,
            "logical": null, "max_bpc": null
        }))
        .unwrap();
        let live = Outputs::from([("eDP-1".to_owned(), output)]);
        let data: MonitorProfileData = serde_json::from_value(json!({
            "match": { "name": "eDP-*" },
            "outputs": { "template": { "off": false, "scale": 2.0 } }
        }))
        .unwrap();
        let desired = monitor_outputs(&data, &live);
        assert_eq!(desired["eDP-1"].scale, Some(2.0));
    }

    #[test]
    fn disabled_profiles_are_empty() {
        assert!(Profiles::default().is_empty());
        let profiles = Profiles {
            audio: Some(AudioProfile {
                profile: "work".to_owned(),
                config: None,
                command: None,
                hooks: ProfileHooks::default(),
            }),
            ..Profiles::default()
        };
        assert!(!profiles.is_empty());
    }

    #[test]
    fn shell_quotes_only_when_needed() {
        assert_eq!(shell_quote("work"), "work");
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }
}
