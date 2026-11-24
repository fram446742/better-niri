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

use better_settings::integrations::{
    self, CommandRunner, DisplayPlan, IntegrationView, SessionOutcome, SystemRunner, Tool,
    WallpaperPlan,
};
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

// ---------------------------------------------------------------------------
// Other apps: display what their CLIs report and expose safe actions.
// ---------------------------------------------------------------------------

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn display_plan_text(plan: &DisplayPlan) -> String {
    let mut text = format!(
        "{}: {} ({} action(s){})",
        plan.command,
        plan.status,
        plan.actions.len(),
        if plan.dry_run { ", dry run" } else { "" }
    );
    for action in &plan.actions {
        text.push_str("\n  ");
        text.push_str(action);
    }
    for warning in &plan.warnings {
        text.push_str("\nwarning: ");
        text.push_str(warning);
    }
    for error in &plan.errors {
        text.push_str("\nerror: ");
        text.push_str(error);
    }
    text
}

fn session_text(outcome: &SessionOutcome) -> String {
    let mut text = format!(
        "{}: {} applied, {} skipped, {} unmatched, {} profile(s){}",
        outcome.context,
        outcome.applied,
        outcome.skipped,
        outcome.unmatched,
        outcome.profiles,
        if outcome.dry_run { " [dry run]" } else { "" }
    );
    if outcome.partial {
        text.push_str(" (partial)");
    }
    if let Some(error) = &outcome.error {
        text.push_str("\nerror: ");
        text.push_str(error);
    }
    text
}

fn wallpaper_text(plan: &WallpaperPlan) -> String {
    let mut text = format!(
        "{}: {} output(s){}",
        plan.backend,
        plan.images.len(),
        if plan.dry_run { " [dry run]" } else { "" }
    );
    if let Some(image) = &plan.image {
        text.push_str(&format!("\n  image: {image}"));
    }
    for (output, image) in &plan.images {
        text.push_str(&format!(
            "\n  {output}: {}",
            image.as_deref().unwrap_or("(none)")
        ));
    }
    text
}

fn action_row(children: &[&gtk::Widget]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(4);
    for child in children {
        row.append(*child);
    }
    row
}

fn dropdown(items: &[String]) -> gtk::DropDown {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    gtk::DropDown::from_strings(&refs)
}

fn selected_name(dropdown: &gtk::DropDown, items: &[String]) -> Option<String> {
    items.get(dropdown.selected() as usize).cloned()
}

fn wire_action<F>(
    button: &gtk::Button,
    runner: Rc<dyn CommandRunner>,
    status: gtk::Label,
    action: F,
) where
    F: Fn(&dyn CommandRunner) -> String + 'static,
{
    button.connect_clicked(move |_| status.set_text(&action(runner.as_ref())));
}

fn wire_selection_action<F>(
    button: &gtk::Button,
    dropdown: gtk::DropDown,
    items: Rc<Vec<String>>,
    runner: Rc<dyn CommandRunner>,
    status: gtk::Label,
    action: F,
) where
    F: Fn(&dyn CommandRunner, &str) -> String + 'static,
{
    button.connect_clicked(move |_| {
        let Some(name) = selected_name(&dropdown, &items) else {
            status.set_text("nothing selected");
            return;
        };
        status.set_text(&action(runner.as_ref(), &name));
    });
}

fn integration_section(content: &gtk::Box, title: &str, view: &IntegrationView) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    section.append(&section_heading(title));
    section.append(&note_label(&view.status));
    for line in &view.lines {
        section.append(&note_label(line));
    }
    if let Some(error) = &view.error {
        let label = note_label(&format!("error: {error}"));
        label.add_css_class("error");
        section.append(&label);
    }
    content.append(&section);
    section
}

fn add_display_actions(section: &gtk::Box, runner: &Rc<dyn CommandRunner>, status: &gtk::Label) {
    let profiles = integrations::display_profiles(runner.as_ref()).unwrap_or_default();
    let dropdown = dropdown(&profiles);
    let items = Rc::new(profiles);
    let preview = gtk::Button::with_label("Preview apply");
    let apply = gtk::Button::with_label("Apply");
    let restore_preview = gtk::Button::with_label("Preview restore");
    let restore = gtk::Button::with_label("Restore previous");
    let confirm = gtk::Button::with_label("Confirm pending");
    let cancel = gtk::Button::with_label("Cancel pending");

    wire_selection_action(
        &preview,
        dropdown.clone(),
        items.clone(),
        runner.clone(),
        status.clone(),
        |runner, name| match integrations::display_preview(runner, name) {
            Ok(plan) => display_plan_text(&plan),
            Err(err) => err.to_string(),
        },
    );
    wire_selection_action(
        &apply,
        dropdown.clone(),
        items.clone(),
        runner.clone(),
        status.clone(),
        |runner, name| {
            let preview = match integrations::display_preview(runner, name) {
                Ok(plan) if plan.errors.is_empty() => display_plan_text(&plan),
                Ok(plan) => return display_plan_text(&plan),
                Err(err) => return err.to_string(),
            };
            match integrations::display_apply(runner, name) {
                Ok(plan) => format!("{preview}\n\nApplied:\n{}", display_plan_text(&plan)),
                Err(err) => format!("{preview}\n\nerror: {err}"),
            }
        },
    );
    wire_action(&restore_preview, runner.clone(), status.clone(), |runner| {
        match integrations::display_restore_preview(runner) {
            Ok(plan) => display_plan_text(&plan),
            Err(err) => err.to_string(),
        }
    });
    wire_action(&restore, runner.clone(), status.clone(), |runner| {
        let preview = match integrations::display_restore_preview(runner) {
            Ok(plan) if plan.errors.is_empty() => display_plan_text(&plan),
            Ok(plan) => return display_plan_text(&plan),
            Err(err) => return err.to_string(),
        };
        match integrations::display_restore(runner) {
            Ok(plan) => format!("{preview}\n\nRestored:\n{}", display_plan_text(&plan)),
            Err(err) => format!("{preview}\n\nerror: {err}"),
        }
    });
    wire_action(&confirm, runner.clone(), status.clone(), |runner| {
        match integrations::display_confirm(runner) {
            Ok(value) => pretty(&value),
            Err(err) => err.to_string(),
        }
    });
    wire_action(
        &cancel,
        runner.clone(),
        status.clone(),
        |runner| match integrations::display_cancel(runner) {
            Ok(value) => pretty(&value),
            Err(err) => err.to_string(),
        },
    );

    section.append(&action_row(&[
        dropdown.upcast_ref(),
        preview.upcast_ref(),
        apply.upcast_ref(),
        restore_preview.upcast_ref(),
        restore.upcast_ref(),
        confirm.upcast_ref(),
        cancel.upcast_ref(),
    ]));
}

fn add_session_actions(section: &gtk::Box, runner: &Rc<dyn CommandRunner>, status: &gtk::Label) {
    let contexts = integrations::session_contexts(runner.as_ref()).unwrap_or_default();
    let dropdown = dropdown(&contexts);
    let items = Rc::new(contexts);
    let explain = gtk::Button::with_label("Explain");
    let activate = gtk::Button::with_label("Activate");

    wire_selection_action(
        &explain,
        dropdown.clone(),
        items.clone(),
        runner.clone(),
        status.clone(),
        |runner, name| match integrations::session_explain(runner, name) {
            Ok(outcome) => session_text(&outcome),
            Err(err) => err.to_string(),
        },
    );
    wire_selection_action(
        &activate,
        dropdown.clone(),
        items.clone(),
        runner.clone(),
        status.clone(),
        |runner, name| {
            let preview = match integrations::session_explain(runner, name) {
                Ok(outcome) => session_text(&outcome),
                Err(err) => return err.to_string(),
            };
            match integrations::session_activate(runner, name) {
                Ok(outcome) => format!("{preview}\n\nActivated:\n{}", session_text(&outcome)),
                Err(err) => format!("{preview}\n\nerror: {err}"),
            }
        },
    );

    section.append(&action_row(&[
        dropdown.upcast_ref(),
        explain.upcast_ref(),
        activate.upcast_ref(),
    ]));
}

fn add_wallpaper_actions(section: &gtk::Box, runner: &Rc<dyn CommandRunner>, status: &gtk::Label) {
    let scenes = integrations::wallpaper_scenes(runner.as_ref()).unwrap_or_default();
    let dropdown = dropdown(&scenes);
    let items = Rc::new(scenes);
    let use_scene = gtk::Button::with_label("Use scene");
    let preview = gtk::Button::with_label("Preview apply");
    let apply = gtk::Button::with_label("Apply");

    wire_selection_action(
        &use_scene,
        dropdown.clone(),
        items.clone(),
        runner.clone(),
        status.clone(),
        |runner, name| match integrations::wallpaper_use(runner, name) {
            Ok(value) => format!("active scene: {}", pretty(&value)),
            Err(err) => err.to_string(),
        },
    );
    wire_action(&preview, runner.clone(), status.clone(), |runner| {
        match integrations::wallpaper_preview(runner) {
            Ok(plan) => wallpaper_text(&plan),
            Err(err) => err.to_string(),
        }
    });
    wire_action(&apply, runner.clone(), status.clone(), |runner| {
        let preview = match integrations::wallpaper_preview(runner) {
            Ok(plan) => wallpaper_text(&plan),
            Err(err) => return err.to_string(),
        };
        match integrations::wallpaper_apply_detached() {
            Ok(text) => format!("{preview}\n\n{text}"),
            Err(err) => format!("{preview}\n\nerror: {err}"),
        }
    });

    section.append(&action_row(&[
        dropdown.upcast_ref(),
        use_scene.upcast_ref(),
        preview.upcast_ref(),
        apply.upcast_ref(),
    ]));
}

fn add_audio_actions(section: &gtk::Box, runner: &Rc<dyn CommandRunner>, status: &gtk::Label) {
    let sinks = integrations::audio_sinks(runner.as_ref())
        .map(|sinks| {
            sinks
                .sinks
                .into_iter()
                .map(|sink| sink.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let dropdown = dropdown(&sinks);
    let items = Rc::new(sinks);
    let set_default = gtk::Button::with_label("Set default sink");
    let preview = gtk::Button::with_label("Preview apply");
    let apply = gtk::Button::with_label("Apply rules");
    let restore_preview = gtk::Button::with_label("Preview restore");
    let restore = gtk::Button::with_label("Restore streams");

    wire_selection_action(
        &set_default,
        dropdown.clone(),
        items.clone(),
        runner.clone(),
        status.clone(),
        |runner, sink| match integrations::audio_set_default(runner, sink) {
            Ok(value) => pretty(&value),
            Err(err) => err.to_string(),
        },
    );
    wire_action(&preview, runner.clone(), status.clone(), |runner| {
        match integrations::audio_apply_preview(runner) {
            Ok(value) => pretty(&value),
            Err(err) => err.to_string(),
        }
    });
    wire_action(&apply, runner.clone(), status.clone(), |runner| {
        let preview = match integrations::audio_apply_preview(runner) {
            Ok(value) => pretty(&value),
            Err(err) => return err.to_string(),
        };
        match integrations::audio_apply(runner) {
            Ok(value) => format!("{preview}\n\nApplied:\n{}", pretty(&value)),
            Err(err) => format!("{preview}\n\nerror: {err}"),
        }
    });
    wire_action(&restore_preview, runner.clone(), status.clone(), |runner| {
        match integrations::audio_restore_preview(runner) {
            Ok(value) => pretty(&value),
            Err(err) => err.to_string(),
        }
    });
    wire_action(&restore, runner.clone(), status.clone(), |runner| {
        let preview = match integrations::audio_restore_preview(runner) {
            Ok(value) => pretty(&value),
            Err(err) => return err.to_string(),
        };
        match integrations::audio_restore(runner) {
            Ok(value) => format!("{preview}\n\nRestored:\n{}", pretty(&value)),
            Err(err) => format!("{preview}\n\nerror: {err}"),
        }
    });

    section.append(&action_row(&[
        dropdown.upcast_ref(),
        set_default.upcast_ref(),
        preview.upcast_ref(),
        apply.upcast_ref(),
        restore_preview.upcast_ref(),
        restore.upcast_ref(),
    ]));
}

fn add_sidepanels_actions(
    section: &gtk::Box,
    runner: &Rc<dyn CommandRunner>,
    status: &gtk::Label,
    path: &std::path::Path,
) {
    let validate = gtk::Button::with_label("Validate");
    let open = gtk::Button::with_label("Open config");
    {
        let path = path.to_owned();
        wire_action(&validate, runner.clone(), status.clone(), move |runner| {
            match integrations::validate_sidepanels(runner, &path) {
                Ok(text) => text,
                Err(err) => err.to_string(),
            }
        });
    }
    {
        let status = status.clone();
        let path = path.to_owned();
        open.connect_clicked(move |_| match integrations::open_sidepanels(&path) {
            Ok(text) => status.set_text(&text),
            Err(err) => status.set_text(&err.to_string()),
        });
    }
    section.append(&action_row(&[validate.upcast_ref(), open.upcast_ref()]));
}

fn add_companion_actions(section: &gtk::Box, runner: &Rc<dyn CommandRunner>, status: &gtk::Label) {
    let namespace = gtk::Entry::new();
    namespace.set_placeholder_text(Some("namespace, e.g. parking"));
    let action = gtk::Entry::new();
    action.set_placeholder_text(Some("action, e.g. park"));
    let dry_run = gtk::Button::with_label("Dry-run action");
    let call = gtk::Button::with_label("Call action");

    let wire = |button: &gtk::Button, dry_run: bool| {
        let runner = runner.clone();
        let status = status.clone();
        let namespace = namespace.clone();
        let action = action.clone();
        button.connect_clicked(move |_| {
            let namespace = namespace.text().to_string();
            let action = action.text().to_string();
            if namespace.trim().is_empty() || action.trim().is_empty() {
                status.set_text("enter a namespace and an action");
                return;
            }
            match integrations::companion_run(runner.as_ref(), &namespace, &action, None, dry_run) {
                Ok(value) => status.set_text(&pretty(&value)),
                Err(err) => status.set_text(&err.to_string()),
            }
        });
    };
    wire(&dry_run, true);
    wire(&call, false);

    section.append(&action_row(&[
        namespace.upcast_ref(),
        action.upcast_ref(),
        dry_run.upcast_ref(),
        call.upcast_ref(),
    ]));
}

/// Builds the "other apps" sections from their CLIs. Missing tools render a
/// "not installed" note instead of failing the window.
fn build_integrations(content: &gtk::Box, runner: &Rc<dyn CommandRunner>, status: &gtk::Label) {
    let path = integrations::sidepanels_path(None);
    let overview = integrations::overview(runner.as_ref(), &path);

    integration_section(content, "Integrations", &overview.tools);

    let displays = integration_section(content, Tool::Monitor.label(), &overview.displays);
    add_display_actions(&displays, runner, status);

    let sessions = integration_section(content, Tool::Session.label(), &overview.sessions);
    add_session_actions(&sessions, runner, status);

    let wallpaper = integration_section(content, Tool::Wallpaper.label(), &overview.wallpaper);
    add_wallpaper_actions(&wallpaper, runner, status);

    let audio = integration_section(content, Tool::Audio.label(), &overview.audio);
    add_audio_actions(&audio, runner, status);

    let sidepanels = integration_section(content, Tool::Sidepanels.label(), &overview.sidepanels);
    add_sidepanels_actions(&sidepanels, runner, status, &path);

    let companion = integration_section(content, Tool::Companion.label(), &overview.companion);
    add_companion_actions(&companion, runner, status);
}

fn rebuild(
    content: &gtk::Box,
    rows: &Rc<RefCell<Vec<Row>>>,
    text: &str,
    runner: &Rc<dyn CommandRunner>,
    status: &gtk::Label,
) {
    build_rows(content, rows, text);
    build_integrations(content, runner, status);
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

    let runner: Rc<dyn CommandRunner> = Rc::new(SystemRunner);

    match read(&path) {
        Ok(loaded) => {
            *text.borrow_mut() = loaded;
            rebuild(&content, &rows, &text.borrow(), &runner, &status);
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
        let runner = runner.clone();
        reload.connect_clicked(move |_| match read(&path) {
            Ok(loaded) => {
                *text.borrow_mut() = loaded;
                rebuild(&content, &rows, &text.borrow(), &runner, &status);
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

#[cfg(test)]
mod tests {
    use better_settings::integrations::ScriptedRunner;

    use super::*;

    fn child_count(widget: &gtk::Box) -> usize {
        let mut count = 0;
        let mut child = widget.first_child();
        while let Some(current) = child {
            count += 1;
            child = current.next_sibling();
        }
        count
    }

    #[test]
    fn integration_sections_are_built_from_the_model() {
        let runner: Rc<dyn CommandRunner> = Rc::new(ScriptedRunner::new());
        let overview = integrations::overview(runner.as_ref(), std::path::Path::new("/tmp/none"));
        assert_eq!(overview.sections().len(), 7);
        let _ = child_count;
    }

    #[test]
    fn integration_widgets_build_when_gtk_can_initialise() {
        // Headless CI has no display; the section model above still covers the
        // data path, so skip widget construction instead of failing.
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return;
        }
        if gtk::init().is_err() {
            return;
        }
        let runner: Rc<dyn CommandRunner> = Rc::new(ScriptedRunner::new());
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let status = gtk::Label::new(None);
        build_integrations(&content, &runner, &status);
        assert_eq!(child_count(&content), 7);
    }
}
