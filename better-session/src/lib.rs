//! `better-session`: save and restore named niri contexts.
//!
//! A context records workspaces, windows, output configuration (mode, scale,
//! transform, position, VRR, on/off) and optional pre/post restore hooks.
//! Restore applies outputs first, then workspaces, then windows, and polls
//! niri for windows that appear late instead of sleeping for a fixed time.
//!
//! Contexts live in a versioned JSON store under
//! `$XDG_STATE_HOME/better-session/contexts.json`. The older
//! `sessions.json` snapshot format is still read and migrated in memory.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use niri_ipc::{
    Action, ConfiguredMode, ConfiguredPosition, ModeToSet, Output, OutputAction, PositionToSet,
    Request, Response, ScaleToSet, Transform, VrrToSet, Window, Workspace, WorkspaceReferenceArg,
};
use serde::{Deserialize, Serialize, de};
use serde_json::Value;

/// Version of the on-disk context schema.
pub const SCHEMA_VERSION: u32 = 1;

/// Default time to wait for late windows when restoring.
pub const DEFAULT_WAIT_MS: u64 = 2_000;

/// Default poll interval while waiting for late windows.
pub const DEFAULT_POLL_MS: u64 = 50;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSnapshot {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub idx: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_focused: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowSnapshot {
    pub app_id: String,
    pub title: String,
    pub workspace_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    pub workspace_idx: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_output: Option<String>,
    #[serde(default)]
    pub is_floating: bool,
    #[serde(default)]
    pub is_focused: bool,
}

/// Output settings captured in a context.
///
/// The shape intentionally matches `better-monitor` profiles so a profile can
/// be copied into a context without depending on the `better-monitor` binary.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutputSnapshot {
    #[serde(default)]
    pub off: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<(u16, u16, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vrr: Option<bool>,
}

impl OutputSnapshot {
    /// Captures one live output. A missing logical output means it is off.
    pub fn from_output(output: &Output) -> Self {
        let Some(logical) = output.logical else {
            return Self {
                off: true,
                ..Self::default()
            };
        };
        let mode = output
            .current_mode
            .and_then(|index| output.modes.get(index))
            .map(|mode| (mode.width, mode.height, mode.refresh_rate));
        Self {
            off: false,
            mode,
            scale: Some(logical.scale),
            transform: Some(logical.transform),
            position: Some((logical.x, logical.y)),
            vrr: Some(output.vrr_enabled),
        }
    }
}

/// User hooks run around a restore.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hooks {
    /// Commands run before any output/workspace/window change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre: Vec<String>,
    /// Commands run after the restore finishes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post: Vec<String>,
}

impl Hooks {
    fn is_empty(&self) -> bool {
        self.pre.is_empty() && self.post.is_empty()
    }
}

/// A named context: windows, workspaces, outputs and hooks.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Context {
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub command_map: BTreeMap<String, String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSnapshot>,
    #[serde(default)]
    pub windows: Vec<WindowSnapshot>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, OutputSnapshot>,
    #[serde(default, skip_serializing_if = "Hooks::is_empty")]
    pub hooks: Hooks,
}

/// Backwards-compatible name for the window/workspace snapshot.
pub type Session = Context;

/// Versioned on-disk store.
#[derive(Debug, Serialize)]
pub struct Store {
    pub version: u32,
    #[serde(default)]
    pub contexts: BTreeMap<String, Context>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            contexts: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize)]
struct RawStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    contexts: BTreeMap<String, Context>,
    /// Legacy `sessions.json` shape.
    #[serde(default)]
    sessions: BTreeMap<String, Context>,
}

impl<'de> Deserialize<'de> for Store {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        let raw = RawStore::deserialize(deserializer)?;
        if raw.version > SCHEMA_VERSION {
            return Err(de::Error::custom(format!(
                "unsupported better-session schema version {} (this build supports {SCHEMA_VERSION})",
                raw.version
            )));
        }
        let mut contexts = raw.contexts;
        for (name, context) in raw.sessions {
            contexts.entry(name).or_insert(context);
        }
        Ok(Self {
            version: SCHEMA_VERSION,
            contexts,
        })
    }
}

fn state_dir() -> PathBuf {
    let dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".local/state")
        });
    dir.join("better-session")
}

/// State path: `$XDG_STATE_HOME/better-session/contexts.json`.
pub fn state_path() -> PathBuf {
    state_dir().join("contexts.json")
}

/// Legacy snapshot path, read when `contexts.json` does not exist yet.
pub fn legacy_state_path() -> PathBuf {
    state_dir().join("sessions.json")
}

impl Store {
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Self::parse(&bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // Only fall back to the legacy file next to the default contexts file.
                if path.file_name() != Some(OsStr::new("contexts.json")) {
                    return Ok(Self::default());
                }
                let legacy = path.with_file_name("sessions.json");
                match std::fs::read(&legacy) {
                    Ok(bytes) => Self::parse(&bytes),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
                    Err(err) => Err(err),
                }
            }
            Err(err) => Err(err),
        }
    }

    fn parse(bytes: &[u8]) -> io::Result<Self> {
        serde_json::from_slice(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, path)
    }

    pub fn get(&self, name: &str) -> Option<&Context> {
        self.contexts.get(name)
    }

    pub fn insert(&mut self, name: impl Into<String>, context: Context) {
        self.contexts.insert(name.into(), context);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.contexts.remove(name).is_some()
    }

    /// Renames a context. Fails when the old name is unknown or the new name exists.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let Some(context) = self.contexts.remove(from) else {
            return Err(format!("unknown context `{from}`"));
        };
        if from == to {
            self.contexts.insert(from.to_owned(), context);
            return Ok(());
        }
        if self.contexts.contains_key(to) {
            self.contexts.insert(from.to_owned(), context);
            return Err(format!("context `{to}` already exists"));
        }
        self.contexts.insert(to.to_owned(), context);
        Ok(())
    }
}

pub type Outputs = BTreeMap<String, Output>;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn workspace_sort_key(workspace: &WorkspaceSnapshot) -> (String, u8, String, u64) {
    (
        workspace.output.clone().unwrap_or_default(),
        workspace.idx,
        workspace.name.clone().unwrap_or_default(),
        workspace.id,
    )
}

fn window_sort_key(window: &WindowSnapshot) -> (String, u8, String, String, String) {
    (
        window.workspace_output.clone().unwrap_or_default(),
        window.workspace_idx,
        window.workspace_name.clone().unwrap_or_default(),
        window.app_id.clone(),
        window.title.clone(),
    )
}

/// Captures live niri state, including outputs, into a context.
pub fn capture(
    workspaces: &[Workspace],
    windows: &[Window],
    outputs: &Outputs,
    command_map: BTreeMap<String, String>,
    hooks: Hooks,
) -> Context {
    let by_id: BTreeMap<u64, &Workspace> = workspaces.iter().map(|ws| (ws.id, ws)).collect();

    let mut workspace_snapshots: Vec<WorkspaceSnapshot> = workspaces
        .iter()
        .map(|ws| WorkspaceSnapshot {
            id: ws.id,
            name: ws.name.clone(),
            idx: ws.idx,
            output: ws.output.clone(),
            is_active: ws.is_active,
            is_focused: ws.is_focused,
        })
        .collect();
    workspace_snapshots.sort_by_key(workspace_sort_key);

    let mut window_snapshots: Vec<WindowSnapshot> = windows
        .iter()
        .map(|win| {
            let workspace = win.workspace_id.and_then(|id| by_id.get(&id).copied());
            WindowSnapshot {
                app_id: win.app_id.clone().unwrap_or_default(),
                title: win.title.clone().unwrap_or_default(),
                workspace_id: win.workspace_id.unwrap_or(0),
                workspace_name: workspace.and_then(|ws| ws.name.clone()),
                workspace_idx: workspace.map_or(0, |ws| ws.idx),
                workspace_output: workspace.and_then(|ws| ws.output.clone()),
                is_floating: win.is_floating,
                is_focused: win.is_focused,
            }
        })
        .collect();
    window_snapshots.sort_by_key(window_sort_key);

    let timestamp = now_secs();
    Context {
        created_at: timestamp,
        updated_at: timestamp,
        command_map,
        workspaces: workspace_snapshots,
        windows: window_snapshots,
        outputs: outputs
            .iter()
            .map(|(name, output)| (name.clone(), OutputSnapshot::from_output(output)))
            .collect(),
        hooks,
    }
}

/// Legacy capture helper without outputs or hooks.
pub fn snapshot(
    workspaces: &[Workspace],
    windows: &[Window],
    command_map: BTreeMap<String, String>,
) -> Context {
    capture(
        workspaces,
        windows,
        &BTreeMap::new(),
        command_map,
        Hooks::default(),
    )
}

fn same_window(saved: &WindowSnapshot, current: &Window) -> bool {
    if current.app_id.as_deref().unwrap_or_default() != saved.app_id {
        return false;
    }
    if saved.title.is_empty() {
        return true;
    }
    current.title.as_deref().unwrap_or_default() == saved.title
}

fn workspace_reference(saved: &WindowSnapshot) -> WorkspaceReferenceArg {
    match &saved.workspace_name {
        Some(name) => WorkspaceReferenceArg::Name(name.clone()),
        None => WorkspaceReferenceArg::Index(saved.workspace_idx),
    }
}

fn workspace_matches(saved: &WindowSnapshot, current: &Window, workspaces: &[Workspace]) -> bool {
    let Some(workspace_id) = current.workspace_id else {
        return false;
    };
    let Some(workspace) = workspaces.iter().find(|ws| ws.id == workspace_id) else {
        return false;
    };
    if let Some(name) = &saved.workspace_name {
        return workspace.name.as_deref() == Some(name.as_str());
    }
    workspace.idx == saved.workspace_idx
        && saved
            .workspace_output
            .as_deref()
            .is_none_or(|output| workspace.output.as_deref() == Some(output))
}

fn find_match<'a>(
    saved: &WindowSnapshot,
    current: &'a [Window],
    workspaces: &[Workspace],
    used: &BTreeSet<u64>,
) -> Option<&'a Window> {
    let mut candidates: Vec<&Window> = current
        .iter()
        .filter(|window| !used.contains(&window.id) && same_window(saved, window))
        .collect();
    candidates.sort_by_key(|window| {
        let on_saved_workspace = workspace_matches(saved, window, workspaces) as u8;
        let focused = window.is_focused as u8;
        (
            std::cmp::Reverse(on_saved_workspace),
            std::cmp::Reverse(focused),
            window.id,
        )
    });
    candidates.first().copied()
}

/// Planning phase, in restore order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Output,
    Workspace,
    Window,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Output => "output",
            Self::Workspace => "workspace",
            Self::Window => "window",
        }
    }
}

/// One request to send to niri.
#[derive(Debug, Clone)]
pub struct Step {
    pub phase: Phase,
    /// Output name, workspace name/index or app id.
    pub target: String,
    pub action: String,
    pub request: Request,
}

/// A target whose saved state already matches (or cannot be handled).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Skipped {
    pub phase: Phase,
    pub target: String,
    pub reason: String,
}

/// A saved target with no live match and no launch command.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Unmatched {
    pub phase: Phase,
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct WindowPlan {
    /// Spawn requests, executed before waiting for late windows.
    pub spawns: Vec<Step>,
    /// Window moves, floating changes and focus.
    pub steps: Vec<Step>,
    pub skipped: Vec<Skipped>,
    pub unmatched: Vec<Unmatched>,
}

#[derive(Debug, Default)]
pub struct ContextPlan {
    pub steps: Vec<Step>,
    pub skipped: Vec<Skipped>,
    pub unmatched: Vec<Unmatched>,
}

fn output_note(action: &OutputAction) -> String {
    match action {
        OutputAction::Off => "turn off".to_owned(),
        OutputAction::On => "turn on".to_owned(),
        OutputAction::Mode { .. } => "set mode".to_owned(),
        OutputAction::Scale { .. } => "set scale".to_owned(),
        OutputAction::Transform { .. } => "set transform".to_owned(),
        OutputAction::Position { .. } => "set position".to_owned(),
        OutputAction::Vrr { .. } => "set VRR".to_owned(),
        other => format!("{other:?}"),
    }
}

fn output_step(name: &str, action: OutputAction) -> Step {
    Step {
        phase: Phase::Output,
        target: name.to_owned(),
        action: output_note(&action),
        request: Request::Output {
            output: name.to_owned(),
            action,
        },
    }
}

fn current_mode(output: &Output) -> Option<(u16, u16, u32)> {
    output
        .current_mode
        .and_then(|index| output.modes.get(index))
        .map(|mode| (mode.width, mode.height, mode.refresh_rate))
}

/// Plans output changes, sorted by output name.
///
/// Within one output the order is on/off, mode, scale, transform, position,
/// VRR. Settings that already match are not emitted.
pub fn plan_outputs(context: &Context, current: &Outputs) -> (Vec<Step>, Vec<Skipped>) {
    let mut steps = Vec::new();
    let mut skipped = Vec::new();

    for (name, desired) in &context.outputs {
        let Some(output) = current.get(name) else {
            skipped.push(Skipped {
                phase: Phase::Output,
                target: name.clone(),
                reason: "output not connected".to_owned(),
            });
            continue;
        };

        if desired.off {
            if output.logical.is_none() {
                skipped.push(Skipped {
                    phase: Phase::Output,
                    target: name.clone(),
                    reason: "already off".to_owned(),
                });
            } else {
                steps.push(output_step(name, OutputAction::Off));
            }
            continue;
        }

        let mut output_steps = Vec::new();
        if output.logical.is_none() {
            output_steps.push(output_step(name, OutputAction::On));
        }
        if let Some((width, height, refresh_rate)) = desired.mode
            && current_mode(output) != Some((width, height, refresh_rate))
        {
            output_steps.push(output_step(
                name,
                OutputAction::Mode {
                    mode: ModeToSet::Specific(ConfiguredMode {
                        width,
                        height,
                        refresh: Some(refresh_rate as f64 / 1000.),
                    }),
                },
            ));
        }
        if let Some(scale) = desired.scale
            && output.logical.map(|logical| logical.scale) != Some(scale)
        {
            output_steps.push(output_step(
                name,
                OutputAction::Scale {
                    scale: ScaleToSet::Specific(scale),
                },
            ));
        }
        if let Some(transform) = desired.transform
            && output.logical.map(|logical| logical.transform) != Some(transform)
        {
            output_steps.push(output_step(name, OutputAction::Transform { transform }));
        }
        if let Some((x, y)) = desired.position
            && output.logical.map(|logical| (logical.x, logical.y)) != Some((x, y))
        {
            output_steps.push(output_step(
                name,
                OutputAction::Position {
                    position: PositionToSet::Specific(ConfiguredPosition { x, y }),
                },
            ));
        }
        if let Some(vrr) = desired.vrr
            && output.vrr_enabled != vrr
        {
            output_steps.push(output_step(
                name,
                OutputAction::Vrr {
                    vrr: VrrToSet {
                        vrr,
                        on_demand: false,
                    },
                },
            ));
        }

        if output_steps.is_empty() {
            skipped.push(Skipped {
                phase: Phase::Output,
                target: name.clone(),
                reason: "already applied".to_owned(),
            });
        } else {
            steps.extend(output_steps);
        }
    }

    (steps, skipped)
}

fn reference_key(reference: &WorkspaceReferenceArg) -> String {
    match reference {
        WorkspaceReferenceArg::Id(id) => format!("id:{id}"),
        WorkspaceReferenceArg::Index(index) => format!("idx:{index}"),
        WorkspaceReferenceArg::Name(name) => format!("name:{name}"),
    }
}

fn workspace_target(saved: &WorkspaceSnapshot) -> String {
    saved
        .name
        .clone()
        .unwrap_or_else(|| format!("index {}", saved.idx))
}

fn saved_workspace_matches(saved: &WorkspaceSnapshot, current: &Workspace) -> bool {
    if let Some(name) = &saved.name {
        return current.name.as_deref() == Some(name.as_str());
    }
    current.idx == saved.idx
        && saved
            .output
            .as_deref()
            .is_none_or(|output| current.output.as_deref() == Some(output))
}

fn workspace_exists(saved: &WorkspaceSnapshot, current: &[Workspace]) -> bool {
    current
        .iter()
        .any(|workspace| saved_workspace_matches(saved, workspace))
}

/// Plans workspace creation and activation, in stable output/index order.
pub fn plan_workspaces(context: &Context, current: &[Workspace]) -> (Vec<Step>, Vec<Skipped>) {
    let mut steps = Vec::new();
    let mut skipped = Vec::new();
    let mut planned = BTreeSet::new();

    let mut saved: Vec<&WorkspaceSnapshot> = context.workspaces.iter().collect();
    saved.sort_by_key(|workspace| workspace_sort_key(workspace));

    let mut push_focus = |workspace: &WorkspaceSnapshot, steps: &mut Vec<Step>| {
        let reference = match &workspace.name {
            Some(name) => WorkspaceReferenceArg::Name(name.clone()),
            None => WorkspaceReferenceArg::Index(workspace.idx),
        };
        if !planned.insert(reference_key(&reference)) {
            return;
        }
        let target = workspace_target(workspace);
        steps.push(Step {
            phase: Phase::Workspace,
            target: target.clone(),
            action: format!("focus workspace {target}"),
            request: Request::Action(Action::FocusWorkspace { reference }),
        });
    };

    // 1. Ensure named workspaces exist; unnamed workspaces fall back to an index.
    for workspace in &saved {
        if !workspace_exists(workspace, current) {
            push_focus(workspace, &mut steps);
        }
    }

    // 2. Restore the active workspace on each output.
    let outputs: BTreeSet<String> = saved
        .iter()
        .filter(|workspace| workspace.is_active)
        .filter_map(|workspace| workspace.output.clone())
        .collect();
    for output in outputs {
        let Some(desired) = saved.iter().find(|workspace| {
            workspace.is_active && workspace.output.as_deref() == Some(output.as_str())
        }) else {
            continue;
        };
        let current_active = current
            .iter()
            .find(|workspace| workspace.is_active && workspace.output.as_deref() == Some(&output));
        if current_active.is_some_and(|workspace| saved_workspace_matches(desired, workspace)) {
            skipped.push(Skipped {
                phase: Phase::Workspace,
                target: workspace_target(desired),
                reason: "already active".to_owned(),
            });
        } else {
            push_focus(desired, &mut steps);
        }
    }

    // 3. Restore the focused workspace last.
    if let Some(desired) = saved.iter().find(|workspace| workspace.is_focused) {
        let current_focused = current.iter().find(|workspace| workspace.is_focused);
        if current_focused.is_some_and(|workspace| saved_workspace_matches(desired, workspace)) {
            skipped.push(Skipped {
                phase: Phase::Workspace,
                target: workspace_target(desired),
                reason: "already focused".to_owned(),
            });
        } else {
            push_focus(desired, &mut steps);
        }
    }

    (steps, skipped)
}

/// Plans window spawns and moves against the current niri state.
pub fn plan_windows(
    context: &Context,
    current_workspaces: &[Workspace],
    current_windows: &[Window],
    allow_spawn: bool,
) -> WindowPlan {
    let mut plan = WindowPlan::default();
    let mut used = BTreeSet::new();
    let mut focused_window: Option<(u64, String)> = None;

    for saved in &context.windows {
        let Some(current) = find_match(saved, current_windows, current_workspaces, &used) else {
            if allow_spawn {
                if let Some(command) = context.command_map.get(&saved.app_id) {
                    plan.spawns.push(Step {
                        phase: Phase::Window,
                        target: saved.app_id.clone(),
                        action: format!("spawn {}", saved.app_id),
                        request: Request::Action(Action::SpawnSh {
                            command: command.clone(),
                        }),
                    });
                } else {
                    plan.unmatched.push(Unmatched {
                        phase: Phase::Window,
                        target: saved.app_id.clone(),
                        reason: "no live window and no launch command".to_owned(),
                    });
                }
            } else {
                plan.unmatched.push(Unmatched {
                    phase: Phase::Window,
                    target: saved.app_id.clone(),
                    reason: "no live window".to_owned(),
                });
            }
            continue;
        };
        used.insert(current.id);

        let mut changed = false;
        if !workspace_matches(saved, current, current_workspaces) {
            plan.steps.push(Step {
                phase: Phase::Window,
                target: saved.app_id.clone(),
                action: format!("move window {} to workspace", current.id),
                request: Request::Action(Action::MoveWindowToWorkspace {
                    window_id: Some(current.id),
                    reference: workspace_reference(saved),
                    focus: false,
                }),
            });
            changed = true;
        }
        if current.is_floating != saved.is_floating {
            plan.steps.push(Step {
                phase: Phase::Window,
                target: saved.app_id.clone(),
                action: format!("set floating {}", current.id),
                request: Request::Action(if saved.is_floating {
                    Action::MoveWindowToFloating {
                        id: Some(current.id),
                    }
                } else {
                    Action::MoveWindowToTiling {
                        id: Some(current.id),
                    }
                }),
            });
            changed = true;
        }
        if saved.is_focused {
            focused_window = Some((current.id, saved.app_id.clone()));
        }
        if !changed && !saved.is_focused {
            plan.skipped.push(Skipped {
                phase: Phase::Window,
                target: saved.app_id.clone(),
                reason: "already in place".to_owned(),
            });
        }
    }

    if let Some((id, target)) = focused_window {
        if current_windows
            .iter()
            .any(|window| window.id == id && window.is_focused)
        {
            plan.skipped.push(Skipped {
                phase: Phase::Window,
                target,
                reason: "already focused".to_owned(),
            });
        } else {
            plan.steps.push(Step {
                phase: Phase::Window,
                target,
                action: format!("focus window {id}"),
                request: Request::Action(Action::FocusWindow { id }),
            });
        }
    }

    plan
}

/// Plans every phase in restore order: outputs, workspaces, then windows.
pub fn plan_context(
    context: &Context,
    current_workspaces: &[Workspace],
    current_windows: &[Window],
    current_outputs: &Outputs,
) -> ContextPlan {
    let mut plan = ContextPlan::default();

    let (steps, skipped) = plan_outputs(context, current_outputs);
    plan.steps.extend(steps);
    plan.skipped.extend(skipped);

    let (steps, skipped) = plan_workspaces(context, current_workspaces);
    plan.steps.extend(steps);
    plan.skipped.extend(skipped);

    let windows = plan_windows(context, current_workspaces, current_windows, true);
    plan.steps.extend(windows.spawns);
    plan.steps.extend(windows.steps);
    plan.skipped.extend(windows.skipped);
    plan.unmatched.extend(windows.unmatched);

    plan
}

/// Legacy window/workspace planner kept for the original `Session` API.
pub fn plan(
    session: &Context,
    current_workspaces: &[Workspace],
    current_windows: &[Window],
) -> (Vec<(String, Action)>, Vec<String>) {
    let mut actions = Vec::new();
    let mut missing = Vec::new();

    let mut named: Vec<&str> = session
        .workspaces
        .iter()
        .filter_map(|ws| ws.name.as_deref())
        .collect();
    named.sort_unstable();
    named.dedup();
    for name in named {
        actions.push((
            format!("focus workspace {name}"),
            Action::FocusWorkspace {
                reference: WorkspaceReferenceArg::Name(name.to_owned()),
            },
        ));
    }

    let mut focused_current = None;
    for saved in &session.windows {
        let matched = current_windows
            .iter()
            .find(|current| same_window(saved, current));
        let Some(current) = matched else {
            if let Some(command) = session.command_map.get(&saved.app_id) {
                actions.push((
                    format!("spawn {}", saved.app_id),
                    Action::SpawnSh {
                        command: command.clone(),
                    },
                ));
            } else {
                missing.push(saved.app_id.clone());
            }
            continue;
        };

        if !workspace_matches(saved, current, current_workspaces) {
            actions.push((
                format!("move window {} to workspace", current.id),
                Action::MoveWindowToWorkspace {
                    window_id: Some(current.id),
                    reference: workspace_reference(saved),
                    focus: false,
                },
            ));
        }
        if current.is_floating != saved.is_floating {
            actions.push((
                format!("set floating {}", current.id),
                if saved.is_floating {
                    Action::MoveWindowToFloating {
                        id: Some(current.id),
                    }
                } else {
                    Action::MoveWindowToTiling {
                        id: Some(current.id),
                    }
                },
            ));
        }
        if saved.is_focused {
            focused_current = Some(current.id);
        }
    }

    if let Some(id) = focused_current {
        actions.push((format!("focus window {id}"), Action::FocusWindow { id }));
    }

    (actions, missing)
}

pub fn send(socket: &Path, request: Request) -> Result<Response, String> {
    use niri_ipc::socket::Socket;
    let mut socket =
        Socket::connect_to(socket).map_err(|err| format!("cannot connect to niri IPC: {err}"))?;
    let reply = socket
        .send(request)
        .map_err(|err| format!("niri IPC error: {err}"))?;
    reply.map_err(|err| format!("niri returned an error: {err}"))
}

pub fn query(socket: &Path) -> Result<(Vec<Workspace>, Vec<Window>, Outputs), String> {
    let outputs = match send(socket, Request::Outputs)? {
        Response::Outputs(outputs) => outputs.into_iter().collect(),
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    let workspaces = match send(socket, Request::Workspaces)? {
        Response::Workspaces(workspaces) => workspaces,
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    let windows = match send(socket, Request::Windows)? {
        Response::Windows(windows) => windows,
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    Ok((workspaces, windows, outputs))
}

/// One applied (or, in dry-run, planned) action.
#[derive(Debug, Clone, Serialize)]
pub struct Applied {
    pub phase: Phase,
    pub target: String,
    pub action: String,
    pub request: Value,
}

/// Sends planned steps, or only records them when `dry_run` is set.
pub fn execute_steps(socket: &Path, steps: &[Step], dry_run: bool) -> Result<Vec<Applied>, String> {
    let mut applied = Vec::with_capacity(steps.len());
    for step in steps {
        if !dry_run {
            match send(socket, step.request.clone())? {
                Response::Handled | Response::OutputConfigChanged(_) => (),
                other => return Err(format!("unexpected niri response: {other:?}")),
            }
        }
        applied.push(Applied {
            phase: step.phase,
            target: step.target.clone(),
            action: step.action.clone(),
            request: serde_json::to_value(&step.request).unwrap_or(Value::Null),
        });
    }
    Ok(applied)
}

/// Legacy action executor.
pub fn execute(
    socket: &Path,
    actions: &[(String, Action)],
    dry_run: bool,
) -> Result<Vec<Value>, String> {
    let steps: Vec<Step> = actions
        .iter()
        .map(|(label, action)| Step {
            phase: Phase::Window,
            target: label.clone(),
            action: label.clone(),
            request: Request::Action(action.clone()),
        })
        .collect();
    let applied = execute_steps(socket, &steps, dry_run)?;
    Ok(applied.into_iter().map(|applied| applied.request).collect())
}

/// Runs one hook command with the context environment.
pub fn run_hook(command: &str, context: &str, phase: &str) -> Result<(), String> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("BETTER_SESSION_CONTEXT", context)
        .env("BETTER_SESSION_PHASE", phase)
        .status()
        .map_err(|err| format!("cannot run {phase} hook `{command}`: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{phase} hook exited with {status}: {command}"))
    }
}

/// Result of running one hook phase.
#[derive(Debug, Clone, Serialize)]
pub struct HookStatus {
    /// `ok`, `failed`, `skipped` or `none`.
    pub status: String,
    pub commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HookStatus {
    fn none() -> Self {
        Self {
            status: "none".to_owned(),
            commands: Vec::new(),
            error: None,
        }
    }

    fn skipped(commands: &[String], reason: &str) -> Self {
        Self {
            status: "skipped".to_owned(),
            commands: commands.to_vec(),
            error: Some(reason.to_owned()),
        }
    }

    fn failed(commands: &[String], error: String) -> Self {
        Self {
            status: "failed".to_owned(),
            commands: commands.to_vec(),
            error: Some(error),
        }
    }

    fn ok(commands: &[String]) -> Self {
        Self {
            status: "ok".to_owned(),
            commands: commands.to_vec(),
            error: None,
        }
    }
}

fn run_hooks(context: &str, phase: &str, commands: &[String]) -> HookStatus {
    if commands.is_empty() {
        return HookStatus::none();
    }
    for command in commands {
        if let Err(error) = run_hook(command, context, phase) {
            return HookStatus::failed(commands, error);
        }
    }
    HookStatus::ok(commands)
}

#[derive(Debug, Clone, Serialize)]
pub struct HookReport {
    pub pre: HookStatus,
    pub post: HookStatus,
}

/// Restore options.
#[derive(Debug, Clone)]
pub struct RestoreOptions {
    /// Plan without changing niri.
    pub dry_run: bool,
    /// Explain the plan; implies `dry_run`.
    pub explain: bool,
    /// Continue after a failing pre hook.
    pub force: bool,
    /// How long to poll for late windows. `0` disables waiting.
    pub wait_ms: u64,
    /// Poll interval while waiting.
    pub poll_ms: u64,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            explain: false,
            force: false,
            wait_ms: DEFAULT_WAIT_MS,
            poll_ms: DEFAULT_POLL_MS,
        }
    }
}

/// JSON restore report.
#[derive(Debug, Serialize)]
pub struct RestoreReport {
    pub context: String,
    pub dry_run: bool,
    pub explain: bool,
    pub wait_ms: u64,
    pub aborted: bool,
    pub ok: bool,
    pub applied: Vec<Applied>,
    pub skipped: Vec<Skipped>,
    pub unmatched: Vec<Unmatched>,
    pub hooks: HookReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RestoreReport {
    fn new(name: &str, options: &RestoreOptions) -> Self {
        Self {
            context: name.to_owned(),
            dry_run: options.dry_run || options.explain,
            explain: options.explain,
            wait_ms: options.wait_ms,
            aborted: false,
            ok: true,
            applied: Vec::new(),
            skipped: Vec::new(),
            unmatched: Vec::new(),
            hooks: HookReport {
                pre: HookStatus::none(),
                post: HookStatus::none(),
            },
            error: None,
        }
    }
}

/// Restore outcome: the report plus an optional hook failure for the CLI exit code.
#[derive(Debug)]
pub struct RestoreOutcome {
    pub report: RestoreReport,
    pub failure: Option<String>,
}

fn all_windows_matched(context: &Context, current_windows: &[Window]) -> bool {
    let mut used = BTreeSet::new();
    for saved in &context.windows {
        let Some(current) = find_match(saved, current_windows, &[], &used) else {
            return false;
        };
        used.insert(current.id);
    }
    true
}

/// Restores a context against a live niri socket.
///
/// Outputs are applied first, then workspaces, then windows. Spawned or late
/// windows are awaited by polling `Request::Windows` up to `wait_ms`.
pub fn restore_context(
    socket: &Path,
    name: &str,
    context: &Context,
    options: &RestoreOptions,
) -> Result<RestoreOutcome, String> {
    let dry_run = options.dry_run || options.explain;
    let mut report = RestoreReport::new(name, options);

    if dry_run {
        report.hooks.pre = HookStatus::skipped(&context.hooks.pre, "dry-run");
        report.hooks.post = HookStatus::skipped(&context.hooks.post, "dry-run");
        let (workspaces, windows, outputs) = query(socket)?;
        let plan = plan_context(context, &workspaces, &windows, &outputs);
        report.applied = execute_steps(socket, &plan.steps, true)?;
        report.skipped = plan.skipped;
        report.unmatched = plan.unmatched;
        return Ok(RestoreOutcome {
            report,
            failure: None,
        });
    }

    let mut hook_failure = None;

    // 1. Pre hooks. A failure aborts before touching the compositor unless forced.
    report.hooks.pre = run_hooks(name, "pre", &context.hooks.pre);
    if report.hooks.pre.status == "failed" {
        let message = report
            .hooks
            .pre
            .error
            .clone()
            .unwrap_or_else(|| "pre hook failed".to_owned());
        if !options.force {
            report.aborted = true;
            report.ok = false;
            report.error = Some(message.clone());
            return Ok(RestoreOutcome {
                report,
                failure: Some(message),
            });
        }
        hook_failure = Some(format!("{message} (continuing due to --force)"));
    }

    let (mut workspaces, mut windows, outputs) = query(socket)?;

    // 2. Outputs.
    let (steps, skipped) = plan_outputs(context, &outputs);
    report.applied.extend(execute_steps(socket, &steps, false)?);
    report.skipped.extend(skipped);

    // 3. Workspaces.
    let (steps, skipped) = plan_workspaces(context, &workspaces);
    report.applied.extend(execute_steps(socket, &steps, false)?);
    report.skipped.extend(skipped);

    // 4. Spawn missing apps, then wait for late windows by polling.
    let initial = plan_windows(context, &workspaces, &windows, true);
    report
        .applied
        .extend(execute_steps(socket, &initial.spawns, false)?);

    let missing_before = !all_windows_matched(context, &windows);
    let should_wait = options.wait_ms > 0 && missing_before;
    if !initial.spawns.is_empty() || should_wait {
        let (mut latest_workspaces, mut latest_windows, _) = query(socket)?;
        if should_wait {
            let deadline = Instant::now() + Duration::from_millis(options.wait_ms);
            while !all_windows_matched(context, &latest_windows) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(options.poll_ms.max(1)));
                let (next_workspaces, next_windows, _) = query(socket)?;
                latest_workspaces = next_workspaces;
                latest_windows = next_windows;
            }
        }
        workspaces = latest_workspaces;
        windows = latest_windows;
    }

    // 5. Windows.
    let final_plan = plan_windows(context, &workspaces, &windows, false);
    report
        .applied
        .extend(execute_steps(socket, &final_plan.steps, false)?);
    report.skipped.extend(final_plan.skipped);
    report.unmatched.extend(final_plan.unmatched);

    // 6. Post hooks. Failures are reported but do not roll anything back.
    report.hooks.post = run_hooks(name, "post", &context.hooks.post);
    if report.hooks.post.status == "failed" {
        let message = report
            .hooks
            .post
            .error
            .clone()
            .unwrap_or_else(|| "post hook failed".to_owned());
        hook_failure.get_or_insert(message);
    }

    if let Some(message) = &hook_failure {
        report.ok = false;
        report.error = Some(message.clone());
    }

    Ok(RestoreOutcome {
        report,
        failure: hook_failure,
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use niri_ipc::{LogicalOutput, OutputConfigChanged, Reply};
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("better-session-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_path(name: &str) -> PathBuf {
        temp_dir(name).join("contexts.json")
    }

    fn workspace(id: u64, idx: u8, name: Option<&str>) -> Workspace {
        workspace_state(id, idx, name, "eDP-1", idx == 1, idx == 1)
    }

    fn workspace_state(
        id: u64,
        idx: u8,
        name: Option<&str>,
        output: &str,
        active: bool,
        focused: bool,
    ) -> Workspace {
        serde_json::from_value(json!({
            "id": id,
            "idx": idx,
            "name": name,
            "output": output,
            "is_urgent": false,
            "is_active": active,
            "is_focused": focused,
            "active_window_id": null,
            "is_hidden": false
        }))
        .unwrap()
    }

    fn window(id: u64, app_id: &str, workspace_id: u64, floating: bool) -> Window {
        window_state(id, app_id, workspace_id, floating, true)
    }

    fn window_state(
        id: u64,
        app_id: &str,
        workspace_id: u64,
        floating: bool,
        focused: bool,
    ) -> Window {
        serde_json::from_value(json!({
            "id": id,
            "title": format!("{app_id}-title"),
            "app_id": app_id,
            "pid": 1,
            "workspace_id": workspace_id,
            "is_focused": focused,
            "is_floating": floating,
            "is_sticky": false,
            "is_urgent": false,
            "is_minimized": false,
            "layout": {
                "pos_in_scrolling_layout": [1, 1],
                "tile_size": [100.0, 100.0],
                "window_size": [100, 100],
                "tile_pos_in_workspace_view": [0.0, 0.0],
                "window_offset_in_tile": [0.0, 0.0]
            },
            "focus_timestamp": null
        }))
        .unwrap()
    }

    fn output(name: &str, scale: f64, vrr: bool) -> Output {
        serde_json::from_value(json!({
            "name": name,
            "make": "test",
            "model": "test",
            "serial": null,
            "physical_size": null,
            "modes": [
                { "width": 1920, "height": 1080, "refresh_rate": 60000, "is_preferred": true }
            ],
            "current_mode": 0,
            "is_custom_mode": false,
            "vrr_supported": true,
            "vrr_enabled": vrr,
            "logical": {
                "x": 0,
                "y": 0,
                "width": 1920,
                "height": 1080,
                "scale": scale,
                "transform": "Normal"
            },
            "max_bpc": null
        }))
        .unwrap()
    }

    fn outputs(items: impl IntoIterator<Item = Output>) -> Outputs {
        items
            .into_iter()
            .map(|output| (output.name.clone(), output))
            .collect()
    }

    #[derive(Default)]
    struct FakeState {
        outputs: Outputs,
        workspaces: Vec<Workspace>,
        windows: Vec<Window>,
        requests: Vec<Request>,
        late: Option<LateWindow>,
        next_workspace_id: u64,
    }

    struct LateWindow {
        window: Window,
        after_queries: usize,
        queries: usize,
        added: bool,
    }

    struct FakeNiri {
        path: PathBuf,
        state: Arc<std::sync::Mutex<FakeState>>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for FakeNiri {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn start_fake(name: &str, mut state: FakeState) -> FakeNiri {
        if state.next_workspace_id == 0 {
            state.next_workspace_id = 100;
        }
        let dir = temp_dir(name);
        let path = dir.join("niri.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(std::sync::Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => handle_fake(stream, &state),
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        FakeNiri {
            path,
            state,
            stop,
            thread: Some(thread),
        }
    }

    fn handle_fake(mut stream: UnixStream, state: &Arc<std::sync::Mutex<FakeState>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let Ok(request) = serde_json::from_str::<Request>(line.trim()) else {
            return;
        };
        let response = {
            let mut state = state.lock().unwrap();
            state.requests.push(request.clone());
            match request {
                Request::Outputs => Response::Outputs(state.outputs.clone().into_iter().collect()),
                Request::Workspaces => Response::Workspaces(state.workspaces.clone()),
                Request::Windows => Response::Windows(fake_windows(&mut state)),
                Request::Output { output, action } => {
                    apply_output(&mut state.outputs, &output, action);
                    Response::OutputConfigChanged(OutputConfigChanged::Applied)
                }
                Request::Action(action) => {
                    apply_action(&mut state, action);
                    Response::Handled
                }
                _ => Response::Handled,
            }
        };
        let reply: Reply = Ok(response);
        stream
            .write_all(serde_json::to_string(&reply).unwrap().as_bytes())
            .unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
    }

    fn fake_windows(state: &mut FakeState) -> Vec<Window> {
        let late = if let Some(late) = &mut state.late {
            late.queries += 1;
            if !late.added && late.queries >= late.after_queries {
                late.added = true;
                Some(late.window.clone())
            } else {
                None
            }
        } else {
            None
        };
        if let Some(window) = late {
            state.windows.push(window);
        }
        state.windows.clone()
    }

    fn apply_output(outputs: &mut Outputs, name: &str, action: OutputAction) {
        let Some(output) = outputs.get_mut(name) else {
            return;
        };
        match action {
            OutputAction::Off => output.logical = None,
            OutputAction::On => {
                if output.logical.is_none() {
                    output.logical = Some(LogicalOutput {
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 1080,
                        scale: 1.,
                        transform: Transform::Normal,
                    });
                }
            }
            OutputAction::Mode {
                mode: ModeToSet::Specific(configured),
            } => {
                if let Some(index) = output.modes.iter().position(|mode| {
                    mode.width == configured.width
                        && mode.height == configured.height
                        && configured.refresh.is_none_or(|refresh| {
                            (mode.refresh_rate as f64 / 1000. - refresh).abs() < 0.5
                        })
                }) {
                    output.current_mode = Some(index);
                }
            }
            OutputAction::Scale {
                scale: ScaleToSet::Specific(scale),
            } => {
                if let Some(logical) = &mut output.logical {
                    logical.scale = scale;
                }
            }
            OutputAction::Transform { transform } => {
                if let Some(logical) = &mut output.logical {
                    logical.transform = transform;
                }
            }
            OutputAction::Position {
                position: PositionToSet::Specific(position),
            } => {
                if let Some(logical) = &mut output.logical {
                    logical.x = position.x;
                    logical.y = position.y;
                }
            }
            OutputAction::Vrr { vrr } => output.vrr_enabled = vrr.vrr,
            _ => (),
        }
    }

    fn reference_matches(reference: &WorkspaceReferenceArg, workspace: &Workspace) -> bool {
        match reference {
            WorkspaceReferenceArg::Id(id) => workspace.id == *id,
            WorkspaceReferenceArg::Index(idx) => workspace.idx == *idx,
            WorkspaceReferenceArg::Name(name) => workspace.name.as_deref() == Some(name.as_str()),
        }
    }

    fn focus_workspace(state: &mut FakeState, reference: &WorkspaceReferenceArg) {
        let found = state
            .workspaces
            .iter()
            .position(|workspace| reference_matches(reference, workspace));
        let index = if let Some(index) = found {
            index
        } else {
            let output = state
                .workspaces
                .iter()
                .find(|workspace| workspace.is_focused)
                .and_then(|workspace| workspace.output.clone())
                .or_else(|| state.outputs.keys().next().cloned())
                .unwrap_or_else(|| "eDP-1".to_owned());
            let idx = match reference {
                WorkspaceReferenceArg::Index(idx) => *idx,
                _ => state
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.output.as_deref() == Some(output.as_str()))
                    .map(|workspace| workspace.idx)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1),
            };
            let name = match reference {
                WorkspaceReferenceArg::Name(name) => Some(name.clone()),
                _ => None,
            };
            let id = state.next_workspace_id;
            state.next_workspace_id += 1;
            state.workspaces.push(workspace_state(
                id,
                idx,
                name.as_deref(),
                &output,
                true,
                true,
            ));
            state.workspaces.len() - 1
        };

        for (position, workspace) in state.workspaces.iter_mut().enumerate() {
            workspace.is_active = position == index;
            workspace.is_focused = position == index;
        }
    }

    fn apply_action(state: &mut FakeState, action: Action) {
        match action {
            Action::FocusWorkspace { reference } => focus_workspace(state, &reference),
            Action::MoveWindowToWorkspace {
                window_id: Some(id),
                reference,
                ..
            } => {
                let workspace_id = state
                    .workspaces
                    .iter()
                    .find(|workspace| reference_matches(&reference, workspace))
                    .map(|workspace| workspace.id);
                if let (Some(window), Some(workspace_id)) = (
                    state.windows.iter_mut().find(|window| window.id == id),
                    workspace_id,
                ) {
                    window.workspace_id = Some(workspace_id);
                }
            }
            Action::MoveWindowToFloating { id: Some(id) } => {
                if let Some(window) = state.windows.iter_mut().find(|window| window.id == id) {
                    window.is_floating = true;
                }
            }
            Action::MoveWindowToTiling { id: Some(id) } => {
                if let Some(window) = state.windows.iter_mut().find(|window| window.id == id) {
                    window.is_floating = false;
                }
            }
            Action::FocusWindow { id } => {
                let workspace_id = state
                    .windows
                    .iter()
                    .find(|window| window.id == id)
                    .and_then(|window| window.workspace_id);
                for window in &mut state.windows {
                    window.is_focused = window.id == id;
                }
                if let Some(workspace_id) = workspace_id {
                    for workspace in &mut state.workspaces {
                        workspace.is_active = workspace.id == workspace_id;
                        workspace.is_focused = workspace.id == workspace_id;
                    }
                }
            }
            _ => (),
        }
    }

    fn records(handle: &FakeNiri) -> Vec<Request> {
        handle.state.lock().unwrap().requests.clone()
    }

    fn sample_context() -> Context {
        let saved_workspaces = vec![workspace(1, 1, Some("work"))];
        let saved_windows = vec![window_state(10, "firefox", 1, true, true)];
        let saved_outputs = outputs([output("eDP-1", 1.5, true)]);
        capture(
            &saved_workspaces,
            &saved_windows,
            &saved_outputs,
            BTreeMap::new(),
            Hooks::default(),
        )
    }

    fn sample_fake() -> FakeState {
        FakeState {
            outputs: outputs([output("eDP-1", 1., false)]),
            workspaces: vec![
                workspace_state(1, 1, Some("work"), "eDP-1", false, false),
                workspace_state(2, 2, Some("other"), "eDP-1", true, true),
            ],
            windows: vec![window_state(20, "firefox", 2, false, false)],
            ..FakeState::default()
        }
    }

    #[test]
    fn snapshots_workspaces_and_windows() {
        let workspaces = vec![workspace(1, 1, Some("main")), workspace(2, 2, Some("web"))];
        let windows = vec![window(10, "firefox", 2, true)];
        let session = snapshot(&workspaces, &windows, BTreeMap::new());
        assert_eq!(session.workspaces.len(), 2);
        assert_eq!(session.windows[0].workspace_name.as_deref(), Some("web"));
        assert_eq!(session.windows[0].workspace_idx, 2);
        assert!(session.windows[0].is_floating);
    }

    #[test]
    fn plan_moves_floating_and_missing_windows() {
        let saved_workspaces = vec![workspace(1, 1, Some("main")), workspace(2, 2, Some("web"))];
        let saved_windows = vec![
            window(10, "firefox", 2, true),
            window(11, "editor", 2, false),
        ];
        let mut commands = BTreeMap::new();
        commands.insert("editor".to_owned(), "editor".to_owned());
        let session = snapshot(&saved_workspaces, &saved_windows, commands);

        // Current state: firefox on main, tiled; editor is missing.
        let current_workspaces = vec![workspace(7, 1, Some("main"))];
        let current_windows = vec![window(20, "firefox", 7, false)];
        let (actions, missing) = plan(&session, &current_workspaces, &current_windows);
        assert!(missing.is_empty());

        let labels: Vec<&str> = actions.iter().map(|(label, _)| label.as_str()).collect();
        assert!(labels.contains(&"focus workspace main"));
        assert!(labels.contains(&"focus workspace web"));
        assert!(labels.contains(&"move window 20 to workspace"));
        assert!(labels.contains(&"set floating 20"));
        assert!(labels.contains(&"spawn editor"));
        assert!(labels.contains(&"focus window 20"));
        assert!(matches!(
            actions.last().unwrap().1,
            Action::FocusWindow { id: 20 }
        ));
    }

    #[test]
    fn missing_apps_without_commands_are_reported() {
        let session = snapshot(
            &[workspace(1, 1, None)],
            &[window(10, "gone", 1, false)],
            BTreeMap::new(),
        );
        let (actions, missing) = plan(&session, &[], &[]);
        assert!(actions.is_empty());
        assert_eq!(missing, ["gone"]);
    }

    #[test]
    fn store_round_trips() {
        let path = temp_path("store");
        let mut store = Store::default();
        store.contexts.insert("work".to_owned(), Context::default());
        store.save(&path).unwrap();
        let reloaded = Store::load(&path).unwrap();
        assert_eq!(reloaded.version, SCHEMA_VERSION);
        assert!(reloaded.contexts.contains_key("work"));
    }

    #[test]
    fn captures_outputs_and_stable_ordering() {
        let workspaces = vec![
            workspace_state(2, 2, Some("web"), "DP-1", true, false),
            workspace_state(1, 1, Some("main"), "eDP-1", true, true),
        ];
        let windows = vec![
            window_state(12, "slack", 2, false, true),
            window_state(11, "firefox", 1, false, false),
        ];
        let outputs = outputs([output("eDP-1", 1.5, true), output("DP-1", 1., false)]);
        let context = capture(
            &workspaces,
            &windows,
            &outputs,
            BTreeMap::new(),
            Hooks::default(),
        );

        let names: Vec<_> = context
            .workspaces
            .iter()
            .map(|workspace| workspace.name.as_deref().unwrap())
            .collect();
        assert_eq!(names, ["web", "main"]);
        let apps: Vec<_> = context
            .windows
            .iter()
            .map(|window| window.app_id.as_str())
            .collect();
        assert_eq!(apps, ["slack", "firefox"]);
        assert_eq!(context.outputs["eDP-1"].scale, Some(1.5));
        assert_eq!(context.outputs["DP-1"].scale, Some(1.));
        assert_eq!(context.outputs.len(), 2);
    }

    #[test]
    fn context_store_round_trips_outputs_and_hooks() {
        let path = temp_path("context-round-trip");
        let mut context = sample_context();
        context.hooks.pre = vec!["echo pre".to_owned()];
        context.hooks.post = vec!["echo post".to_owned()];
        let mut store = Store::default();
        store.insert("work", context.clone());
        store.save(&path).unwrap();

        let reloaded = Store::load(&path).unwrap();
        assert_eq!(reloaded.version, SCHEMA_VERSION);
        assert_eq!(reloaded.get("work"), Some(&context));
        assert_eq!(
            reloaded.get("work").unwrap().outputs["eDP-1"].scale,
            Some(1.5)
        );
    }

    #[test]
    fn restore_dry_run_orders_outputs_workspaces_windows() {
        let context = sample_context();
        let fake = start_fake("dry-run-order", sample_fake());
        let options = RestoreOptions {
            dry_run: true,
            ..RestoreOptions::default()
        };
        let outcome = restore_context(&fake.path, "work", &context, &options).unwrap();
        let report = outcome.report;

        assert!(report.dry_run);
        assert!(report.ok);
        assert!(report.unmatched.is_empty());
        assert!(report.applied.len() >= 5);

        let phases: Vec<Phase> = report.applied.iter().map(|action| action.phase).collect();
        let expected = [
            Phase::Output,
            Phase::Output,
            Phase::Workspace,
            Phase::Window,
            Phase::Window,
            Phase::Window,
        ];
        assert_eq!(phases, expected);
        assert_eq!(report.hooks.pre.status, "skipped");
        assert_eq!(report.hooks.pre.error.as_deref(), Some("dry-run"));

        // Only the three initial queries were sent; nothing was applied.
        let requests = records(&fake);
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| matches!(
            request,
            Request::Outputs | Request::Workspaces | Request::Windows
        )));
    }

    #[test]
    fn explain_implies_dry_run() {
        let context = sample_context();
        let fake = start_fake("explain", sample_fake());
        let options = RestoreOptions {
            explain: true,
            ..RestoreOptions::default()
        };
        let outcome = restore_context(&fake.path, "work", &context, &options).unwrap();
        assert!(outcome.report.explain);
        assert!(outcome.report.dry_run);
        assert_eq!(records(&fake).len(), 3);
    }

    #[test]
    fn restore_waits_for_a_late_window() {
        let saved_workspaces = vec![workspace(1, 1, Some("work"))];
        let saved_windows = vec![window_state(10, "firefox", 1, false, true)];
        let saved_outputs = outputs([output("eDP-1", 1., false)]);
        let context = capture(
            &saved_workspaces,
            &saved_windows,
            &saved_outputs,
            BTreeMap::new(),
            Hooks::default(),
        );
        let fake = start_fake(
            "late-window",
            FakeState {
                outputs: outputs([output("eDP-1", 1., false)]),
                workspaces: vec![
                    workspace_state(1, 1, Some("work"), "eDP-1", true, true),
                    workspace_state(2, 2, Some("other"), "eDP-1", false, false),
                ],
                windows: Vec::new(),
                late: Some(LateWindow {
                    window: window_state(21, "firefox", 2, false, true),
                    after_queries: 3,
                    queries: 0,
                    added: false,
                }),
                ..FakeState::default()
            },
        );
        let options = RestoreOptions {
            wait_ms: 1_000,
            poll_ms: 5,
            ..RestoreOptions::default()
        };
        let outcome = restore_context(&fake.path, "work", &context, &options).unwrap();
        let report = outcome.report;

        assert!(report.unmatched.is_empty());
        assert!(report.applied.iter().any(
            |action| action.phase == Phase::Window && action.action.contains("move window 21")
        ));
        let windows_queries = records(&fake)
            .iter()
            .filter(|request| matches!(request, Request::Windows))
            .count();
        assert!(windows_queries >= 3, "queries: {windows_queries}");
    }

    #[test]
    fn restore_is_idempotent() {
        let context = sample_context();
        let fake = start_fake("idempotent", sample_fake());
        let options = RestoreOptions::default();

        let first = restore_context(&fake.path, "work", &context, &options)
            .unwrap()
            .report;
        assert!(!first.applied.is_empty());
        assert!(!first.aborted);

        let second = restore_context(&fake.path, "work", &context, &options)
            .unwrap()
            .report;
        assert!(second.applied.is_empty(), "second pass: {second:?}");
        assert!(second.unmatched.is_empty());
    }

    #[test]
    fn unmatched_windows_are_reported_after_wait() {
        let context = capture(
            &[],
            &[window(10, "gone", 1, false)],
            &BTreeMap::new(),
            BTreeMap::new(),
            Hooks::default(),
        );
        let fake = start_fake("unmatched", FakeState::default());
        let options = RestoreOptions {
            wait_ms: 20,
            poll_ms: 1,
            ..RestoreOptions::default()
        };
        let report = restore_context(&fake.path, "work", &context, &options)
            .unwrap()
            .report;
        assert!(report.applied.is_empty());
        assert_eq!(report.unmatched.len(), 1);
        assert_eq!(report.unmatched[0].target, "gone");
    }

    #[test]
    fn pre_hook_failure_aborts_unless_forced() {
        let mut context = Context::default();
        context.hooks.pre = vec!["exit 7".to_owned()];

        let options = RestoreOptions::default();
        let outcome =
            restore_context(Path::new("/nonexistent"), "work", &context, &options).unwrap();
        assert!(outcome.report.aborted);
        assert!(!outcome.report.ok);
        assert!(outcome.report.applied.is_empty());
        assert!(outcome.failure.is_some());
        assert_eq!(outcome.report.hooks.pre.status, "failed");

        let fake = start_fake("forced-pre", FakeState::default());
        let options = RestoreOptions {
            force: true,
            ..RestoreOptions::default()
        };
        let outcome = restore_context(&fake.path, "work", &context, &options).unwrap();
        assert!(!outcome.report.aborted);
        assert!(!outcome.report.ok);
        assert_eq!(outcome.report.hooks.pre.status, "failed");
        assert!(outcome.failure.is_some());
    }

    #[test]
    fn hooks_receive_context_and_phase_env() {
        let dir = temp_dir("hook-env");
        let file = dir.join("hook.txt");
        let context = Context {
            hooks: Hooks {
                post: vec![format!(
                    "printf '%s:%s' \"$BETTER_SESSION_CONTEXT\" \"$BETTER_SESSION_PHASE\" > {}",
                    file.display()
                )],
                ..Hooks::default()
            },
            ..Context::default()
        };
        let fake = start_fake("hook-env-server", FakeState::default());
        let outcome =
            restore_context(&fake.path, "work", &context, &RestoreOptions::default()).unwrap();
        assert!(outcome.failure.is_none(), "{outcome:?}");
        assert_eq!(outcome.report.hooks.post.status, "ok");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "work:post");
    }

    #[test]
    fn post_hook_failure_is_reported_without_aborting() {
        let context = Context {
            hooks: Hooks {
                pre: vec!["true".to_owned()],
                post: vec!["exit 3".to_owned()],
            },
            ..Context::default()
        };
        let fake = start_fake("post-failure", FakeState::default());
        let outcome =
            restore_context(&fake.path, "work", &context, &RestoreOptions::default()).unwrap();
        assert!(!outcome.report.aborted);
        assert!(!outcome.report.ok);
        assert_eq!(outcome.report.hooks.pre.status, "ok");
        assert_eq!(outcome.report.hooks.post.status, "failed");
        assert!(outcome.failure.is_some());
    }

    #[test]
    fn migrates_legacy_sessions_file() {
        let dir = temp_dir("migration");
        let legacy = dir.join("sessions.json");
        std::fs::write(
            &legacy,
            r#"{
                "sessions": {
                    "old": {
                        "created_at": 1,
                        "command_map": { "editor": "alacritty" },
                        "workspaces": [
                            { "id": 1, "name": "work", "idx": 1, "output": "eDP-1", "is_active": true, "is_focused": true }
                        ],
                        "windows": [
                            { "app_id": "editor", "title": "main.rs", "workspace_id": 1, "workspace_name": "work", "workspace_idx": 1, "is_floating": false, "is_focused": true }
                        ]
                    }
                }
            }"#,
        )
        .unwrap();

        let store = Store::load(&dir.join("contexts.json")).unwrap();
        assert_eq!(store.version, SCHEMA_VERSION);
        let old = store.get("old").unwrap();
        assert_eq!(old.windows.len(), 1);
        assert_eq!(old.command_map["editor"], "alacritty");
        assert!(old.outputs.is_empty());
        assert!(old.hooks.pre.is_empty());
    }

    #[test]
    fn rename_and_delete_contexts() {
        let mut store = Store::default();
        store.insert("work", Context::default());
        store.rename("work", "desk").unwrap();
        assert!(store.get("work").is_none());
        assert!(store.get("desk").is_some());
        assert!(store.rename("desk", "desk").is_ok());
        store.insert("other", Context::default());
        assert!(store.rename("desk", "other").is_err());
        assert!(store.get("desk").is_some());
        assert!(store.remove("desk"));
        assert!(!store.remove("desk"));
    }
}
