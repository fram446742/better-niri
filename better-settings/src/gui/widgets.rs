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

/// Scrolls arbitrary page content inside the same 600 px clamp that
/// `AdwPreferencesPage` uses, so custom dashboards keep the native rhythm.
pub fn custom_page(content: gtk::Box) -> gtk::ScrolledWindow {
    content.set_spacing(18);
    content.set_margin_top(12);
    content.set_margin_bottom(24);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(600);
    clamp.set_tightening_threshold(400);
    clamp.set_valign(gtk::Align::Start);
    clamp.set_child(Some(&content));

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .hexpand(true)
        .child(&clamp)
        .build()
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

/// A page introduction: a large title and a wrapping description.
pub fn intro(title: &str, subtitle: &str) -> gtk::Box {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
    body.add_css_class("page-intro");

    let heading = gtk::Label::new(Some(title));
    heading.set_xalign(0.0);
    heading.add_css_class("title-1");
    body.append(&heading);

    let description = gtk::Label::new(Some(subtitle));
    description.set_xalign(0.0);
    description.set_wrap(true);
    description.add_css_class("dim-label");
    body.append(&description);

    body
}

/// A responsive row of big-number summary cards.
pub fn stat_grid(items: Vec<(String, &'static str)>) -> gtk::Box {
    let grid = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    grid.add_css_class("stat-grid");
    grid.set_homogeneous(true);
    for (value, label) in items {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
        card.add_css_class("stat-card");

        let value_label = gtk::Label::new(Some(&value));
        value_label.set_xalign(0.0);
        value_label.add_css_class("stat-value");
        card.append(&value_label);

        let text = gtk::Label::new(Some(label));
        text.set_xalign(0.0);
        text.set_wrap(true);
        text.add_css_class("stat-label");
        card.append(&text);

        grid.append(&card);
    }
    grid
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
