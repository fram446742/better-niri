//! Compositor: the full niri catalog, grouped and searchable.

use std::collections::BTreeMap;
use std::rc::Rc;

use better_settings::{
    Bind, RuleKind, binds, default_column_width, environment, preset_column_widths, rules,
    spawn_at_startup,
};
use gtk4 as gtk;
use libadwaita::prelude::*;

use crate::gui::{App, settings, widgets};

fn group_title(section: &str) -> &'static str {
    if section == "Layout" || section.starts_with("Layout /") || section == "Outputs / Layout" {
        return "Layout";
    }
    if section.starts_with("Input") {
        return "Input";
    }
    if section == "Outputs" {
        return "Outputs";
    }
    if section == "Animations" {
        return "Animations";
    }
    if section == "Cursor" {
        return "Cursor";
    }
    "Misc"
}

fn group_description(title: &str) -> Option<&'static str> {
    match title {
        "Layout" => Some("Gaps, columns, borders, shadows and per-output layout overrides."),
        "Input" => Some("Keyboard, touchpad, mouse, touch, tablet and global input options."),
        "Outputs" => Some(
            "Output mode, scale, transform, VRR and colour settings. HDR lives on the Displays page.",
        ),
        "Animations" => Some("Animation timings and curves."),
        "Cursor" => Some("Cursor theme, size and hiding behaviour."),
        "Misc" => Some("Gestures, overview, clipboard and miscellaneous options."),
        _ => None,
    }
}

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let text = app.config_text();
    let page = widgets::preferences_page();
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Filter settings by name, path or section"));
    search.set_margin_start(12);
    search.set_margin_end(12);
    search.set_margin_top(6);
    search.set_margin_bottom(6);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&search);
    root.append(&page);

    let index = Rc::new(settings::SearchIndex::new());

    let mut grouped: BTreeMap<&'static str, Vec<better_settings::OwnedSetting>> = BTreeMap::new();
    for spec in better_settings::catalog_for(&text) {
        // HDR is edited on the Displays page next to the runtime controls, so
        // there is exactly one row per setting.
        if spec.path.ends_with(".hdr.mode") || spec.path.ends_with(".hdr.reference-luminance") {
            continue;
        }
        grouped
            .entry(group_title(spec.section))
            .or_default()
            .push(spec);
    }

    add_catalog_groups(
        app,
        &page,
        &index,
        &text,
        &mut grouped,
        &["Layout", "Input", "Outputs"],
    );

    add_read_only(
        &page,
        &index,
        &format!("Binds ({})", binds(&text).len()),
        "Key bindings from the config. Edit them with better-settingsctl add-bind/remove-bind.",
        binds(&text).iter().map(Bind::summary).collect::<Vec<_>>(),
    );
    add_rule_group(
        &page,
        &index,
        &text,
        RuleKind::Window,
        "Window rules",
        "Window matching rules. Edit them with better-settingsctl add-window-rule.",
    );
    add_rule_group(
        &page,
        &index,
        &text,
        RuleKind::Layer,
        "Layer rules",
        "Layer-shell matching rules. Edit them with better-settingsctl add-layer-rule.",
    );
    add_read_only(
        &page,
        &index,
        &format!("Startup ({})", spawn_at_startup(&text).len()),
        "spawn-at-startup commands.",
        spawn_at_startup(&text)
            .iter()
            .map(|command| format!("spawn-at-startup {}", command.join(" ")))
            .collect(),
    );

    add_catalog_groups(
        app,
        &page,
        &index,
        &text,
        &mut grouped,
        &["Animations", "Cursor", "Misc"],
    );

    add_read_only(
        &page,
        &index,
        &format!("Environment ({})", environment(&text).len()),
        "Environment variables passed to niri and spawned apps.",
        environment(&text)
            .into_iter()
            .map(|(name, value)| match value {
                Some(value) => format!("{name} = {value}"),
                None => format!("{name} (unset)"),
            })
            .collect(),
    );
    add_read_only(
        &page,
        &index,
        "Column widths",
        "Default and preset column widths.",
        vec![
            format!(
                "default: {}",
                default_column_width(&text).unwrap_or_else(|| "(not set)".to_owned())
            ),
            format!(
                "presets: {}",
                if preset_column_widths(&text).is_empty() {
                    "(not set)".to_owned()
                } else {
                    preset_column_widths(&text).join(", ")
                }
            ),
        ],
    );

    {
        let index = index.clone();
        search.connect_search_changed(move |search| {
            index.apply(&search.text());
        });
    }

    root.upcast()
}

fn add_catalog_groups(
    app: &Rc<App>,
    page: &libadwaita::PreferencesPage,
    index: &Rc<settings::SearchIndex>,
    text: &str,
    grouped: &mut BTreeMap<&'static str, Vec<better_settings::OwnedSetting>>,
    order: &[&'static str],
) {
    for title in order {
        let Some(specs) = grouped.remove(title) else {
            continue;
        };
        let group = widgets::group(title, group_description(title));
        let group_index = index.group(&group);
        for spec in specs {
            let haystack = format!(
                "{} {} {} {}",
                spec.label, spec.path, spec.help, spec.section
            );
            let row = settings::build_row(spec, text);
            group.add(&row.widget);
            index.row(group_index, &row.widget, haystack);
            app.rows.borrow_mut().push(row);
        }
        page.add(&group);
    }
}

fn add_read_only(
    page: &libadwaita::PreferencesPage,
    index: &Rc<settings::SearchIndex>,
    title: &str,
    description: &str,
    lines: Vec<String>,
) {
    if lines.is_empty() {
        return;
    }
    let group = widgets::group(title, Some(description));
    let group_index = index.group(&group);
    for line in lines {
        let row = settings::summary_row(&line, title);
        group.add(&row);
        index.row(group_index, &row, format!("{line} {title}"));
    }
    page.add(&group);
}

fn add_rule_group(
    page: &libadwaita::PreferencesPage,
    index: &Rc<settings::SearchIndex>,
    text: &str,
    kind: RuleKind,
    title: &str,
    description: &str,
) {
    let entries = rules(text, kind);
    let lines: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(position, rule)| format!("#{position}: {}", rule.summary()))
        .collect();
    add_read_only(
        page,
        index,
        &format!("{title} ({})", entries.len()),
        description,
        lines,
    );
}
