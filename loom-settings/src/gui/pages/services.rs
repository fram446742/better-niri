//! Services: companion tool installation, portal backend, Noctalia source and diagnostics.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita::prelude::*;
use loom_settings::integrations;

use crate::gui::{App, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();

    let tools = widgets::group(
        "Companion tools",
        Some(
            "Every companion stays the authority for its own data; installed CLIs are probed with --version.",
        ),
    );
    let statuses = integrations::tool_statuses(app.runner.as_ref());
    let installed = statuses.iter().filter(|status| status.installed).count();
    tools.add(&widgets::info_row(
        "Installed",
        &format!("{installed}/{} companion CLIs", statuses.len()),
    ));
    for status in &statuses {
        let detail = if status.installed {
            match (&status.version, &status.detail) {
                (Some(version), _) => format!("{} · {version}", status.program),
                (None, Some(detail)) => format!("{} · {detail}", status.program),
                (None, None) => format!("{} · version unknown", status.program),
            }
        } else {
            format!("not installed · {}", status.program)
        };
        let row = widgets::info_row(status.tool.program(), &detail);
        let icon = gtk::Image::from_icon_name(if status.installed {
            "emblem-ok-symbolic"
        } else {
            "action-unavailable-symbolic"
        });
        if status.installed {
            icon.add_css_class("success");
        } else {
            icon.add_css_class("dim-label");
        }
        row.add_suffix(&icon);
        tools.add(&row);
    }
    page.add(&tools);

    let portals_conf = integrations::portal_config_path(None);
    let portal = widgets::group(
        "Portal",
        Some(
            "loom-portal is a standalone xdg-desktop-portal backend for screenshots and ScreenCast.",
        ),
    );
    let view = integrations::portal_view(app.runner.as_ref(), &portals_conf);
    app.view_group(&portal, &view);
    page.add(&portal);

    let noctalia_path = integrations::noctalia_source(None);
    let noctalia = widgets::group(
        "Noctalia plugins",
        Some(
            "The plugin source is a Luau tree; loom-settings only reports whether it is installed.",
        ),
    );
    let view = integrations::noctalia_view(&noctalia_path);
    app.view_group(&noctalia, &view);
    page.add(&noctalia);

    let diagnostics = widgets::group(
        "Diagnostics",
        Some("Collect versions, paths and live status into one report you can copy."),
    );
    {
        let app = app.clone();
        let portals_conf = portals_conf.clone();
        diagnostics.add(&widgets::button_row(
            "Collect diagnostics",
            "niri version/capabilities, tool versions, portal unit, plugin source and config path.",
            "Collect",
            false,
            move || app.report("Diagnostics", &diagnostics_text(&app, &portals_conf)),
        ));
    }
    page.add(&diagnostics);

    page.upcast()
}

fn diagnostics_text(app: &Rc<App>, portals_conf: &std::path::Path) -> String {
    let mut lines = Vec::new();
    lines.push(format!("loom-settings {}", env!("CARGO_PKG_VERSION")));
    lines.push(format!("config: {}", app.path.display()));
    lines.push(format!("portals.conf: {}", portals_conf.display()));
    lines.push(String::new());
    lines.push("== compositor ==".to_owned());
    let compositor = integrations::compositor_status(app.runner.as_ref());
    lines.push(format!(
        "version: compositor {} / cli {}",
        compositor
            .compositor_version
            .as_deref()
            .unwrap_or("unknown"),
        compositor.cli_version.as_deref().unwrap_or("unknown")
    ));
    lines.push(format!(
        "renderer: {}; hdr {}; outputs {} ({} HDR, {} active)",
        compositor.renderer(),
        compositor.hdr,
        compositor.outputs,
        compositor.hdr_outputs,
        compositor.hdr_active
    ));
    if let Some(error) = &compositor.error {
        lines.push(format!("error: {error}"));
    }

    lines.push(String::new());
    lines.push("== tools ==".to_owned());
    for status in integrations::tool_statuses(app.runner.as_ref()) {
        lines.push(format!(
            "{}: {}",
            status.tool.program(),
            if status.installed {
                status.version.as_deref().unwrap_or("installed")
            } else {
                "not installed"
            }
        ));
    }

    lines.push(String::new());
    lines.push("== portal ==".to_owned());
    let portal = integrations::portal_status(app.runner.as_ref(), portals_conf);
    lines.push(format!(
        "{}: {} ({})",
        portal.unit,
        portal.active.as_deref().unwrap_or("unknown"),
        portal.enabled.as_deref().unwrap_or("unknown")
    ));
    lines.push(format!(
        "frontend selection: {}",
        if portal.selected {
            "Loom backend named"
        } else {
            "Loom backend not named"
        }
    ));

    lines.push(String::new());
    lines.push("== integrations ==".to_owned());
    let sidepanels = integrations::sidepanels_path(None);
    let overview = integrations::overview(app.runner.as_ref(), &sidepanels);
    for (title, view) in overview.sections() {
        lines.push(format!("{title}: {}", view.status));
        if let Some(error) = &view.error {
            lines.push(format!("  error: {error}"));
        }
    }

    lines.push(String::new());
    lines.push("== noctalia ==".to_owned());
    let source = integrations::read_noctalia_source(&integrations::noctalia_source(None));
    lines.push(format!("source: {}", source.path));
    lines.push(format!(
        "exists: {}; plugins: {}; verify.sh: {}",
        source.exists,
        if source.plugins.is_empty() {
            "(none)".to_owned()
        } else {
            source.plugins.join(", ")
        },
        source.verify_script
    ));

    lines.join("\n")
}
