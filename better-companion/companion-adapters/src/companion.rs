//! Companion-action events.
//!
//! The adapter engine already tracks niri. Companion actions such as parking or
//! rotating are not visible in niri, so [`companion_events`] creates events from
//! completed registry calls. The daemon installs it as a
//! [`companion_core::CallObserver`], which runs after an action has succeeded.

use serde_json::{Map, Value, json};

use crate::event::AdapterEvent;

/// Translates one successful action into zero or more adapter events.
pub fn companion_events(
    namespace: &str,
    action: &str,
    params: &Value,
    result: &Value,
) -> Vec<AdapterEvent> {
    let payload = merged_payload(params, result);
    match (namespace, action) {
        ("parker", "park") => vec![event("park", "park", payload)],
        ("parker", "park-workspace") => vec![event("park", "park-workspace", payload)],
        ("parker", "restore") => vec![event("restore", "restore", payload)],
        ("parker", "restore-workspace") => {
            vec![event("restore", "restore-workspace", payload)]
        }
        ("stage", "stage" | "add") => vec![event("stage", "stage", payload)],
        ("stage", "toggle") => vec![event("stage", "toggle", payload)],
        ("stage", "activate" | "unstage" | "restore") => {
            vec![event("unstage", "activate", payload)]
        }
        ("scratchpad", "assign" | "add") => {
            vec![event("scratchpad-hide", "assign", payload)]
        }
        ("scratchpad", "hide") => vec![event("scratchpad-hide", "hide", payload)],
        ("scratchpad", "show" | "restore") => {
            vec![event("scratchpad-show", "show", payload)]
        }
        ("scratchpad", "toggle") => vec![event("scratchpad-toggle", "toggle", payload)],
        ("idle", "reset") => vec![event("idle-active", "reset", payload)],
        ("idle", "tick") => idle_transitions(payload, result),
        ("rotate", "tick" | "set")
            if result.get("applied").and_then(Value::as_bool) == Some(true) =>
        {
            vec![event("rotation-applied", action, payload)]
        }
        _ => Vec::new(),
    }
}

fn event(kind: &str, phase: &str, mut payload: Value) -> AdapterEvent {
    payload["phase"] = json!(phase);
    AdapterEvent::new(kind, payload)
}

/// Idle `tick` reports executed timeout actions; each one is a transition.
fn idle_transitions(payload: Value, result: &Value) -> Vec<AdapterEvent> {
    let idle_secs = result.get("idle_secs").cloned().unwrap_or(Value::Null);
    let executed = result
        .get("executed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut events = Vec::new();
    for entry in executed {
        let phase = entry
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("timeout")
            .to_owned();
        let mut transition = payload.clone();
        if let Some(object) = transition.as_object_mut() {
            object.insert("phase".to_owned(), json!(phase));
            object.insert("idle_secs".to_owned(), idle_secs.clone());
            object.insert(
                "action".to_owned(),
                entry.get("action").cloned().unwrap_or(Value::Null),
            );
            object.insert(
                "after_secs".to_owned(),
                entry.get("after_secs").cloned().unwrap_or(Value::Null),
            );
        }
        events.push(AdapterEvent::new("idle-transition", transition));
    }
    events
}

/// Result fields win; request fields fill gaps. Common `window`/`workspace`
/// ids are also exposed under the template names `window_id`/`workspace_id`.
fn merged_payload(params: &Value, result: &Value) -> Value {
    let mut merged: Map<String, Value> = Map::new();
    if let Some(params) = params.as_object() {
        for (key, value) in params {
            merged.insert(key.clone(), value.clone());
        }
    }
    if let Some(result) = result.as_object() {
        for (key, value) in result {
            merged.insert(key.clone(), value.clone());
        }
    }
    if let Some(window) = merged.get("window").and_then(Value::as_u64) {
        merged.insert("window_id".to_owned(), json!(window));
    }
    if let Some(workspace) = merged.get("workspace").and_then(Value::as_u64) {
        merged.insert("workspace_id".to_owned(), json!(workspace));
    }
    Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(events: &[AdapterEvent]) -> Vec<&str> {
        events.iter().map(|event| event.kind.as_str()).collect()
    }

    #[test]
    fn park_restore_stage_and_scratchpad_map_to_events() {
        let events = companion_events(
            "parker",
            "park",
            &json!({ "window": 10 }),
            &json!({ "window": 10, "parked": true }),
        );
        assert_eq!(kinds(&events), ["park"]);
        assert_eq!(events[0].payload["window_id"], 10);
        assert_eq!(events[0].payload["phase"], "park");

        let events = companion_events(
            "stage",
            "activate",
            &json!({ "window": 4 }),
            &json!({ "window": 4, "active": true }),
        );
        assert_eq!(kinds(&events), ["unstage"]);
        assert_eq!(events[0].payload["phase"], "activate");

        let events = companion_events(
            "scratchpad",
            "toggle",
            &json!({ "name": "term" }),
            &json!({ "name": "term", "visible": true }),
        );
        assert_eq!(kinds(&events), ["scratchpad-toggle"]);
        assert_eq!(events[0].payload["name"], "term");
    }

    #[test]
    fn idle_tick_emits_one_transition_per_executed_timeout() {
        let events = companion_events(
            "idle",
            "tick",
            &json!({}),
            &json!({
                "idle_secs": 300,
                "executed": [
                    { "after_secs": 300, "action": "lock", "ok": true },
                    { "after_secs": 0, "action": "dpms-on", "ok": true }
                ]
            }),
        );
        assert_eq!(kinds(&events), ["idle-transition", "idle-transition"]);
        assert_eq!(events[0].payload["phase"], "lock");
        assert_eq!(events[0].payload["idle_secs"], 300);
        assert_eq!(events[1].payload["phase"], "dpms-on");
    }

    #[test]
    fn idle_reset_and_rotation_apply_are_visible() {
        let events = companion_events("idle", "reset", &json!({}), &json!({ "dpms_off": false }));
        assert_eq!(kinds(&events), ["idle-active"]);

        let events = companion_events(
            "rotate",
            "tick",
            &json!({}),
            &json!({ "applied": true, "output": "eDP-1", "transform": "90" }),
        );
        assert_eq!(kinds(&events), ["rotation-applied"]);
        assert_eq!(events[0].payload["output"], "eDP-1");

        assert!(
            companion_events("rotate", "tick", &json!({}), &json!({ "applied": false })).is_empty()
        );
    }

    #[test]
    fn unrelated_actions_are_silent() {
        for (namespace, action) in [
            ("parking", "park"),
            ("compositor", "action"),
            ("session", "dispatch"),
            ("rotate", "status"),
        ] {
            assert!(
                companion_events(namespace, action, &json!({}), &json!({})).is_empty(),
                "{namespace}.{action}"
            );
        }
    }
}
