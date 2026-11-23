//! `better-settings`: a small GTK4 frontend for niri's KDL config.
//!
//! The window edits a fixed catalog of settings and writes them through the
//! text-preserving model in the library, so comments and formatting survive
//! and every save keeps a timestamped backup.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use better_settings::{
    config_path, current, normalize, patch_many, save, Edit, Kind, SettingSpec, CATALOG,
};
use gtk::prelude::*;
use gtk4 as gtk;

enum Control {
    Switch(gtk::Switch),
    DropDown(gtk::DropDown, &'static [&'static str]),
    Entry(gtk::Entry),
}

struct Row {
    spec: SettingSpec,
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
            let active = matches!(value, Some("true"));
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

fn build_control(spec: &SettingSpec) -> Control {
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
        .default_width(620)
        .default_height(780)
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

    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
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
            for spec in CATALOG {
                let control = build_control(spec);
                let value = current(&text.borrow(), spec.path).unwrap_or(None);
                let initial = set_initial(&control, value.as_deref());

                let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let label = gtk::Label::new(Some(spec.label));
                label.set_xalign(0.0);
                label.set_width_chars(26);
                label.set_tooltip_text(Some(spec.help));
                line.append(&label);
                line.append(&widget_of(&control));
                row.append(&line);

                let help = gtk::Label::new(Some(spec.help));
                help.set_xalign(0.0);
                help.set_wrap(true);
                help.add_css_class("dim-label");
                help.add_css_class("caption");
                row.append(&help);

                content.append(&row);
                rows.borrow_mut().push(Row {
                    spec: *spec,
                    control,
                    initial,
                });
            }
        }
        Err(err) => {
            status.set_text(&err);
        }
    }

    // Reload: re-read the file and reset every widget to its stored value.
    {
        let rows = rows.clone();
        let text = text.clone();
        let status = status.clone();
        let path = path.clone();
        reload.connect_clicked(move |_| match read(&path) {
            Ok(loaded) => {
                *text.borrow_mut() = loaded;
                for row in rows.borrow_mut().iter_mut() {
                    let value = current(&text.borrow(), row.spec.path).unwrap_or(None);
                    row.initial = set_initial(&row.control, value.as_deref());
                }
                status.set_text("Reloaded");
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
                let flag = matches!(row.spec.kind, Kind::Flag);
                match normalize(row.spec.kind, &raw) {
                    Ok(value) => {
                        edits.push(Edit {
                            path: row.spec.path.to_owned(),
                            value,
                            flag,
                        });
                        changed += 1;
                    }
                    Err(err) => errors.push(format!("{}: {err}", row.spec.label)),
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
                Ok(patched) => match save(&path, &patched, false) {
                    Ok(backup) => {
                        *text.borrow_mut() = patched;
                        for row in rows.borrow_mut().iter_mut() {
                            row.initial = value_of(&row.control);
                        }
                        match backup {
                            Some(backup) => status.set_text(&format!(
                                "Saved {changed} settings; backup at {}",
                                backup.display()
                            )),
                            None => status.set_text(&format!("Saved {changed} settings")),
                        }
                    }
                    Err(err) => status.set_text(&format!("cannot write {}: {err}", path.display())),
                },
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
