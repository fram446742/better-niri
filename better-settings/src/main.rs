//! `better-settings`: a small GTK4 frontend for niri's KDL config.
//!
//! The window edits the expanded catalog and writes through the
//! text-preserving model in the library, so comments and formatting survive
//! and every save keeps a timestamped backup. Binds, rules, startup commands
//! and environment variables are shown as read-only summaries; use
//! `better-settingsctl` to edit those lists.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use better_settings::{
    Bind, Kind, OwnedSetting, RuleKind, binds, catalog_for, config_path, current,
    default_column_width, environment, normalize, patch_many, preset_column_widths, rules, save,
    spawn_at_startup, validate,
};
use gtk::prelude::*;
use gtk4 as gtk;

enum Control {
    Switch(gtk::Switch),
    DropDown(gtk::DropDown, &'static [&'static str]),
    Entry(gtk::Entry),
}

struct Row {
    spec: OwnedSetting,
    control: Control,
    initial: String,
}

fn value_of(control: &Control) -> String {
    match control {
        Control::Switch(switch) => switch.is_active().to_string(),
        Control::DropDown(dropdown, choices) => choices
            .get(dropdown.selected() as usize)
            .copied()
            .unwrap_or("")
            .to_owned(),
        Control::Entry(entry) => entry.text().to_string(),
    }
}

fn set_initial(control: &Control, value: Option<&str>) -> String {
    match control {
        Control::Switch(switch) => {
            // A flag with properties (`variable-refresh-rate on-demand=true`)
            // still counts as enabled.
            let active = matches!(value, Some(value) if value != "false");
            switch.set_active(active);
            active.to_string()
        }
        Control::DropDown(dropdown, choices) => {
            let index = value
                .and_then(|value| choices.iter().position(|choice| *choice == value))
                .unwrap_or(0);
            dropdown.set_selected(index as u32);
            choices[index].to_owned()
        }
        Control::Entry(entry) => {
            entry.set_text(value.unwrap_or_default());
            value.unwrap_or_default().to_owned()
        }
    }
}

fn build_control(spec: &OwnedSetting) -> Control {
    match spec.kind {
        Kind::Bool | Kind::Flag => Control::Switch(gtk::Switch::new()),
        Kind::Choice(choices) => Control::DropDown(gtk::DropDown::from_strings(choices), choices),
        _ => {
            let entry = gtk::Entry::new();
            entry.set_hexpand(true);
            Control::Entry(entry)
        }
    }
}

fn widget_of(control: &Control) -> gtk::Widget {
    match control {
        Control::Switch(switch) => switch.clone().upcast(),
        Control::DropDown(dropdown, _) => dropdown.clone().upcast(),
        Control::Entry(entry) => entry.clone().upcast(),
    }
}

fn read(path: &PathBuf) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
    }
}

fn section_heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("heading");
    label.set_margin_top(8);
    label
}

fn note_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    label
}

fn add_read_only(content: &gtk::Box, title: &str, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    content.append(&section_heading(title));
    for item in items {
        content.append(&note_label(&item));
    }
}

fn build_rows(content: &gtk::Box, rows: &Rc<RefCell<Vec<Row>>>, text: &str) {
    while let Some(child) = content.first_child() {
        content.remove(&child);
    }
    rows.borrow_mut().clear();

    let mut section = "";
    for spec in catalog_for(text) {
        if spec.section != section {
            section = spec.section;
            content.append(&section_heading(section));
        }
        let control = build_control(&spec);
        let value = current(text, &spec.path).unwrap_or(None);
        let initial = set_initial(&control, value.as_deref());

        let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let label = gtk::Label::new(Some(&spec.label));
        label.set_xalign(0.0);
        label.set_width_chars(28);
        label.set_tooltip_text(Some(spec.help));
        line.append(&label);
        line.append(&widget_of(&control));
        row.append(&line);
        row.append(&note_label(spec.help));
        content.append(&row);

        rows.borrow_mut().push(Row {
            spec,
            control,
            initial,
        });
    }

    add_read_only(
        content,
        &format!("Binds ({})", binds(text).len()),
        binds(text).iter().map(Bind::summary).collect::<Vec<_>>(),
    );
    add_read_only(
        content,
        &format!("Window rules ({})", rules(text, RuleKind::Window).len()),
        rules(text, RuleKind::Window)
            .iter()
            .enumerate()
            .map(|(index, rule)| format!("#{index}: {}", rule.summary()))
            .collect(),
    );
    add_read_only(
        content,
        &format!("Layer rules ({})", rules(text, RuleKind::Layer).len()),
        rules(text, RuleKind::Layer)
            .iter()
            .enumerate()
            .map(|(index, rule)| format!("#{index}: {}", rule.summary()))
            .collect(),
    );
    add_read_only(
        content,
        &format!("Startup ({})", spawn_at_startup(text).len()),
        spawn_at_startup(text)
            .iter()
            .map(|command| format!("spawn-at-startup {}", command.join(" ")))
            .collect(),
    );
    add_read_only(
        content,
        &format!("Environment ({})", environment(text).len()),
        environment(text)
            .into_iter()
            .map(|(name, value)| match value {
                Some(value) => format!("{name} = {value}"),
                None => format!("{name} (unset)"),
            })
            .collect(),
    );
    add_read_only(
        content,
        "Column widths",
        vec![
            format!(
                "default: {}",
                default_column_width(text).unwrap_or_else(|| "(not set)".to_owned())
            ),
            format!(
                "presets: {}",
                if preset_column_widths(text).is_empty() {
                    "(not set)".to_owned()
                } else {
                    preset_column_widths(text).join(", ")
                }
            ),
        ],
    );
}

fn validation_message(text: &str) -> String {
    let report = validate(text);
    if report.ok() {
        format!("Config OK: {} blocks, {} keys", report.blocks, report.keys)
    } else {
        format!(
            "Config problems:\n{}",
            report
                .errors
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

fn main() -> gtk::glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("org.better.Settings")
        .build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &gtk::Application) {
    let path = std::env::var_os("BETTER_SETTINGS_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("NIRI_CONFIG").map(PathBuf::from))
        .unwrap_or_else(config_path);

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Better Settings — niri")
        .default_width(720)
        .default_height(820)
        .build();

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_top(12);
    header.set_margin_bottom(4);
    header.set_margin_start(16);
    header.set_margin_end(16);

    let title = gtk::Label::new(Some("niri settings"));
    title.add_css_class("title-4");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    header.append(&title);

    let reload = gtk::Button::with_label("Reload");
    let save_button = gtk::Button::with_label("Save");
    save_button.add_css_class("suggested-action");
    header.append(&reload);
    header.append(&save_button);

    let path_label = gtk::Label::new(Some(&path.display().to_string()));
    path_label.set_xalign(0.0);
    path_label.add_css_class("dim-label");
    path_label.set_margin_start(16);
    path_label.set_margin_end(16);
    path_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.set_margin_top(8);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let text = Rc::new(RefCell::new(String::new()));
    let rows: Rc<RefCell<Vec<Row>>> = Rc::new(RefCell::new(Vec::new()));

    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.set_margin_start(16);
    status.set_margin_end(16);
    status.set_margin_bottom(10);
    status.set_wrap(true);

    match read(&path) {
        Ok(loaded) => {
            *text.borrow_mut() = loaded;
            build_rows(&content, &rows, &text.borrow());
            status.set_text(&validation_message(&text.borrow()));
        }
        Err(err) => {
            status.set_text(&err);
        }
    }

    // Reload: re-read the file, rebuild the sections and report validation.
    {
        let rows = rows.clone();
        let text = text.clone();
        let status = status.clone();
        let path = path.clone();
        let content = content.clone();
        reload.connect_clicked(move |_| match read(&path) {
            Ok(loaded) => {
                *text.borrow_mut() = loaded;
                build_rows(&content, &rows, &text.borrow());
                status.set_text(&validation_message(&text.borrow()));
            }
            Err(err) => status.set_text(&err),
        });
    }

    // Save: collect changed rows, validate, patch the text and write a backup.
    {
        let rows = rows.clone();
        let text = text.clone();
        let status = status.clone();
        let path = path.clone();
        save_button.connect_clicked(move |_| {
            let current_text = text.borrow().clone();
            let mut edits = Vec::new();
            let mut errors = Vec::new();
            let mut changed = 0;
            for row in rows.borrow().iter() {
                let raw = value_of(&row.control);
                if raw == row.initial {
                    continue;
                }
                let edit = match row.spec.kind {
                    Kind::Flag => {
                        let value = match normalize(Kind::Flag, &raw) {
                            Ok(value) => value,
                            Err(err) => {
                                errors.push(format!("{}: {err}", row.spec.label));
                                continue;
                            }
                        };
                        Some(better_settings::Edit::flag(
                            row.spec.path.clone(),
                            value == "true",
                        ))
                    }
                    Kind::Raw => Some(better_settings::Edit::raw(
                        row.spec.path.clone(),
                        raw.trim(),
                    )),
                    kind => match normalize(kind, &raw) {
                        Ok(value) => Some(better_settings::Edit::new(row.spec.path.clone(), value)),
                        Err(err) => {
                            errors.push(format!("{}: {err}", row.spec.label));
                            None
                        }
                    },
                };
                if let Some(edit) = edit {
                    edits.push(edit);
                    changed += 1;
                }
            }
            if !errors.is_empty() {
                status.set_text(&errors.join("\n"));
                return;
            }
            if changed == 0 {
                status.set_text("No changes");
                return;
            }
            match patch_many(&current_text, &edits) {
                Ok(patched) => {
                    let report = validate(&patched);
                    if !report.ok() {
                        status.set_text(&format!(
                            "Refusing to save invalid config:\n{}",
                            report.errors.join("\n")
                        ));
                        return;
                    }
                    match save(&path, &patched, false) {
                        Ok(backup) => {
                            *text.borrow_mut() = patched;
                            for row in rows.borrow_mut().iter_mut() {
                                row.initial = value_of(&row.control);
                            }
                            let summary = match backup {
                                Some(backup) => format!(
                                    "Saved {changed} settings; backup at {}",
                                    backup.display()
                                ),
                                None => format!("Saved {changed} settings"),
                            };
                            status.set_text(&format!(
                                "{summary}\n{}",
                                validation_message(&text.borrow())
                            ));
                        }
                        Err(err) => {
                            status.set_text(&format!("cannot write {}: {err}", path.display()))
                        }
                    }
                }
                Err(err) => status.set_text(&format!("cannot patch config: {err}")),
            }
        });
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&path_label);
    root.append(&scrolled);
    root.append(&status);
    window.set_child(Some(&root));
    window.present();
}
