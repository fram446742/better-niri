//! Read-only views and safe actions for the other Better Desktop apps.
//!
//! `better-settings` owns niri's KDL config, but the companion apps own their
//! own state: output profiles live in `better-monitor`, contexts in
//! `better-session`, scenes in `better-wallpaper`, routing rules in
//! `better-audio`, panels in `better-sidepanels/config.json` and automation in
//! `better-companion`. This module is deliberately thin: it shells out to each
//! app's CLI (or reads its JSON config), parses the JSON the tool already
//! prints, and never rewrites another app's files.
//!
//! Everything goes through [`CommandRunner`] so the whole module can be tested
//! without the tools installed and without a compositor. Mutating actions are
//! explicit (`--apply` on the CLI, a button in the GTK window) and default to
//! the tool's own `--dry-run` preview where one exists.

use std::cell::RefCell;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::arrange::Arrangement;

/// The companion apps this module integrates with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    Monitor,
    Session,
    Wallpaper,
    Audio,
    Sidepanels,
    Companion,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Monitor,
        Tool::Session,
        Tool::Wallpaper,
        Tool::Audio,
        Tool::Sidepanels,
        Tool::Companion,
    ];

    /// Executable name in `$PATH`.
    pub fn program(self) -> &'static str {
        match self {
            Tool::Monitor => "better-monitor",
            Tool::Session => "better-session",
            Tool::Wallpaper => "better-wallpaper",
            Tool::Audio => "better-audio",
            Tool::Sidepanels => "better-sidepanels",
            Tool::Companion => "companionctl",
        }
    }

    /// Environment variable that overrides the executable (useful for tests,
    /// wrappers and non-standard prefixes).
    pub fn override_env(self) -> &'static str {
        match self {
            Tool::Monitor => "BETTER_SETTINGS_BETTER_MONITOR",
            Tool::Session => "BETTER_SETTINGS_BETTER_SESSION",
            Tool::Wallpaper => "BETTER_SETTINGS_BETTER_WALLPAPER",
            Tool::Audio => "BETTER_SETTINGS_BETTER_AUDIO",
            Tool::Sidepanels => "BETTER_SETTINGS_BETTER_SIDEPANELS",
            Tool::Companion => "BETTER_SETTINGS_COMPANIONCTL",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Monitor => "Displays (better-monitor)",
            Tool::Session => "Sessions (better-session)",
            Tool::Wallpaper => "Wallpaper (better-wallpaper)",
            Tool::Audio => "Audio (better-audio)",
            Tool::Sidepanels => "Side panels (better-sidepanels)",
            Tool::Companion => "Automation (better-companion)",
        }
    }
}

/// Resolves the executable for a tool, honouring its override variable.
pub fn program_for(tool: Tool) -> String {
    std::env::var(tool.override_env())
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| tool.program().to_owned())
}

/// One command invocation, including a missing-executable result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub program: String,
    pub args: Vec<String>,
    /// `None` when the process could not be spawned (usually not installed).
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandResult {
    pub fn output(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
        status: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            status: Some(status),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    pub fn missing(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            status: None,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    pub fn installed(&self) -> bool {
        self.status.is_some()
    }

    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// Short description used in status messages.
    pub fn describe(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
            .trim()
            .to_owned()
    }
}

/// Runs one process. Tests use [`ScriptedRunner`]; the app uses
/// [`SystemRunner`].
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> CommandResult;
}

/// Spawns real processes with inherited environment and captured output.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> CommandResult {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        match std::process::Command::new(program).args(&args).output() {
            Ok(output) => CommandResult {
                program: program.to_owned(),
                args: args.clone(),
                status: output.status.code().or(Some(-1)),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(err) => CommandResult {
                program: program.to_owned(),
                args,
                status: None,
                stdout: String::new(),
                stderr: err.to_string(),
            },
        }
    }
}

/// In-memory runner for tests. Unmatched commands look like a missing tool, so
/// "not installed" degradation is the default.
#[derive(Default)]
pub struct ScriptedRunner {
    rules: RefCell<Vec<(String, Vec<String>, CommandResult)>>,
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl ScriptedRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an exact `program` + `args` response.
    pub fn on(self, program: &str, args: &[&str], result: CommandResult) -> Self {
        self.rules.borrow_mut().push((
            program.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            result,
        ));
        self
    }

    /// Records and returns the commands that were run, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .map(|(program, args)| format!("{} {}", program, args.join(" ")).trim().to_owned())
            .collect()
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, program: &str, args: &[&str]) -> CommandResult {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        self.calls
            .borrow_mut()
            .push((program.to_owned(), args.clone()));
        for (rule_program, rule_args, result) in self.rules.borrow().iter() {
            if rule_program == program && *rule_args == args {
                return result.clone();
            }
        }
        CommandResult::missing(program, args)
    }
}

/// A failed integration call. `tool` is the app that failed; missing tools are
/// reported as such so the GUI can degrade instead of failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationError {
    pub tool: &'static str,
    pub message: String,
}

impl IntegrationError {
    fn new(tool: Tool, message: impl Into<String>) -> Self {
        Self {
            tool: tool.program(),
            message: message.into(),
        }
    }

    fn missing(tool: Tool) -> Self {
        Self::new(
            tool,
            format!(
                "{} is not installed; install it or set {}",
                tool.program(),
                tool.override_env()
            ),
        )
    }

    pub fn is_missing(&self) -> bool {
        self.message.contains("is not installed")
    }
}

impl fmt::Display for IntegrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.tool, self.message)
    }
}

impl std::error::Error for IntegrationError {}

fn run_tool(runner: &dyn CommandRunner, tool: Tool, args: &[&str]) -> CommandResult {
    let program = program_for(tool);
    runner.run(&program, args)
}

fn ensure_success(result: &CommandResult, tool: Tool) -> Result<(), IntegrationError> {
    if !result.installed() {
        return Err(IntegrationError::missing(tool));
    }
    if !result.success() {
        let detail = detail_of(result);
        let status = result
            .status
            .map(|code| code.to_string())
            .unwrap_or_default();
        return Err(IntegrationError::new(
            tool,
            format!("`{}` failed (exit {status}): {detail}", result.describe()),
        ));
    }
    Ok(())
}

/// The most useful text a failed command produced.
fn detail_of(result: &CommandResult) -> String {
    if result.stderr.trim().is_empty() {
        result.stdout.trim().to_owned()
    } else {
        result.stderr.trim().to_owned()
    }
}

fn tool_json(
    runner: &dyn CommandRunner,
    tool: Tool,
    args: &[&str],
) -> Result<Value, IntegrationError> {
    let result = run_tool(runner, tool, args);
    ensure_success(&result, tool)?;
    serde_json::from_str(&result.stdout).map_err(|err| {
        IntegrationError::new(
            tool,
            format!("cannot parse `{}` output as JSON: {err}", result.describe()),
        )
    })
}

fn as_array<'a>(
    value: &'a Value,
    tool: Tool,
    what: &str,
) -> Result<&'a Vec<Value>, IntegrationError> {
    value
        .as_array()
        .ok_or_else(|| IntegrationError::new(tool, format!("expected a JSON array for {what}")))
}

fn as_object<'a>(
    value: &'a Value,
    tool: Tool,
    what: &str,
) -> Result<&'a serde_json::Map<String, Value>, IntegrationError> {
    value
        .as_object()
        .ok_or_else(|| IntegrationError::new(tool, format!("expected a JSON object for {what}")))
}

fn string_list(value: &Value, tool: Tool, what: &str) -> Result<Vec<String>, IntegrationError> {
    let mut out = Vec::new();
    for item in as_array(value, tool, what)? {
        let text = item.as_str().ok_or_else(|| {
            IntegrationError::new(tool, format!("{what} contains a non-string entry"))
        })?;
        out.push(text.to_owned());
    }
    Ok(out)
}

/// Unwraps better-monitor's `{"command":..,"ok":..,"data":..}` envelope.
fn monitor_data<'a>(
    value: &'a Value,
    tool: Tool,
    command: &str,
) -> Result<&'a Value, IntegrationError> {
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("command reported failure");
        return Err(IntegrationError::new(tool, message.to_owned()));
    }
    value
        .get("data")
        .ok_or_else(|| IntegrationError::new(tool, format!("`{command}` JSON has no `data` field")))
}

fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn compact_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().map(compact).collect())
        .unwrap_or_default()
}

fn option_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn required_string(value: &Value, key: &str, tool: Tool) -> Result<String, IntegrationError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| IntegrationError::new(tool, format!("JSON is missing `{key}`")))
}

// ---------------------------------------------------------------------------
// Tool discovery
// ---------------------------------------------------------------------------

/// What `--version`/`--help` say about one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolStatus {
    pub tool: Tool,
    pub program: String,
    pub installed: bool,
    pub version: Option<String>,
    /// Short detail when the tool is installed but not usable yet.
    pub detail: Option<String>,
}

/// Probes a tool without failing when it is absent.
pub fn tool_status(runner: &dyn CommandRunner, tool: Tool) -> ToolStatus {
    let program = program_for(tool);
    let version = runner.run(&program, &["--version"]);
    let (installed, version, detail) = if version.installed() {
        let parsed = if version.success() {
            first_line(&version.stdout).or_else(|| first_line(&version.stderr))
        } else {
            None
        };
        let detail = parsed
            .is_none()
            .then(|| "version unavailable (tool has no --version)".to_owned());
        (true, parsed, detail)
    } else {
        let help = runner.run(&program, &["--help"]);
        let installed = help.installed();
        (
            installed,
            None,
            installed.then(|| "installed but the version probe failed".to_owned()),
        )
    };
    ToolStatus {
        tool,
        program,
        installed,
        version,
        detail,
    }
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Probes every companion CLI. Never fails.
pub fn tool_statuses(runner: &dyn CommandRunner) -> Vec<ToolStatus> {
    Tool::ALL
        .iter()
        .map(|tool| tool_status(runner, *tool))
        .collect()
}

// ---------------------------------------------------------------------------
// Compositor: niri
// ---------------------------------------------------------------------------

/// A never-failing summary of the running compositor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompositorStatus {
    /// Version reported by the compositor (`niri msg --json version`).
    pub compositor_version: Option<String>,
    /// Version of the `niri` CLI on `PATH`.
    pub cli_version: Option<String>,
    /// The build advertises Vulkan renderer support.
    pub vulkan: bool,
    /// The build advertises HDR output support.
    pub hdr: bool,
    /// Number of connected outputs (`niri msg --json outputs`).
    pub outputs: usize,
    /// Outputs that advertise HDR capability.
    pub hdr_outputs: usize,
    /// Outputs currently signalling HDR.
    pub hdr_active: usize,
    /// First error encountered while probing, if any.
    pub error: Option<String>,
}

impl CompositorStatus {
    /// Human-readable renderer line. `niri` does not expose the active
    /// renderer over IPC, so this reports the build capability the compositor
    /// advertises (`capabilities.vulkan`).
    pub fn renderer(&self) -> &'static str {
        if self.vulkan {
            "Vulkan capable"
        } else {
            "GLES (Vulkan not advertised)"
        }
    }
}

/// Probes version, capabilities and outputs. Never fails; a missing
/// compositor is reported in [`CompositorStatus::error`].
pub fn compositor_status(runner: &dyn CommandRunner) -> CompositorStatus {
    let program = niri_program();
    let mut status = CompositorStatus {
        compositor_version: None,
        cli_version: None,
        vulkan: false,
        hdr: false,
        outputs: 0,
        hdr_outputs: 0,
        hdr_active: 0,
        error: None,
    };

    let version = runner.run(&program, &["msg", "--json", "version"]);
    if !version.installed() {
        status.error = Some(format!(
            "{program} is not installed; the compositor status is unavailable"
        ));
        return status;
    }
    match version.success() {
        true => match serde_json::from_str::<Value>(&version.stdout) {
            Ok(value) => {
                status.compositor_version = option_string(value.get("compositor"));
                status.cli_version = option_string(value.get("cli"));
            }
            Err(err) => status.error = Some(format!("cannot parse niri version JSON: {err}")),
        },
        false => {
            status.error = Some(detail_of(&version));
        }
    }

    let capabilities = runner.run(&program, &["msg", "--json", "capabilities"]);
    if capabilities.success() {
        if let Ok(value) = serde_json::from_str::<Value>(&capabilities.stdout) {
            status.vulkan = value
                .get("vulkan")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            status.hdr = value.get("hdr").and_then(Value::as_bool).unwrap_or(false);
        }
    } else if status.error.is_none() && capabilities.installed() {
        status.error = Some(detail_of(&capabilities));
    }

    let outputs = runner.run(&program, &["msg", "--json", "outputs"]);
    if outputs.success() {
        if let Ok(value) = serde_json::from_str::<Value>(&outputs.stdout)
            && let Some(outputs) = value.as_object()
        {
            status.outputs = outputs.len();
            for output in outputs.values() {
                if output
                    .get("hdr_supported")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    status.hdr_outputs += 1;
                }
                if output
                    .get("hdr_active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    status.hdr_active += 1;
                }
            }
        }
    } else if status.error.is_none() && outputs.installed() {
        status.error = Some(detail_of(&outputs));
    }

    status
}

/// Reloads niri's config through the compositor (`niri msg action reload-config`).
pub fn reload_config(runner: &dyn CommandRunner) -> Result<(), IntegrationError> {
    let program = niri_program();
    let result = runner.run(&program, &["msg", "action", "reload-config"]);
    ensure_success(&result, Tool::Monitor)
}

// ---------------------------------------------------------------------------
// Displays: better-monitor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisplayProfile {
    pub name: String,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisplayPlan {
    pub command: String,
    pub ok: bool,
    pub dry_run: bool,
    pub status: String,
    pub actions: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub resolved: Vec<(String, String)>,
}

pub fn display_profiles(runner: &dyn CommandRunner) -> Result<Vec<String>, IntegrationError> {
    let value = tool_json(runner, Tool::Monitor, &["--json", "list"])?;
    let data = monitor_data(&value, Tool::Monitor, "list")?;
    let profiles = data
        .get("profiles")
        .ok_or_else(|| IntegrationError::new(Tool::Monitor, "list JSON has no `profiles`"))?;
    string_list(profiles, Tool::Monitor, "profiles")
}

/// Reads one profile and the output names it manages.
pub fn display_profile(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<DisplayProfile, IntegrationError> {
    let value = tool_json(runner, Tool::Monitor, &["--json", "show", name])?;
    let data = monitor_data(&value, Tool::Monitor, "show")?;
    let profile = data
        .get("profile")
        .ok_or_else(|| IntegrationError::new(Tool::Monitor, "show JSON has no `profile`"))?;
    let outputs = profile
        .get("outputs")
        .and_then(Value::as_object)
        .map(|outputs| outputs.keys().cloned().collect())
        .unwrap_or_default();
    Ok(DisplayProfile {
        name: data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(name)
            .to_owned(),
        outputs,
    })
}

fn display_report(value: &Value, command: &str) -> Result<DisplayPlan, IntegrationError> {
    let data = monitor_data(value, Tool::Monitor, command)?;
    let status = required_string(data, "status", Tool::Monitor)?;
    let actions = data
        .get("actions")
        .map(|actions| {
            actions
                .as_array()
                .map(|actions| {
                    actions
                        .iter()
                        .map(|action| {
                            let output =
                                action.get("output").and_then(Value::as_str).unwrap_or("?");
                            let kind = action
                                .get("action")
                                .map(compact)
                                .unwrap_or_else(|| "?".to_owned());
                            format!("{output}: {kind}")
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let resolved = data
        .get("resolved")
        .and_then(Value::as_object)
        .map(|resolved| {
            resolved
                .iter()
                .map(|(from, to)| (from.clone(), compact(to)))
                .collect()
        })
        .unwrap_or_default();
    Ok(DisplayPlan {
        command: command.to_owned(),
        ok: value.get("ok").and_then(Value::as_bool).unwrap_or(true),
        dry_run: data
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status,
        actions,
        warnings: compact_list(data.get("warnings")),
        errors: compact_list(data.get("errors")),
        resolved,
    })
}

/// Runs `better-monitor apply <name> --dry-run` and parses the report.
pub fn display_preview(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<DisplayPlan, IntegrationError> {
    let value = tool_json(
        runner,
        Tool::Monitor,
        &["--json", "apply", name, "--dry-run"],
    )?;
    display_report(&value, "apply")
}

/// Applies a profile. Only called from the explicit GTK button / `--apply`.
pub fn display_apply(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<DisplayPlan, IntegrationError> {
    let value = tool_json(runner, Tool::Monitor, &["--json", "apply", name])?;
    display_report(&value, "apply")
}

pub fn display_restore_preview(
    runner: &dyn CommandRunner,
) -> Result<DisplayPlan, IntegrationError> {
    let value = tool_json(runner, Tool::Monitor, &["--json", "restore", "--dry-run"])?;
    display_report(&value, "restore")
}

pub fn display_restore(runner: &dyn CommandRunner) -> Result<DisplayPlan, IntegrationError> {
    let value = tool_json(runner, Tool::Monitor, &["--json", "restore"])?;
    display_report(&value, "restore")
}

/// Confirms a pending safe-apply and removes the rollback timer.
pub fn display_confirm(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Monitor, &["--json", "confirm"])
}

/// Cancels a pending safe-apply and rolls the previous layout back.
pub fn display_cancel(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Monitor, &["--json", "cancel"])
}

/// Reads the current logical output geometry for the arrangement editor.
pub fn display_positions(runner: &dyn CommandRunner) -> Result<Arrangement, IntegrationError> {
    let value = tool_json(runner, Tool::Monitor, &["--json", "position"])?;
    Arrangement::from_monitor_json(&value).map_err(|err| IntegrationError::new(Tool::Monitor, err))
}

/// One output's HDR state, read from `niri msg --json outputs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HdrOutput {
    pub name: String,
    pub supported: bool,
    pub enabled: bool,
    pub active: bool,
    /// Configured mode: `auto`, `on` or `off`.
    pub mode: String,
}

fn niri_program() -> String {
    std::env::var("BETTER_SETTINGS_NIRI")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "niri".to_owned())
}

fn niri_json(runner: &dyn CommandRunner, args: &[&str]) -> Result<Value, IntegrationError> {
    let program = niri_program();
    let result = runner.run(&program, args);
    ensure_success(&result, Tool::Monitor)?;
    serde_json::from_str(&result.stdout).map_err(|err| {
        IntegrationError::new(
            Tool::Monitor,
            format!("cannot parse `{}` output as JSON: {err}", result.describe()),
        )
    })
}

/// Reads every output's HDR capability and current mode for the Displays section.
pub fn display_hdr_outputs(runner: &dyn CommandRunner) -> Result<Vec<HdrOutput>, IntegrationError> {
    let value = niri_json(runner, &["msg", "--json", "outputs"])?;
    let outputs = value.as_object().ok_or_else(|| {
        IntegrationError::new(Tool::Monitor, "niri outputs JSON is not an object")
    })?;
    let mut result = Vec::new();
    for (name, output) in outputs {
        result.push(HdrOutput {
            name: name.clone(),
            supported: output
                .get("hdr_supported")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            enabled: output
                .get("hdr_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            active: output
                .get("hdr_active")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            mode: output
                .get("hdr_mode")
                .and_then(Value::as_str)
                .unwrap_or("auto")
                .to_owned(),
        });
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

/// Applies a runtime HDR mode through `niri msg output <output> hdr <mode>`.
pub fn display_hdr_set(
    runner: &dyn CommandRunner,
    output: &str,
    mode: &str,
) -> Result<(), IntegrationError> {
    if !matches!(mode, "auto" | "on" | "off") {
        return Err(IntegrationError::new(
            Tool::Monitor,
            format!("invalid HDR mode `{mode}`"),
        ));
    }
    let program = niri_program();
    let result = runner.run(&program, &["msg", "output", output, "hdr", mode]);
    ensure_success(&result, Tool::Monitor)
}

fn display_arrange_plan(
    runner: &dyn CommandRunner,
    plan: &str,
    dry_run: bool,
) -> Result<DisplayPlan, IntegrationError> {
    let mut args = vec!["--json", "arrange", "--plan", plan];
    if dry_run {
        args.push("--dry-run");
    }
    let value = tool_json(runner, Tool::Monitor, &args)?;
    display_report(&value, "arrange")
}

/// Dry-runs an arrangement plan (`better-monitor arrange --plan ... --dry-run`).
pub fn display_arrange_preview(
    runner: &dyn CommandRunner,
    plan: &str,
) -> Result<DisplayPlan, IntegrationError> {
    display_arrange_plan(runner, plan, true)
}

/// Applies an arrangement plan. Only called from an explicit Apply button.
pub fn display_arrange(
    runner: &dyn CommandRunner,
    plan: &str,
) -> Result<DisplayPlan, IntegrationError> {
    display_arrange_plan(runner, plan, false)
}

// ---------------------------------------------------------------------------
// Sessions: better-session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionContext {
    pub name: String,
    pub workspaces: usize,
    pub windows: usize,
    pub outputs: Vec<String>,
    pub commands: usize,
    pub template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionOutcome {
    pub context: String,
    pub ok: bool,
    pub dry_run: bool,
    pub partial: bool,
    pub applied: usize,
    pub skipped: usize,
    pub unmatched: usize,
    pub profiles: usize,
    pub error: Option<String>,
}

pub fn session_contexts(runner: &dyn CommandRunner) -> Result<Vec<String>, IntegrationError> {
    let value = tool_json(runner, Tool::Session, &["list"])?;
    string_list(&value, Tool::Session, "contexts")
}

pub fn session_context(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<SessionContext, IntegrationError> {
    let value = tool_json(runner, Tool::Session, &["show", name])?;
    Ok(session_context_value(name, &value))
}

fn session_context_value(name: &str, value: &Value) -> SessionContext {
    let count = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    };
    SessionContext {
        name: name.to_owned(),
        workspaces: count("workspaces"),
        windows: count("windows"),
        outputs: value
            .get("outputs")
            .and_then(Value::as_object)
            .map(|outputs| outputs.keys().cloned().collect())
            .unwrap_or_default(),
        commands: value
            .get("command_map")
            .and_then(Value::as_object)
            .map(|commands| commands.len())
            .unwrap_or(0),
        template: option_string(value.get("template")),
    }
}

/// Captures the current session as a context (`better-session capture <name>`).
///
/// This is a mutation with no preview; callers must ask the user first.
pub fn session_capture(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<SessionContext, IntegrationError> {
    let value = tool_json(runner, Tool::Session, &["capture", name])?;
    Ok(session_context_value(name, &value))
}

/// One snapshot in `better-session history list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionHistoryEntry {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub trigger: String,
    pub windows: usize,
    pub workspaces: usize,
}

pub fn session_history(
    runner: &dyn CommandRunner,
) -> Result<Vec<SessionHistoryEntry>, IntegrationError> {
    let value = tool_json(runner, Tool::Session, &["history", "list"])?;
    let entries = as_array(&value, Tool::Session, "history")?;
    entries
        .iter()
        .map(|entry| {
            Ok(SessionHistoryEntry {
                id: required_string(entry, "id", Tool::Session)?,
                name: entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                created_at: entry.get("created_at").and_then(Value::as_u64).unwrap_or(0),
                trigger: entry
                    .get("trigger")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                windows: entry.get("windows").and_then(Value::as_u64).unwrap_or(0) as usize,
                workspaces: entry.get("workspaces").and_then(Value::as_u64).unwrap_or(0) as usize,
            })
        })
        .collect()
}

fn session_outcome(value: &Value) -> Result<SessionOutcome, IntegrationError> {
    let count = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    };
    Ok(SessionOutcome {
        context: required_string(value, "context", Tool::Session)?,
        ok: value.get("ok").and_then(Value::as_bool).unwrap_or(false),
        dry_run: value
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        partial: value
            .get("partial")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        applied: count("applied"),
        skipped: count("skipped"),
        unmatched: count("unmatched"),
        profiles: count("profiles"),
        error: option_string(value.get("error")),
    })
}

/// Plans a restore without changing anything (`better-session explain`).
pub fn session_explain(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<SessionOutcome, IntegrationError> {
    let value = tool_json(runner, Tool::Session, &["explain", name])?;
    session_outcome(&value)
}

/// Restores a context. Only called from an explicit action.
pub fn session_activate(
    runner: &dyn CommandRunner,
    name: &str,
) -> Result<SessionOutcome, IntegrationError> {
    let value = tool_json(runner, Tool::Session, &["restore", name])?;
    session_outcome(&value)
}

// ---------------------------------------------------------------------------
// Wallpaper: better-wallpaper
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WallpaperScene {
    pub name: String,
    pub default: Option<String>,
    pub workspaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WallpaperStatus {
    pub active: Option<String>,
    pub scenes: Vec<WallpaperScene>,
    pub backend: String,
    pub native_available: bool,
    pub native_error: Option<String>,
    pub template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WallpaperPlan {
    pub dry_run: bool,
    pub backend: String,
    pub mode: Option<String>,
    pub images: Vec<(String, Option<String>)>,
    pub image: Option<String>,
    pub template: Option<String>,
}

pub fn wallpaper_status(runner: &dyn CommandRunner) -> Result<WallpaperStatus, IntegrationError> {
    let value = tool_json(runner, Tool::Wallpaper, &["status"])?;
    let scenes = value
        .get("scenes")
        .and_then(Value::as_object)
        .map(|scenes| {
            scenes
                .iter()
                .map(|(name, scene)| WallpaperScene {
                    name: name.clone(),
                    default: option_string(scene.get("default")),
                    workspaces: scene
                        .get("wallpapers")
                        .and_then(Value::as_object)
                        .map(|workspaces| workspaces.keys().cloned().collect())
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let backend = value.get("backend");
    Ok(WallpaperStatus {
        active: option_string(value.get("active")),
        scenes,
        backend: backend
            .and_then(|backend| backend.get("requested"))
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .to_owned(),
        native_available: backend
            .and_then(|backend| backend.get("native_available"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        native_error: backend.and_then(|backend| option_string(backend.get("native_error"))),
        template: option_string(value.get("template")),
    })
}

pub fn wallpaper_scenes(runner: &dyn CommandRunner) -> Result<Vec<String>, IntegrationError> {
    let value = tool_json(runner, Tool::Wallpaper, &["scene", "list"])?;
    string_list(&value, Tool::Wallpaper, "scenes")
}

pub fn wallpaper_use(runner: &dyn CommandRunner, name: &str) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Wallpaper, &["scene", "use", name])
}

fn wallpaper_plan(value: Value) -> Result<WallpaperPlan, IntegrationError> {
    let images = value
        .get("images")
        .and_then(Value::as_object)
        .map(|images| {
            images
                .iter()
                .map(|(output, image)| (output.clone(), option_string(Some(image))))
                .collect()
        })
        .unwrap_or_default();
    Ok(WallpaperPlan {
        dry_run: value
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        backend: value
            .get("backend")
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .to_owned(),
        mode: option_string(value.get("mode")),
        images,
        image: option_string(value.get("image")),
        template: option_string(value.get("template")),
    })
}

pub fn wallpaper_preview(runner: &dyn CommandRunner) -> Result<WallpaperPlan, IntegrationError> {
    let value = tool_json(runner, Tool::Wallpaper, &["apply", "--dry-run"])?;
    wallpaper_plan(value)
}

pub fn wallpaper_apply(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Wallpaper, &["apply"])
}

/// Starts `better-wallpaper apply` without waiting for it.
///
/// The native backend keeps running to stay on screen, so the GTK window must
/// not block on it. The command backend also detaches cleanly; its output is
/// not needed to refresh the status view.
pub fn wallpaper_apply_detached() -> Result<String, IntegrationError> {
    let program = program_for(Tool::Wallpaper);
    let child = std::process::Command::new(&program)
        .arg("apply")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| {
            IntegrationError::new(Tool::Wallpaper, format!("cannot run {program}: {err}"))
        })?;
    Ok(format!(
        "started {program} apply (pid {}); use better-wallpaper watch to follow workspaces",
        child.id()
    ))
}

// ---------------------------------------------------------------------------
// Audio: better-audio
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AudioStatus {
    pub requested_backend: String,
    pub active_backend: Option<String>,
    pub description: Option<String>,
    pub degraded: bool,
    pub reason: Option<String>,
    pub config: Option<String>,
    pub current_default: Option<String>,
    pub configured_default: Option<String>,
    pub rules: usize,
    pub restore_count: usize,
    pub attempts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioSink {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub volume: Option<f64>,
    pub mute: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioSource {
    pub id: u32,
    pub name: String,
    pub description: String,
    pub volume: Option<f64>,
    pub mute: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioSinks {
    pub backend: String,
    pub default: Option<String>,
    pub sinks: Vec<AudioSink>,
}

/// The active backend plus every sink and source, with current volume/mute.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioDevices {
    pub backend: String,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
    pub sinks: Vec<AudioSink>,
    pub sources: Vec<AudioSource>,
}

/// Which device list a mixer change addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioDeviceKind {
    Sink,
    Source,
}

impl AudioDeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sink => "sink",
            Self::Source => "source",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioRule {
    pub app: String,
    pub sink: String,
    pub priority: i32,
    pub volume: Option<f64>,
    pub mute: Option<bool>,
}

pub fn audio_status(runner: &dyn CommandRunner) -> Result<AudioStatus, IntegrationError> {
    let value = tool_json(runner, Tool::Audio, &["status"])?;
    Ok(AudioStatus {
        requested_backend: value
            .get("requested_backend")
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .to_owned(),
        active_backend: option_string(value.get("active_backend")),
        description: option_string(value.get("active_backend_description")),
        degraded: value
            .get("degraded")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        reason: option_string(value.get("reason")),
        config: option_string(value.get("config")),
        current_default: option_string(value.get("current_default")),
        configured_default: option_string(value.get("configured_default")),
        rules: value
            .get("rules")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
        restore_count: value
            .get("restore_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize,
        attempts: value
            .get("attempts")
            .and_then(Value::as_array)
            .map(|attempts| {
                attempts
                    .iter()
                    .map(|attempt| {
                        format!(
                            "{}: {}",
                            attempt
                                .get("backend")
                                .and_then(Value::as_str)
                                .unwrap_or("?"),
                            attempt.get("error").map(compact).unwrap_or_default()
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_audio_sinks(value: &Value) -> Result<Vec<AudioSink>, IntegrationError> {
    let sinks = value
        .get("sinks")
        .map(|sinks| {
            as_array(sinks, Tool::Audio, "sinks").map(|sinks| {
                sinks
                    .iter()
                    .map(|sink| AudioSink {
                        id: sink.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                        name: sink
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        description: sink
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        volume: sink.get("volume").and_then(Value::as_f64),
                        mute: sink.get("mute").and_then(Value::as_bool),
                    })
                    .collect()
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(sinks)
}

fn parse_audio_sources(value: &Value) -> Result<Vec<AudioSource>, IntegrationError> {
    let sources = value
        .get("sources")
        .map(|sources| {
            as_array(sources, Tool::Audio, "sources").map(|sources| {
                sources
                    .iter()
                    .map(|source| AudioSource {
                        id: source.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                        name: source
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        description: source
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        volume: source.get("volume").and_then(Value::as_f64),
                        mute: source.get("mute").and_then(Value::as_bool),
                    })
                    .collect()
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(sources)
}

pub fn audio_sinks(runner: &dyn CommandRunner) -> Result<AudioSinks, IntegrationError> {
    let value = tool_json(runner, Tool::Audio, &["sinks"])?;
    Ok(AudioSinks {
        backend: value
            .get("backend")
            .and_then(Value::as_str)
            .unwrap_or("auto")
            .to_owned(),
        default: option_string(value.get("default")),
        sinks: parse_audio_sinks(&value)?,
    })
}

/// Reads both device lists and their defaults. This is what the mixer UI uses.
pub fn audio_devices(runner: &dyn CommandRunner) -> Result<AudioDevices, IntegrationError> {
    let sinks = tool_json(runner, Tool::Audio, &["sinks"])?;
    let sources = tool_json(runner, Tool::Audio, &["sources"])?;
    Ok(AudioDevices {
        backend: sinks
            .get("backend")
            .and_then(Value::as_str)
            .or_else(|| sources.get("backend").and_then(Value::as_str))
            .unwrap_or("auto")
            .to_owned(),
        default_sink: option_string(sinks.get("default")),
        default_source: option_string(sources.get("default")),
        sinks: parse_audio_sinks(&sinks)?,
        sources: parse_audio_sources(&sources)?,
    })
}

fn volume_argument(volume: f64) -> String {
    let rounded = (volume.clamp(0.0, 1.0) * 1000.0).round() / 1000.0;
    format!("{rounded}")
}

/// Sets a sink/source volume. With `dry_run`, better-audio's `--dry-run`
/// reports the change without writing.
pub fn audio_set_volume(
    runner: &dyn CommandRunner,
    kind: AudioDeviceKind,
    name: &str,
    volume: f64,
    dry_run: bool,
) -> Result<Value, IntegrationError> {
    let level = volume_argument(volume);
    let mut args = vec!["volume", kind.as_str(), name, level.as_str()];
    if dry_run {
        args.push("--dry-run");
    }
    tool_json(runner, Tool::Audio, &args)
}

/// Sets a sink/source mute flag, with the same preview rule.
pub fn audio_set_mute(
    runner: &dyn CommandRunner,
    kind: AudioDeviceKind,
    name: &str,
    mute: bool,
    dry_run: bool,
) -> Result<Value, IntegrationError> {
    let state = if mute { "on" } else { "off" };
    let mut args = vec!["mute", kind.as_str(), name, state];
    if dry_run {
        args.push("--dry-run");
    }
    tool_json(runner, Tool::Audio, &args)
}

pub fn audio_rules(runner: &dyn CommandRunner) -> Result<Vec<AudioRule>, IntegrationError> {
    let value = tool_json(runner, Tool::Audio, &["rules"])?;
    as_array(&value, Tool::Audio, "rules")?
        .iter()
        .map(|rule| {
            Ok(AudioRule {
                app: rule
                    .get("app")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                sink: rule
                    .get("sink")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                priority: rule.get("priority").and_then(Value::as_i64).unwrap_or(0) as i32,
                volume: rule.get("volume").and_then(Value::as_f64),
                mute: rule.get("mute").and_then(Value::as_bool),
            })
        })
        .collect()
}

pub fn audio_set_default(
    runner: &dyn CommandRunner,
    sink: &str,
) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Audio, &["default", sink])
}

pub fn audio_apply_preview(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Audio, &["apply", "--dry-run"])
}

pub fn audio_apply(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Audio, &["apply"])
}

pub fn audio_restore_preview(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Audio, &["restore", "--dry-run"])
}

pub fn audio_restore(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    tool_json(runner, Tool::Audio, &["restore"])
}

// ---------------------------------------------------------------------------
// Side panels: better-sidepanels/config.json
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SidepanelSummary {
    pub output: Option<String>,
    pub side: String,
    pub width: u32,
    pub title: String,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SidepanelsConfig {
    pub path: String,
    pub exists: bool,
    pub version: u32,
    pub outputs: Vec<String>,
    /// Named layouts/profiles the daemon accepts with `--profile`/`--layout`.
    pub layouts: Vec<String>,
    pub panels: Vec<SidepanelSummary>,
}

/// Resolves the sidepanels config path exactly like the app does.
pub fn sidepanels_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_owned();
    }
    if let Some(path) = std::env::var_os("BETTER_SIDEPANELS_CONFIG") {
        return PathBuf::from(path);
    }
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(base).join("better-sidepanels/config.json");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config/better-sidepanels/config.json")
}

/// Reads and summarizes the JSON config without rewriting it.
pub fn read_sidepanels(path: &Path) -> Result<SidepanelsConfig, IntegrationError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SidepanelsConfig {
                path: path.display().to_string(),
                exists: false,
                version: 2,
                outputs: Vec::new(),
                layouts: Vec::new(),
                panels: Vec::new(),
            });
        }
        Err(err) => {
            return Err(IntegrationError::new(
                Tool::Sidepanels,
                format!("cannot read {}: {err}", path.display()),
            ));
        }
    };
    let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
        IntegrationError::new(
            Tool::Sidepanels,
            format!("cannot parse {}: {err}", path.display()),
        )
    })?;
    parse_sidepanels(path, &value)
}

/// Parses a v2 (`panels`) or v1 (bare panel) config.
pub fn parse_sidepanels(path: &Path, value: &Value) -> Result<SidepanelsConfig, IntegrationError> {
    let layouts = ["layouts", "profiles"]
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_object))
        .map(|layouts| layouts.keys().cloned().collect())
        .unwrap_or_default();
    let (version, outputs, panels) = if value.get("panels").is_some() {
        let panels = match value.get("panels") {
            Some(panels) => as_array(panels, Tool::Sidepanels, "panels")?.clone(),
            None => Vec::new(),
        };
        (
            value.get("version").and_then(Value::as_u64).unwrap_or(2) as u32,
            value
                .get("outputs")
                .and_then(Value::as_object)
                .map(|outputs| outputs.keys().cloned().collect())
                .unwrap_or_default(),
            panels,
        )
    } else {
        (1, Vec::new(), vec![value.clone()])
    };
    let panels = panels
        .iter()
        .map(|panel| {
            as_object(panel, Tool::Sidepanels, "panel")?;
            Ok(SidepanelSummary {
                output: panel
                    .get("output")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                side: panel
                    .get("side")
                    .and_then(Value::as_str)
                    .unwrap_or("left")
                    .to_owned(),
                width: panel.get("width").and_then(Value::as_u64).unwrap_or(260) as u32,
                title: panel
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                rows: panel
                    .get("rows")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
            })
        })
        .collect::<Result<Vec<_>, IntegrationError>>()?;
    Ok(SidepanelsConfig {
        path: path.display().to_string(),
        exists: true,
        version,
        outputs,
        layouts,
        panels,
    })
}

/// Runs `better-sidepanels --check --profile <name>` to print what a named
/// layout would load without starting the daemon.
pub fn check_sidepanels_layout(
    runner: &dyn CommandRunner,
    path: &Path,
    layout: &str,
) -> Result<String, IntegrationError> {
    let path_text = path.display().to_string();
    let result = run_tool(
        runner,
        Tool::Sidepanels,
        &["--check", "--profile", layout, "--config", &path_text],
    );
    ensure_success(&result, Tool::Sidepanels)?;
    Ok(if result.stdout.trim().is_empty() {
        format!("{path_text}: {layout} is valid")
    } else {
        result.stdout.trim().to_owned()
    })
}

/// Runs the app's own validator (`better-sidepanels --validate --config`).
pub fn validate_sidepanels(
    runner: &dyn CommandRunner,
    path: &Path,
) -> Result<String, IntegrationError> {
    let path_text = path.display().to_string();
    let result = run_tool(
        runner,
        Tool::Sidepanels,
        &["--validate", "--config", &path_text],
    );
    ensure_success(&result, Tool::Sidepanels)?;
    Ok(if result.stdout.trim().is_empty() {
        format!("{path_text}: ok")
    } else {
        result.stdout.trim().to_owned()
    })
}

/// The command used to open the config in the user's editor. `$VISUAL` and
/// `$EDITOR` win; otherwise the desktop opener.
pub fn sidepanels_open_command(path: &Path) -> (String, Vec<String>) {
    let path_text = path.display().to_string();
    if let Some(editor) = std::env::var_os("VISUAL").or_else(|| std::env::var_os("EDITOR")) {
        let editor = editor.to_string_lossy().into_owned();
        let mut parts = editor.split_whitespace();
        if let Some(program) = parts.next() {
            let mut args: Vec<String> = parts.map(str::to_owned).collect();
            args.push(path_text);
            return (program.to_owned(), args);
        }
    }
    ("xdg-open".to_owned(), vec![path_text])
}

/// Opens the config for editing; never rewrites the file.
pub fn open_sidepanels(path: &Path) -> Result<String, IntegrationError> {
    let (program, args) = sidepanels_open_command(path);
    let result = std::process::Command::new(&program)
        .args(&args)
        .status()
        .map_err(|err| {
            IntegrationError::new(Tool::Sidepanels, format!("cannot run {program}: {err}"))
        })?;
    if !result.success() {
        return Err(IntegrationError::new(
            Tool::Sidepanels,
            format!("{program} exited with {result}"),
        ));
    }
    Ok(format!("opened {}", path.display()))
}

// ---------------------------------------------------------------------------
// Portal and Noctalia plugin source
// ---------------------------------------------------------------------------

/// Where better-portal publishes its frontend selection.
pub fn portal_config_path(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_owned();
    }
    if let Some(path) = std::env::var_os("BETTER_SETTINGS_PORTALS_CONF") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".config")
        });
    base.join("xdg-desktop-portal/portals.conf")
}

/// Portal backend/unit status. Read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortalStatus {
    pub unit: String,
    pub active: Option<String>,
    pub enabled: Option<String>,
    pub config: String,
    pub config_exists: bool,
    /// The frontend config names `org.freedesktop.impl.portal.desktop.better`.
    pub selected: bool,
}

pub fn portal_status(runner: &dyn CommandRunner, config: &Path) -> PortalStatus {
    let unit = "better-portal".to_owned();
    let probe = |verb: &str| {
        let result = runner.run("systemctl", &["--user", verb, &unit]);
        if result.installed() {
            first_line(&result.stdout)
        } else {
            None
        }
    };
    let config_text = std::fs::read_to_string(config).ok();
    let active = probe("is-active");
    let enabled = probe("is-enabled");
    PortalStatus {
        unit,
        active,
        enabled,
        config: config.display().to_string(),
        config_exists: config_text.is_some(),
        selected: config_text
            .as_deref()
            .map(|text| text.contains("org.freedesktop.impl.portal.desktop.better"))
            .unwrap_or(false),
    }
}

/// Resolves a Noctalia plugin source directory.
pub fn noctalia_source(explicit: Option<&Path>) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_owned();
    }
    if let Some(path) = std::env::var_os("BETTER_SETTINGS_NOCTALIA_PLUGINS") {
        return PathBuf::from(path);
    }
    PathBuf::from("/usr/share/better-desktop/noctalia-plugins")
}

/// Read-only status of a Noctalia plugin source tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoctaliaSource {
    pub path: String,
    pub exists: bool,
    pub plugins: Vec<String>,
    pub verify_script: bool,
}

pub fn read_noctalia_source(path: &Path) -> NoctaliaSource {
    let mut plugins = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir()
                && entry_path.join("plugin.toml").is_file()
                && let Some(name) = entry_path.file_name()
            {
                plugins.push(name.to_string_lossy().into_owned());
            }
        }
    }
    plugins.sort();
    NoctaliaSource {
        path: path.display().to_string(),
        exists: path.is_dir(),
        verify_script: path.join("verify.sh").is_file(),
        plugins,
    }
}

// ---------------------------------------------------------------------------
// Automation: better-companion / companionctl
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompanionAction {
    pub name: String,
    pub description: String,
    pub owner: String,
    pub available: bool,
    pub params: usize,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompanionNamespace {
    pub namespace: String,
    pub actions: Vec<CompanionAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompanionDescription {
    pub protocol: String,
    pub version: u64,
    pub namespaces: Vec<CompanionNamespace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompanionStatus {
    pub version: String,
    pub pid: u64,
    pub uptime_secs: u64,
    pub socket: String,
    pub contexts: Vec<String>,
}

fn companion_namespaces(
    value: &Value,
    field: &str,
    tool: Tool,
) -> Result<Vec<CompanionNamespace>, IntegrationError> {
    let map = value
        .get(field)
        .ok_or_else(|| IntegrationError::new(tool, format!("JSON has no `{field}`")))?
        .as_object()
        .ok_or_else(|| IntegrationError::new(tool, format!("`{field}` is not an object")))?;
    map.iter()
        .map(|(namespace, actions)| {
            let actions = as_array(actions, tool, "actions")?
                .iter()
                .map(|action| {
                    Ok(CompanionAction {
                        name: action
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        description: action
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        owner: action
                            .get("owner")
                            .and_then(Value::as_str)
                            .unwrap_or(namespace)
                            .to_owned(),
                        available: action
                            .get("available")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        params: action
                            .get("params")
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0),
                        permissions: compact_list(action.get("permissions")),
                    })
                })
                .collect::<Result<Vec<_>, IntegrationError>>()?;
            Ok(CompanionNamespace {
                namespace: namespace.clone(),
                actions,
            })
        })
        .collect()
}

pub fn companion_describe(
    runner: &dyn CommandRunner,
) -> Result<CompanionDescription, IntegrationError> {
    let value = tool_json(runner, Tool::Companion, &["describe"])?;
    Ok(CompanionDescription {
        protocol: required_string(&value, "protocol", Tool::Companion)?,
        version: value.get("version").and_then(Value::as_u64).unwrap_or(0),
        namespaces: companion_namespaces(&value, "actions", Tool::Companion)?,
    })
}

pub fn companion_status(runner: &dyn CommandRunner) -> Result<CompanionStatus, IntegrationError> {
    let value = tool_json(runner, Tool::Companion, &["status"])?;
    Ok(CompanionStatus {
        version: required_string(&value, "version", Tool::Companion)?,
        pid: value.get("pid").and_then(Value::as_u64).unwrap_or(0),
        uptime_secs: value
            .get("uptime_secs")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        socket: required_string(&value, "socket", Tool::Companion)?,
        contexts: value
            .get("contexts")
            .map(|contexts| string_list(contexts, Tool::Companion, "contexts"))
            .transpose()?
            .unwrap_or_default(),
    })
}

pub fn companion_namespace_list(
    runner: &dyn CommandRunner,
) -> Result<Vec<CompanionNamespace>, IntegrationError> {
    let value = tool_json(runner, Tool::Companion, &["list"])?;
    companion_namespaces(&value, "contexts", Tool::Companion)
}

/// Calls an action. `dry_run` adds companionctl's `--dry-run`; mutating calls
/// only happen when the caller explicitly asks for them.
pub fn companion_run(
    runner: &dyn CommandRunner,
    namespace: &str,
    action: &str,
    params: Option<&str>,
    dry_run: bool,
) -> Result<Value, IntegrationError> {
    let mut args: Vec<&str> = Vec::new();
    if dry_run {
        args.push("--dry-run");
    }
    args.extend(["call", namespace, action]);
    if let Some(params) = params {
        args.push(params);
    }
    let value = tool_json(runner, Tool::Companion, &args)?;
    if value.get("kind").and_then(Value::as_str) == Some("error") {
        return Err(IntegrationError::new(
            Tool::Companion,
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("daemon returned an error"),
        ));
    }
    Ok(value)
}

/// One completed rule/automation run from `companionctl history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompanionHistoryEntry {
    pub timestamp_ms: u64,
    pub source: String,
    pub name: String,
    pub steps: usize,
    pub ok: bool,
    pub error: Option<String>,
}

/// Recent rule/automation runs, newest first.
pub fn companion_history(
    runner: &dyn CommandRunner,
    limit: u32,
) -> Result<Vec<CompanionHistoryEntry>, IntegrationError> {
    let limit = limit.to_string();
    let value = tool_json(runner, Tool::Companion, &["history", &limit])?;
    let records = as_array(
        value.get("records").ok_or_else(|| {
            IntegrationError::new(Tool::Companion, "history JSON has no `records`")
        })?,
        Tool::Companion,
        "records",
    )?;
    records
        .iter()
        .map(|record| {
            let result = record.get("result");
            Ok(CompanionHistoryEntry {
                timestamp_ms: record
                    .get("timestamp_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                source: record
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                name: record
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                steps: record
                    .get("steps")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
                ok: result
                    .and_then(|result| result.get("ok"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                error: result.and_then(|result| option_string(result.get("error"))),
            })
        })
        .collect()
}

/// `companionctl explain <target>` for `rules`, `windows` or `adapters`.
/// Explanation is read-only; it never executes steps.
pub fn companion_explain(
    runner: &dyn CommandRunner,
    target: &str,
) -> Result<Value, IntegrationError> {
    if !matches!(target, "rules" | "windows" | "adapters") {
        return Err(IntegrationError::new(
            Tool::Companion,
            format!("unknown explain target `{target}`"),
        ));
    }
    tool_json(runner, Tool::Companion, &["explain", target])
}

/// Runs matching rules. The caller must confirm first; `dry_run` only explains.
pub fn companion_rules_apply(
    runner: &dyn CommandRunner,
    dry_run: bool,
) -> Result<Value, IntegrationError> {
    if dry_run {
        return companion_explain(runner, "rules");
    }
    tool_json(runner, Tool::Companion, &["call", "rules", "run"])
}

/// Idle or rotation policy state from `companionctl call <ns> status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompanionAdapterStatus {
    pub namespace: String,
    pub enabled: bool,
    pub lines: Vec<String>,
    pub value: Value,
}

fn companion_adapter_status(
    runner: &dyn CommandRunner,
    namespace: &str,
) -> Result<CompanionAdapterStatus, IntegrationError> {
    let value = companion_run(runner, namespace, "status", None, false)?;
    let enabled = value
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lines = if namespace == "idle" {
        idle_lines(&value)
    } else {
        rotate_lines(&value)
    };
    Ok(CompanionAdapterStatus {
        namespace: namespace.to_owned(),
        enabled,
        lines,
        value,
    })
}

fn idle_lines(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(idle) = value.get("idle_secs").and_then(Value::as_u64) {
        lines.push(format!("idle for {idle}s"));
    }
    if let Some(source) = value.get("source").and_then(Value::as_str) {
        lines.push(format!("idle source: {source}"));
    }
    if let Some(inhibited) = value.get("inhibited").and_then(Value::as_bool) {
        lines.push(format!(
            "inhibited: {}",
            if inhibited { "yes" } else { "no" }
        ));
    }
    if let Some(due) = value.get("due").and_then(Value::as_array) {
        lines.push(format!("{} step(s) due", due.len()));
    }
    if let Some(next) = value.get("next").and_then(Value::as_object)
        && let Some(after) = next.get("after_secs").and_then(Value::as_u64)
    {
        lines.push(format!("next timeout at {after}s idle"));
    }
    if let Some(skipped) = value.get("skipped").and_then(Value::as_str) {
        lines.push(format!("skipped: {skipped}"));
    }
    lines
}

fn rotate_lines(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_str) {
        lines.push(format!("output: {output}"));
    }
    if let Some(source) = value.get("source") {
        lines.push(format!("source: {}", compact(source)));
    }
    if let Some(orientation) = value.get("last_orientation").and_then(Value::as_str) {
        lines.push(format!("last orientation: {orientation}"));
    }
    if let Some(transform) = value.get("last_transform").and_then(Value::as_str) {
        lines.push(format!("last transform: {transform}"));
    }
    if let Some(locked) = value.get("locked").and_then(Value::as_bool) {
        lines.push(format!("locked: {}", if locked { "yes" } else { "no" }));
    }
    if let Some(skipped) = value.get("skipped").and_then(Value::as_str) {
        lines.push(format!("skipped: {skipped}"));
    }
    lines
}

pub fn idle_status(runner: &dyn CommandRunner) -> Result<CompanionAdapterStatus, IntegrationError> {
    companion_adapter_status(runner, "idle")
}

pub fn rotate_status(
    runner: &dyn CommandRunner,
) -> Result<CompanionAdapterStatus, IntegrationError> {
    companion_adapter_status(runner, "rotate")
}

/// Enables or disables the idle or rotation policy (`idle.enable`,
/// `rotate.disable`, ...). Mutating; callers confirm first.
pub fn companion_adapter_set_enabled(
    runner: &dyn CommandRunner,
    namespace: &str,
    enabled: bool,
    dry_run: bool,
) -> Result<Value, IntegrationError> {
    if !matches!(namespace, "idle" | "rotate") {
        return Err(IntegrationError::new(
            Tool::Companion,
            format!("unknown adapter namespace `{namespace}`"),
        ));
    }
    let action = if enabled { "enable" } else { "disable" };
    companion_run(runner, namespace, action, None, dry_run)
}

/// Window marks tracked by the companion (`marks.list`).
pub fn companion_marks(runner: &dyn CommandRunner) -> Result<Value, IntegrationError> {
    companion_run(runner, "marks", "list", None, false)
}

/// A validated automatic-rotation or idle action pair for the Automation page.
pub fn companion_adapters(
    runner: &dyn CommandRunner,
) -> [Result<CompanionAdapterStatus, IntegrationError>; 2] {
    [idle_status(runner), rotate_status(runner)]
}

// ---------------------------------------------------------------------------
// Combined views used by the GTK sections and the CLI
// ---------------------------------------------------------------------------

/// A display-ready section: a status line, detail lines and an optional error.
/// Building a view never fails because a tool is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationView {
    pub status: String,
    pub lines: Vec<String>,
    pub error: Option<String>,
}

impl IntegrationView {
    fn ready(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            lines: Vec::new(),
            error: None,
        }
    }

    fn missing(tool: Tool) -> Self {
        Self {
            status: format!("not installed ({})", program_for(tool)),
            lines: Vec::new(),
            error: None,
        }
    }

    fn with_lines(mut self, lines: Vec<String>) -> Self {
        self.lines = lines;
        self
    }
}

fn guard<T>(
    tool: Tool,
    result: Result<T, IntegrationError>,
    describe: impl FnOnce(T) -> IntegrationView,
) -> IntegrationView {
    match result {
        Ok(value) => describe(value),
        Err(err) if err.is_missing() => IntegrationView::missing(tool),
        Err(err) => IntegrationView {
            status: "error".to_owned(),
            lines: Vec::new(),
            error: Some(err.to_string()),
        },
    }
}

/// Tools section: installed CLIs, versions and status notes.
pub fn tools_view(runner: &dyn CommandRunner) -> IntegrationView {
    let statuses = tool_statuses(runner);
    let installed = statuses.iter().filter(|status| status.installed).count();
    let lines = statuses
        .iter()
        .map(|status| {
            if status.installed {
                match (&status.version, &status.detail) {
                    (Some(version), _) => {
                        format!("{}: {} ({version})", status.tool.program(), status.program)
                    }
                    (None, Some(detail)) => {
                        format!("{}: {} ({detail})", status.tool.program(), status.program)
                    }
                    (None, None) => format!(
                        "{}: {} (version unknown)",
                        status.tool.program(),
                        status.program
                    ),
                }
            } else {
                format!("{}: not installed", status.tool.program())
            }
        })
        .collect();
    IntegrationView::ready(format!(
        "{installed}/{} companion CLIs installed",
        Tool::ALL.len()
    ))
    .with_lines(lines)
}

pub fn displays_view(runner: &dyn CommandRunner) -> IntegrationView {
    if !tool_status(runner, Tool::Monitor).installed {
        return IntegrationView::missing(Tool::Monitor);
    }
    guard(Tool::Monitor, display_profiles(runner), |profiles| {
        let mut lines = vec![if profiles.is_empty() {
            "no saved profiles".to_owned()
        } else {
            format!("profiles ({}): {}", profiles.len(), profiles.join(", "))
        }];
        for name in profiles.iter().take(8) {
            if let Ok(profile) = display_profile(runner, name) {
                lines.push(format!(
                    "{}: {}",
                    profile.name,
                    if profile.outputs.is_empty() {
                        "no outputs".to_owned()
                    } else {
                        profile.outputs.join(", ")
                    }
                ));
            }
        }
        IntegrationView::ready(format!("{} profile(s)", profiles.len())).with_lines(lines)
    })
}

pub fn sessions_view(runner: &dyn CommandRunner) -> IntegrationView {
    if !tool_status(runner, Tool::Session).installed {
        return IntegrationView::missing(Tool::Session);
    }
    guard(Tool::Session, session_contexts(runner), |contexts| {
        let lines = contexts
            .iter()
            .map(|name| match session_context(runner, name) {
                Ok(context) => format!(
                    "{name}: {} workspace(s), {} window(s), {} output(s){}",
                    context.workspaces,
                    context.windows,
                    context.outputs.len(),
                    context
                        .template
                        .as_deref()
                        .map(|template| format!(", template {template}"))
                        .unwrap_or_default()
                ),
                Err(err) => format!("{name}: {}", err.message),
            })
            .collect();
        IntegrationView::ready(format!("{} context(s)", contexts.len())).with_lines(lines)
    })
}

pub fn wallpaper_view(runner: &dyn CommandRunner) -> IntegrationView {
    if !tool_status(runner, Tool::Wallpaper).installed {
        return IntegrationView::missing(Tool::Wallpaper);
    }
    guard(Tool::Wallpaper, wallpaper_status(runner), |status| {
        let mut lines = vec![format!(
            "active scene: {}",
            status.active.as_deref().unwrap_or("(none)")
        )];
        lines.push(format!(
            "backend: {} (native {})",
            status.backend,
            if status.native_available {
                "available"
            } else {
                "unavailable"
            }
        ));
        if let Some(error) = &status.native_error {
            lines.push(format!("native backend: {error}"));
        }
        lines.push(if status.scenes.is_empty() {
            "no scenes".to_owned()
        } else {
            format!(
                "scenes ({}): {}",
                status.scenes.len(),
                status
                    .scenes
                    .iter()
                    .map(|scene| scene.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        IntegrationView::ready(format!("{} scene(s)", status.scenes.len())).with_lines(lines)
    })
}

pub fn audio_view(runner: &dyn CommandRunner) -> IntegrationView {
    if !tool_status(runner, Tool::Audio).installed {
        return IntegrationView::missing(Tool::Audio);
    }
    guard(Tool::Audio, audio_status(runner), |status| {
        let mut lines = vec![format!(
            "backend: {} (requested {})",
            status.active_backend.as_deref().unwrap_or("unavailable"),
            status.requested_backend
        )];
        if status.degraded {
            lines.push(format!(
                "degraded: {}",
                status.reason.as_deref().unwrap_or("yes")
            ));
        }
        for attempt in &status.attempts {
            lines.push(format!("probe: {attempt}"));
        }
        lines.push(format!(
            "default sink: {}",
            status.current_default.as_deref().unwrap_or("(none)")
        ));
        lines.push(format!(
            "rules: {} (configured default: {})",
            status.rules,
            status.configured_default.as_deref().unwrap_or("(none)")
        ));
        IntegrationView::ready(format!("{} rule(s)", status.rules)).with_lines(lines)
    })
}

pub fn sidepanels_view(runner: &dyn CommandRunner, path: &Path) -> IntegrationView {
    let installed = tool_status(runner, Tool::Sidepanels).installed;
    let config = match read_sidepanels(path) {
        Ok(config) => config,
        Err(err) => {
            return IntegrationView {
                status: "error".to_owned(),
                lines: vec![format!("config: {}", path.display())],
                error: Some(err.to_string()),
            };
        }
    };
    let mut lines = vec![format!("config: {}", config.path)];
    if !config.exists {
        lines.push("no config file; the built-in default bar is used".to_owned());
    } else {
        lines.push(format!(
            "version {}: {} panel(s), {} output override(s)",
            config.version,
            config.panels.len(),
            config.outputs.len()
        ));
        for (index, panel) in config.panels.iter().enumerate() {
            lines.push(format!(
                "panel {index}: {} on {}, {} row(s){}",
                if panel.title.is_empty() {
                    "(untitled)"
                } else {
                    &panel.title
                },
                panel.output.as_deref().unwrap_or("*"),
                panel.rows,
                if panel.side.is_empty() {
                    String::new()
                } else {
                    format!(", {} edge", panel.side)
                }
            ));
        }
        if !config.layouts.is_empty() {
            lines.push(format!("layouts: {}", config.layouts.join(", ")));
        }
    }
    if !installed {
        lines.push(format!(
            "{} is not installed; validate/open still work",
            program_for(Tool::Sidepanels)
        ));
    }
    IntegrationView::ready(if installed {
        "installed".to_owned()
    } else {
        format!("file only ({})", program_for(Tool::Sidepanels))
    })
    .with_lines(lines)
}

pub fn companion_view(runner: &dyn CommandRunner) -> IntegrationView {
    if !tool_status(runner, Tool::Companion).installed {
        return IntegrationView::missing(Tool::Companion);
    }
    match companion_status(runner) {
        Ok(status) => {
            let mut lines = vec![format!(
                "daemon {} on {} (pid {}, up {}s)",
                status.version, status.socket, status.pid, status.uptime_secs
            )];
            lines.push(if status.contexts.is_empty() {
                "no action contexts".to_owned()
            } else {
                format!("contexts: {}", status.contexts.join(", "))
            });
            if let Ok(namespaces) = companion_namespace_list(runner) {
                for namespace in namespaces {
                    let actions = namespace
                        .actions
                        .iter()
                        .map(|action| {
                            if action.available {
                                action.name.clone()
                            } else {
                                format!("{} (unavailable)", action.name)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(format!("{}: {actions}", namespace.namespace));
                }
            }
            IntegrationView::ready("daemon running").with_lines(lines)
        }
        Err(err) => {
            IntegrationView::ready("installed, daemon not reachable").with_lines(vec![err.message])
        }
    }
}

/// Portal daemon and frontend selection status.
pub fn portal_view(runner: &dyn CommandRunner, config: &Path) -> IntegrationView {
    let status = portal_status(runner, config);
    let mut lines = vec![
        format!(
            "unit: {} ({}, {})",
            status.unit,
            status.active.as_deref().unwrap_or("unknown"),
            status.enabled.as_deref().unwrap_or("unknown")
        ),
        format!("frontend config: {}", status.config),
    ];
    if !status.config_exists {
        lines.push("no portals.conf; the frontend picks a backend by itself".to_owned());
    } else if status.selected {
        lines.push(
            "selection: `org.freedesktop.impl.portal.desktop.better` is named in portals.conf"
                .to_owned(),
        );
    } else {
        lines.push(
            "selection: portals.conf does not name `org.freedesktop.impl.portal.desktop.better`"
                .to_owned(),
        );
    }
    lines.push("start with `systemctl --user enable --now better-portal`".to_owned());
    IntegrationView::ready(if status.active.as_deref() == Some("active") {
        "running".to_owned()
    } else {
        status
            .active
            .as_deref()
            .unwrap_or("status unavailable")
            .to_owned()
    })
    .with_lines(lines)
}

/// Noctalia plugin source status.
pub fn noctalia_view(path: &Path) -> IntegrationView {
    let source = read_noctalia_source(path);
    if !source.exists {
        return IntegrationView::ready("plugin source not installed").with_lines(vec![
            format!("looked in: {}", source.path),
            "install the tree from better-noctalia-plugins or set BETTER_SETTINGS_NOCTALIA_PLUGINS"
                .to_owned(),
        ]);
    }
    let mut lines = vec![
        format!("source: {}", source.path),
        if source.plugins.is_empty() {
            "no plugin manifests found".to_owned()
        } else {
            format!(
                "plugins ({}): {}",
                source.plugins.len(),
                source.plugins.join(", ")
            )
        },
    ];
    if source.verify_script {
        lines.push("verify.sh is available for manifest/syntax checks".to_owned());
    }
    IntegrationView::ready(format!("{} plugin(s)", source.plugins.len())).with_lines(lines)
}

/// The small, never-failing aggregate the Overview page renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverviewStatus {
    pub compositor: CompositorStatus,
    pub tools: IntegrationView,
    pub portal: PortalStatus,
    pub companion: IntegrationView,
    pub profiles: usize,
    pub contexts: usize,
    pub scenes: usize,
    pub sessions_history: usize,
}

pub fn overview_status(runner: &dyn CommandRunner, portals_conf: &Path) -> OverviewStatus {
    let count =
        |result: Result<Vec<String>, IntegrationError>| result.map(|v| v.len()).unwrap_or(0);
    OverviewStatus {
        compositor: compositor_status(runner),
        tools: tools_view(runner),
        portal: portal_status(runner, portals_conf),
        companion: companion_view(runner),
        profiles: count(display_profiles(runner)),
        contexts: count(session_contexts(runner)),
        scenes: count(wallpaper_scenes(runner)),
        sessions_history: session_history(runner).map(|v| v.len()).unwrap_or(0),
    }
}

/// Every section, in the order the GTK window shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationOverview {
    pub tools: IntegrationView,
    pub displays: IntegrationView,
    pub sessions: IntegrationView,
    pub wallpaper: IntegrationView,
    pub audio: IntegrationView,
    pub sidepanels: IntegrationView,
    pub companion: IntegrationView,
}

pub fn overview(runner: &dyn CommandRunner, sidepanels: &Path) -> IntegrationOverview {
    IntegrationOverview {
        tools: tools_view(runner),
        displays: displays_view(runner),
        sessions: sessions_view(runner),
        wallpaper: wallpaper_view(runner),
        audio: audio_view(runner),
        sidepanels: sidepanels_view(runner, sidepanels),
        companion: companion_view(runner),
    }
}

impl IntegrationOverview {
    /// `(title, view)` pairs in display order.
    pub fn sections(&self) -> [(&'static str, &IntegrationView); 7] {
        [
            ("Integrations", &self.tools),
            (Tool::Monitor.label(), &self.displays),
            (Tool::Session.label(), &self.sessions),
            (Tool::Wallpaper.label(), &self.wallpaper),
            (Tool::Audio.label(), &self.audio),
            (Tool::Sidepanels.label(), &self.sidepanels),
            (Tool::Companion.label(), &self.companion),
        ]
    }
}

// ---------------------------------------------------------------------------
// CLI dispatch
// ---------------------------------------------------------------------------

pub fn usage() -> &'static str {
    "better-settingsctl <command>\n\n\
     Companion app integrations (each app stays the authority for its own data):\n\
       integrations [status]             Which CLIs are installed and their versions\n\
       compositor status                 niri version, renderer and HDR capability\n\
       compositor reload --apply         Reload the running compositor config\n\
       displays list                     List better-monitor profiles\n\
       displays show <name>              Show one profile and its outputs\n\
       displays preview <name>           Dry-run an apply (no changes)\n\
       displays apply <name> [--apply]   Preview, then apply with --apply\n\
       displays restore [--apply]        Preview or restore the previous layout\n\
       displays positions                Current logical output geometry\n\
       displays arrange <json> [--apply] Preview or apply an arrangement plan\n\
       displays confirm --apply          Confirm a pending safe apply\n\
       displays cancel --apply           Cancel a pending safe apply\n\
       sessions list | show <name>       List contexts or show one\n\
       sessions history                  List recorded snapshots\n\
       sessions capture <name> --apply   Capture the current session explicitly\n\
       sessions explain <name>           Plan a restore without changing anything\n\
       sessions activate <name> --apply  Restore a context (requires --apply)\n\
       wallpaper status | scenes         Active scene, backend and scene names\n\
       wallpaper use <name> --apply      Activate a scene\n\
       wallpaper apply [--apply]         Preview, then apply\n\
       audio status | sinks | rules      Read audio state and routing rules\n\
       audio devices                     Sinks and sources with volume/mute\n\
       audio volume <kind> <dev> <0..1> [--apply]  Set a device volume\n\
       audio mute <kind> <dev> on|off [--apply]    Mute or unmute a device\n\
       audio default <sink> --apply      Set the configured default sink\n\
       audio apply|restore [--apply]     Preview, then route or restore streams\n\
       sidepanels status | validate      Read and validate the JSON config\n\
       sidepanels layouts | check <name> Named layouts and a read-only preview\n\
       sidepanels path                   Print the config path\n\
       sidepanels open                   Open the config in $VISUAL/$EDITOR\n\
       companion describe | status | list  Action registry and daemon status\n\
       companion history [limit]         Recent rule/automation runs\n\
       companion explain rules|windows   Explain matches without acting\n\
       companion rules --apply           Run matching rules (preview, then apply)\n\
       companion idle|rotate status      Idle and auto-rotation policy state\n\
       companion idle|rotate enable|disable --apply\n\
                                    Change an automation policy\n\
       companion marks                   Window marks tracked by the daemon\n\
       companion run <ns> <action> [json]  Dry-run an action; --apply calls it\n\
       portal status                     better-portal unit and portals.conf hint\n\
       noctalia status                   Noctalia plugin source status\n\n\
     Mutating commands print a preview and refuse to run without --apply.\n\
     Tools can be redirected with BETTER_SETTINGS_BETTER_MONITOR,\n\
     BETTER_SETTINGS_BETTER_SESSION, BETTER_SETTINGS_BETTER_WALLPAPER,\n\
     BETTER_SETTINGS_BETTER_AUDIO, BETTER_SETTINGS_BETTER_SIDEPANELS and\n\
     BETTER_SETTINGS_COMPANIONCTL."
}

fn json<T: Serialize>(value: &T) -> Result<String, IntegrationError> {
    serde_json::to_string_pretty(value)
        .map_err(|err| IntegrationError::new(Tool::Monitor, format!("cannot encode JSON: {err}")))
}

fn apply_flag(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--apply")
}

fn without_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .collect()
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn audio_kind(value: Option<&str>) -> Option<AudioDeviceKind> {
    match value?.to_ascii_lowercase().as_str() {
        "sink" | "sinks" => Some(AudioDeviceKind::Sink),
        "source" | "sources" => Some(AudioDeviceKind::Source),
        _ => None,
    }
}

/// Dispatches one `better-settingsctl <integration>` invocation.
///
/// `args` starts with the section name (`displays`, `sessions`, ...).
pub fn dispatch(runner: &dyn CommandRunner, args: &[String]) -> Result<String, IntegrationError> {
    let Some(section) = args.first().map(String::as_str) else {
        return Ok(usage().to_owned());
    };
    let rest = &args[1..];
    if rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(usage().to_owned());
    }
    let positional = without_flags(rest);
    let sub = positional.first().copied();
    let apply = apply_flag(rest);

    match section {
        "integrations" => Ok(json(&tool_statuses(runner))?),
        "overview" => {
            let conf = portal_config_path(None);
            Ok(json(&overview_status(runner, &conf))?)
        }
        "compositor" => match sub.unwrap_or("status") {
            "status" => Ok(json(&compositor_status(runner))?),
            "reload" => {
                if !apply {
                    return Err(IntegrationError::new(
                        Tool::Monitor,
                        "reload tells the compositor to reread its config; pass --apply",
                    ));
                }
                reload_config(runner)?;
                Ok(json(&serde_json::json!({ "reloaded": true }))?)
            }
            other => Err(IntegrationError::new(
                Tool::Monitor,
                format!("unknown compositor command `{other}`"),
            )),
        },
        "displays" => match sub.unwrap_or("list") {
            "list" => Ok(json(&display_profiles(runner)?)?),
            "show" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Monitor, "displays show needs a profile name")
                })?;
                Ok(json(&display_profile(runner, name)?)?)
            }
            "preview" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Monitor, "displays preview needs a profile name")
                })?;
                Ok(json(&display_preview(runner, name)?)?)
            }
            "apply" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Monitor, "displays apply needs a profile name")
                })?;
                if apply {
                    Ok(json(&display_apply(runner, name)?)?)
                } else {
                    let preview = display_preview(runner, name)?;
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": preview,
                        "hint": "re-run with --apply to apply this profile",
                    }))?)
                }
            }
            "restore" => {
                if apply {
                    Ok(json(&display_restore(runner)?)?)
                } else {
                    let preview = display_restore_preview(runner)?;
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": preview,
                        "hint": "re-run with --apply to restore the previous layout",
                    }))?)
                }
            }
            "positions" => Ok(json(&display_positions(runner)?)?),
            "arrange" => {
                let plan = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Monitor, "displays arrange needs a JSON plan")
                })?;
                if apply {
                    Ok(json(&display_arrange(runner, plan)?)?)
                } else {
                    let preview = display_arrange_preview(runner, plan)?;
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": preview,
                        "hint": "re-run with --apply to move the outputs",
                    }))?)
                }
            }
            "confirm" => {
                if !apply {
                    return Err(IntegrationError::new(
                        Tool::Monitor,
                        "confirm changes the compositor; pass --apply",
                    ));
                }
                Ok(json(&display_confirm(runner)?)?)
            }
            "cancel" => {
                if !apply {
                    return Err(IntegrationError::new(
                        Tool::Monitor,
                        "cancel rolls the previous layout back; pass --apply",
                    ));
                }
                Ok(json(&display_cancel(runner)?)?)
            }
            other => Err(IntegrationError::new(
                Tool::Monitor,
                format!("unknown displays command `{other}`"),
            )),
        },
        "sessions" => match sub.unwrap_or("list") {
            "list" => Ok(json(&session_contexts(runner)?)?),
            "show" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Session, "sessions show needs a context name")
                })?;
                Ok(json(&session_context(runner, name)?)?)
            }
            "explain" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Session, "sessions explain needs a context name")
                })?;
                Ok(json(&session_explain(runner, name)?)?)
            }
            "history" => Ok(json(&session_history(runner)?)?),
            "capture" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Session, "sessions capture needs a context name")
                })?;
                if !apply {
                    return Err(IntegrationError::new(
                        Tool::Session,
                        format!("capturing `{name}` overwrites a context; pass --apply"),
                    ));
                }
                Ok(json(&session_capture(runner, name)?)?)
            }
            "activate" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Session, "sessions activate needs a context name")
                })?;
                if apply {
                    Ok(json(&session_activate(runner, name)?)?)
                } else {
                    let preview = session_explain(runner, name)?;
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": preview,
                        "hint": "re-run with --apply to restore this context",
                    }))?)
                }
            }
            other => Err(IntegrationError::new(
                Tool::Session,
                format!("unknown sessions command `{other}`"),
            )),
        },
        "wallpaper" => match sub.unwrap_or("status") {
            "status" => Ok(json(&wallpaper_status(runner)?)?),
            "scenes" => Ok(json(&wallpaper_scenes(runner)?)?),
            "use" => {
                let name = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Wallpaper, "wallpaper use needs a scene name")
                })?;
                if !apply {
                    return Err(IntegrationError::new(
                        Tool::Wallpaper,
                        format!("activating `{name}` changes the active scene; pass --apply"),
                    ));
                }
                Ok(json(&wallpaper_use(runner, name)?)?)
            }
            "apply" => {
                if apply {
                    Ok(json(&wallpaper_apply(runner)?)?)
                } else {
                    let preview = wallpaper_preview(runner)?;
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": preview,
                        "hint": "re-run with --apply to apply the active scene",
                    }))?)
                }
            }
            other => Err(IntegrationError::new(
                Tool::Wallpaper,
                format!("unknown wallpaper command `{other}`"),
            )),
        },
        "audio" => match sub.unwrap_or("status") {
            "status" => Ok(json(&audio_status(runner)?)?),
            "sinks" => Ok(json(&audio_sinks(runner)?)?),
            "rules" => Ok(json(&audio_rules(runner)?)?),
            "devices" => Ok(json(&audio_devices(runner)?)?),
            "volume" => {
                let kind = audio_kind(positional.get(1).copied()).ok_or_else(|| {
                    IntegrationError::new(Tool::Audio, "audio volume needs sink or source")
                })?;
                let name = positional.get(2).ok_or_else(|| {
                    IntegrationError::new(Tool::Audio, "audio volume needs a device name")
                })?;
                let volume: f64 = positional
                    .get(3)
                    .ok_or_else(|| IntegrationError::new(Tool::Audio, "audio volume needs 0..1"))?
                    .parse()
                    .map_err(|_| {
                        IntegrationError::new(Tool::Audio, "audio volume needs a number in 0..1")
                    })?;
                if !(0.0..=1.0).contains(&volume) || !volume.is_finite() {
                    return Err(IntegrationError::new(
                        Tool::Audio,
                        format!("volume {volume} is outside 0..1"),
                    ));
                }
                let value = audio_set_volume(runner, kind, name, volume, !apply)?;
                if apply {
                    Ok(json(&value)?)
                } else {
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": value,
                        "hint": "re-run with --apply to set this volume",
                    }))?)
                }
            }
            "mute" => {
                let kind = audio_kind(positional.get(1).copied()).ok_or_else(|| {
                    IntegrationError::new(Tool::Audio, "audio mute needs sink or source")
                })?;
                let name = positional.get(2).ok_or_else(|| {
                    IntegrationError::new(Tool::Audio, "audio mute needs a device name")
                })?;
                let mute = match positional.get(3).map(|state| state.to_ascii_lowercase()) {
                    Some(state) if matches!(state.as_str(), "on" | "true" | "1") => true,
                    Some(state) if matches!(state.as_str(), "off" | "false" | "0") => false,
                    _ => {
                        return Err(IntegrationError::new(
                            Tool::Audio,
                            "audio mute needs on or off",
                        ));
                    }
                };
                let value = audio_set_mute(runner, kind, name, mute, !apply)?;
                if apply {
                    Ok(json(&value)?)
                } else {
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": value,
                        "hint": "re-run with --apply to change mute",
                    }))?)
                }
            }
            "default" => {
                let sink = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(Tool::Audio, "audio default needs a sink name or `none`")
                })?;
                if !apply {
                    return Err(IntegrationError::new(
                        Tool::Audio,
                        format!("setting the default sink to `{sink}` changes state; pass --apply"),
                    ));
                }
                Ok(json(&audio_set_default(runner, sink)?)?)
            }
            "apply" => {
                if apply {
                    Ok(json(&audio_apply(runner)?)?)
                } else {
                    let preview = audio_apply_preview(runner)?;
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": preview,
                        "hint": "re-run with --apply to route matching streams",
                    }))?)
                }
            }
            "restore" => {
                if apply {
                    Ok(json(&audio_restore(runner)?)?)
                } else {
                    let preview = audio_restore_preview(runner)?;
                    Ok(json(&serde_json::json!({
                        "dry_run": true,
                        "applied": false,
                        "preview": preview,
                        "hint": "re-run with --apply to move streams back",
                    }))?)
                }
            }
            other => Err(IntegrationError::new(
                Tool::Audio,
                format!("unknown audio command `{other}`"),
            )),
        },
        "sidepanels" => {
            let explicit = value_after(rest, "--config").map(PathBuf::from);
            let path = sidepanels_path(explicit.as_deref());
            match sub.unwrap_or("status") {
                "status" => Ok(json(&read_sidepanels(&path)?)?),
                "layouts" => Ok(json(&read_sidepanels(&path)?.layouts)?),
                "check" => {
                    let layout = positional.get(1).ok_or_else(|| {
                        IntegrationError::new(
                            Tool::Sidepanels,
                            "sidepanels check needs a layout name",
                        )
                    })?;
                    Ok(check_sidepanels_layout(runner, &path, layout)?)
                }
                "path" => Ok(path.display().to_string()),
                "validate" => Ok(validate_sidepanels(runner, &path)?),
                "open" => Ok(open_sidepanels(&path)?),
                other => Err(IntegrationError::new(
                    Tool::Sidepanels,
                    format!("unknown sidepanels command `{other}`"),
                )),
            }
        }
        "portal" => {
            let conf = portal_config_path(None);
            Ok(json(&portal_status(runner, &conf))?)
        }
        "noctalia" => {
            let path = noctalia_source(None);
            Ok(json(&read_noctalia_source(&path))?)
        }
        "companion" => match sub.unwrap_or("status") {
            "describe" => Ok(json(&companion_describe(runner)?)?),
            "status" => Ok(json(&companion_status(runner)?)?),
            "list" => Ok(json(&companion_namespace_list(runner)?)?),
            "history" => {
                let limit = positional
                    .get(1)
                    .map(|limit| {
                        limit.parse::<u32>().map_err(|_| {
                            IntegrationError::new(
                                Tool::Companion,
                                format!("history limit must be an integer, got `{limit}`"),
                            )
                        })
                    })
                    .transpose()?
                    .unwrap_or(20);
                Ok(json(&companion_history(runner, limit)?)?)
            }
            "explain" => {
                let target = positional.get(1).copied().unwrap_or("rules");
                Ok(json(&companion_explain(runner, target)?)?)
            }
            "rules" => Ok(json(&companion_rules_apply(runner, !apply)?)?),
            "idle" | "rotate" => {
                let namespace = sub.unwrap_or("idle");
                let action = positional.get(1).copied().unwrap_or("status");
                match action {
                    "status" => {
                        let status = if namespace == "idle" {
                            idle_status(runner)?
                        } else {
                            rotate_status(runner)?
                        };
                        Ok(json(&status)?)
                    }
                    "enable" | "disable" => {
                        if !apply {
                            return Err(IntegrationError::new(
                                Tool::Companion,
                                format!(
                                    "{namespace} {action} changes the policy; preview with `status` then pass --apply"
                                ),
                            ));
                        }
                        Ok(json(&companion_adapter_set_enabled(
                            runner,
                            namespace,
                            action == "enable",
                            false,
                        )?)?)
                    }
                    other => Err(IntegrationError::new(
                        Tool::Companion,
                        format!("unknown {namespace} command `{other}`"),
                    )),
                }
            }
            "marks" => Ok(json(&companion_marks(runner)?)?),
            "run" => {
                let namespace = positional.get(1).ok_or_else(|| {
                    IntegrationError::new(
                        Tool::Companion,
                        "companion run needs a namespace and an action",
                    )
                })?;
                let action = positional.get(2).ok_or_else(|| {
                    IntegrationError::new(Tool::Companion, "companion run needs an action")
                })?;
                let params = positional.get(3).copied();
                let dry_run = !apply;
                let value = companion_run(runner, namespace, action, params, dry_run)?;
                Ok(json(&serde_json::json!({
                    "dry_run": dry_run,
                    "applied": apply,
                    "response": value,
                }))?)
            }
            other => Err(IntegrationError::new(
                Tool::Companion,
                format!("unknown companion command `{other}`"),
            )),
        },
        other => Err(IntegrationError::new(
            Tool::Monitor,
            format!("unknown integration `{other}`\n\n{}", usage()),
        )),
    }
}

/// `true` when `args[0]` is an integration section handled by [`dispatch`].
pub fn is_section(name: &str) -> bool {
    matches!(
        name,
        "integrations"
            | "overview"
            | "compositor"
            | "displays"
            | "sessions"
            | "wallpaper"
            | "audio"
            | "sidepanels"
            | "portal"
            | "noctalia"
            | "companion"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ARRANGE_PLAN: &str = r#"{"outputs":{"DP-1":[1280,0]}}"#;

    fn fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        serde_json::from_slice(&bytes).unwrap()
    }

    fn result(program: &str, args: &[&str], value: &Value) -> CommandResult {
        CommandResult::output(program, args.to_vec(), 0, value.to_string(), "")
    }

    fn ready(program: &str, args: &[&str]) -> CommandResult {
        CommandResult::output(program, args.to_vec(), 0, "", "")
    }

    fn reference_runner() -> ScriptedRunner {
        ScriptedRunner::new()
            .on(
                "better-monitor",
                &["--version"],
                ready("better-monitor", &["--version"]),
            )
            .on(
                "better-monitor",
                &["--json", "position"],
                result(
                    "better-monitor",
                    &["--json", "position"],
                    &fixture("monitor-position.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "arrange", "--plan", ARRANGE_PLAN, "--dry-run"],
                result(
                    "better-monitor",
                    &["--json", "arrange", "--plan", ARRANGE_PLAN, "--dry-run"],
                    &fixture("monitor-arrange-dry-run.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "arrange", "--plan", ARRANGE_PLAN],
                result(
                    "better-monitor",
                    &["--json", "arrange", "--plan", ARRANGE_PLAN],
                    &fixture("monitor-arrange.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "list"],
                result(
                    "better-monitor",
                    &["--json", "list"],
                    &fixture("monitor-list.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "show", "desk"],
                result(
                    "better-monitor",
                    &["--json", "show", "desk"],
                    &fixture("monitor-show.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "apply", "desk", "--dry-run"],
                result(
                    "better-monitor",
                    &["--json", "apply", "desk", "--dry-run"],
                    &fixture("monitor-apply-dry-run.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "apply", "desk"],
                result(
                    "better-monitor",
                    &["--json", "apply", "desk"],
                    &fixture("monitor-apply.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "restore", "--dry-run"],
                result(
                    "better-monitor",
                    &["--json", "restore", "--dry-run"],
                    &fixture("monitor-restore-dry-run.json"),
                ),
            )
            .on(
                "better-monitor",
                &["--json", "confirm"],
                CommandResult::output(
                    "better-monitor",
                    vec!["--json", "confirm"],
                    0,
                    r#"{"command":"confirm","ok":true,"data":{"confirmed":true}}"#,
                    "",
                ),
            )
            .on(
                "better-session",
                &["--version"],
                ready("better-session", &["--version"]),
            )
            .on(
                "better-session",
                &["list"],
                result("better-session", &["list"], &fixture("session-list.json")),
            )
            .on(
                "better-session",
                &["show", "work"],
                result(
                    "better-session",
                    &["show", "work"],
                    &fixture("session-show.json"),
                ),
            )
            .on(
                "better-session",
                &["explain", "work"],
                result(
                    "better-session",
                    &["explain", "work"],
                    &fixture("session-explain.json"),
                ),
            )
            .on(
                "better-session",
                &["restore", "work"],
                result(
                    "better-session",
                    &["restore", "work"],
                    &fixture("session-restore.json"),
                ),
            )
            .on(
                "better-wallpaper",
                &["--version"],
                ready("better-wallpaper", &["--version"]),
            )
            .on(
                "better-wallpaper",
                &["status"],
                result(
                    "better-wallpaper",
                    &["status"],
                    &fixture("wallpaper-status.json"),
                ),
            )
            .on(
                "better-wallpaper",
                &["scene", "list"],
                result(
                    "better-wallpaper",
                    &["scene", "list"],
                    &fixture("wallpaper-scene-list.json"),
                ),
            )
            .on(
                "better-wallpaper",
                &["scene", "use", "night"],
                result(
                    "better-wallpaper",
                    &["scene", "use", "night"],
                    &fixture("wallpaper-use.json"),
                ),
            )
            .on(
                "better-wallpaper",
                &["apply", "--dry-run"],
                result(
                    "better-wallpaper",
                    &["apply", "--dry-run"],
                    &fixture("wallpaper-apply-dry-run.json"),
                ),
            )
            .on(
                "better-wallpaper",
                &["apply"],
                result(
                    "better-wallpaper",
                    &["apply"],
                    &fixture("wallpaper-apply.json"),
                ),
            )
            .on(
                "better-wallpaper",
                &["apply", "--backend=command", "--dry-run"],
                result(
                    "better-wallpaper",
                    &["apply", "--backend=command", "--dry-run"],
                    &fixture("wallpaper-apply-command-dry-run.json"),
                ),
            )
            .on(
                "better-audio",
                &["--version"],
                ready("better-audio", &["--version"]),
            )
            .on(
                "better-audio",
                &["status"],
                result("better-audio", &["status"], &fixture("audio-status.json")),
            )
            .on(
                "better-audio",
                &["sinks"],
                result("better-audio", &["sinks"], &fixture("audio-sinks.json")),
            )
            .on(
                "better-audio",
                &["sources"],
                result("better-audio", &["sources"], &fixture("audio-sources.json")),
            )
            .on(
                "better-audio",
                &["volume", "sink", "alsa_output.speakers", "0.5", "--dry-run"],
                result(
                    "better-audio",
                    &["volume", "sink", "alsa_output.speakers", "0.5", "--dry-run"],
                    &fixture("audio-volume-dry-run.json"),
                ),
            )
            .on(
                "better-audio",
                &["mute", "source", "alsa_input.usb-mic", "on", "--dry-run"],
                result(
                    "better-audio",
                    &["mute", "source", "alsa_input.usb-mic", "on", "--dry-run"],
                    &fixture("audio-mute-dry-run.json"),
                ),
            )
            .on(
                "better-audio",
                &["rules"],
                result("better-audio", &["rules"], &fixture("audio-rules.json")),
            )
            .on(
                "better-audio",
                &["default", "alsa_output.speakers"],
                result(
                    "better-audio",
                    &["default", "alsa_output.speakers"],
                    &fixture("audio-default.json"),
                ),
            )
            .on(
                "better-audio",
                &["apply", "--dry-run"],
                result(
                    "better-audio",
                    &["apply", "--dry-run"],
                    &fixture("audio-apply-dry-run.json"),
                ),
            )
            .on(
                "better-audio",
                &["apply"],
                result("better-audio", &["apply"], &fixture("audio-apply.json")),
            )
            .on(
                "better-sidepanels",
                &["--version"],
                ready("better-sidepanels", &["--version"]),
            )
            .on(
                "companionctl",
                &["--version"],
                ready("companionctl", &["--version"]),
            )
            .on(
                "companionctl",
                &["status"],
                result(
                    "companionctl",
                    &["status"],
                    &fixture("companion-status.json"),
                ),
            )
            .on(
                "companionctl",
                &["describe"],
                result(
                    "companionctl",
                    &["describe"],
                    &fixture("companion-describe.json"),
                ),
            )
            .on(
                "companionctl",
                &["list"],
                result("companionctl", &["list"], &fixture("companion-list.json")),
            )
            .on(
                "companionctl",
                &["--dry-run", "call", "parking", "park"],
                result(
                    "companionctl",
                    &["--dry-run", "call", "parking", "park"],
                    &fixture("companion-run-dry-run.json"),
                ),
            )
    }

    #[test]
    fn command_mapping_is_exact_and_dry_run_is_default() {
        let runner = reference_runner();
        assert_eq!(display_profiles(&runner).unwrap(), vec!["desk", "mobile"]);
        display_preview(&runner, "desk").unwrap();
        display_apply(&runner, "desk").unwrap();
        display_restore_preview(&runner).unwrap();
        display_confirm(&runner).unwrap();
        session_explain(&runner, "work").unwrap();
        session_activate(&runner, "work").unwrap();
        wallpaper_preview(&runner).unwrap();
        audio_apply_preview(&runner).unwrap();
        audio_set_default(&runner, "alsa_output.speakers").unwrap();
        companion_run(&runner, "parking", "park", None, true).unwrap();
        let calls = runner.calls();
        for expected in [
            "better-monitor --json list",
            "better-monitor --json apply desk --dry-run",
            "better-monitor --json apply desk",
            "better-monitor --json restore --dry-run",
            "better-monitor --json confirm",
            "better-session explain work",
            "better-session restore work",
            "better-wallpaper apply --dry-run",
            "better-audio apply --dry-run",
            "better-audio default alsa_output.speakers",
            "companionctl --dry-run call parking park",
        ] {
            assert!(
                calls.iter().any(|call| call == expected),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn parses_monitor_profiles_and_reports() {
        let runner = reference_runner();
        let profile = display_profile(&runner, "desk").unwrap();
        assert_eq!(profile.name, "desk");
        assert_eq!(profile.outputs, vec!["DP-1", "eDP-1"]);
        let preview = display_preview(&runner, "desk").unwrap();
        assert!(preview.dry_run);
        assert_eq!(preview.status, "planned");
        assert_eq!(preview.actions.len(), 2);
        assert!(preview.actions[0].starts_with("eDP-1: "));
        assert_eq!(preview.resolved.len(), 2);
        let applied = display_apply(&runner, "desk").unwrap();
        assert!(!applied.dry_run);
        assert_eq!(applied.status, "applied");
    }

    #[test]
    fn parses_session_contexts_and_outcomes() {
        let runner = reference_runner();
        assert_eq!(session_contexts(&runner).unwrap(), vec!["home", "work"]);
        let context = session_context(&runner, "work").unwrap();
        assert_eq!((context.workspaces, context.windows), (2, 1));
        assert_eq!(context.outputs, vec!["DP-1"]);
        assert_eq!(context.commands, 1);
        assert_eq!(context.template.as_deref(), Some("Development"));
        let outcome = session_explain(&runner, "work").unwrap();
        assert!(outcome.dry_run);
        assert!(outcome.ok);
        assert_eq!(outcome.applied, 2);
    }

    #[test]
    fn parses_wallpaper_status_and_plan() {
        let runner = reference_runner();
        let status = wallpaper_status(&runner).unwrap();
        assert_eq!(status.active.as_deref(), Some("night"));
        assert_eq!(status.backend, "auto");
        assert!(status.native_available);
        assert_eq!(status.scenes.len(), 2);
        let night = status
            .scenes
            .iter()
            .find(|scene| scene.name == "night")
            .unwrap();
        assert_eq!(night.workspaces, vec!["1"]);
        let plan = wallpaper_preview(&runner).unwrap();
        assert!(plan.dry_run);
        assert_eq!(plan.images.len(), 1);
        assert_eq!(plan.images[0].0, "eDP-1");
        let command = wallpaper_plan(fixture("wallpaper-apply-command-dry-run.json")).unwrap();
        assert_eq!(command.backend, "command");
        assert_eq!(
            command.image.as_deref(),
            Some("/home/user/Pictures/night.png")
        );
    }

    #[test]
    fn parses_audio_status_sinks_and_rules() {
        let runner = reference_runner();
        let status = audio_status(&runner).unwrap();
        assert_eq!(status.active_backend.as_deref(), Some("native"));
        assert!(!status.degraded);
        assert_eq!(status.rules, 1);
        let sinks = audio_sinks(&runner).unwrap();
        assert_eq!(sinks.default.as_deref(), Some("alsa_output.speakers"));
        assert_eq!(sinks.sinks.len(), 2);
        assert_eq!(sinks.sinks[0].volume, Some(1.0));
        assert_eq!(sinks.sinks[1].mute, Some(true));
        let rules = audio_rules(&runner).unwrap();
        assert_eq!(rules[0].app, "firefox");
        assert_eq!(rules[0].sink, "alsa_output.speakers");
    }

    #[test]
    fn reads_audio_devices_with_volume_and_mute() {
        let runner = reference_runner();
        let devices = audio_devices(&runner).unwrap();
        assert_eq!(devices.backend, "native");
        assert_eq!(
            devices.default_sink.as_deref(),
            Some("alsa_output.speakers")
        );
        assert_eq!(
            devices.default_source.as_deref(),
            Some("alsa_input.usb-mic")
        );
        assert_eq!(devices.sinks.len(), 2);
        assert_eq!(devices.sinks[1].volume, Some(0.5));
        assert_eq!(devices.sinks[1].mute, Some(true));
        assert_eq!(devices.sources.len(), 1);
        assert_eq!(devices.sources[0].volume, Some(0.5));
        assert_eq!(devices.sources[0].mute, Some(false));
    }

    #[test]
    fn mixer_commands_map_to_volume_and_mute_with_preview() {
        let runner = reference_runner();
        let volume = audio_set_volume(
            &runner,
            AudioDeviceKind::Sink,
            "alsa_output.speakers",
            0.5,
            true,
        )
        .unwrap();
        assert_eq!(volume["data"]["volume"], 0.5);
        assert_eq!(volume["data"]["kind"], "sink");

        let mute = audio_set_mute(
            &runner,
            AudioDeviceKind::Source,
            "alsa_input.usb-mic",
            true,
            true,
        )
        .unwrap();
        assert_eq!(mute["data"]["mute"], true);
        assert_eq!(mute["data"]["kind"], "source");

        let calls = runner.calls();
        assert!(
            calls
                .iter()
                .any(|call| call == "better-audio volume sink alsa_output.speakers 0.5 --dry-run")
        );
        assert!(
            calls
                .iter()
                .any(|call| call == "better-audio mute source alsa_input.usb-mic on --dry-run")
        );
    }

    #[test]
    fn audio_slider_values_map_to_rounded_command_arguments() {
        let runner = ScriptedRunner::new();
        let _ = audio_set_volume(&runner, AudioDeviceKind::Sink, "dev", 0.7564, true);
        let _ = audio_set_volume(&runner, AudioDeviceKind::Sink, "dev", 1.4, true);
        let calls = runner.calls();
        assert_eq!(calls[0], "better-audio volume sink dev 0.756 --dry-run");
        assert_eq!(calls[1], "better-audio volume sink dev 1 --dry-run");
    }

    #[test]
    fn reads_positions_and_maps_arrangement_commands() {
        let runner = reference_runner();
        let arrangement = display_positions(&runner).unwrap();
        assert_eq!(arrangement.displays.len(), 2);
        assert_eq!(arrangement.get("DP-1").unwrap().x, 2560);
        assert_eq!(arrangement.get("eDP-1").unwrap().width, 1280);

        let preview = display_arrange_preview(&runner, ARRANGE_PLAN).unwrap();
        assert!(preview.dry_run);
        assert_eq!(preview.status, "planned");
        let applied = display_arrange(&runner, ARRANGE_PLAN).unwrap();
        assert!(!applied.dry_run);
        assert_eq!(applied.status, "applied");

        let calls = runner.calls();
        assert!(calls.iter().any(|call| call
            == &format!("better-monitor --json arrange --plan {ARRANGE_PLAN} --dry-run")));
        assert!(calls.iter().any(|call| call
            == &format!("better-monitor --json arrange --plan {ARRANGE_PLAN}")));
    }

    #[test]
    fn parses_sidepanels_config_v2_and_v1() {
        let path = Path::new("/tmp/panels.json");
        let v2 = parse_sidepanels(path, &fixture("sidepanels-config.json")).unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(v2.panels.len(), 1);
        assert_eq!(v2.outputs, vec!["DP-1"]);
        assert_eq!(v2.panels[0].side, "left");
        assert_eq!(v2.panels[0].rows, 2);
        let v1 =
            parse_sidepanels(path, &json!({"title": "OLD", "rows": [{"label": "Clock"}]})).unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(v1.panels[0].title, "OLD");
    }

    #[test]
    fn sidepanels_validate_uses_the_app_and_surfaces_errors() {
        let path = Path::new("/tmp/panels.json");
        let path_text = path.display().to_string();
        let good = ScriptedRunner::new().on(
            "better-sidepanels",
            &["--validate", "--config", &path_text],
            CommandResult::output("better-sidepanels", vec!["--validate"], 0, "ok\n", ""),
        );
        assert_eq!(validate_sidepanels(&good, path).unwrap(), "ok");

        let bad = ScriptedRunner::new().on(
            "better-sidepanels",
            &["--validate", "--config", &path_text],
            CommandResult::output(
                "better-sidepanels",
                vec!["--validate"],
                1,
                "",
                "panel 0: too narrow\n",
            ),
        );
        let error = validate_sidepanels(&bad, path).unwrap_err();
        assert!(error.message.contains("too narrow"));
    }

    #[test]
    fn parses_companion_describe_status_and_list() {
        let runner = reference_runner();
        let description = companion_describe(&runner).unwrap();
        assert_eq!(description.protocol, "better-companion");
        assert_eq!(description.version, 2);
        assert_eq!(description.namespaces.len(), 1);
        assert_eq!(description.namespaces[0].actions[0].name, "park");
        assert!(description.namespaces[0].actions[0].available);
        let status = companion_status(&runner).unwrap();
        assert_eq!(status.pid, 4242);
        assert_eq!(status.contexts, vec!["parking"]);
        let namespaces = companion_namespace_list(&runner).unwrap();
        assert_eq!(namespaces[0].namespace, "parking");
    }

    #[test]
    fn missing_tools_degrade_instead_of_failing() {
        let runner = ScriptedRunner::new();
        let statuses = tool_statuses(&runner);
        assert!(statuses.iter().all(|status| !status.installed));
        let view = tools_view(&runner);
        assert_eq!(view.status, "0/6 companion CLIs installed");
        assert!(view.error.is_none());
        let displays = displays_view(&runner);
        assert!(displays.status.contains("not installed"));
        assert!(displays.error.is_none());
        let missing = std::env::temp_dir().join(format!(
            "better-settings-missing-{}.json",
            std::process::id()
        ));
        let overview = overview(&runner, &missing);
        assert!(
            overview
                .sections()
                .iter()
                .all(|(_, view)| view.error.is_none())
        );
        assert!(display_profiles(&runner).unwrap_err().is_missing());
        assert!(display_positions(&runner).unwrap_err().is_missing());
        assert!(audio_devices(&runner).unwrap_err().is_missing());
        assert!(
            audio_set_volume(
                &runner,
                AudioDeviceKind::Sink,
                "alsa_output.speakers",
                0.5,
                true
            )
            .unwrap_err()
            .is_missing()
        );
    }

    #[test]
    fn malformed_output_is_an_error_not_a_panic() {
        let runner = ScriptedRunner::new().on(
            "better-monitor",
            &["--json", "list"],
            CommandResult::output("better-monitor", vec!["--json", "list"], 0, "not json", ""),
        );
        let error = display_profiles(&runner).unwrap_err();
        assert!(error.message.contains("cannot parse"));
    }

    #[test]
    fn parses_hdr_outputs_and_maps_the_runtime_command() {
        let runner = ScriptedRunner::new().on(
            "niri",
            &["msg", "--json", "outputs"],
            CommandResult::output(
                "niri",
                vec!["msg", "--json", "outputs"],
                0,
                r#"{"DP-1":{"hdr_supported":true,"hdr_enabled":true,"hdr_active":false,"hdr_mode":"auto"}}"#,
                "",
            ),
        );
        let outputs = display_hdr_outputs(&runner).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "DP-1");
        assert!(outputs[0].supported);
        assert!(outputs[0].enabled);
        assert!(!outputs[0].active);
        assert_eq!(outputs[0].mode, "auto");

        let runner = ScriptedRunner::new().on(
            "niri",
            &["msg", "output", "DP-1", "hdr", "on"],
            CommandResult::output(
                "niri",
                vec!["msg", "output", "DP-1", "hdr", "on"],
                0,
                "{\"Ok\":\"Handled\"}",
                "",
            ),
        );
        display_hdr_set(&runner, "DP-1", "on").unwrap();
        assert!(display_hdr_set(&runner, "DP-1", "sometimes").is_err());
    }

    #[test]
    fn command_failures_include_stderr() {
        let runner = ScriptedRunner::new().on(
            "better-monitor",
            &["--json", "list"],
            CommandResult::output(
                "better-monitor",
                vec!["--json", "list"],
                1,
                "",
                "state is corrupt",
            ),
        );
        let error = display_profiles(&runner).unwrap_err();
        assert!(error.message.contains("state is corrupt"));
    }

    #[test]
    fn cli_previews_by_default_and_requires_apply_for_mutations() {
        let runner = reference_runner();
        let args = |rest: &[&str]| rest.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();

        let preview = dispatch(&runner, &args(&["displays", "apply", "desk"])).unwrap();
        let parsed: Value = serde_json::from_str(&preview).unwrap();
        assert_eq!(parsed["dry_run"], true);
        assert_eq!(parsed["applied"], false);
        assert_eq!(parsed["preview"]["status"], "planned");

        let applied = dispatch(&runner, &args(&["displays", "apply", "desk", "--apply"])).unwrap();
        let parsed: Value = serde_json::from_str(&applied).unwrap();
        assert_eq!(parsed["status"], "applied");

        let error = dispatch(&runner, &args(&["wallpaper", "use", "night"])).unwrap_err();
        assert!(error.message.contains("--apply"));
        let error = dispatch(&runner, &args(&["audio", "default", "none"])).unwrap_err();
        assert!(error.message.contains("--apply"));

        let session = dispatch(&runner, &args(&["sessions", "activate", "work"])).unwrap();
        let parsed: Value = serde_json::from_str(&session).unwrap();
        assert_eq!(parsed["dry_run"], true);
        assert_eq!(parsed["preview"]["context"], "work");

        let run = dispatch(&runner, &args(&["companion", "run", "parking", "park"])).unwrap();
        let parsed: Value = serde_json::from_str(&run).unwrap();
        assert_eq!(parsed["dry_run"], true);

        let wallpaper = dispatch(&runner, &args(&["wallpaper", "apply", "--apply"])).unwrap();
        let parsed: Value = serde_json::from_str(&wallpaper).unwrap();
        assert_eq!(parsed["dry_run"], false);
        assert_eq!(parsed["backend"], "command");

        let audio = dispatch(&runner, &args(&["audio", "apply", "--apply"])).unwrap();
        let parsed: Value = serde_json::from_str(&audio).unwrap();
        assert_eq!(parsed["dry_run"], false);
        assert_eq!(parsed["remembered"], 1);

        let positions = dispatch(&runner, &args(&["displays", "positions"])).unwrap();
        let parsed: Value = serde_json::from_str(&positions).unwrap();
        assert_eq!(parsed["displays"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["displays"][0]["output"], "DP-1");

        let arrange = dispatch(&runner, &args(&["displays", "arrange", ARRANGE_PLAN])).unwrap();
        let parsed: Value = serde_json::from_str(&arrange).unwrap();
        assert_eq!(parsed["dry_run"], true);
        assert_eq!(parsed["preview"]["status"], "planned");
        let applied = dispatch(
            &runner,
            &args(&["displays", "arrange", ARRANGE_PLAN, "--apply"]),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&applied).unwrap();
        assert_eq!(parsed["status"], "applied");

        let volume = dispatch(
            &runner,
            &args(&["audio", "volume", "sink", "alsa_output.speakers", "0.5"]),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&volume).unwrap();
        assert_eq!(parsed["dry_run"], true);
        assert_eq!(parsed["preview"]["data"]["volume"], 0.5);

        let mute = dispatch(
            &runner,
            &args(&["audio", "mute", "source", "alsa_input.usb-mic", "on"]),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&mute).unwrap();
        assert_eq!(parsed["preview"]["data"]["mute"], true);
    }

    #[test]
    fn status_section_never_fails() {
        let output = dispatch(&ScriptedRunner::new(), &["integrations".to_owned()]).unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 6);
        assert!(
            parsed
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| tool["installed"] == false)
        );
    }

    #[test]
    fn overview_contains_every_section() {
        let missing = std::env::temp_dir().join(format!(
            "better-settings-overview-{}.json",
            std::process::id()
        ));
        let overview = overview(&reference_runner(), &missing);
        let titles = overview
            .sections()
            .iter()
            .map(|(title, _)| *title)
            .collect::<Vec<_>>();
        assert_eq!(titles.len(), 7);
        assert_eq!(overview.tools.status, "6/6 companion CLIs installed");
        assert!(overview.displays.lines[0].contains("profiles"));
        assert_eq!(overview.audio.status, "1 rule(s)");
        assert_eq!(overview.companion.status, "daemon running");
        assert!(
            overview
                .companion
                .lines
                .iter()
                .any(|line| line.contains("parking: park"))
        );
    }

    #[test]
    fn every_tool_exposes_an_override_variable() {
        for tool in Tool::ALL {
            assert!(tool.override_env().starts_with("BETTER_SETTINGS_"));
            assert_eq!(program_for(tool), tool.program());
        }
    }

    fn niri_runner() -> ScriptedRunner {
        ScriptedRunner::new()
            .on(
                "niri",
                &["msg", "--json", "version"],
                result(
                    "niri",
                    &["msg", "--json", "version"],
                    &fixture("niri-version.json"),
                ),
            )
            .on(
                "niri",
                &["msg", "--json", "capabilities"],
                result(
                    "niri",
                    &["msg", "--json", "capabilities"],
                    &fixture("niri-capabilities.json"),
                ),
            )
            .on(
                "niri",
                &["msg", "--json", "outputs"],
                result(
                    "niri",
                    &["msg", "--json", "outputs"],
                    &fixture("niri-outputs.json"),
                ),
            )
            .on(
                "niri",
                &["msg", "action", "reload-config"],
                CommandResult::output("niri", ["msg", "action", "reload-config"], 0, "", ""),
            )
    }

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "better-settings-test-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn compositor_status_reads_version_capabilities_and_outputs() {
        let runner = niri_runner();
        let status = compositor_status(&runner);
        assert_eq!(
            status.compositor_version.as_deref(),
            Some("better-niri 26.1")
        );
        assert_eq!(status.cli_version.as_deref(), Some("better-niri 26.1"));
        assert!(status.vulkan);
        assert!(status.hdr);
        assert_eq!(status.renderer(), "Vulkan capable");
        assert_eq!(status.outputs, 2);
        assert_eq!(status.hdr_outputs, 1);
        assert_eq!(status.hdr_active, 1);
        assert!(status.error.is_none());

        let missing = compositor_status(&ScriptedRunner::new());
        assert!(missing.error.as_ref().unwrap().contains("not installed"));
        assert_eq!(missing.renderer(), "GLES (Vulkan not advertised)");
    }

    #[test]
    fn reload_config_posts_the_compositor_action() {
        let runner = ScriptedRunner::new().on(
            "niri",
            &["msg", "action", "reload-config"],
            CommandResult::output("niri", ["msg", "action", "reload-config"], 0, "", ""),
        );
        reload_config(&runner).unwrap();
        assert!(
            runner
                .calls()
                .contains(&"niri msg action reload-config".to_owned())
        );
        assert!(reload_config(&ScriptedRunner::new()).is_err());
    }

    #[test]
    fn session_history_and_capture_parse() {
        let runner = ScriptedRunner::new()
            .on(
                "better-session",
                &["history", "list"],
                result(
                    "better-session",
                    &["history", "list"],
                    &fixture("session-history.json"),
                ),
            )
            .on(
                "better-session",
                &["capture", "work"],
                result(
                    "better-session",
                    &["capture", "work"],
                    &fixture("session-show.json"),
                ),
            );
        let history = session_history(&runner).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].name, "work");
        assert_eq!(history[0].windows, 7);
        assert_eq!(history[0].trigger, "manual");
        let captured = session_capture(&runner, "work").unwrap();
        assert_eq!(captured.name, "work");
        assert!(captured.workspaces > 0);
        assert!(
            runner
                .calls()
                .contains(&"better-session capture work".to_owned())
        );
    }

    fn companion_automation_runner() -> ScriptedRunner {
        ScriptedRunner::new()
            .on(
                "companionctl",
                &["history", "20"],
                result(
                    "companionctl",
                    &["history", "20"],
                    &fixture("companion-history.json"),
                ),
            )
            .on(
                "companionctl",
                &["explain", "rules"],
                result(
                    "companionctl",
                    &["explain", "rules"],
                    &fixture("companion-explanation.json"),
                ),
            )
            .on(
                "companionctl",
                &["call", "rules", "run"],
                result(
                    "companionctl",
                    &["call", "rules", "run"],
                    &fixture("companion-explanation.json"),
                ),
            )
            .on(
                "companionctl",
                &["call", "idle", "status"],
                result(
                    "companionctl",
                    &["call", "idle", "status"],
                    &fixture("companion-idle-status.json"),
                ),
            )
            .on(
                "companionctl",
                &["call", "rotate", "status"],
                result(
                    "companionctl",
                    &["call", "rotate", "status"],
                    &fixture("companion-rotate-status.json"),
                ),
            )
            .on(
                "companionctl",
                &["call", "idle", "enable"],
                result(
                    "companionctl",
                    &["call", "idle", "enable"],
                    &json!({"enabled": true}),
                ),
            )
            .on(
                "companionctl",
                &["call", "marks", "list"],
                result(
                    "companionctl",
                    &["call", "marks", "list"],
                    &fixture("companion-marks.json"),
                ),
            )
    }

    #[test]
    fn companion_automation_reads_history_idle_rotate_and_marks() {
        let runner = companion_automation_runner();
        let history = companion_history(&runner, 20).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].name, "browser-on-dp1");
        assert_eq!(history[0].steps, 1);
        assert!(history[0].ok);
        assert!(!history[1].ok);
        assert_eq!(
            history[1].error.as_deref(),
            Some("rotate: sensor unavailable")
        );

        let idle = idle_status(&runner).unwrap();
        assert!(idle.enabled);
        assert_eq!(idle.namespace, "idle");
        assert!(idle.lines.iter().any(|line| line.contains("idle for 120s")));
        assert!(idle.lines.iter().any(|line| line.contains("next timeout")));

        let rotate = rotate_status(&runner).unwrap();
        assert!(rotate.enabled);
        assert!(rotate.lines.iter().any(|line| line.contains("eDP-1")));
        assert!(rotate.lines.iter().any(|line| line.contains("locked: no")));

        let explanation = companion_explain(&runner, "rules").unwrap();
        assert_eq!(explanation["count"], 1);
        assert!(companion_explain(&runner, "nope").is_err());
        let applied = companion_rules_apply(&runner, false).unwrap();
        assert_eq!(applied["count"], 1);
        assert!(
            runner
                .calls()
                .contains(&"companionctl call rules run".to_owned())
        );

        let marks = companion_marks(&runner).unwrap();
        assert_eq!(marks["count"], 2);
    }

    #[test]
    fn companion_adapter_enable_is_explicit() {
        let runner = companion_automation_runner();
        let value = companion_adapter_set_enabled(&runner, "idle", true, false).unwrap();
        assert_eq!(value["enabled"], true);
        assert!(
            runner
                .calls()
                .contains(&"companionctl call idle enable".to_owned())
        );
        assert!(companion_adapter_set_enabled(&runner, "nope", true, false).is_err());
    }

    #[test]
    fn sidepanels_layouts_and_check() {
        let path = Path::new("/tmp/panels.json");
        let config = parse_sidepanels(
            path,
            &json!({
                "version": 2,
                "panels": [],
                "layouts": {"work": {"panels": []}, "media": {"panels": []}}
            }),
        )
        .unwrap();
        assert_eq!(config.layouts, vec!["media", "work"]);

        let path_text = path.display().to_string();
        let runner = ScriptedRunner::new().on(
            "better-sidepanels",
            &["--check", "--profile", "work", "--config", &path_text],
            CommandResult::output(
                "better-sidepanels",
                ["--check", "--profile", "work"],
                0,
                "layout work: 2 panels\n",
                "",
            ),
        );
        let text = check_sidepanels_layout(&runner, path, "work").unwrap();
        assert!(text.contains("2 panels"));
    }

    #[test]
    fn portal_status_reads_unit_and_selection() {
        let dir = temp_dir("portal");
        let conf = dir.join("portals.conf");
        std::fs::write(
            &conf,
            "[preferred]\ndefault=gnome;gtk;\nscreenshot=org.freedesktop.impl.portal.desktop.better\n",
        )
        .unwrap();
        let runner = ScriptedRunner::new()
            .on(
                "systemctl",
                &["--user", "is-active", "better-portal"],
                CommandResult::output(
                    "systemctl",
                    ["--user", "is-active", "better-portal"],
                    0,
                    "active\n",
                    "",
                ),
            )
            .on(
                "systemctl",
                &["--user", "is-enabled", "better-portal"],
                CommandResult::output(
                    "systemctl",
                    ["--user", "is-enabled", "better-portal"],
                    0,
                    "enabled\n",
                    "",
                ),
            );
        let status = portal_status(&runner, &conf);
        assert_eq!(status.active.as_deref(), Some("active"));
        assert_eq!(status.enabled.as_deref(), Some("enabled"));
        assert!(status.config_exists);
        assert!(status.selected);
        let view = portal_view(&runner, &conf);
        assert_eq!(view.status, "running");
        assert!(view.lines.iter().any(|line| line.contains("portals.conf")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn noctalia_source_lists_plugins() {
        let dir = temp_dir("noctalia");
        std::fs::create_dir_all(dir.join("better-audio")).unwrap();
        std::fs::write(dir.join("better-audio/plugin.toml"), "id = \"audio\"\n").unwrap();
        std::fs::create_dir_all(dir.join("better-sticky")).unwrap();
        std::fs::write(dir.join("better-sticky/plugin.toml"), "id = \"sticky\"\n").unwrap();
        std::fs::create_dir_all(dir.join("not-a-plugin")).unwrap();
        std::fs::write(dir.join("verify.sh"), "#!/bin/sh\n").unwrap();
        let source = read_noctalia_source(&dir);
        assert!(source.exists);
        assert!(source.verify_script);
        assert_eq!(source.plugins, vec!["better-audio", "better-sticky"]);
        let view = noctalia_view(&dir);
        assert!(view.status.contains("2 plugin"));
        let missing = noctalia_view(Path::new("/tmp/no-such-noctalia-source"));
        assert_eq!(missing.status, "plugin source not installed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overview_status_counts_companions_without_failing() {
        let runner = reference_runner()
            .on(
                "better-session",
                &["history", "list"],
                result(
                    "better-session",
                    &["history", "list"],
                    &fixture("session-history.json"),
                ),
            )
            .on(
                "niri",
                &["msg", "--json", "version"],
                result(
                    "niri",
                    &["msg", "--json", "version"],
                    &fixture("niri-version.json"),
                ),
            );
        let conf = Path::new("/tmp/no-portals.conf");
        let status = overview_status(&runner, conf);
        assert_eq!(status.profiles, 2);
        assert_eq!(status.contexts, 2);
        assert_eq!(status.scenes, 2);
        assert_eq!(status.sessions_history, 2);
        assert_eq!(
            status.compositor.compositor_version.as_deref(),
            Some("better-niri 26.1")
        );
        assert_eq!(status.companion.status, "daemon running");
        assert!(status.tools.status.contains("installed"));
    }

    #[test]
    fn dispatch_exposes_the_new_sections_and_gates_mutations() {
        let runner = niri_runner();
        let call = |args: &[&str]| {
            dispatch(
                &runner,
                &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            )
        };
        assert!(
            call(&["compositor", "status"])
                .unwrap()
                .contains("compositor")
        );
        assert!(call(&["compositor", "reload"]).is_err());
        assert!(call(&["compositor", "reload", "--apply"]).is_ok());
        assert!(is_section("compositor"));
        assert!(is_section("portal"));
        assert!(is_section("noctalia"));
        assert!(is_section("overview"));
        assert!(usage().contains("companion idle|rotate"));
    }
}
