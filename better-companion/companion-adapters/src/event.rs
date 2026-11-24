//! Event model and the niri event-stream mapper.
//!
//! niri only publishes snapshots and low-level changes: there is no
//! `WorkspaceAdded` or `OutputRemoved` event. The mapper therefore keeps the
//! previous workspace/window/output sets and derives the higher-level adapter
//! events from consecutive snapshots and changes. The first snapshot after a
//! (re)connect is used only to seed state, so a daemon restart does not run
//! every subscription once per open window.

use std::collections::{BTreeMap, BTreeSet};

use niri_ipc::{Event, Window, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::normalize_event;

/// A normalized event delivered to adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterEvent {
    #[serde(rename = "event")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

impl AdapterEvent {
    pub fn new(kind: impl AsRef<str>, payload: Value) -> Self {
        Self {
            kind: normalize_event(kind.as_ref()),
            payload: if payload.is_object() {
                payload
            } else {
                json!({ "value": payload })
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
struct WorkspaceInfo {
    id: u64,
    idx: u8,
    name: Option<String>,
    output: Option<String>,
    focused: bool,
}

impl From<&Workspace> for WorkspaceInfo {
    fn from(workspace: &Workspace) -> Self {
        Self {
            id: workspace.id,
            idx: workspace.idx,
            name: workspace.name.clone(),
            output: workspace.output.clone(),
            focused: workspace.is_focused,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct WindowInfo {
    id: u64,
    app_id: Option<String>,
    title: Option<String>,
    workspace_id: Option<u64>,
    focused: bool,
}

impl From<&Window> for WindowInfo {
    fn from(window: &Window) -> Self {
        Self {
            id: window.id,
            app_id: window.app_id.clone(),
            title: window.title.clone(),
            workspace_id: window.workspace_id,
            focused: window.is_focused,
        }
    }
}

/// Stateful conversion from `niri_ipc::Event` to adapter events.
#[derive(Debug, Default)]
pub struct NiriEventMapper {
    primed: bool,
    windows_primed: bool,
    workspaces: BTreeMap<u64, WorkspaceInfo>,
    windows: BTreeMap<u64, WindowInfo>,
    outputs: BTreeSet<String>,
}

impl NiriEventMapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the previous snapshot. Call this before every new niri
    /// subscription; niri replays the full snapshot on connect.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Converts one niri event. The first snapshot is intentionally silent.
    pub fn map(&mut self, event: &Event) -> Vec<AdapterEvent> {
        match event {
            Event::WorkspacesChanged { workspaces } => self.workspaces_changed(workspaces),
            Event::WorkspaceActivated { id, focused } => self.workspace_activated(*id, *focused),
            Event::WindowsChanged { windows } => self.windows_changed(windows),
            Event::WindowOpenedOrChanged { window } => self.window_changed(window),
            Event::WindowClosed { id } => self.window_closed(*id),
            Event::WindowFocusChanged { id } => self.window_focus_changed(*id),
            _ => Vec::new(),
        }
    }

    fn workspace_payload(info: &WorkspaceInfo, phase: &str) -> Value {
        json!({
            "workspace_id": info.id,
            "workspace_name": info.name,
            "workspace_index": info.idx,
            "output": info.output,
            "focused": info.focused,
            "phase": phase,
        })
    }

    fn window_payload(&self, info: &WindowInfo, phase: &str) -> Value {
        let workspace = info.workspace_id.and_then(|id| self.workspaces.get(&id));
        json!({
            "window_id": info.id,
            "app_id": info.app_id,
            "title": info.title,
            "workspace_id": info.workspace_id,
            "workspace_name": workspace.and_then(|workspace| workspace.name.clone()),
            "workspace_index": workspace.map(|workspace| workspace.idx),
            "output": workspace.and_then(|workspace| workspace.output.clone()),
            "focused": info.focused,
            "phase": phase,
        })
    }

    fn workspace_activated(&mut self, id: u64, focused: bool) -> Vec<AdapterEvent> {
        if focused {
            for workspace in self.workspaces.values_mut() {
                workspace.focused = workspace.id == id;
            }
        }
        let Some(info) = self.workspaces.get_mut(&id) else {
            return vec![AdapterEvent::new(
                "workspace-activated",
                json!({ "workspace_id": id, "focused": focused, "phase": "activated" }),
            )];
        };
        info.focused = focused;
        vec![AdapterEvent::new(
            "workspace-activated",
            Self::workspace_payload(info, "activated"),
        )]
    }

    fn workspaces_changed(&mut self, workspaces: &[Workspace]) -> Vec<AdapterEvent> {
        let new: BTreeMap<u64, WorkspaceInfo> = workspaces
            .iter()
            .map(|workspace| (workspace.id, WorkspaceInfo::from(workspace)))
            .collect();
        let new_outputs: BTreeSet<String> = new
            .values()
            .filter_map(|workspace| workspace.output.clone())
            .collect();

        if !self.primed {
            self.primed = true;
            self.workspaces = new;
            self.outputs = new_outputs;
            return Vec::new();
        }

        let old = std::mem::replace(&mut self.workspaces, new);
        let old_outputs = std::mem::replace(&mut self.outputs, new_outputs);
        let mut events = Vec::new();

        for output in self.outputs.difference(&old_outputs) {
            events.push(AdapterEvent::new(
                "output-added",
                json!({ "output": output, "phase": "added" }),
            ));
        }
        for output in old_outputs.difference(&self.outputs) {
            events.push(AdapterEvent::new(
                "output-removed",
                json!({ "output": output, "phase": "removed" }),
            ));
        }

        for (id, info) in &self.workspaces {
            match old.get(id) {
                None => events.push(AdapterEvent::new(
                    "workspace-added",
                    Self::workspace_payload(info, "added"),
                )),
                Some(previous) => {
                    if previous.output != info.output || previous.idx != info.idx {
                        let mut payload = Self::workspace_payload(info, "moved");
                        payload["previous_output"] = json!(previous.output);
                        payload["previous_index"] = json!(previous.idx);
                        events.push(AdapterEvent::new("workspace-moved", payload));
                    }
                }
            }
        }
        for (id, info) in &old {
            if !self.workspaces.contains_key(id) {
                events.push(AdapterEvent::new(
                    "workspace-removed",
                    Self::workspace_payload(info, "removed"),
                ));
            }
        }
        events
    }

    fn windows_changed(&mut self, windows: &[Window]) -> Vec<AdapterEvent> {
        let new: BTreeMap<u64, WindowInfo> = windows
            .iter()
            .map(|window| (window.id, WindowInfo::from(window)))
            .collect();
        if !self.windows_primed {
            self.windows_primed = true;
            self.windows = new;
            return Vec::new();
        }

        let old = std::mem::replace(&mut self.windows, new);
        let mut events = Vec::new();
        for (id, info) in &self.windows {
            match old.get(id) {
                None => events.push(AdapterEvent::new(
                    "window-opened",
                    self.window_payload(info, "opened"),
                )),
                Some(previous) if previous.workspace_id != info.workspace_id => {
                    let mut payload = self.window_payload(info, "moved");
                    payload["previous_workspace_id"] = json!(previous.workspace_id);
                    events.push(AdapterEvent::new("window-moved", payload));
                }
                Some(_) => {}
            }
        }
        for (id, info) in &old {
            if !self.windows.contains_key(id) {
                events.push(AdapterEvent::new(
                    "window-closed",
                    self.window_payload(info, "closed"),
                ));
            }
        }
        events
    }

    fn window_changed(&mut self, window: &Window) -> Vec<AdapterEvent> {
        self.windows_primed = true;
        let info = WindowInfo::from(window);
        let previous = self.windows.insert(window.id, info.clone());
        match previous {
            None => vec![AdapterEvent::new(
                "window-opened",
                self.window_payload(&info, "opened"),
            )],
            Some(previous) if previous.workspace_id != info.workspace_id => {
                let mut payload = self.window_payload(&info, "moved");
                payload["previous_workspace_id"] = json!(previous.workspace_id);
                vec![AdapterEvent::new("window-moved", payload)]
            }
            Some(_) => Vec::new(),
        }
    }

    fn window_closed(&mut self, id: u64) -> Vec<AdapterEvent> {
        let info = self.windows.remove(&id).unwrap_or(WindowInfo {
            id,
            ..WindowInfo::default()
        });
        vec![AdapterEvent::new(
            "window-closed",
            self.window_payload(&info, "closed"),
        )]
    }

    fn window_focus_changed(&mut self, id: Option<u64>) -> Vec<AdapterEvent> {
        for window in self.windows.values_mut() {
            window.focused = Some(window.id) == id;
        }
        match id {
            Some(id) => {
                let info = self.windows.get(&id).cloned().unwrap_or(WindowInfo {
                    id,
                    focused: true,
                    ..WindowInfo::default()
                });
                vec![AdapterEvent::new(
                    "window-focused",
                    self.window_payload(&info, "focused"),
                )]
            }
            None => vec![AdapterEvent::new(
                "window-focused",
                json!({
                    "window_id": null,
                    "app_id": null,
                    "title": null,
                    "focused": false,
                    "phase": "unfocused",
                }),
            )],
        }
    }
}

#[cfg(test)]
mod tests {
    use niri_ipc::{WindowLayout, Workspace};

    use super::*;

    fn workspace(id: u64, idx: u8, name: &str, output: &str, focused: bool) -> Workspace {
        Workspace {
            id,
            idx,
            name: Some(name.to_owned()),
            output: Some(output.to_owned()),
            is_urgent: false,
            is_active: focused,
            is_focused: focused,
            active_window_id: None,
            is_hidden: false,
        }
    }

    fn window(id: u64, app_id: &str, title: &str, workspace: u64, focused: bool) -> Window {
        Window {
            id,
            title: Some(title.to_owned()),
            app_id: Some(app_id.to_owned()),
            pid: Some(42),
            workspace_id: Some(workspace),
            is_focused: focused,
            is_floating: false,
            is_sticky: false,
            is_urgent: false,
            is_minimized: false,
            is_agent: false,
            content_type: None,
            tearing_allowed: false,
            layout: WindowLayout {
                pos_in_scrolling_layout: Some((1, 1)),
                tile_size: (800.0, 600.0),
                window_size: (800, 600),
                tile_pos_in_workspace_view: Some((0.0, 0.0)),
                window_offset_in_tile: (0.0, 0.0),
            },
            focus_timestamp: None,
        }
    }

    fn kinds(events: &[AdapterEvent]) -> Vec<&str> {
        events.iter().map(|event| event.kind.as_str()).collect()
    }

    #[test]
    fn first_snapshot_is_silent_then_workspace_and_output_changes_are_derived() {
        let mut mapper = NiriEventMapper::new();
        assert!(
            mapper
                .map(&Event::WorkspacesChanged {
                    workspaces: vec![workspace(1, 1, "main", "eDP-1", true)],
                })
                .is_empty()
        );

        let events = mapper.map(&Event::WorkspacesChanged {
            workspaces: vec![
                workspace(1, 1, "main", "eDP-1", true),
                workspace(2, 2, "dev", "HDMI-A-1", false),
            ],
        });
        assert_eq!(kinds(&events), ["output-added", "workspace-added"]);
        assert_eq!(events[0].payload["output"], "HDMI-A-1");

        let events = mapper.map(&Event::WorkspacesChanged {
            workspaces: vec![workspace(2, 1, "dev", "HDMI-A-1", true)],
        });
        assert_eq!(
            kinds(&events),
            ["output-removed", "workspace-moved", "workspace-removed"]
        );
        assert_eq!(events[1].payload["previous_index"], 2);
        assert_eq!(events[2].payload["workspace_name"], "main");
    }

    #[test]
    fn workspace_activation_and_window_lifecycle_keep_payload_context() {
        let mut mapper = NiriEventMapper::new();
        mapper.map(&Event::WorkspacesChanged {
            workspaces: vec![workspace(1, 1, "main", "eDP-1", true)],
        });
        mapper.map(&Event::WindowsChanged { windows: vec![] });

        let events = mapper.map(&Event::WorkspaceActivated {
            id: 1,
            focused: true,
        });
        assert_eq!(kinds(&events), ["workspace-activated"]);
        assert_eq!(events[0].payload["workspace_name"], "main");
        assert_eq!(events[0].payload["output"], "eDP-1");
        assert_eq!(events[0].payload["focused"], true);

        let events = mapper.map(&Event::WindowOpenedOrChanged {
            window: window(10, "firefox", "Mozilla", 1, false),
        });
        assert_eq!(kinds(&events), ["window-opened"]);
        assert_eq!(events[0].payload["app_id"], "firefox");
        assert_eq!(events[0].payload["workspace_name"], "main");
        assert_eq!(events[0].payload["output"], "eDP-1");

        let events = mapper.map(&Event::WindowFocusChanged { id: Some(10) });
        assert_eq!(kinds(&events), ["window-focused"]);
        assert_eq!(events[0].payload["focused"], true);

        // A title-only update is not an adapter event.
        let mut changed = window(10, "firefox", "Mozilla — new tab", 1, true);
        changed.workspace_id = Some(1);
        assert!(
            mapper
                .map(&Event::WindowOpenedOrChanged { window: changed })
                .is_empty()
        );

        let events = mapper.map(&Event::WindowClosed { id: 10 });
        assert_eq!(kinds(&events), ["window-closed"]);
        assert_eq!(events[0].payload["title"], "Mozilla — new tab");
    }

    #[test]
    fn window_moves_and_focus_loss_are_visible() {
        let mut mapper = NiriEventMapper::new();
        mapper.map(&Event::WorkspacesChanged {
            workspaces: vec![
                workspace(1, 1, "main", "eDP-1", true),
                workspace(2, 2, "dev", "eDP-1", false),
            ],
        });
        mapper.map(&Event::WindowsChanged {
            windows: vec![window(7, "kitty", "term", 1, true)],
        });

        let events = mapper.map(&Event::WindowOpenedOrChanged {
            window: window(7, "kitty", "term", 2, true),
        });
        assert_eq!(kinds(&events), ["window-moved"]);
        assert_eq!(events[0].payload["workspace_name"], "dev");
        assert_eq!(events[0].payload["previous_workspace_id"], 1);

        let events = mapper.map(&Event::WindowFocusChanged { id: None });
        assert_eq!(kinds(&events), ["window-focused"]);
        assert_eq!(events[0].payload["window_id"], Value::Null);
        assert_eq!(events[0].payload["focused"], false);
    }

    #[test]
    fn windows_changed_snapshot_diffs_open_and_close_but_primes_silently() {
        let mut mapper = NiriEventMapper::new();
        assert!(
            mapper
                .map(&Event::WindowsChanged {
                    windows: vec![window(1, "a", "A", 1, false)],
                })
                .is_empty()
        );
        let events = mapper.map(&Event::WindowsChanged {
            windows: vec![window(1, "a", "A", 1, false), window(2, "b", "B", 2, false)],
        });
        assert_eq!(kinds(&events), ["window-opened"]);

        let events = mapper.map(&Event::WindowsChanged {
            windows: vec![window(2, "b", "B", 1, false)],
        });
        assert_eq!(kinds(&events), ["window-moved", "window-closed"]);
    }

    #[test]
    fn reset_makes_the_next_snapshot_silent_again() {
        let mut mapper = NiriEventMapper::new();
        mapper.map(&Event::WorkspacesChanged {
            workspaces: vec![workspace(1, 1, "main", "eDP-1", true)],
        });
        mapper.reset();
        assert!(
            mapper
                .map(&Event::WorkspacesChanged {
                    workspaces: vec![workspace(2, 2, "new", "HDMI-A-1", true)],
                })
                .is_empty()
        );
    }
}
