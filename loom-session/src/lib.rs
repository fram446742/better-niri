//! `loom-session`: save and restore named niri contexts.
//!
//! A context records workspaces, windows, output configuration (mode, scale,
//! transform, position, VRR, on/off), optional hooks and optional profiles for
//! the other Loom apps. Restore applies outputs first, then
//! workspaces, then windows, and polls niri for windows that appear late
//! instead of sleeping for a fixed time.
//!
//! Contexts live in a versioned JSON store under
//! `$XDG_STATE_HOME/loom-session/contexts.json`. The older
//! `sessions.json` snapshot format is still read and migrated in memory.
//! Bounded snapshots live under `$XDG_STATE_HOME/loom-session/history/`.

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

pub mod history;
pub mod launch;
pub mod profiles;
pub mod templates;

pub use history::{
    DEFAULT_HISTORY_KEEP, HISTORY_VERSION, HistoryEntry, HistoryListing, HistoryStore,
    HistorySummary, history_dir,
};
pub use launch::{LaunchEnv, LaunchSource, ResolvedLaunch};
pub use profiles::{
    AudioProfile, CompanionProfile, MonitorApply, MonitorProfile, ProfileApplyOptions, ProfileEnv,
    ProfileHooks, ProfileStatus, Profiles, WallpaperProfile,
};
pub use templates::Template;

/// Version of the on-disk context schema.
pub const SCHEMA_VERSION: u32 = 1;

/// Default time to wait for late windows when restoring.
pub const DEFAULT_WAIT_MS: u64 = 2_000;

/// Default poll interval while waiting for late windows.
pub const DEFAULT_POLL_MS: u64 = 50;

/// Default number of reconnect attempts when a niri request fails.
pub const DEFAULT_RETRIES: u32 = 3;

/// Backoff base between niri reconnect attempts.
const RETRY_BASE_MS: u64 = 10;

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
/// The shape intentionally matches `loom-monitor` profiles so a profile can
/// be copied into a context without depending on the `loom-monitor` binary.
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
    #[serde(
        default,
        alias = "adaptive_sync",
        alias = "variable_refresh_rate",
        skip_serializing_if = "Option::is_none"
    )]
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

/// User hooks run around a restore or deactivation.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hooks {
    /// Commands run before any output/workspace/window change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre: Vec<String>,
    /// Commands run after the context and its optional profiles are active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activate: Vec<String>,
    /// Commands run after the restore finishes (after `activate`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post: Vec<String>,
    /// Commands run by `loom-session deactivate`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deactivate: Vec<String>,
}

impl Hooks {
    fn is_empty(&self) -> bool {
        self.pre.is_empty()
            && self.activate.is_empty()
            && self.post.is_empty()
            && self.deactivate.is_empty()
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
    #[serde(default, skip_serializing_if = "Profiles::is_empty")]
    pub profiles: Profiles,
    /// Built-in template this context was shaped from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
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
                "unsupported loom-session schema version {} (this build supports {SCHEMA_VERSION})",
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
    dir.join("loom-session")
}

/// State path: `$XDG_STATE_HOME/loom-session/contexts.json`.
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
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        write_atomic(path, &bytes)
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

pub(crate) fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

/// Writes `bytes` to `path` through a sibling temporary file and rename.
///
/// The temporary name includes the process id and a monotonic counter so
/// concurrent writers cannot steal each other's temporary file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let tmp = path.with_file_name(format!(".{file_name}.{}.{counter}.tmp", std::process::id()));
    let result = (|| {
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub(crate) fn workspace_sort_key(workspace: &WorkspaceSnapshot) -> (String, u8, String, u64) {
    (
        workspace.output.clone().unwrap_or_default(),
        workspace.idx,
        workspace.name.clone().unwrap_or_default(),
        workspace.id,
    )
}

pub(crate) fn window_sort_key(window: &WindowSnapshot) -> (String, u8, String, String, String) {
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
        ..Context::default()
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

fn same_app(saved: &WindowSnapshot, current: &Window) -> bool {
    !saved.app_id.is_empty() && current.app_id.as_deref() == Some(saved.app_id.as_str())
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
    let available = |window: &&Window| !used.contains(&window.id);
    let mut candidates: Vec<&Window> = current
        .iter()
        .filter(available)
        .filter(|window| same_window(saved, window))
        .collect();
    if candidates.is_empty() {
        // Titles change while an app runs; fall back to the app id so a restore
        // does not launch a duplicate before moving the existing window.
        candidates = current
            .iter()
            .filter(available)
            .filter(|window| same_app(saved, window))
            .collect();
    }
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
    /// Re-checked immediately before the request when user changes are
    /// preserved. `None` means the step is safe regardless of current state.
    pub guard: Option<Guard>,
}

/// State guard used to detect user changes made after planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// Skip when the output is no longer connected.
    OutputAlive(String),
    /// Skip when the window no longer exists.
    WindowAlive(u64),
    /// Skip when the window disappeared or already reached `reference`.
    MoveWindow {
        id: u64,
        reference: WorkspaceReferenceArg,
    },
    /// Skip when the window disappeared or already has `floating`.
    Floating { id: u64, floating: bool },
    /// Skip when the window disappeared or is already focused.
    FocusWindow(u64),
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

/// A live window that no saved window claimed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExtraWindow {
    pub id: u64,
    pub app_id: String,
    pub title: String,
}

#[derive(Debug, Default)]
pub struct WindowPlan {
    /// Spawn requests, executed before waiting for late windows.
    pub spawns: Vec<Step>,
    /// Window moves, floating changes and focus.
    pub steps: Vec<Step>,
    pub skipped: Vec<Skipped>,
    pub unmatched: Vec<Unmatched>,
    /// Live windows left alone because no saved window claimed them.
    pub extra: Vec<ExtraWindow>,
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
        guard: Some(Guard::OutputAlive(name.to_owned())),
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
            guard: None,
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
///
/// Only an explicit `command_map` is used to resolve launches. Use
/// [`plan_windows_with`] to also resolve `.desktop` files, Flatpaks and
/// executables.
pub fn plan_windows(
    context: &Context,
    current_workspaces: &[Workspace],
    current_windows: &[Window],
    allow_spawn: bool,
) -> WindowPlan {
    plan_windows_with(
        context,
        current_workspaces,
        current_windows,
        allow_spawn,
        &LaunchEnv::default(),
    )
}

/// Plans window spawns and moves, resolving missing apps with `launch_env`.
///
/// A given app id is spawned at most once per plan even when several saved
/// windows share it; the extra windows are awaited by the late-window poll.
pub fn plan_windows_with(
    context: &Context,
    current_workspaces: &[Workspace],
    current_windows: &[Window],
    allow_spawn: bool,
    launch_env: &LaunchEnv,
) -> WindowPlan {
    let mut plan = WindowPlan::default();
    let mut used = BTreeSet::new();
    let mut focused_window: Option<(u64, String)> = None;
    let mut spawned = BTreeSet::new();

    for saved in &context.windows {
        let Some(current) = find_match(saved, current_windows, current_workspaces, &used) else {
            if allow_spawn {
                if spawned.contains(&saved.app_id) {
                    continue;
                }
                match launch_env.resolve(&saved.app_id, &context.command_map) {
                    Some(launch) => {
                        spawned.insert(saved.app_id.clone());
                        plan.spawns.push(Step {
                            phase: Phase::Window,
                            target: saved.app_id.clone(),
                            action: format!("spawn {}", saved.app_id),
                            request: Request::Action(Action::SpawnSh {
                                command: launch.command,
                            }),
                            guard: None,
                        });
                    }
                    None => plan.unmatched.push(Unmatched {
                        phase: Phase::Window,
                        target: saved.app_id.clone(),
                        reason: "no live window and no launch command".to_owned(),
                    }),
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
            let reference = workspace_reference(saved);
            plan.steps.push(Step {
                phase: Phase::Window,
                target: saved.app_id.clone(),
                action: format!("move window {} to workspace", current.id),
                request: Request::Action(Action::MoveWindowToWorkspace {
                    window_id: Some(current.id),
                    reference: reference.clone(),
                    focus: false,
                }),
                guard: Some(Guard::MoveWindow {
                    id: current.id,
                    reference,
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
                guard: Some(Guard::Floating {
                    id: current.id,
                    floating: saved.is_floating,
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
                guard: Some(Guard::FocusWindow(id)),
            });
        }
    }

    for window in current_windows {
        if !used.contains(&window.id) {
            plan.extra.push(ExtraWindow {
                id: window.id,
                app_id: window.app_id.clone().unwrap_or_default(),
                title: window.title.clone().unwrap_or_default(),
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
    plan_context_with(
        context,
        current_workspaces,
        current_windows,
        current_outputs,
        &LaunchEnv::default(),
    )
}

/// Plans every phase, resolving missing apps with `launch_env`.
pub fn plan_context_with(
    context: &Context,
    current_workspaces: &[Workspace],
    current_windows: &[Window],
    current_outputs: &Outputs,
    launch_env: &LaunchEnv,
) -> ContextPlan {
    let mut plan = ContextPlan::default();

    let (steps, skipped) = plan_outputs(context, current_outputs);
    plan.steps.extend(steps);
    plan.skipped.extend(skipped);

    let (steps, skipped) = plan_workspaces(context, current_workspaces);
    plan.steps.extend(steps);
    plan.skipped.extend(skipped);

    let windows = plan_windows_with(
        context,
        current_workspaces,
        current_windows,
        true,
        launch_env,
    );
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
    send_with_retry(socket, request, 0)
}

fn is_transient(error: &str) -> bool {
    error.contains("cannot connect to niri IPC") || error.contains("niri IPC error")
}

/// Sends one request, reconnecting with exponential backoff up to `retries`.
pub fn send_with_retry(socket: &Path, request: Request, retries: u32) -> Result<Response, String> {
    use niri_ipc::socket::Socket;
    let mut attempt = 0;
    loop {
        let result = Socket::connect_to(socket)
            .map_err(|err| format!("cannot connect to niri IPC: {err}"))
            .and_then(|mut socket| {
                socket
                    .send(request.clone())
                    .map_err(|err| format!("niri IPC error: {err}"))
            })
            .and_then(|reply| reply.map_err(|err| format!("niri returned an error: {err}")));
        match result {
            Ok(response) => return Ok(response),
            Err(error) if attempt < retries && is_transient(&error) => {
                let delay = RETRY_BASE_MS.saturating_mul(1u64 << attempt.min(5));
                std::thread::sleep(Duration::from_millis(delay));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn query(socket: &Path) -> Result<(Vec<Workspace>, Vec<Window>, Outputs), String> {
    query_with_retry(socket, 0)
}

/// Queries outputs, workspaces and windows, retrying transient reconnects.
pub fn query_with_retry(
    socket: &Path,
    retries: u32,
) -> Result<(Vec<Workspace>, Vec<Window>, Outputs), String> {
    let outputs = match send_with_retry(socket, Request::Outputs, retries)? {
        Response::Outputs(outputs) => outputs.into_iter().collect(),
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    let workspaces = match send_with_retry(socket, Request::Workspaces, retries)? {
        Response::Workspaces(workspaces) => workspaces,
        other => return Err(format!("unexpected niri response: {other:?}")),
    };
    let windows = match send_with_retry(socket, Request::Windows, retries)? {
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

/// Result of executing a plan with user-change guards.
#[derive(Debug, Default)]
pub struct ExecutionReport {
    pub applied: Vec<Applied>,
    pub skipped: Vec<Skipped>,
}

/// Sends planned steps, or only records them when `dry_run` is set.
pub fn execute_steps(socket: &Path, steps: &[Step], dry_run: bool) -> Result<Vec<Applied>, String> {
    Ok(execute_steps_guarded(socket, steps, dry_run, 0, false)?.applied)
}

/// Sends planned steps and re-checks guards immediately before each request.
///
/// When `preserve_user_changes` is set, a window that disappeared or already
/// reached the planned state is skipped instead of being moved back. The check
/// is best-effort: only changes observable through niri IPC are detected.
pub fn execute_steps_guarded(
    socket: &Path,
    steps: &[Step],
    dry_run: bool,
    retries: u32,
    preserve_user_changes: bool,
) -> Result<ExecutionReport, String> {
    let mut report = ExecutionReport {
        applied: Vec::with_capacity(steps.len()),
        skipped: Vec::new(),
    };
    for step in steps {
        if !dry_run
            && preserve_user_changes
            && let Some(guard) = &step.guard
            && let Some(reason) = guard_skip(socket, guard, retries)?
        {
            report.skipped.push(Skipped {
                phase: step.phase,
                target: step.target.clone(),
                reason,
            });
            continue;
        }
        if !dry_run {
            match send_with_retry(socket, step.request.clone(), retries)? {
                Response::Handled | Response::OutputConfigChanged(_) => (),
                other => return Err(format!("unexpected niri response: {other:?}")),
            }
        }
        report.applied.push(Applied {
            phase: step.phase,
            target: step.target.clone(),
            action: step.action.clone(),
            request: serde_json::to_value(&step.request).unwrap_or(Value::Null),
        });
    }
    Ok(report)
}

fn guard_skip(socket: &Path, guard: &Guard, retries: u32) -> Result<Option<String>, String> {
    match guard {
        Guard::OutputAlive(name) => {
            let outputs = match send_with_retry(socket, Request::Outputs, retries)? {
                Response::Outputs(outputs) => outputs,
                other => return Err(format!("unexpected niri response: {other:?}")),
            };
            Ok((!outputs.contains_key(name)).then(|| "output disconnected before step".to_owned()))
        }
        Guard::WindowAlive(id) => {
            let windows = current_windows(socket, retries)?;
            Ok((!windows.iter().any(|window| window.id == *id))
                .then(|| "window disappeared before step".to_owned()))
        }
        Guard::MoveWindow { id, reference } => {
            let windows = current_windows(socket, retries)?;
            let Some(window) = windows.iter().find(|window| window.id == *id) else {
                return Ok(Some("window disappeared before step".to_owned()));
            };
            let workspaces = current_workspaces(socket, retries)?;
            let on_target = window.workspace_id.is_some_and(|workspace_id| {
                workspaces.iter().any(|workspace| {
                    workspace.id == workspace_id
                        && match reference {
                            WorkspaceReferenceArg::Id(id) => workspace.id == *id,
                            WorkspaceReferenceArg::Index(idx) => workspace.idx == *idx,
                            WorkspaceReferenceArg::Name(name) => {
                                workspace.name.as_deref() == Some(name.as_str())
                            }
                        }
                })
            });
            Ok(on_target.then(|| "already on the saved workspace (changed by user)".to_owned()))
        }
        Guard::Floating { id, floating } => {
            let windows = current_windows(socket, retries)?;
            let Some(window) = windows.iter().find(|window| window.id == *id) else {
                return Ok(Some("window disappeared before step".to_owned()));
            };
            Ok((window.is_floating == *floating)
                .then(|| "floating state already changed by user".to_owned()))
        }
        Guard::FocusWindow(id) => {
            let windows = current_windows(socket, retries)?;
            let Some(window) = windows.iter().find(|window| window.id == *id) else {
                return Ok(Some("window disappeared before step".to_owned()));
            };
            Ok(window
                .is_focused
                .then(|| "window already focused (changed by user)".to_owned()))
        }
    }
}

fn current_windows(socket: &Path, retries: u32) -> Result<Vec<Window>, String> {
    match send_with_retry(socket, Request::Windows, retries)? {
        Response::Windows(windows) => Ok(windows),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

fn current_workspaces(socket: &Path, retries: u32) -> Result<Vec<Workspace>, String> {
    match send_with_retry(socket, Request::Workspaces, retries)? {
        Response::Workspaces(workspaces) => Ok(workspaces),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
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
            guard: None,
        })
        .collect();
    let applied = execute_steps(socket, &steps, dry_run)?;
    Ok(applied.into_iter().map(|applied| applied.request).collect())
}

/// Runs one hook command with the context environment.
pub fn run_hook(command: &str, context: &str, phase: &str) -> Result<(), String> {
    run_hook_with(command, context, phase, None, &BTreeMap::new())
}

/// Runs one hook with an optional profile name and extra environment.
pub fn run_hook_with(
    command: &str,
    context: &str,
    phase: &str,
    profile: Option<&str>,
    extra: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut child = Command::new("sh");
    child
        .arg("-c")
        .arg(command)
        .env("LOOM_SESSION_CONTEXT", context)
        .env("LOOM_SESSION_PHASE", phase)
        .envs(extra);
    if let Some(profile) = profile {
        child.env("LOOM_SESSION_PROFILE", profile);
    }
    let status = child
        .status()
        .map_err(|err| format!("cannot run {phase} hook `{command}`: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{phase} hook exited with {status}: {command}"))
    }
}

/// Result of running one hook phase.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HookStatus {
    /// `ok`, `failed`, `skipped` or `none`.
    pub status: String,
    pub commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HookStatus {
    pub(crate) fn none() -> Self {
        Self {
            status: "none".to_owned(),
            commands: Vec::new(),
            error: None,
        }
    }

    pub(crate) fn skipped(commands: &[String], reason: &str) -> Self {
        Self {
            status: "skipped".to_owned(),
            commands: commands.to_vec(),
            error: Some(reason.to_owned()),
        }
    }

    pub(crate) fn failed(commands: &[String], error: String) -> Self {
        Self {
            status: "failed".to_owned(),
            commands: commands.to_vec(),
            error: Some(error),
        }
    }

    pub(crate) fn ok(commands: &[String]) -> Self {
        Self {
            status: "ok".to_owned(),
            commands: commands.to_vec(),
            error: None,
        }
    }
}

fn run_hooks(context: &str, phase: &str, commands: &[String]) -> HookStatus {
    run_hooks_env(context, phase, commands, None, &BTreeMap::new())
}

/// Runs every command in a phase, stopping at the first failure.
pub fn run_hooks_env(
    context: &str,
    phase: &str,
    commands: &[String],
    profile: Option<&str>,
    extra: &BTreeMap<String, String>,
) -> HookStatus {
    if commands.is_empty() {
        return HookStatus::none();
    }
    for command in commands {
        if let Err(error) = run_hook_with(command, context, phase, profile, extra) {
            return HookStatus::failed(commands, error);
        }
    }
    HookStatus::ok(commands)
}

#[derive(Debug, Clone, Serialize)]
pub struct HookReport {
    pub pre: HookStatus,
    #[serde(default, skip_serializing_if = "HookStatus::is_none")]
    pub activate: HookStatus,
    pub post: HookStatus,
    #[serde(default, skip_serializing_if = "HookStatus::is_none")]
    pub deactivate: HookStatus,
}

impl HookStatus {
    fn is_none(&self) -> bool {
        self.status == "none"
    }
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
    /// Reconnect attempts for a failing niri socket (per request).
    pub retries: u32,
    /// Re-check each step against live state to keep user changes.
    pub preserve_user_changes: bool,
    /// Launch resolution for missing apps.
    pub launch_env: LaunchEnv,
    /// Locations of optional app profiles and commands.
    pub profile_env: ProfileEnv,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            explain: false,
            force: false,
            wait_ms: DEFAULT_WAIT_MS,
            poll_ms: DEFAULT_POLL_MS,
            retries: DEFAULT_RETRIES,
            preserve_user_changes: true,
            launch_env: LaunchEnv::default(),
            profile_env: ProfileEnv::detect(),
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
    /// Core restore succeeded; optional profiles may still be degraded.
    pub ok: bool,
    /// At least one optional profile was `missing` or `failed`.
    pub partial: bool,
    pub applied: Vec<Applied>,
    pub skipped: Vec<Skipped>,
    pub unmatched: Vec<Unmatched>,
    /// Live windows left untouched because no saved window claimed them.
    pub extra: Vec<ExtraWindow>,
    /// Optional app profile outcomes, in monitor/audio/wallpaper/companion order.
    pub profiles: Vec<ProfileStatus>,
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
            partial: false,
            applied: Vec::new(),
            skipped: Vec::new(),
            unmatched: Vec::new(),
            extra: Vec::new(),
            profiles: Vec::new(),
            hooks: HookReport {
                pre: HookStatus::none(),
                activate: HookStatus::none(),
                post: HookStatus::none(),
                deactivate: HookStatus::none(),
            },
            error: None,
        }
    }

    fn note_profiles(&mut self, statuses: Vec<ProfileStatus>) {
        if statuses.iter().any(|status| {
            status.status == profiles::STATUS_FAILED || status.status == profiles::STATUS_MISSING
        }) {
            self.partial = true;
        }
        self.profiles.extend(statuses);
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
/// Outputs are applied first, then the optional monitor profile, workspaces,
/// windows and finally audio/wallpaper/companion profiles and the `activate`
/// hook. Spawned or late windows are awaited by polling `Request::Windows` up
/// to `wait_ms`. Missing or failing optional apps set `partial` but never abort
/// the core restore.
pub fn restore_context(
    socket: &Path,
    name: &str,
    context: &Context,
    options: &RestoreOptions,
) -> Result<RestoreOutcome, String> {
    let dry_run = options.dry_run || options.explain;
    let mut report = RestoreReport::new(name, options);
    let profile_options = ProfileApplyOptions {
        dry_run,
        preserve_user_changes: options.preserve_user_changes,
        retries: options.retries,
    };

    if dry_run {
        report.hooks.pre = HookStatus::skipped(&context.hooks.pre, "dry-run");
        report.hooks.activate = HookStatus::skipped(&context.hooks.activate, "dry-run");
        report.hooks.post = HookStatus::skipped(&context.hooks.post, "dry-run");
        report.hooks.deactivate = HookStatus::none();
        let (workspaces, windows, outputs) = query_with_retry(socket, options.retries)?;
        let plan = plan_context_with(
            context,
            &workspaces,
            &windows,
            &outputs,
            &options.launch_env,
        );
        report.applied = execute_steps(socket, &plan.steps, true)?;
        report.skipped = plan.skipped;
        report.unmatched = plan.unmatched;
        report.note_profiles(profiles::apply_profiles(
            socket,
            name,
            context,
            &profile_options,
            &options.profile_env,
        ));
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

    let (mut workspaces, mut windows, outputs) = query_with_retry(socket, options.retries)?;

    // 2. Outputs.
    let (steps, skipped) = plan_outputs(context, &outputs);
    let outcome = execute_steps_guarded(
        socket,
        &steps,
        false,
        options.retries,
        options.preserve_user_changes,
    )?;
    report.applied.extend(outcome.applied);
    report.skipped.extend(outcome.skipped);
    report.skipped.extend(skipped);

    // 3. Optional monitor profile, before workspaces so geometry is final.
    if let Some(monitor) = &context.profiles.monitor {
        report.note_profiles(vec![profiles::apply_monitor_profile(
            socket,
            name,
            monitor,
            &profile_options,
            &options.profile_env,
        )]);
        let (latest_workspaces, latest_windows, latest_outputs) =
            query_with_retry(socket, options.retries)?;
        workspaces = latest_workspaces;
        windows = latest_windows;
        let _ = latest_outputs;
    }

    // 4. Workspaces.
    let (steps, skipped) = plan_workspaces(context, &workspaces);
    let outcome = execute_steps_guarded(
        socket,
        &steps,
        false,
        options.retries,
        options.preserve_user_changes,
    )?;
    report.applied.extend(outcome.applied);
    report.skipped.extend(outcome.skipped);
    report.skipped.extend(skipped);

    // 5. Spawn missing apps, then wait for late windows by polling.
    let initial = plan_windows_with(context, &workspaces, &windows, true, &options.launch_env);
    report.applied.extend(
        execute_steps_guarded(
            socket,
            &initial.spawns,
            false,
            options.retries,
            options.preserve_user_changes,
        )?
        .applied,
    );

    let missing_before = !all_windows_matched(context, &windows);
    let should_wait = options.wait_ms > 0 && missing_before;
    if !initial.spawns.is_empty() || should_wait {
        let (mut latest_workspaces, mut latest_windows, _) =
            query_with_retry(socket, options.retries)?;
        if should_wait {
            let deadline = Instant::now() + Duration::from_millis(options.wait_ms);
            while !all_windows_matched(context, &latest_windows) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(options.poll_ms.max(1)));
                let (next_workspaces, next_windows, _) = query_with_retry(socket, options.retries)?;
                latest_workspaces = next_workspaces;
                latest_windows = next_windows;
            }
        }
        workspaces = latest_workspaces;
        windows = latest_windows;
    }

    // 6. Windows.
    let final_plan = plan_windows_with(context, &workspaces, &windows, false, &options.launch_env);
    let outcome = execute_steps_guarded(
        socket,
        &final_plan.steps,
        false,
        options.retries,
        options.preserve_user_changes,
    )?;
    report.applied.extend(outcome.applied);
    report.skipped.extend(outcome.skipped);
    report.skipped.extend(final_plan.skipped);
    report.unmatched.extend(final_plan.unmatched);
    report.extra = final_plan.extra;

    // 7. Audio, wallpaper and companion profiles. Monitor already ran.
    report.note_profiles(profiles::apply_other_profiles(
        socket,
        name,
        context,
        &profile_options,
        &options.profile_env,
    ));

    // 8. Activate hooks. Failures are reported but do not roll anything back.
    report.hooks.activate = run_hooks(name, "activate", &context.hooks.activate);
    if report.hooks.activate.status == "failed" {
        let message = report
            .hooks
            .activate
            .error
            .clone()
            .unwrap_or_else(|| "activate hook failed".to_owned());
        hook_failure.get_or_insert(message);
    }

    // 9. Post hooks.
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

/// JSON deactivation report.
#[derive(Debug, Serialize)]
pub struct DeactivateReport {
    pub context: String,
    pub dry_run: bool,
    pub ok: bool,
    pub profiles: Vec<ProfileStatus>,
    pub hooks: HookReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Runs deactivation hooks for a context and its profiles.
///
/// No niri state is changed. Profile deactivation is hook-only: the optional
/// apps expose no portable "deactivate" protocol.
pub fn deactivate_context(
    name: &str,
    context: &Context,
    dry_run: bool,
    profile_env: &ProfileEnv,
) -> DeactivateReport {
    let mut report = DeactivateReport {
        context: name.to_owned(),
        dry_run,
        ok: true,
        profiles: Vec::new(),
        hooks: HookReport {
            pre: HookStatus::none(),
            activate: HookStatus::none(),
            post: HookStatus::none(),
            deactivate: HookStatus::none(),
        },
        error: None,
    };
    if dry_run {
        report.hooks.deactivate = HookStatus::skipped(&context.hooks.deactivate, "dry-run");
        report.profiles = profiles::deactivate_profiles(name, context, true, profile_env);
        return report;
    }

    report.profiles = profiles::deactivate_profiles(name, context, false, profile_env);
    report.hooks.deactivate = run_hooks(name, "deactivate", &context.hooks.deactivate);
    let hook_failed = report.hooks.deactivate.status == "failed";
    let profile_failed = report
        .profiles
        .iter()
        .any(|status| status.status == profiles::STATUS_FAILED);
    if hook_failed || profile_failed {
        report.ok = false;
        report.error = report.hooks.deactivate.error.clone().or_else(|| {
            report
                .profiles
                .iter()
                .find(|status| status.status == profiles::STATUS_FAILED)
                .and_then(|status| status.error.clone())
        });
    }
    report
}

/// Import merge strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// Keep existing contexts; imported contexts win on name collisions.
    Merge,
    /// Replace the whole store.
    Replace,
}

/// Counts from an import.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct ImportReport {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub replaced: bool,
}

/// Exports the whole store as a JSON value.
pub fn export_store(store: &Store) -> Value {
    serde_json::to_value(store).unwrap_or(Value::Null)
}

/// Exports one context in a self-describing envelope.
pub fn export_context(name: &str, context: &Context) -> Value {
    serde_json::json!({
        "version": SCHEMA_VERSION,
        "name": name,
        "context": context,
    })
}

fn checked_version(value: &Value) -> Result<u32, String> {
    let version = value.get("version").and_then(Value::as_u64).unwrap_or(0) as u32;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "unsupported loom-session schema version {version} (this build supports {SCHEMA_VERSION})"
        ));
    }
    Ok(version)
}

/// Parses an import document into named contexts.
///
/// Accepted shapes: a full store (`{"version","contexts"}`), a single-context
/// envelope (`{"version","name","context"}`), or a plain context when
/// `fallback_name` is provided.
pub fn parse_import(
    value: &Value,
    fallback_name: Option<&str>,
) -> Result<BTreeMap<String, Context>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "import must be a JSON object".to_owned())?;
    checked_version(value)?;
    if object.contains_key("contexts") || object.contains_key("sessions") {
        let store: Store = serde_json::from_value(value.clone())
            .map_err(|err| format!("invalid context store: {err}"))?;
        return Ok(store.contexts);
    }
    if let Some(context) = object.get("context") {
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .or(fallback_name)
            .ok_or_else(|| "single-context import needs a name".to_owned())?
            .to_owned();
        let context: Context = serde_json::from_value(context.clone())
            .map_err(|err| format!("invalid context `{name}`: {err}"))?;
        return Ok(BTreeMap::from([(name, context)]));
    }
    let name = fallback_name
        .ok_or_else(|| "plain context import needs --name".to_owned())?
        .to_owned();
    let context: Context = serde_json::from_value(value.clone())
        .map_err(|err| format!("invalid context `{name}`: {err}"))?;
    Ok(BTreeMap::from([(name, context)]))
}

/// Merges or replaces contexts in `store` from an import document.
pub fn import_into(
    store: &mut Store,
    value: &Value,
    mode: ImportMode,
    fallback_name: Option<&str>,
) -> Result<ImportReport, String> {
    let imported = parse_import(value, fallback_name)?;
    let mut report = ImportReport {
        replaced: mode == ImportMode::Replace,
        ..ImportReport::default()
    };
    match mode {
        ImportMode::Merge => {
            for (name, context) in imported {
                if store.contexts.contains_key(&name) {
                    report.updated.push(name.clone());
                } else {
                    report.added.push(name.clone());
                }
                store.contexts.insert(name, context);
            }
        }
        ImportMode::Replace => {
            for name in store.contexts.keys() {
                if !imported.contains_key(name) {
                    report.removed.push(name.clone());
                }
            }
            store.contexts = imported;
        }
    }
    report.added.sort();
    report.updated.sort();
    report.removed.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use niri_ipc::{LogicalOutput, OutputConfigChanged, Reply};
    use serde_json::json;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loom-session-{}-{name}", std::process::id()));
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

    fn start_fake(name: &str, state: FakeState) -> FakeNiri {
        start_fake_after(name, Duration::ZERO, state)
    }

    /// Starts a fake niri socket after a delay, to exercise reconnect retries.
    fn start_fake_after(name: &str, delay: Duration, mut state: FakeState) -> FakeNiri {
        if state.next_workspace_id == 0 {
            state.next_workspace_id = 100;
        }
        let dir = temp_dir(name);
        let path = dir.join("niri.sock");
        let early_listener = if delay.is_zero() {
            Some(UnixListener::bind(&path).unwrap())
        } else {
            None
        };
        let state = Arc::new(std::sync::Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            let path = path.clone();
            move || {
                let listener = match early_listener {
                    Some(listener) => listener,
                    None => {
                        std::thread::sleep(delay);
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        match UnixListener::bind(&path) {
                            Ok(listener) => listener,
                            Err(_) => return,
                        }
                    }
                };
                listener.set_nonblocking(true).unwrap();
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
                    "printf '%s:%s' \"$LOOM_SESSION_CONTEXT\" \"$LOOM_SESSION_PHASE\" > {}",
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
                ..Hooks::default()
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

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn test_profile_env(bin: Option<&Path>, state: &Path) -> ProfileEnv {
        let mut path_dirs = vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
        if let Some(bin) = bin {
            path_dirs.insert(0, bin.to_path_buf());
        }
        ProfileEnv {
            path_dirs: Some(path_dirs),
            monitor_state: state.join("profiles.json"),
            audio_config_dir: state.join("loom-audio"),
            companion_socket: None,
        }
    }

    fn empty_options(profile_env: ProfileEnv) -> RestoreOptions {
        RestoreOptions {
            wait_ms: 0,
            launch_env: LaunchEnv::default(),
            profile_env,
            ..RestoreOptions::default()
        }
    }

    #[test]
    fn plan_resolves_desktop_files_and_deduplicates_spawns() {
        let root = temp_dir("plan-desktop");
        std::fs::create_dir_all(root.join("applications")).unwrap();
        std::fs::write(
            root.join("applications/firefox.desktop"),
            "[Desktop Entry]\nExec=firefox --new-window %U\n",
        )
        .unwrap();
        let launch_env = LaunchEnv::with_dirs(vec![root], vec![]);
        let context = capture(
            &[workspace(1, 1, Some("work"))],
            &[
                window(10, "firefox", 1, false),
                window(11, "firefox", 1, false),
            ],
            &BTreeMap::new(),
            BTreeMap::new(),
            Hooks::default(),
        );
        let plan = plan_windows_with(&context, &[], &[], true, &launch_env);
        assert_eq!(plan.spawns.len(), 1, "one launch per app id");
        match &plan.spawns[0].request {
            Request::Action(Action::SpawnSh { command }) => {
                assert_eq!(command, "firefox --new-window")
            }
            other => panic!("unexpected spawn request: {other:?}"),
        }
    }

    #[test]
    fn changed_titles_match_by_app_id_before_spawning_a_duplicate() {
        let saved_workspaces = vec![workspace(1, 1, Some("work"))];
        let saved_windows = vec![window(10, "firefox", 1, false)];
        let mut commands = BTreeMap::new();
        commands.insert("firefox".to_owned(), "firefox".to_owned());
        let context = capture(
            &saved_workspaces,
            &saved_windows,
            &BTreeMap::new(),
            commands,
            Hooks::default(),
        );
        let current_workspaces = vec![workspace(1, 1, Some("work"))];
        let mut live = window_state(20, "firefox", 1, false, true);
        live.title = Some("Mail - new message".to_owned());
        let plan = plan_windows_with(
            &context,
            &current_workspaces,
            &[live],
            true,
            &LaunchEnv::default(),
        );
        assert!(plan.spawns.is_empty(), "must not spawn a duplicate");
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn plan_uses_flatpak_when_installed_and_has_no_native_binary() {
        let launch_env = LaunchEnv {
            data_dirs: vec![],
            path_dirs: vec![],
            flatpak_apps: Some(BTreeSet::from(["org.example.Game".to_owned()])),
        };
        let context = capture(
            &[],
            &[window(10, "org.example.Game", 1, false)],
            &BTreeMap::new(),
            BTreeMap::new(),
            Hooks::default(),
        );
        let plan = plan_windows_with(&context, &[], &[], true, &launch_env);
        match &plan.spawns[0].request {
            Request::Action(Action::SpawnSh { command }) => {
                assert_eq!(command, "flatpak run org.example.Game")
            }
            other => panic!("unexpected spawn request: {other:?}"),
        }
    }

    #[test]
    fn extra_windows_are_reported_and_left_alone() {
        let context = capture(
            &[workspace(1, 1, Some("work"))],
            &[window(10, "firefox", 1, false)],
            &BTreeMap::new(),
            BTreeMap::new(),
            Hooks::default(),
        );
        let current_workspaces = vec![workspace(1, 1, Some("work"))];
        let current_windows = vec![
            window_state(20, "firefox", 1, false, true),
            window_state(21, "firefox", 1, false, false),
        ];
        let plan = plan_windows_with(
            &context,
            &current_workspaces,
            &current_windows,
            false,
            &LaunchEnv::default(),
        );
        assert_eq!(plan.extra.len(), 1);
        assert_eq!(plan.extra[0].id, 21);
    }

    #[test]
    fn monitor_profile_is_applied_over_niri_ipc() {
        let dir = temp_dir("monitor-ipc");
        std::fs::write(
            dir.join("profiles.json"),
            serde_json::to_vec_pretty(&json!({
                "version": 2,
                "profiles": {
                    "wide": {
                        "version": 2,
                        "outputs": {
                            "eDP-1": { "off": false, "scale": 2.0 }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let fake = start_fake(
            "monitor-ipc-server",
            FakeState {
                outputs: outputs([output("eDP-1", 1.0, false)]),
                ..FakeState::default()
            },
        );
        let context = Context {
            profiles: Profiles {
                monitor: Some(MonitorProfile {
                    profile: "wide".to_owned(),
                    apply: MonitorApply::Auto,
                    command: None,
                    hooks: ProfileHooks::default(),
                }),
                ..Profiles::default()
            },
            ..Context::default()
        };
        let outcome = restore_context(
            &fake.path,
            "work",
            &context,
            &empty_options(test_profile_env(None, &dir)),
        )
        .unwrap();
        assert_eq!(outcome.report.profiles.len(), 1);
        assert_eq!(outcome.report.profiles[0].status, "applied");
        assert!(!outcome.report.partial);
        assert_eq!(
            fake.state.lock().unwrap().outputs["eDP-1"]
                .logical
                .unwrap()
                .scale,
            2.0
        );
    }

    #[test]
    fn monitor_falls_back_to_the_command_when_the_profile_is_missing() {
        let dir = temp_dir("monitor-command");
        let bin = dir.join("bin");
        let recorder = dir.join("recorder");
        write_script(
            &bin,
            "loom-monitor",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n",
                recorder.display()
            ),
        );
        let fake = start_fake("monitor-command-server", FakeState::default());
        let context = Context {
            profiles: Profiles {
                monitor: Some(MonitorProfile {
                    profile: "wide".to_owned(),
                    apply: MonitorApply::Auto,
                    command: None,
                    hooks: ProfileHooks::default(),
                }),
                ..Profiles::default()
            },
            ..Context::default()
        };
        let outcome = restore_context(
            &fake.path,
            "work",
            &context,
            &empty_options(test_profile_env(Some(&bin), &dir)),
        )
        .unwrap();
        assert_eq!(outcome.report.profiles[0].status, "applied");
        let recorded = std::fs::read_to_string(&recorder).unwrap();
        assert!(recorded.contains("apply wide"), "{recorded}");
    }

    #[test]
    fn audio_wallpaper_and_companion_use_their_commands() {
        let dir = temp_dir("other-profiles");
        let bin = dir.join("bin");
        let recorder = dir.join("recorder");
        let audio_config = dir.join("audio.json");
        std::fs::write(&audio_config, "{}").unwrap();
        write_script(
            &bin,
            "loom-audio",
            &format!(
                "#!/bin/sh\nprintf 'audio %s\\n' \"$*\" >> {}\n",
                recorder.display()
            ),
        );
        write_script(
            &bin,
            "loom-wallpaper",
            &format!(
                "#!/bin/sh\nprintf 'wallpaper %s\\n' \"$*\" >> {}\n",
                recorder.display()
            ),
        );
        write_script(
            &bin,
            "loomctl",
            &format!(
                "#!/bin/sh\nprintf 'companion %s marks=%s\\n' \"$*\" \"$LOOM_SESSION_COMPANION_MARKS\" >> {}\n",
                recorder.display()
            ),
        );
        let fake = start_fake("other-profiles-server", FakeState::default());
        let context = Context {
            profiles: Profiles {
                audio: Some(AudioProfile {
                    profile: "work".to_owned(),
                    config: Some(audio_config),
                    command: None,
                    hooks: ProfileHooks::default(),
                }),
                wallpaper: Some(WallpaperProfile {
                    scene: "night".to_owned(),
                    command: None,
                    hooks: ProfileHooks::default(),
                }),
                companion: Some(CompanionProfile {
                    rules: Vec::new(),
                    marks: BTreeMap::from([("project".to_owned(), "work".to_owned())]),
                    calls: vec!["rules.run".to_owned()],
                    command: None,
                    hooks: ProfileHooks::default(),
                }),
                ..Profiles::default()
            },
            ..Context::default()
        };
        let outcome = restore_context(
            &fake.path,
            "work",
            &context,
            &empty_options(test_profile_env(Some(&bin), &dir)),
        )
        .unwrap();
        let statuses: Vec<_> = outcome
            .report
            .profiles
            .iter()
            .map(|status| (status.profile.as_str(), status.status.as_str()))
            .collect();
        assert_eq!(
            statuses,
            [
                ("audio", "applied"),
                ("wallpaper", "applied"),
                ("companion", "applied")
            ]
        );
        assert!(!outcome.report.partial);
        let recorded = std::fs::read_to_string(&recorder).unwrap();
        assert!(recorded.contains("audio --config"), "{recorded}");
        assert!(recorded.contains("wallpaper --socket"), "{recorded}");
        assert!(recorded.contains("scene use night"), "{recorded}");
        assert!(recorded.contains("wallpaper --socket"), "{recorded}");
        assert!(recorded.contains("companion call rules run"), "{recorded}");
        assert!(
            recorded.contains("marks={\"project\":\"work\"}"),
            "{recorded}"
        );
    }

    #[test]
    fn optional_profile_failures_mark_partial_but_do_not_abort() {
        let dir = temp_dir("partial");
        let fake = start_fake("partial-server", FakeState::default());
        let context = Context {
            profiles: Profiles {
                monitor: Some(MonitorProfile {
                    profile: "missing".to_owned(),
                    apply: MonitorApply::Auto,
                    command: None,
                    hooks: ProfileHooks::default(),
                }),
                audio: Some(AudioProfile {
                    profile: "broken".to_owned(),
                    config: None,
                    command: Some("exit 4".to_owned()),
                    hooks: ProfileHooks::default(),
                }),
                ..Profiles::default()
            },
            ..Context::default()
        };
        let outcome = restore_context(
            &fake.path,
            "work",
            &context,
            &empty_options(test_profile_env(None, &dir)),
        )
        .unwrap();
        assert!(outcome.report.ok);
        assert!(outcome.report.partial);
        assert!(!outcome.report.aborted);
        assert_eq!(outcome.report.profiles[0].status, "missing");
        assert_eq!(outcome.report.profiles[1].status, "failed");
        assert!(outcome.failure.is_none());
    }

    #[test]
    fn dry_run_skips_profile_commands_and_hooks() {
        let dir = temp_dir("profile-dry");
        let recorder = dir.join("recorder");
        let fake = start_fake("profile-dry-server", FakeState::default());
        let context = Context {
            profiles: Profiles {
                audio: Some(AudioProfile {
                    profile: "work".to_owned(),
                    config: None,
                    command: Some(format!("printf 'ran' >> {}", recorder.display())),
                    hooks: ProfileHooks {
                        activate: vec!["exit 9".to_owned()],
                        ..ProfileHooks::default()
                    },
                }),
                ..Profiles::default()
            },
            ..Context::default()
        };
        let mut options = empty_options(test_profile_env(None, &dir));
        options.dry_run = true;
        let outcome = restore_context(&fake.path, "work", &context, &options).unwrap();
        assert_eq!(outcome.report.profiles[0].status, "skipped");
        assert_eq!(
            outcome.report.profiles[0].activate.as_ref().unwrap().status,
            "skipped"
        );
        assert!(!recorder.exists());
        assert_eq!(outcome.report.hooks.activate.status, "skipped");
    }

    #[test]
    fn activate_and_deactivate_hooks_run_in_order_with_phase_env() {
        let dir = temp_dir("hook-phases");
        let file = dir.join("phases");
        let path = file.display().to_string();
        let context = Context {
            hooks: Hooks {
                activate: vec![format!(
                    "printf 'activate:%s\\n' \"$LOOM_SESSION_PHASE\" >> {path}"
                )],
                deactivate: vec![format!(
                    "printf 'deactivate:%s\\n' \"$LOOM_SESSION_PHASE\" >> {path}"
                )],
                ..Hooks::default()
            },
            profiles: Profiles {
                audio: Some(AudioProfile {
                    profile: "work".to_owned(),
                    config: None,
                    command: Some("true".to_owned()),
                    hooks: ProfileHooks {
                        activate: vec![format!(
                            "printf 'profile:%s:%s\\n' \"$LOOM_SESSION_PROFILE\" \"$LOOM_SESSION_PHASE\" >> {path}"
                        )],
                        deactivate: vec![format!(
                            "printf 'profile:%s:%s\\n' \"$LOOM_SESSION_PROFILE\" \"$LOOM_SESSION_PHASE\" >> {path}"
                        )],
                    },
                }),
                ..Profiles::default()
            },
            ..Context::default()
        };
        let fake = start_fake("hook-phases-server", FakeState::default());
        let options = empty_options(test_profile_env(None, &dir));
        let outcome = restore_context(&fake.path, "work", &context, &options).unwrap();
        assert!(outcome.failure.is_none());
        assert_eq!(outcome.report.hooks.activate.status, "ok");
        assert_eq!(
            outcome.report.profiles[0].activate.as_ref().unwrap().status,
            "ok"
        );

        let report = deactivate_context("work", &context, false, &options.profile_env);
        assert!(report.ok, "{report:?}");
        assert_eq!(report.hooks.deactivate.status, "ok");
        assert_eq!(report.profiles[0].deactivate.as_ref().unwrap().status, "ok");

        let lines: Vec<String> = std::fs::read_to_string(&file)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            lines,
            [
                "profile:audio:activate",
                "activate:activate",
                "profile:audio:deactivate",
                "deactivate:deactivate"
            ]
        );

        let dry = deactivate_context("work", &context, true, &options.profile_env);
        assert!(dry.dry_run);
        assert_eq!(dry.hooks.deactivate.status, "skipped");
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 4);
    }

    #[test]
    fn guards_skip_steps_that_user_changes_already_satisfied() {
        let fake = start_fake(
            "guards",
            FakeState {
                workspaces: vec![workspace_state(1, 1, Some("work"), "eDP-1", true, true)],
                windows: vec![window_state(20, "firefox", 1, false, true)],
                outputs: outputs([output("eDP-1", 1.0, false)]),
                ..FakeState::default()
            },
        );
        let steps = vec![
            Step {
                phase: Phase::Window,
                target: "firefox".to_owned(),
                action: "focus".to_owned(),
                request: Request::Action(Action::FocusWindow { id: 20 }),
                guard: Some(Guard::FocusWindow(20)),
            },
            Step {
                phase: Phase::Window,
                target: "firefox".to_owned(),
                action: "tile".to_owned(),
                request: Request::Action(Action::MoveWindowToTiling { id: Some(20) }),
                guard: Some(Guard::Floating {
                    id: 20,
                    floating: false,
                }),
            },
            Step {
                phase: Phase::Window,
                target: "firefox".to_owned(),
                action: "move".to_owned(),
                request: Request::Action(Action::MoveWindowToWorkspace {
                    window_id: Some(20),
                    reference: WorkspaceReferenceArg::Name("work".to_owned()),
                    focus: false,
                }),
                guard: Some(Guard::MoveWindow {
                    id: 20,
                    reference: WorkspaceReferenceArg::Name("work".to_owned()),
                }),
            },
            Step {
                phase: Phase::Window,
                target: "gone".to_owned(),
                action: "focus".to_owned(),
                request: Request::Action(Action::FocusWindow { id: 99 }),
                guard: Some(Guard::WindowAlive(99)),
            },
            Step {
                phase: Phase::Output,
                target: "DP-9".to_owned(),
                action: "set scale".to_owned(),
                request: Request::Output {
                    output: "DP-9".to_owned(),
                    action: OutputAction::Off,
                },
                guard: Some(Guard::OutputAlive("DP-9".to_owned())),
            },
        ];
        let outcome = execute_steps_guarded(&fake.path, &steps, false, 0, true).unwrap();
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.skipped.len(), 5);
        assert!(
            outcome
                .skipped
                .iter()
                .all(|skipped| !skipped.reason.is_empty())
        );

        let outcome = execute_steps_guarded(&fake.path, &steps, false, 0, false).unwrap();
        assert_eq!(outcome.applied.len(), 5);
        assert!(outcome.skipped.is_empty());
    }

    #[test]
    fn restore_reconnects_to_a_late_niri_socket() {
        let fake = start_fake_after(
            "retry-socket",
            Duration::from_millis(40),
            FakeState::default(),
        );
        let context = Context::default();
        let mut options = empty_options(test_profile_env(None, Path::new("/nonexistent")));
        options.retries = 8;
        let outcome = restore_context(&fake.path, "work", &context, &options).unwrap();
        assert!(outcome.report.ok);

        options.retries = 0;
        let missing = Path::new("/nonexistent/loom-session.sock");
        assert!(restore_context(missing, "work", &context, &options).is_err());
    }

    #[test]
    fn companion_rules_run_all_when_no_calls_are_selected() {
        let dir = temp_dir("companion-rules");
        let bin = dir.join("bin");
        let recorder = dir.join("recorder");
        write_script(
            &bin,
            "loomctl",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n",
                recorder.display()
            ),
        );
        let fake = start_fake("companion-rules-server", FakeState::default());
        let context = Context {
            profiles: Profiles {
                companion: Some(CompanionProfile {
                    rules: vec!["work".to_owned()],
                    marks: BTreeMap::new(),
                    calls: Vec::new(),
                    command: None,
                    hooks: ProfileHooks::default(),
                }),
                ..Profiles::default()
            },
            ..Context::default()
        };
        let mut env = test_profile_env(Some(&bin), &dir);
        env.companion_socket = Some(dir.join("companion.sock"));
        let outcome = restore_context(&fake.path, "work", &context, &empty_options(env)).unwrap();
        assert_eq!(outcome.report.profiles[0].status, "applied");
        let recorded = std::fs::read_to_string(&recorder).unwrap();
        assert!(recorded.contains("--socket"), "{recorded}");
        assert!(recorded.contains("call rules run"), "{recorded}");
    }

    #[test]
    fn import_export_round_trip_merge_replace_and_validation() {
        let mut source = Store::default();
        source.insert("work", sample_context());
        source.insert("home", Context::default());
        let exported = export_store(&source);

        let mut target = Store::default();
        target.insert("work", Context::default());
        target.insert("old", Context::default());
        let report = import_into(&mut target, &exported, ImportMode::Merge, None).unwrap();
        assert_eq!(report.added, ["home"]);
        assert_eq!(report.updated, ["work"]);
        assert_eq!(target.get("work"), source.get("work"));

        let single = export_context("desk", &sample_context());
        let report = import_into(&mut target, &single, ImportMode::Merge, None).unwrap();
        assert_eq!(report.added, ["desk"]);

        let report = import_into(&mut target, &single, ImportMode::Replace, None).unwrap();
        assert!(report.replaced);
        assert_eq!(report.removed, ["home", "old", "work"]);
        assert_eq!(target.contexts.len(), 1);
        assert!(target.get("desk").is_some());

        let plain = serde_json::to_value(sample_context()).unwrap();
        assert!(import_into(&mut target, &plain, ImportMode::Merge, None).is_err());
        import_into(&mut target, &plain, ImportMode::Merge, Some("plain")).unwrap();
        assert!(target.get("plain").is_some());

        let newer = json!({ "version": SCHEMA_VERSION + 1, "contexts": {} });
        let error = import_into(&mut target, &newer, ImportMode::Merge, None).unwrap_err();
        assert!(error.contains("unsupported"), "{error}");
    }

    #[test]
    fn version_one_contexts_without_new_fields_still_load() {
        let path = temp_path("v1-new-fields");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 1,
                "contexts": {
                    "legacy": {
                        "created_at": 1,
                        "windows": [],
                        "workspaces": [],
                        "outputs": {},
                        "hooks": { "pre": ["true"], "post": ["true"] }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let store = Store::load(&path).unwrap();
        let context = store.get("legacy").unwrap();
        assert!(context.profiles.is_empty());
        assert!(context.template.is_none());
        assert!(context.hooks.activate.is_empty());
        assert!(context.hooks.deactivate.is_empty());
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
