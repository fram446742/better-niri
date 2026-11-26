//! Overview: compositor/health summary and safe quick actions.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;
use loom_settings::integrations;

use crate::gui::{App, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let text = app.config_text();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    let portals_conf = integrations::portal_config_path(None);
    let status = integrations::overview_status(app.runner.as_ref(), &portals_conf);

    page.append(&widgets::intro(
        "Loom Niri",
        "One control center for the compositor, displays, sessions, audio and automation.",
    ));
    page.append(&widgets::stat_grid(vec![
        (status.compositor.outputs.to_string(), "outputs"),
        (status.profiles.to_string(), "monitor profiles"),
        (status.contexts.to_string(), "session contexts"),
        (status.scenes.to_string(), "wallpaper scenes"),
    ]));

    let summary = widgets::group(
        "Compositor",
        Some("Live state from `niri msg` and the companion CLIs. Nothing is changed here."),
    );
    summary.add(&widgets::info_row(
        "Version",
        &format!(
            "compositor {} / CLI {}",
            status
                .compositor
                .compositor_version
                .as_deref()
                .unwrap_or("unknown"),
            status
                .compositor
                .cli_version
                .as_deref()
                .unwrap_or("unknown")
        ),
    ));
    summary.add(&widgets::info_row(
        "Renderer",
        &format!(
            "{}; HDR support {}",
            status.compositor.renderer(),
            if status.compositor.hdr { "yes" } else { "no" }
        ),
    ));
    summary.add(&widgets::info_row(
        "Outputs",
        &format!(
            "{} connected, {} HDR capable, {} signalling HDR",
            status.compositor.outputs, status.compositor.hdr_outputs, status.compositor.hdr_active
        ),
    ));
    if let Some(error) = &status.compositor.error {
        let row = widgets::info_row("Compositor probe", error);
        row.add_css_class("error");
        summary.add(&row);
    }
    summary.add(&widgets::info_row(
        "Config",
        &format!("{} ({} bytes)", app.path.display(), text.len()),
    ));
    page.append(&summary);

    let validation = widgets::group(
        "Validation",
        Some("Structural KDL checks and catalog round-trips."),
    );
    validation.add(&widgets::info_row(
        "Status",
        &crate::gui::validation_message(&text),
    ));
    page.append(&validation);

    let actions = widgets::group(
        "Quick actions",
        Some("Explicit actions only; no automatic writes."),
    );
    {
        let app = app.clone();
        actions.add(&widgets::button_row(
            "Validate config",
            "Check KDL structure and every catalog value.",
            "Validate",
            false,
            move || app.validate(),
        ));
    }
    {
        let app = app.clone();
        actions.add(&widgets::button_row(
            "Open config",
            "Open the KDL file in $VISUAL, $EDITOR or the desktop opener.",
            "Open",
            false,
            move || app.open_config(),
        ));
    }
    {
        let app = app.clone();
        actions.add(&widgets::button_row(
            "Reload niri config",
            "Ask the running compositor to reread its config (niri msg action reload-config).",
            "Reload",
            true,
            move || match integrations::reload_config(app.runner.as_ref()) {
                Ok(()) => app.toast("niri reloaded the config"),
                Err(err) => app.report("Reload failed", &err.to_string()),
            },
        ));
    }
    page.append(&actions);

    let outputs_group = widgets::group(
        "Outputs",
        Some("HDR capability and signalling reported by the compositor."),
    );
    match integrations::display_hdr_outputs(app.runner.as_ref()) {
        Ok(outputs) if !outputs.is_empty() => {
            for output in outputs {
                let subtitle = format!(
                    "{}, mode {}{}",
                    if output.supported {
                        "HDR capable"
                    } else {
                        "SDR"
                    },
                    output.mode,
                    if output.active {
                        ", signalling HDR"
                    } else {
                        ""
                    }
                );
                outputs_group.add(&widgets::info_row(&output.name, &subtitle));
            }
        }
        Ok(_) => outputs_group.add(&widgets::info_row(
            "No connected outputs",
            "The compositor reported none; connect a monitor or start niri.",
        )),
        Err(err) => {
            let row = widgets::info_row("Output state unavailable", &err.to_string());
            row.add_css_class("error");
            outputs_group.add(&row);
        }
    }
    page.append(&outputs_group);

    let counts = widgets::group(
        "Saved state",
        Some("Read-only counts from loom-monitor, loom-session and loom-wallpaper."),
    );
    counts.add(&widgets::info_row(
        "Monitor profiles",
        &format!("{} profile(s) in loom-monitor", status.profiles),
    ));
    counts.add(&widgets::info_row(
        "Session contexts",
        &format!(
            "{} context(s), {} history snapshot(s)",
            status.contexts, status.sessions_history
        ),
    ));
    counts.add(&widgets::info_row(
        "Wallpaper scenes",
        &format!("{} scene(s) in loom-wallpaper", status.scenes),
    ));
    page.append(&counts);

    let companion = widgets::group(
        "Automation",
        Some("loom-companion daemon and its action registry."),
    );
    app.view_group(&companion, &status.companion);
    page.append(&companion);

    let portal = widgets::group(
        "Portal",
        Some("loom-portal unit and xdg-desktop-portal frontend selection."),
    );
    portal.add(&widgets::info_row(
        "Unit",
        &format!(
            "{}: {} ({})",
            status.portal.unit,
            status.portal.active.as_deref().unwrap_or("unknown"),
            status.portal.enabled.as_deref().unwrap_or("unknown")
        ),
    ));
    portal.add(&widgets::info_row(
        "Frontend selection",
        &if status.portal.selected {
            format!("{} names the Loom backend", status.portal.config)
        } else {
            format!("{} does not name the Loom backend", status.portal.config)
        },
    ));
    page.append(&portal);

    let tools = widgets::group(
        "Companion tools",
        Some("Installed CLIs and their versions."),
    );
    app.view_group(&tools, &status.tools);
    page.append(&tools);

    widgets::custom_page(page).upcast()
}
