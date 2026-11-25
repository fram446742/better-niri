//! Side panels: config summary, validation and named layouts.

use std::rc::Rc;

use better_settings::integrations;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::gui::{App, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();
    let path = integrations::sidepanels_path(None);

    let status_group = widgets::group(
        "Configuration",
        Some(
            "better-sidepanels owns this JSON file; better-settings only reads, validates and opens it.",
        ),
    );
    let view = integrations::sidepanels_view(app.runner.as_ref(), &path);
    app.view_group(&status_group, &view);
    status_group.add(&widgets::info_row(
        "Config path",
        &path.display().to_string(),
    ));
    {
        let app = app.clone();
        let path = path.clone();
        status_group.add(&widgets::button_row(
            "Validate with better-sidepanels",
            "Run the app's own `--validate` against this file.",
            "Validate",
            false,
            move || match integrations::validate_sidepanels(app.runner.as_ref(), &path) {
                Ok(text) => app.report("Side panels validation", &text),
                Err(err) => app.report("Validation failed", &err.to_string()),
            },
        ));
    }
    {
        let app = app.clone();
        let path = path.clone();
        status_group.add(&widgets::button_row(
            "Open config",
            "Open the JSON file in $VISUAL, $EDITOR or the desktop opener.",
            "Open",
            false,
            move || match integrations::open_sidepanels(&path) {
                Ok(text) => app.toast(text),
                Err(err) => app.report("Cannot open config", &err.to_string()),
            },
        ));
    }
    page.add(&status_group);

    let config = integrations::read_sidepanels(&path).ok();
    let layouts = config
        .as_ref()
        .map(|config| config.layouts.clone())
        .unwrap_or_default();
    let layouts_group = widgets::group(
        "Named layouts",
        Some(
            "Preview a layout with `better-sidepanels --check --profile NAME`; the daemon is not started.",
        ),
    );
    if layouts.is_empty() {
        layouts_group.add(&widgets::info_row(
            "No named layouts",
            "Add `layouts`/`profiles` entries to the JSON config to switch between panel sets.",
        ));
    } else {
        let combo = adw::ComboRow::new();
        combo.set_title("Layout");
        combo.set_subtitle("Named panel sets from the config file.");
        let model = gtk::StringList::new(&layouts.iter().map(String::as_str).collect::<Vec<_>>());
        combo.set_model(Some(&model));
        let items = Rc::new(layouts);
        {
            let app = app.clone();
            let selected_combo = combo.clone();
            let items = items.clone();
            let path = path.clone();
            let button = gtk::Button::with_label("Preview");
            button.set_valign(gtk::Align::Center);
            button.connect_clicked(move |_| {
                let Some(layout) = widgets::selected(&selected_combo, &items) else {
                    return;
                };
                match integrations::check_sidepanels_layout(app.runner.as_ref(), &path, &layout) {
                    Ok(text) => app.report(&format!("Layout `{layout}`"), &text),
                    Err(err) => app.report("Layout check failed", &err.to_string()),
                }
            });
            combo.add_suffix(&button);
        }
        layouts_group.add(&combo);
    }
    page.add(&layouts_group);

    let panels_group = widgets::group(
        "Panels",
        Some("Read-only summary of every panel in the active config."),
    );
    match config {
        Some(config) if !config.panels.is_empty() => {
            for (index, panel) in config.panels.iter().enumerate() {
                let title = if panel.title.is_empty() {
                    format!("Panel {index}")
                } else {
                    format!("{} · panel {index}", panel.title)
                };
                panels_group.add(&widgets::info_row(
                    &title,
                    &format!(
                        "{} edge, width {}, on {}, {} row(s)",
                        panel.side,
                        panel.width,
                        panel.output.as_deref().unwrap_or("*"),
                        panel.rows
                    ),
                ));
            }
        }
        Some(_) => panels_group.add(&widgets::info_row(
            "No config panels",
            "The built-in default bar is used when no config file exists.",
        )),
        None => panels_group.add(&widgets::info_row(
            "Config unreadable",
            "Fix the JSON file, then refresh this page.",
        )),
    }
    page.add(&panels_group);

    page.upcast()
}
