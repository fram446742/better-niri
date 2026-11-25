//! Small libadwaita building blocks used by every page.

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

/// A preferences page whose groups scroll with the window.
pub fn preferences_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_vexpand(true);
    page.set_hexpand(true);
    page
}

/// A titled group with an optional description.
pub fn group(title: &str, description: Option<&str>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(title);
    if let Some(description) = description {
        group.set_description(Some(description));
    }
    group
}

/// A read-only row.
pub fn info_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_subtitle_lines(3);
    row.set_activatable(false);
    row
}

/// A row with a suffix button and an explicit callback.
pub fn button_row<F>(
    title: &str,
    subtitle: &str,
    label: &str,
    suggested: bool,
    action: F,
) -> adw::ActionRow
where
    F: Fn() + 'static,
{
    let row = info_row(title, subtitle);
    let button = gtk::Button::with_label(label);
    button.set_valign(gtk::Align::Center);
    if suggested {
        button.add_css_class("suggested-action");
    }
    button.connect_clicked(move |_| action());
    row.add_suffix(&button);
    row
}

/// A wrapping dim note, for page-level explanations.
pub fn note(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    label.set_margin_top(6);
    label.set_margin_bottom(6);
    label
}

/// The text selected in a combo fed by [`selection_row`].
pub fn selected(combo: &adw::ComboRow, items: &[String]) -> Option<String> {
    items.get(combo.selected() as usize).cloned()
}
