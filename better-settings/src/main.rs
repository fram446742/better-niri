//! `better-settings`: a GTK4/libadwaita control center for niri.
//!
//! The window edits the expanded catalog through the text-preserving model in
//! the library, so comments and formatting survive and every save keeps a
//! timestamped backup. Companion-app pages preview each tool's own dry run and
//! never rewrite another app's config; `better-settingsctl` exposes the same
//! operations from a script.

mod gui;

use gtk4 as gtk;

fn main() -> gtk::glib::ExitCode {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("better-settings {}", env!("CARGO_PKG_VERSION"));
        return gtk::glib::ExitCode::SUCCESS;
    }
    gui::run()
}
