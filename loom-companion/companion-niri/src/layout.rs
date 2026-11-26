//! Workspace and output target resolution shared by the niri contexts.
//!
//! Callers may target a workspace by stable id (`workspace`), by name
//! (`workspace_name`), or by its current index on a monitor (`workspace_idx`).
//! An optional `workspace_output` restricts name/index resolution to one
//! output, which matters because indices are only unique per monitor.
//!
//! Resolution is deterministic: name matching is case-insensitive and exact,
//! and an index that exists on several outputs prefers the focused workspace,
//! then the active one, then the lowest id. Ambiguous names are reported as an
//! error rather than silently picking one.

use std::collections::BTreeSet;

use companion_core::AUTO_WORKSPACES;
use companion_parking::Parking;
use niri_ipc::{Request, Response, Workspace};
use serde_json::Value;

use crate::Compositor;

/// A workspace selector parsed from action parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceTarget {
    pub id: Option<u64>,
    pub name: Option<String>,
    pub index: Option<u8>,
    /// Restricts name/index resolution to one output name.
    pub output: Option<String>,
}

impl WorkspaceTarget {
    /// Parses `workspace` (id or name), `workspace_name`, `workspace_idx`
    /// (alias `workspace_index`) and `workspace_output`.
    ///
    /// Returns `Ok(None)` when no selector is present. Supplying more than one
    /// of id/name/index is rejected so a typo cannot silently pick a target.
    pub fn from_params(params: &Value) -> Result<Option<Self>, String> {
        let mut target = Self::default();

        if let Some(value) = params.get("workspace").filter(|value| !value.is_null()) {
            if let Some(id) = value.as_u64() {
                target.id = Some(id);
            } else if let Some(name) = value.as_str() {
                target.name = Some(name.to_owned());
            } else {
                return Err("`workspace` must be a workspace id or name".to_owned());
            }
        }
        if let Some(name) = params.get("workspace_name").and_then(Value::as_str) {
            if target.id.is_some() || target.name.is_some() {
                return Err(
                    "use only one of `workspace`, `workspace_name` or `workspace_idx`".to_owned(),
                );
            }
            target.name = Some(name.to_owned());
        }
        if let Some(value) = params
            .get("workspace_idx")
            .or_else(|| params.get("workspace_index"))
            .filter(|value| !value.is_null())
        {
            if target.id.is_some() || target.name.is_some() {
                return Err(
                    "use only one of `workspace`, `workspace_name` or `workspace_idx`".to_owned(),
                );
            }
            let index = value
                .as_u64()
                .ok_or_else(|| "`workspace_idx` must be an integer".to_owned())?;
            target.index = Some(
                u8::try_from(index).map_err(|_| "`workspace_idx` must be 0..=255".to_owned())?,
            );
        }
        target.output = params
            .get("workspace_output")
            .and_then(Value::as_str)
            .map(str::to_owned);

        if target.id.is_none()
            && target.name.is_none()
            && target.index.is_none()
            && target.output.is_none()
        {
            return Ok(None);
        }
        Ok(Some(target))
    }

    pub fn is_empty(&self) -> bool {
        self.id.is_none() && self.name.is_none() && self.index.is_none() && self.output.is_none()
    }

    fn output_matches(&self, workspace: &Workspace) -> bool {
        self.output.as_deref().is_none_or(|output| {
            workspace
                .output
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(output))
        })
    }
}

fn workspace_label(workspace: &Workspace) -> String {
    format!(
        "{}{} (id {})",
        workspace.name.as_deref().unwrap_or("<unnamed>"),
        workspace
            .output
            .as_deref()
            .map(|output| format!(" on {output}"))
            .unwrap_or_default(),
        workspace.id
    )
}

/// Resolves `target` inside an already-fetched workspace list.
pub fn resolve_workspace<'a>(
    target: &WorkspaceTarget,
    workspaces: &'a [Workspace],
) -> Result<&'a Workspace, String> {
    let candidates: Vec<&Workspace> = workspaces
        .iter()
        .filter(|workspace| target.output_matches(workspace))
        .collect();
    let output_hint = target
        .output
        .as_deref()
        .map(|output| format!(" on output `{output}`"))
        .unwrap_or_default();

    if let Some(id) = target.id {
        return candidates
            .into_iter()
            .find(|workspace| workspace.id == id)
            .ok_or_else(|| format!("workspace {id} does not exist{output_hint}"));
    }

    if let Some(name) = target.name.as_deref() {
        let mut named: Vec<&Workspace> = candidates
            .into_iter()
            .filter(|workspace| {
                workspace
                    .name
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(name))
            })
            .collect();
        named.sort_by_key(|workspace| workspace.id);
        match named.as_slice() {
            [workspace] => return Ok(workspace),
            [] => {
                let mut available: Vec<String> = workspaces
                    .iter()
                    .filter_map(|workspace| workspace.name.clone())
                    .collect();
                available.sort();
                return Err(format!(
                    "no workspace named `{name}`{output_hint} (available: {})",
                    if available.is_empty() {
                        "none".to_owned()
                    } else {
                        available.join(", ")
                    }
                ));
            }
            _ => {
                return Err(format!(
                    "workspace name `{name}` is ambiguous{output_hint}; set \
                     `workspace_output` or use a workspace id"
                ));
            }
        }
    }

    if let Some(index) = target.index {
        let mut indexed: Vec<&Workspace> = candidates
            .into_iter()
            .filter(|workspace| workspace.idx == index)
            .collect();
        indexed
            .sort_by_key(|workspace| (!workspace.is_focused, !workspace.is_active, workspace.id));
        if let Some(workspace) = indexed.first() {
            return Ok(workspace);
        }
        return Err(format!("no workspace at index {index}{output_hint}"));
    }

    Err(format!(
        "workspace target is incomplete{output_hint}; set `workspace`, \
         `workspace_name` or `workspace_idx`"
    ))
}

/// Describes every workspace for an error message.
pub fn describe_workspaces(workspaces: &[Workspace]) -> String {
    if workspaces.is_empty() {
        return "none".to_owned();
    }
    let mut labels: Vec<String> = workspaces.iter().map(workspace_label).collect();
    labels.sort();
    labels.join(", ")
}

/// Queries the live workspace list from niri.
pub fn query_workspaces(compositor: &Compositor) -> Result<Vec<Workspace>, String> {
    match compositor.send(Request::Workspaces)? {
        Response::Workspaces(workspaces) => Ok(workspaces),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

/// Workspaces from the `auto:workspaces` parking lot, used when the niri
/// socket is unavailable.
pub fn cached_workspaces(parking: &Parking) -> Vec<Workspace> {
    let mut workspaces: Vec<Workspace> = parking
        .lot(AUTO_WORKSPACES)
        .into_values()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    workspaces.sort_by_key(|workspace| workspace.id);
    workspaces
}

/// Queries niri, falling back to the cached auto lot only when the query
/// itself fails.
pub fn query_or_cached_workspaces(
    compositor: &Compositor,
    parking: &Parking,
) -> Result<Vec<Workspace>, String> {
    match query_workspaces(compositor) {
        Ok(workspaces) => Ok(workspaces),
        Err(_) => Ok(cached_workspaces(parking)),
    }
}

/// Resolves `target` against the live workspace list.
pub fn resolve_live_workspace(
    compositor: &Compositor,
    target: &WorkspaceTarget,
) -> Result<Workspace, String> {
    let workspaces = query_workspaces(compositor)?;
    resolve_workspace(target, &workspaces)
        .cloned()
        .map_err(|err| {
            format!(
                "{err}; known workspaces: {}",
                describe_workspaces(&workspaces)
            )
        })
}

/// Connected output names, sorted for stable output.
pub fn output_names(compositor: &Compositor) -> Result<Vec<String>, String> {
    match compositor.send(Request::Outputs)? {
        Response::Outputs(outputs) => {
            let mut names: Vec<String> = outputs.into_keys().collect();
            names.sort();
            Ok(names)
        }
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

/// Canonicalises an output name.
///
/// When the output list cannot be queried the caller's spelling is returned so
/// niri can still validate it; an empty list is treated as "unavailable" for
/// the same reason.
pub fn canonical_output(compositor: &Compositor, requested: &str) -> Result<String, String> {
    let Ok(names) = output_names(compositor) else {
        return Ok(requested.to_owned());
    };
    if names.is_empty() {
        return Ok(requested.to_owned());
    }
    if let Some(name) = names.iter().find(|name| *name == requested) {
        return Ok(name.clone());
    }
    if let Some(name) = names
        .iter()
        .find(|name| name.eq_ignore_ascii_case(requested))
    {
        return Ok(name.clone());
    }
    Err(format!(
        "unknown output `{requested}` (available: {})",
        names.join(", ")
    ))
}

/// Workspace ids that live on `output`, sorted by id.
pub fn workspace_ids_on_output(
    compositor: &Compositor,
    output: &str,
) -> Result<BTreeSet<u64>, String> {
    let workspaces = query_workspaces(compositor)?;
    let ids: BTreeSet<u64> = workspaces
        .iter()
        .filter(|workspace| {
            workspace
                .output
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(output))
        })
        .map(|workspace| workspace.id)
        .collect();
    if ids.is_empty() {
        return Err(format!(
            "no workspaces on output `{output}` (known: {})",
            describe_workspaces(&workspaces)
        ));
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn workspace(id: u64, idx: u8, name: Option<&str>, output: &str, focused: bool) -> Workspace {
        serde_json::from_value(json!({
            "id": id,
            "idx": idx,
            "name": name,
            "output": output,
            "is_urgent": false,
            "is_active": focused,
            "is_focused": focused,
            "active_window_id": null,
            "is_hidden": false
        }))
        .unwrap()
    }

    fn sample() -> Vec<Workspace> {
        vec![
            workspace(1, 1, Some("dev"), "eDP-1", true),
            workspace(2, 2, Some("Web"), "eDP-1", false),
            workspace(3, 1, Some("notes"), "HDMI-1", false),
        ]
    }

    #[test]
    fn resolves_by_id_name_and_index() {
        let workspaces = sample();
        let target = WorkspaceTarget::from_params(&json!({ "workspace": 2 }))
            .unwrap()
            .unwrap();
        assert_eq!(
            resolve_workspace(&target, &workspaces).unwrap().id,
            2,
            "stable id"
        );

        let target = WorkspaceTarget::from_params(&json!({ "workspace_name": "web" }))
            .unwrap()
            .unwrap();
        assert_eq!(
            resolve_workspace(&target, &workspaces).unwrap().id,
            2,
            "names are case-insensitive"
        );

        // Index 1 exists on both outputs; the focused workspace wins.
        let target = WorkspaceTarget::from_params(&json!({ "workspace_idx": 1 }))
            .unwrap()
            .unwrap();
        assert_eq!(resolve_workspace(&target, &workspaces).unwrap().id, 1);

        // Restricting the output disambiguates the index.
        let target = WorkspaceTarget::from_params(
            &json!({ "workspace_idx": 1, "workspace_output": "HDMI-1" }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolve_workspace(&target, &workspaces).unwrap().id, 3);
    }

    #[test]
    fn string_workspace_is_a_name_and_conflicts_are_rejected() {
        let target = WorkspaceTarget::from_params(&json!({ "workspace": "dev" }))
            .unwrap()
            .unwrap();
        assert_eq!(target.name.as_deref(), Some("dev"));
        assert!(
            WorkspaceTarget::from_params(&json!({ "workspace": 1, "workspace_name": "dev" }))
                .is_err()
        );
        assert!(WorkspaceTarget::from_params(&json!({ "workspace_idx": 999 })).is_err());
        assert!(WorkspaceTarget::from_params(&json!({})).unwrap().is_none());
    }

    #[test]
    fn missing_and_ambiguous_names_report_available_workspaces() {
        let workspaces = sample();
        let target = WorkspaceTarget::from_params(&json!({ "workspace_name": "nope" }))
            .unwrap()
            .unwrap();
        let error = resolve_workspace(&target, &workspaces).unwrap_err();
        assert!(error.contains("no workspace named `nope`"), "{error}");
        assert!(error.contains("dev"), "{error}");
        assert!(error.contains("Web"), "{error}");

        let mut duplicated = workspaces;
        duplicated.push(workspace(4, 3, Some("dev"), "HDMI-1", false));
        let target = WorkspaceTarget::from_params(&json!({ "workspace_name": "dev" }))
            .unwrap()
            .unwrap();
        let error = resolve_workspace(&target, &duplicated).unwrap_err();
        assert!(error.contains("ambiguous"), "{error}");
    }

    #[test]
    fn cached_workspaces_are_sorted_by_id() {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-layout-{}-cache",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let parking = Parking::load(dir.join("parking.json")).unwrap();
        parking
            .replace_lot(
                AUTO_WORKSPACES,
                [
                    (
                        "2".to_owned(),
                        json!({
                            "id": 2, "idx": 2, "name": "b", "output": "eDP-1",
                            "is_urgent": false, "is_active": false, "is_focused": false,
                            "active_window_id": null, "is_hidden": false
                        }),
                    ),
                    (
                        "1".to_owned(),
                        json!({
                            "id": 1, "idx": 1, "name": "a", "output": "eDP-1",
                            "is_urgent": false, "is_active": true, "is_focused": true,
                            "active_window_id": null, "is_hidden": false
                        }),
                    ),
                ]
                .into_iter()
                .collect(),
            )
            .unwrap();
        let cached = cached_workspaces(&parking);
        assert_eq!(
            cached
                .iter()
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }
}
