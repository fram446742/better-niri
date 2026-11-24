//! Small window/workspace helpers shared by the workflow contexts.
//!
//! `stage.*`, `scratchpad.*` and `windows.*` all need the same primitives: find
//! the focused window or workspace, take a snapshot, and send one niri action.
//! They live here so the contexts reuse one implementation of the niri bridge
//! instead of each growing its own copy.

use std::collections::BTreeMap;

use companion_core::AUTO_WINDOWS;
use companion_parking::Parking;
use niri_ipc::{Action, Request, Response, Window, Workspace};
use serde_json::Value;

use crate::Compositor;

/// Auto-tracked window snapshots keyed by string id.
pub(crate) fn auto_windows(parking: &Parking) -> BTreeMap<String, Value> {
    parking.lot(AUTO_WINDOWS)
}

/// A window's tracked snapshot, falling back to a live query when the event
/// tracker has not seen it (for example when the daemon runs with `--no-track`).
pub(crate) fn window_snapshot(
    parking: &Parking,
    compositor: &Compositor,
    id: u64,
) -> Result<Value, String> {
    if let Some(value) = auto_windows(parking).get(&id.to_string()) {
        return Ok(value.clone());
    }
    let window = window_query(compositor)?
        .into_iter()
        .find(|window| window.id == id)
        .ok_or_else(|| format!("window {id} is not open"))?;
    serde_json::to_value(window).map_err(|err| format!("encode window: {err}"))
}

pub(crate) fn window_query(compositor: &Compositor) -> Result<Vec<Window>, String> {
    match compositor.send(Request::Windows)? {
        Response::Windows(windows) => Ok(windows),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

pub(crate) fn workspace_query(compositor: &Compositor) -> Result<Vec<Workspace>, String> {
    match compositor.send(Request::Workspaces)? {
        Response::Workspaces(workspaces) => Ok(workspaces),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

/// Focused window id from the auto lot, then from niri.
pub(crate) fn focused_window_id(parking: &Parking, compositor: &Compositor) -> Result<u64, String> {
    if let Some((id, _)) = auto_windows(parking)
        .iter()
        .find(|(_, value)| value.get("is_focused").and_then(Value::as_bool) == Some(true))
    {
        return id
            .parse()
            .map_err(|_| format!("invalid window id `{id}` in {AUTO_WINDOWS}"));
    }

    match compositor.send(Request::FocusedWindow)? {
        Response::FocusedWindow(Some(window)) => Ok(window.id),
        Response::FocusedWindow(None) => Err("no focused window".to_owned()),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

/// Focused workspace id from the auto lot, then from niri.
pub(crate) fn focused_workspace_id(
    parking: &Parking,
    compositor: &Compositor,
) -> Result<u64, String> {
    let workspaces = parking.lot(companion_core::AUTO_WORKSPACES);
    let from_auto = workspaces
        .values()
        .find(|value| value.get("is_focused").and_then(Value::as_bool) == Some(true))
        .or_else(|| {
            workspaces
                .values()
                .find(|value| value.get("is_active").and_then(Value::as_bool) == Some(true))
        })
        .and_then(|value| value.get("id").and_then(Value::as_u64));
    if let Some(id) = from_auto {
        return Ok(id);
    }

    workspace_query(compositor)?
        .into_iter()
        .find(|workspace| workspace.is_focused)
        .map(|workspace| workspace.id)
        .ok_or_else(|| "no focused workspace".to_owned())
}

pub(crate) fn send_action(compositor: &Compositor, action: Action) -> Result<(), String> {
    match compositor.send(Request::Action(action))? {
        Response::Handled => Ok(()),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

/// `window` (or its alias `id`) from action parameters.
pub(crate) fn param_window(params: &Value) -> Option<u64> {
    params
        .get("window")
        .or_else(|| params.get("id"))
        .and_then(Value::as_u64)
}

/// A named scratchpad slot from `name` (or its alias `slot`).
pub(crate) fn slot_name(params: &Value) -> Result<String, String> {
    let name = params
        .get("name")
        .or_else(|| params.get("slot"))
        .and_then(Value::as_str)
        .unwrap_or("default");
    let name = name.trim();
    if name.is_empty() {
        return Err("scratchpad name must not be empty".to_owned());
    }
    if name.len() > 64 {
        return Err("scratchpad name must be at most 64 bytes".to_owned());
    }
    Ok(name.to_owned())
}
