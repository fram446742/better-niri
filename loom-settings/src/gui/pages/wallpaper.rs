//! Wallpaper: scenes, apply previews and workspace/session bindings.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use loom_settings::integrations;

use crate::gui::{App, format, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();

    let status = integrations::wallpaper_status(app.runner.as_ref());
    let status_group = widgets::group("Status", Some("Read-only state from loom-wallpaper."));
    match &status {
        Ok(status) => {
            status_group.add(&widgets::info_row(
                "Active scene",
                status.active.as_deref().unwrap_or("(none)"),
            ));
            status_group.add(&widgets::info_row(
                "Backend",
                &format!(
                    "{} (native {})",
                    status.backend,
                    if status.native_available {
                        "available"
                    } else {
                        "unavailable"
                    }
                ),
            ));
            if let Some(error) = &status.native_error {
                status_group.add(&widgets::info_row("Native backend", error));
            }
            if let Some(template) = &status.template {
                status_group.add(&widgets::info_row("Command template", template));
            }
        }
        Err(err) => {
            let row = widgets::info_row("Status unavailable", &err.to_string());
            row.add_css_class("error");
            status_group.add(&row);
        }
    }
    page.add(&status_group);

    let scenes_group = widgets::group(
        "Scenes",
        Some(
            "Scene selection writes loom-wallpaper state; Apply starts the backend for the active scene.",
        ),
    );
    let scenes = integrations::wallpaper_scenes(app.runner.as_ref()).unwrap_or_default();
    if scenes.is_empty() {
        scenes_group.add(&widgets::info_row(
            "No scenes",
            "Create one with loom-wallpaper, then refresh this page.",
        ));
    } else {
        let combo = adw::ComboRow::new();
        combo.set_title("Scene");
        combo.set_subtitle("A named set of images and workspace overrides.");
        let model = gtk::StringList::new(&scenes.iter().map(String::as_str).collect::<Vec<_>>());
        combo.set_model(Some(&model));
        let items = Rc::new(scenes);

        {
            let app = app.clone();
            let selected_combo = combo.clone();
            let items = items.clone();
            let button = gtk::Button::with_label("Use scene");
            button.set_valign(gtk::Align::Center);
            button.connect_clicked(move |_| {
                let Some(name) = widgets::selected(&selected_combo, &items) else {
                    return;
                };
                let app = app.clone();
                app.clone().confirm(
                    "Activate this wallpaper scene?",
                    &format!("This changes the active scene in loom-wallpaper to `{name}`."),
                    "Use scene",
                    move || match integrations::wallpaper_use(app.runner.as_ref(), &name) {
                        Ok(value) => {
                            app.toast(format!("Active scene `{name}`: {}", format::pretty(&value)))
                        }
                        Err(err) => app.report("Use scene failed", &err.to_string()),
                    },
                );
            });
            combo.add_suffix(&button);
        }
        scenes_group.add(&combo);

        {
            let app = app.clone();
            scenes_group.add(&widgets::button_row(
                "Preview apply",
                "Run `loom-wallpaper apply --dry-run` for the active scene.",
                "Preview",
                false,
                move || match integrations::wallpaper_preview(app.runner.as_ref()) {
                    Ok(plan) => app.report("Wallpaper preview", &format::wallpaper_plan(&plan)),
                    Err(err) => app.report("Preview failed", &err.to_string()),
                },
            ));
        }
        {
            let app = app.clone();
            scenes_group.add(&widgets::button_row(
                "Apply active scene",
                "Preview, then start `loom-wallpaper apply` detached so the native backend stays on screen.",
                "Apply",
                true,
                move || {
                    let preview = match integrations::wallpaper_preview(app.runner.as_ref()) {
                        Ok(plan) => format::wallpaper_plan(&plan),
                        Err(err) => {
                            app.report("Preview failed", &err.to_string());
                            return;
                        }
                    };
                    match integrations::wallpaper_apply_detached() {
                        Ok(text) => app.report(
                            "Wallpaper applied",
                            &format!("{preview}\n\n{text}"),
                        ),
                        Err(err) => {
                            app.report("Apply failed", &format!("{preview}\n\nerror: {err}"))
                        }
                    }
                },
            ));
        }
    }
    page.add(&scenes_group);

    let bindings = widgets::group(
        "Workspace and session bindings",
        Some(
            "Workspace overrides reported by loom-wallpaper. Session contexts keep their own wallpaper profile.",
        ),
    );
    match status {
        Ok(status) if !status.scenes.is_empty() => {
            for scene in &status.scenes {
                bindings.add(&widgets::info_row(
                    &scene.name,
                    &format!("default {}", scene.default.as_deref().unwrap_or("(none)")),
                ));
                for workspace in &scene.workspaces {
                    bindings.add(&widgets::info_row(
                        &format!("{} · workspace {workspace}", scene.name),
                        "workspace-specific image",
                    ));
                }
            }
        }
        Ok(_) => bindings.add(&widgets::info_row(
            "No bindings",
            "No wallpaper scenes are configured.",
        )),
        Err(err) => bindings.add(&widgets::info_row("Bindings unavailable", &err.to_string())),
    }
    page.add(&bindings);

    page.upcast()
}
