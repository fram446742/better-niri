//! Sessions: contexts, explain/activate, capture and history.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use loom_settings::integrations;

use crate::gui::{App, format, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();

    let contexts = integrations::session_contexts(app.runner.as_ref()).unwrap_or_default();
    let group = widgets::group(
        "Contexts",
        Some(
            "Contexts live in loom-session. Explain is always read-only; Activate previews, then restores.",
        ),
    );
    if contexts.is_empty() {
        group.add(&widgets::info_row(
            "No saved contexts",
            "Capture the current session below, or save one with loom-session.",
        ));
    } else {
        let combo = adw::ComboRow::new();
        combo.set_title("Context");
        combo.set_subtitle("Saved windows, workspaces, outputs and profiles.");
        let model = gtk::StringList::new(&contexts.iter().map(String::as_str).collect::<Vec<_>>());
        combo.set_model(Some(&model));
        let items = Rc::new(contexts);

        {
            let app = app.clone();
            let selected_combo = combo.clone();
            let items = items.clone();
            let button = gtk::Button::with_label("Explain");
            button.set_valign(gtk::Align::Center);
            button.connect_clicked(move |_| {
                let Some(name) = widgets::selected(&selected_combo, &items) else {
                    return;
                };
                match integrations::session_explain(app.runner.as_ref(), &name) {
                    Ok(outcome) => {
                        app.report("Session restore plan", &format::session_outcome(&outcome));
                    }
                    Err(err) => app.report("Explain failed", &err.to_string()),
                }
            });
            combo.add_suffix(&button);
        }
        {
            let app = app.clone();
            let selected_combo = combo.clone();
            let items = items.clone();
            let button = gtk::Button::with_label("Activate");
            button.add_css_class("suggested-action");
            button.set_valign(gtk::Align::Center);
            button.connect_clicked(move |_| {
                let Some(name) = widgets::selected(&selected_combo, &items) else {
                    return;
                };
                let preview = match integrations::session_explain(app.runner.as_ref(), &name) {
                    Ok(outcome) => format::session_outcome(&outcome),
                    Err(err) => {
                        app.report("Preview failed", &err.to_string());
                        return;
                    }
                };
                match integrations::session_activate(app.runner.as_ref(), &name) {
                    Ok(outcome) => app.report(
                        "Activated session context",
                        &format!(
                            "{preview}\n\nActivated:\n{}",
                            format::session_outcome(&outcome)
                        ),
                    ),
                    Err(err) => {
                        app.report("Activate failed", &format!("{preview}\n\nerror: {err}"))
                    }
                }
            });
            combo.add_suffix(&button);
        }
        group.add(&combo);
    }
    page.add(&group);

    let capture = widgets::group(
        "Capture current session",
        Some(
            "Snapshots the current windows, workspaces and outputs. This writes a context, so it asks first.",
        ),
    );
    let name_row = adw::EntryRow::new();
    name_row.set_title("New context name");
    name_row.set_text("current");
    name_row.set_show_apply_button(false);
    capture.add(&name_row);
    {
        let app = app.clone();
        let name_row = name_row.clone();
        capture.add(&widgets::button_row(
            "Capture now",
            "Calls `loom-session capture NAME` after confirmation.",
            "Capture",
            true,
            move || {
                let name = name_row.text().trim().to_owned();
                if name.is_empty() {
                    app.toast("Enter a context name first");
                    return;
                }
                let app = app.clone();
                app.clone().confirm(
                    "Capture the current session?",
                    &format!("This writes a context named `{name}` in loom-session."),
                    "Capture",
                    move || match integrations::session_capture(app.runner.as_ref(), &name) {
                        Ok(context) => {
                            app.toast(format!(
                                "Captured `{}`: {} window(s), {} workspace(s)",
                                context.name, context.windows, context.workspaces
                            ));
                            // Rebuild from the main loop, not while this
                            // button's clicked handler still owns the page.
                            let app = app.clone();
                            gtk::glib::idle_add_local_once(move || app.rebuild());
                        }
                        Err(err) => app.report("Capture failed", &err.to_string()),
                    },
                );
            },
        ));
    }
    page.add(&capture);

    let history = widgets::group(
        "History",
        Some("Snapshots recorded by loom-session, newest first."),
    );
    match integrations::session_history(app.runner.as_ref()) {
        Ok(entries) if !entries.is_empty() => {
            for entry in entries {
                let title = format!("{}  ·  {}", entry.name, entry.id);
                let subtitle = format!(
                    "{}  ·  {} window(s), {} workspace(s)",
                    entry.trigger, entry.windows, entry.workspaces
                );
                history.add(&widgets::info_row(&title, &subtitle));
            }
        }
        Ok(_) => history.add(&widgets::info_row(
            "No history yet",
            "Snapshots appear after a pre-restore or `loom-session snapshot`.",
        )),
        Err(err) => {
            let row = widgets::info_row("History unavailable", &err.to_string());
            row.add_css_class("error");
            history.add(&row);
        }
    }
    page.add(&history);

    page.upcast()
}
