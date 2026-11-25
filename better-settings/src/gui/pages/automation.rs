//! Automation: companion daemon, rules, idle/rotation policies, marks and history.

use std::rc::Rc;

use better_settings::integrations;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::gui::{App, format, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();

    let daemon = widgets::group(
        "Companion daemon",
        Some("better-companion owns automation state; this page previews and calls its actions."),
    );
    let view = integrations::companion_view(app.runner.as_ref());
    app.view_group(&daemon, &view);
    page.add(&daemon);

    let registry = widgets::group(
        "Action registry",
        Some("Namespaces and actions advertised by the running daemon."),
    );
    match integrations::companion_describe(app.runner.as_ref()) {
        Ok(description) => {
            registry.add(&widgets::info_row(
                "Protocol",
                &format!(
                    "{} v{} · {} namespace(s)",
                    description.protocol,
                    description.version,
                    description.namespaces.len()
                ),
            ));
            for namespace in description.namespaces {
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
                registry.add(&widgets::info_row(&namespace.namespace, &actions));
            }
        }
        Err(err) => {
            let row = widgets::info_row("Registry unavailable", &err.to_string());
            row.add_css_class("error");
            registry.add(&row);
        }
    }
    page.add(&registry);

    let rules = widgets::group(
        "Rules",
        Some("Explain is read-only. Apply runs the matching rules' steps and asks first."),
    );
    {
        let app = app.clone();
        rules.add(&widgets::button_row(
            "Explain matching rules",
            "Run `companionctl explain rules` and show the full match report.",
            "Explain",
            false,
            move || match integrations::companion_explain(app.runner.as_ref(), "rules") {
                Ok(value) => app.report("Rules explanation", &format::pretty(&value)),
                Err(err) => app.report("Explain failed", &err.to_string()),
            },
        ));
    }
    {
        let app = app.clone();
        rules.add(&widgets::button_row(
            "Apply matching rules",
            "Run `companionctl call rules run`. The daemon executes every matching step.",
            "Apply rules",
            true,
            move || {
                let app = app.clone();
                app.clone().confirm(
                    "Run the matching companion rules?",
                    "This executes the configured steps for every matching window or workspace event.",
                    "Run rules",
                    move || match integrations::companion_rules_apply(app.runner.as_ref(), false) {
                        Ok(value) => app.report("Rules applied", &format::pretty(&value)),
                        Err(err) => app.report("Rules failed", &err.to_string()),
                    },
                );
            },
        ));
    }
    page.add(&rules);

    page.add(&adapter_group(
        app,
        "idle",
        "Idle policy",
        integrations::idle_status,
    ));
    page.add(&adapter_group(
        app,
        "rotate",
        "Auto-rotation",
        integrations::rotate_status,
    ));

    let marks = widgets::group(
        "Window marks",
        Some("Marks tracked by the daemon (`companionctl call marks list`)."),
    );
    match integrations::companion_marks(app.runner.as_ref()) {
        Ok(value) => {
            let count = value
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            marks.add(&widgets::info_row(
                "Marks",
                &format!("{count} tracked mark(s)"),
            ));
            if let Some(entries) = value.get("marks").and_then(serde_json::Value::as_array) {
                for entry in entries {
                    let window = entry
                        .get("window")
                        .and_then(serde_json::Value::as_u64)
                        .map(|window| window.to_string())
                        .unwrap_or_else(|| "?".to_owned());
                    let mark = entry
                        .get("mark")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("(none)");
                    marks.add(&widgets::info_row(mark, &format!("window {window}")));
                }
            }
        }
        Err(err) => {
            let row = widgets::info_row("Marks unavailable", &err.to_string());
            row.add_css_class("error");
            marks.add(&row);
        }
    }
    page.add(&marks);

    let history = widgets::group(
        "History",
        Some("Recent rule and automation runs recorded by better-companion."),
    );
    match integrations::companion_history(app.runner.as_ref(), 20) {
        Ok(entries) if !entries.is_empty() => {
            for entry in entries {
                let when = gtk::glib::DateTime::from_unix_local((entry.timestamp_ms / 1000) as i64)
                    .ok()
                    .and_then(|time| time.format("%F %H:%M").ok())
                    .map(|text| text.to_string())
                    .unwrap_or_else(|| entry.timestamp_ms.to_string());
                let title = format!("{} · {} · {when}", entry.name, entry.source);
                let subtitle = if entry.ok {
                    format!("{} step(s) · ok", entry.steps)
                } else {
                    format!(
                        "{} step(s) · error: {}",
                        entry.steps,
                        entry.error.as_deref().unwrap_or("unknown")
                    )
                };
                let row = widgets::info_row(&title, &subtitle);
                if !entry.ok {
                    row.add_css_class("error");
                }
                history.add(&row);
            }
        }
        Ok(_) => history.add(&widgets::info_row(
            "No runs yet",
            "Rule and automation runs appear here after the daemon executes them.",
        )),
        Err(err) => history.add(&widgets::info_row("History unavailable", &err.to_string())),
    }
    page.add(&history);

    let custom = widgets::group(
        "Custom action",
        Some("Dry-run first; Apply asks before calling the daemon."),
    );
    let namespace = adw::EntryRow::new();
    namespace.set_title("Namespace");
    namespace.set_text("system");
    namespace.set_show_apply_button(false);
    let action = adw::EntryRow::new();
    action.set_title("Action");
    action.set_text("ping");
    action.set_show_apply_button(false);
    custom.add(&namespace);
    custom.add(&action);
    {
        let app = app.clone();
        let namespace = namespace.clone();
        let action = action.clone();
        custom.add(&widgets::button_row(
            "Dry-run action",
            "Call the action with `--dry-run`; mutating actions must honour it.",
            "Dry run",
            false,
            move || {
                let namespace = namespace.text().to_string();
                let action = action.text().to_string();
                if namespace.trim().is_empty() || action.trim().is_empty() {
                    app.toast("Enter a namespace and an action");
                    return;
                }
                match integrations::companion_run(
                    app.runner.as_ref(),
                    &namespace,
                    &action,
                    None,
                    true,
                ) {
                    Ok(value) => app.report("Dry-run result", &format::pretty(&value)),
                    Err(err) => app.report("Dry run failed", &err.to_string()),
                }
            },
        ));
    }
    {
        let app = app.clone();
        let namespace = namespace.clone();
        let action = action.clone();
        custom.add(&widgets::button_row(
            "Call action",
            "Confirm, then call the action without a dry run.",
            "Call",
            true,
            move || {
                let namespace = namespace.text().to_string();
                let action = action.text().to_string();
                if namespace.trim().is_empty() || action.trim().is_empty() {
                    app.toast("Enter a namespace and an action");
                    return;
                }
                let app = app.clone();
                app.clone().confirm(
                    "Call this companion action?",
                    &format!("{namespace}.{action} runs without a dry run."),
                    "Call action",
                    move || match integrations::companion_run(
                        app.runner.as_ref(),
                        &namespace,
                        &action,
                        None,
                        false,
                    ) {
                        Ok(value) => app.report("Action result", &format::pretty(&value)),
                        Err(err) => app.report("Action failed", &err.to_string()),
                    },
                );
            },
        ));
    }
    page.add(&custom);

    page.upcast()
}

fn adapter_group<F>(
    app: &Rc<App>,
    namespace: &'static str,
    title: &'static str,
    status: F,
) -> libadwaita::PreferencesGroup
where
    F: Fn(
            &dyn integrations::CommandRunner,
        ) -> Result<integrations::CompanionAdapterStatus, integrations::IntegrationError>
        + 'static,
{
    let group = widgets::group(
        title,
        Some("State is read from the daemon. Enable/disable is explicit and asks first."),
    );
    match status(app.runner.as_ref()) {
        Ok(status) => {
            group.add(&widgets::info_row(
                "Policy",
                if status.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
            ));
            for line in &status.lines {
                group.add(&widgets::info_row(line, ""));
            }
        }
        Err(err) => {
            let row = widgets::info_row("Policy unavailable", &err.to_string());
            row.add_css_class("error");
            group.add(&row);
        }
    }
    for enable in [true, false] {
        let label = if enable { "Enable" } else { "Disable" };
        let app = app.clone();
        group.add(&widgets::button_row(
            &format!("{label} {title}"),
            &format!(
                "Call `{namespace}.{}`.",
                if enable { "enable" } else { "disable" }
            ),
            label,
            false,
            move || {
                let app = app.clone();
                app.clone().confirm(
                    &format!("{label} {title}?"),
                    &format!("The companion daemon updates the {namespace} policy."),
                    label,
                    move || match integrations::companion_adapter_set_enabled(
                        app.runner.as_ref(),
                        namespace,
                        enable,
                        false,
                    ) {
                        Ok(value) => app.toast(format!(
                            "{namespace} {}: {}",
                            if enable { "enabled" } else { "disabled" },
                            format::pretty(&value)
                        )),
                        Err(err) => app.report("Policy change failed", &err.to_string()),
                    },
                );
            },
        ));
    }
    group
}
