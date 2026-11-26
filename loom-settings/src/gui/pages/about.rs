//! About: versions, docs links and dependency notes.

use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::gui::{App, widgets};

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();

    let about = widgets::group(
        "Loom Settings",
        Some("A modern GTK4/libadwaita control center for niri and its companion apps."),
    );
    about.add(&widgets::info_row("Version", env!("CARGO_PKG_VERSION")));
    about.add(&widgets::info_row(
        "Config",
        &app.path.display().to_string(),
    ));
    about.add(&widgets::info_row(
        "Editing model",
        "Byte-preserving KDL patches: comments, indentation, ordering and line endings survive. Saves keep a timestamped backup and refuse an invalid config.",
    ));
    about.add(&widgets::info_row(
        "Safety",
        "Companion actions are previewed with each tool's own dry run where it exists; otherwise they ask for confirmation. Another app's config is never rewritten here.",
    ));
    page.add(&about);

    let versions = widgets::group("Runtime", Some("Versions this build linked against."));
    versions.add(&widgets::info_row(
        "GTK",
        &format!(
            "{}.{}.{}",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version()
        ),
    ));
    versions.add(&widgets::info_row(
        "libadwaita",
        &format!(
            "{}.{}.{}",
            adw::major_version(),
            adw::minor_version(),
            adw::micro_version()
        ),
    ));
    versions.add(&widgets::info_row(
        "Rust",
        option_env!("RUSTC_VERSION").unwrap_or("rustc (build-time version not recorded)"),
    ));
    page.add(&versions);

    let docs = widgets::group(
        "Documentation",
        Some("Configuration reference and the companion apps."),
    );
    add_link(
        &docs,
        "niri configuration",
        "https://github.com/YaLTeR/niri/wiki/Configuration:-Introduction",
    );
    add_link(
        &docs,
        "niri IPC and actions",
        "https://github.com/YaLTeR/niri/wiki/IPC",
    );
    add_link(&docs, "Loom README", "https://github.com/fram446742/loom");
    docs.add(&widgets::info_row(
        "CLI parity",
        "Every page has a `loom-settingsctl` equivalent; run `loom-settingsctl --help`.",
    ));
    page.add(&docs);

    let access = widgets::group("Keyboard and display", Some("Small conveniences."));
    access.add(&widgets::info_row(
        "About shortcut",
        "Press F1 from any page to open this About page.",
    ));
    access.add(&widgets::info_row(
        "Colour scheme",
        "Follows the system light/dark preference through libadwaita's StyleManager.",
    ));
    access.add(&widgets::info_row(
        "Search",
        "The sidebar search filters pages; the Compositor page search filters settings, binds, rules and startup entries.",
    ));
    page.add(&access);

    page.upcast()
}

fn add_link(group: &adw::PreferencesGroup, title: &str, url: &str) {
    let row = widgets::info_row(title, url);
    let link = gtk::LinkButton::with_label(url, "Open");
    link.set_valign(gtk::Align::Center);
    row.add_suffix(&link);
    group.add(&row);
}
