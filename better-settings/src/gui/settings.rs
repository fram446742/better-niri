//! Catalog rows shared by the Compositor and Displays pages.
//!
//! The row widgets are libadwaita preference rows, but the data path is the
//! same one the CLI uses: every value is read with `current` and every change
//! becomes an [`Edit`] that goes through the text-preserving patcher.

use better_settings::{Edit, Kind, OwnedSetting, normalize};
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

/// The editable control behind one catalog row.
pub enum Control {
    Switch(adw::SwitchRow),
    Combo(adw::ComboRow, &'static [&'static str]),
    Spin(adw::SpinRow),
    Entry(adw::EntryRow),
}

/// One catalog setting plus its live widget state.
pub struct CatalogRow {
    pub spec: OwnedSetting,
    pub control: Control,
    /// The value the widget showed when it was built; a mismatch means dirty.
    pub initial: String,
    pub widget: gtk::Widget,
}

/// The result of collecting dirty rows, mirroring the header Save button.
pub struct PendingEdits {
    pub edits: Vec<Edit>,
    pub errors: Vec<String>,
    pub changed: usize,
}

/// Builds a preference row for one catalog entry.
pub fn build_row(spec: OwnedSetting, text: &str) -> CatalogRow {
    let value = better_settings::current(text, &spec.path).ok().flatten();
    let (control, widget): (Control, gtk::Widget) = match spec.kind {
        Kind::Bool | Kind::Flag => {
            let row = adw::SwitchRow::new();
            row.set_title(&spec.label);
            row.set_subtitle(spec.help);
            // A flag with properties (`variable-refresh-rate on-demand=true`)
            // still counts as enabled.
            row.set_active(matches!(value.as_deref(), Some(value) if value != "false"));
            (Control::Switch(row.clone()), row.upcast())
        }
        Kind::Choice(choices) => {
            let row = adw::ComboRow::new();
            row.set_title(&spec.label);
            row.set_subtitle(spec.help);
            let model = gtk::StringList::new(choices);
            row.set_model(Some(&model));
            let index = value
                .as_deref()
                .and_then(|value| choices.iter().position(|choice| *choice == value))
                .unwrap_or(0);
            row.set_selected(index as u32);
            (Control::Combo(row.clone(), choices), row.upcast())
        }
        Kind::Int { min, max } => {
            let row = adw::SpinRow::with_range(min as f64, max as f64, 1.0);
            row.set_title(&spec.label);
            row.set_subtitle(spec.help);
            row.set_digits(0);
            row.set_numeric(true);
            row.set_value(
                value
                    .as_deref()
                    .and_then(|value| value.trim().parse::<f64>().ok())
                    .unwrap_or(min as f64),
            );
            (Control::Spin(row.clone()), row.upcast())
        }
        Kind::Float { min, max } => {
            let row = adw::SpinRow::with_range(min, max, 0.1);
            row.set_title(&spec.label);
            row.set_subtitle(spec.help);
            row.set_digits(2);
            row.set_numeric(true);
            row.set_value(
                value
                    .as_deref()
                    .and_then(|value| value.trim().parse::<f64>().ok())
                    .unwrap_or(min.max(0.0)),
            );
            (Control::Spin(row.clone()), row.upcast())
        }
        Kind::Text | Kind::Raw => {
            let row = adw::EntryRow::new();
            row.set_title(&spec.label);
            row.set_text(value.as_deref().unwrap_or_default());
            row.set_show_apply_button(false);
            row.set_tooltip_text(Some(spec.help));
            (Control::Entry(row.clone()), row.upcast())
        }
    };

    let mut row = CatalogRow {
        spec,
        control,
        initial: String::new(),
        widget,
    };
    // The initial value is what the widget actually shows: an absent flag is
    // `false`, an absent choice falls back to its first entry and an absent
    // number starts at the control's minimum. Otherwise Save would silently
    // write defaults for every unconfigured setting.
    row.initial = value_of(&row);
    row
}

/// The current widget value as KDL text, before [`normalize`] validates it.
pub fn value_of(row: &CatalogRow) -> String {
    match &row.control {
        Control::Switch(switch) => switch.is_active().to_string(),
        Control::Combo(combo, choices) => choices
            .get(combo.selected() as usize)
            .copied()
            .unwrap_or("")
            .to_owned(),
        Control::Spin(spin) => match row.spec.kind {
            Kind::Int { .. } => (spin.value().round() as i64).to_string(),
            _ => format!("{}", spin.value()),
        },
        Control::Entry(entry) => entry.text().to_string(),
    }
}

/// Collects changed rows and turns them into patcher edits.
pub fn pending_edits(rows: &[CatalogRow]) -> PendingEdits {
    let mut pending = PendingEdits {
        edits: Vec::new(),
        errors: Vec::new(),
        changed: 0,
    };
    for row in rows {
        let raw = value_of(row);
        if raw == row.initial {
            continue;
        }
        let edit = match row.spec.kind {
            Kind::Flag => match normalize(Kind::Flag, &raw) {
                Ok(value) => Some(Edit::flag(row.spec.path.clone(), value == "true")),
                Err(err) => {
                    pending.errors.push(format!("{}: {err}", row.spec.label));
                    None
                }
            },
            Kind::Raw => Some(Edit::raw(row.spec.path.clone(), raw.trim())),
            kind => match normalize(kind, &raw) {
                Ok(value) => Some(Edit::new(row.spec.path.clone(), value)),
                Err(err) => {
                    pending.errors.push(format!("{}: {err}", row.spec.label));
                    None
                }
            },
        };
        if let Some(edit) = edit {
            pending.edits.push(edit);
            pending.changed += 1;
        }
    }
    pending
}

/// One filterable group on a searchable page.
struct FilterGroup {
    container: gtk::Widget,
    rows: Vec<(gtk::Widget, String)>,
}

/// Hides rows and groups that do not match the search query.
#[derive(Default)]
pub struct SearchIndex {
    groups: std::cell::RefCell<Vec<FilterGroup>>,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a preferences group and returns its index.
    pub fn group(&self, group: &impl IsA<gtk::Widget>) -> usize {
        let mut groups = self.groups.borrow_mut();
        groups.push(FilterGroup {
            container: group.clone().upcast(),
            rows: Vec::new(),
        });
        groups.len() - 1
    }

    /// Registers a row inside a group with the text used for matching.
    pub fn row(&self, group: usize, row: &impl IsA<gtk::Widget>, haystack: impl Into<String>) {
        if let Some(entry) = self.groups.borrow_mut().get_mut(group) {
            entry.rows.push((row.clone().upcast(), haystack.into()));
        }
    }

    /// Applies a query and returns the number of matching rows.
    pub fn apply(&self, query: &str) -> usize {
        let query = query.trim().to_lowercase();
        let mut visible = 0;
        for group in self.groups.borrow().iter() {
            let mut any = false;
            for (row, haystack) in &group.rows {
                let show = query.is_empty() || haystack.to_lowercase().contains(&query);
                row.set_visible(show);
                any |= show;
                visible += usize::from(show);
            }
            // A group with no registered rows is structural (notes, buttons).
            group
                .container
                .set_visible(query.is_empty() || any || group.rows.is_empty());
        }
        visible
    }
}

/// A compact action row used for read-only summaries.
pub fn summary_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_subtitle_lines(2);
    row.set_activatable(false);
    row
}
