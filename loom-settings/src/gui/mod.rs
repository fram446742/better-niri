//! The libadwaita control-center shell.
//!
//! Pages never duplicate the model or the CLI dispatch: they read the shared
//! config text, build catalog widgets from `loom-settings`'s catalog, and
//! route every action through the existing [`CommandRunner`] integrations.
//! Mutations stay explicit: preview-then-apply where the companion CLI has a
//! dry run, otherwise a confirmation dialog.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use loom_settings::integrations::{
    CommandRunner, IntegrationView, SystemRunner, sidepanels_open_command,
};
use loom_settings::{config_path, patch_many, save as save_config, unified_diff, validate};
use settings::CatalogRow;

pub mod format;
pub mod pages;
pub mod settings;
pub mod widgets;

use pages::Page;

/// Shared state for every page.
pub(crate) struct App {
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub split: adw::NavigationSplitView,
    pub stack: gtk::Stack,
    pub sidebar: gtk::ListBox,
    pub content_title: gtk::Label,
    pub path: PathBuf,
    pub text: RefCell<String>,
    pub rows: RefCell<Vec<CatalogRow>>,
    pub runner: Rc<dyn CommandRunner>,
}

impl App {
    /// The current config text.
    pub fn config_text(&self) -> String {
        self.text.borrow().clone()
    }

    /// Shows a short toast.
    pub fn toast(&self, text: impl AsRef<str>) {
        let toast = adw::Toast::new(text.as_ref());
        toast.set_timeout(4);
        self.toasts.add_toast(toast);
    }

    /// Shows long command output in a scrollable dialog.
    pub fn report(&self, title: &str, body: &str) {
        let view = gtk::TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_top_margin(12);
        view.set_bottom_margin(12);
        view.buffer()
            .set_text(if body.is_empty() { "(no output)" } else { body });
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&view)
            .build();

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let close = gtk::Button::with_label("Close");
        header.pack_end(&close);
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scrolled));

        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(780)
            .content_height(560)
            .child(&toolbar)
            .build();
        let dialog_for_close = dialog.clone();
        close.connect_clicked(move |_| {
            dialog_for_close.close();
        });
        dialog.present(Some(&self.window));
    }

    /// Shows a modal confirmation and runs `action` only on the destructive
    /// response.
    pub fn confirm<F>(&self, heading: &str, body: &str, label: &str, action: F)
    where
        F: Fn() + 'static,
    {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("go", label);
        dialog.set_response_appearance("go", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let action = Rc::new(action);
        dialog.connect_response(None, move |dialog: &adw::AlertDialog, response: &str| {
            if response == "go" {
                action();
            }
            dialog.close();
        });
        dialog.present(Some(&self.window));
    }

    /// Re-reads the config file and rebuilds every page.
    pub fn reload(self: &Rc<Self>) {
        match read(&self.path) {
            Ok(text) => {
                *self.text.borrow_mut() = text;
                self.rebuild();
                self.toast(validation_message(&self.config_text()));
            }
            Err(err) => self.report("Cannot reload config", &err),
        }
    }

    /// Rebuilds every page from the in-memory text.
    pub fn rebuild(self: &Rc<Self>) {
        let current = self
            .stack
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_else(|| Page::Overview.name().to_owned());
        while let Some(child) = self.stack.first_child() {
            self.stack.remove(&child);
        }
        self.rows.borrow_mut().clear();
        for page in Page::ALL {
            let widget = pages::build(self, page);
            self.stack.add_named(&widget, Some(page.name()));
        }
        self.stack.set_visible_child_name(&current);
    }

    /// Collects changed rows and shows the unified diff without writing.
    pub fn preview(self: &Rc<Self>) {
        let pending = settings::pending_edits(&self.rows.borrow());
        if !pending.errors.is_empty() {
            self.report("Cannot preview", &pending.errors.join("\n"));
            return;
        }
        if pending.changed == 0 {
            self.toast("No changes to preview");
            return;
        }
        let current = self.config_text();
        match patch_many(&current, &pending.edits) {
            Ok(patched) => {
                let diff = unified_diff(&current, &patched, "current", "patched");
                if diff.trim().is_empty() {
                    self.toast("No changes to preview");
                } else {
                    let report = validate(&patched);
                    let suffix = if report.ok() {
                        String::new()
                    } else {
                        format!("\n\nValidation problems:\n{}", report.errors.join("\n"))
                    };
                    self.report("Preview changes", &format!("{diff}{suffix}"));
                }
            }
            Err(err) => self.report("Cannot patch config", &err.to_string()),
        }
    }

    /// Validates and saves changed rows through the text-preserving patcher.
    pub fn save(self: &Rc<Self>) {
        let pending = settings::pending_edits(&self.rows.borrow());
        if !pending.errors.is_empty() {
            self.report("Cannot save", &pending.errors.join("\n"));
            return;
        }
        if pending.changed == 0 {
            self.toast("No changes");
            return;
        }
        let current = self.config_text();
        let patched = match patch_many(&current, &pending.edits) {
            Ok(patched) => patched,
            Err(err) => {
                self.report("Cannot patch config", &err.to_string());
                return;
            }
        };
        let report = validate(&patched);
        if !report.ok() {
            self.report(
                "Refusing to save an invalid config",
                &report.errors.join("\n"),
            );
            return;
        }
        match save_config(&self.path, &patched, false) {
            Ok(backup) => {
                *self.text.borrow_mut() = patched;
                self.rebuild();
                let summary = match backup {
                    Some(backup) => format!(
                        "Saved {} setting(s); backup at {}",
                        pending.changed,
                        backup.display()
                    ),
                    None => format!("Saved {} setting(s)", pending.changed),
                };
                self.toast(summary);
            }
            Err(err) => self.report(
                "Cannot write config",
                &format!("{}: {err}", self.path.display()),
            ),
        }
    }

    /// Opens the config in `$VISUAL`/`$EDITOR` (or the desktop opener).
    pub fn open_config(&self) {
        let (program, args) = sidepanels_open_command(&self.path);
        match std::process::Command::new(&program).args(&args).status() {
            Ok(status) if status.success() => {
                self.toast(format!("Opened {}", self.path.display()));
            }
            Ok(status) => self.report(
                "Cannot open config",
                &format!("{program} exited with {status}"),
            ),
            Err(err) => self.report("Cannot open config", &format!("{program}: {err}")),
        }
    }

    /// Re-runs structural and catalog validation.
    pub fn validate(self: &Rc<Self>) {
        self.report(
            "Config validation",
            &validation_message(&self.config_text()),
        );
    }

    /// Selects a sidebar page and, on narrow layouts, reveals the content.
    pub fn select_page(self: &Rc<Self>, page: Page) {
        let mut child = self.sidebar.first_child();
        while let Some(row) = child {
            if row.widget_name() == page.name() {
                self.sidebar
                    .select_row(row.downcast_ref::<gtk::ListBoxRow>());
                break;
            }
            child = row.next_sibling();
        }
        self.split.set_show_content(true);
    }

    /// Adds a compact card-style group with a status and optional lines.
    pub fn view_group(
        &self,
        group: &adw::PreferencesGroup,
        view: &IntegrationView,
    ) -> adw::PreferencesGroup {
        let details = view.lines.join("\n");
        let row = widgets::info_row(&view.status, &details);
        if !details.is_empty() {
            row.set_subtitle_lines(0);
        }
        group.add(&row);
        if let Some(error) = &view.error {
            let row = widgets::info_row("error", error);
            row.add_css_class("error");
            let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
            icon.add_css_class("error");
            row.add_suffix(&icon);
            group.add(&row);
        }
        group.clone()
    }
}

/// Which page a config path came from.
fn configured_path() -> PathBuf {
    std::env::var_os("LOOM_SETTINGS_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("NIRI_CONFIG").map(PathBuf::from))
        .unwrap_or_else(config_path)
}

pub(crate) fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
    }
}

pub(crate) fn validation_message(text: &str) -> String {
    let report = validate(text);
    if report.ok() {
        format!("Config OK: {} blocks, {} keys", report.blocks, report.keys)
    } else {
        format!(
            "Config problems ({}):\n{}",
            report.errors.len(),
            report.errors.join("\n")
        )
    }
}

fn sidebar_row(page: Page) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(page.name());
    row.set_tooltip_text(Some(page.search_terms()));

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    body.set_margin_top(8);
    body.set_margin_bottom(8);
    body.set_margin_start(10);
    body.set_margin_end(10);

    let icon = gtk::Image::from_icon_name(page.icon());
    icon.set_pixel_size(18);
    body.append(&icon);

    let label = gtk::Label::new(Some(page.title()));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    body.append(&label);

    row.set_child(Some(&body));
    row
}

fn build_application(
    application: &adw::Application,
    path: PathBuf,
    text: String,
    runner: Rc<dyn CommandRunner>,
) -> Rc<App> {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Loom Settings")
        .default_width(1180)
        .default_height(780)
        .width_request(760)
        .height_request(540)
        .build();

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    let content_title = gtk::Label::new(Some("Overview"));
    content_title.add_css_class("title-4");

    let content_header = adw::HeaderBar::new();
    content_header.set_title_widget(Some(&content_title));

    let preview_button = gtk::Button::with_label("Preview");
    preview_button.set_tooltip_text(Some("Preview changes (dry run)"));
    preview_button.add_css_class("flat");
    let reload_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    reload_button.set_tooltip_text(Some("Reload config"));
    let save_button = gtk::Button::with_label("Save");
    save_button.set_tooltip_text(Some("Save changed settings"));
    save_button.add_css_class("suggested-action");

    let content_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content_actions.set_valign(gtk::Align::Center);
    content_actions.append(&preview_button);
    content_actions.append(&reload_button);
    content_actions.append(&save_button);
    content_header.pack_end(&content_actions);

    let content_toolbar = adw::ToolbarView::new();
    content_toolbar.add_top_bar(&content_header);
    content_toolbar.set_content(Some(&stack));
    let content_page = adw::NavigationPage::new(&content_toolbar, "Loom Settings");

    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    sidebar.set_selection_mode(gtk::SelectionMode::Single);

    let sidebar_header = adw::HeaderBar::new();
    let sidebar_title = gtk::Label::new(Some("Loom Settings"));
    sidebar_title.add_css_class("title-4");
    sidebar_header.set_title_widget(Some(&sidebar_title));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search pages"));
    search.set_margin_top(6);
    search.set_margin_bottom(6);
    search.set_margin_start(12);
    search.set_margin_end(12);

    let sidebar_body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_body.append(&search);
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&sidebar)
        .build();
    sidebar_body.append(&sidebar_scroll);

    let version = gtk::Label::new(Some(&format!(
        "Loom Settings {}",
        env!("CARGO_PKG_VERSION")
    )));
    version.add_css_class("dim-label");
    version.add_css_class("caption");
    version.set_margin_top(6);
    version.set_margin_bottom(10);
    sidebar_body.append(&version);

    let sidebar_toolbar = adw::ToolbarView::new();
    sidebar_toolbar.add_css_class("sidebar-pane");
    sidebar_toolbar.add_top_bar(&sidebar_header);
    sidebar_toolbar.set_content(Some(&sidebar_body));
    let sidebar_page = adw::NavigationPage::new(&sidebar_toolbar, "Loom Settings");

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(220.0);
    split.set_max_sidebar_width(330.0);
    split.set_sidebar_width_fraction(0.24);

    // Small visual polish that works with the system palette: a consistent
    // separator between the navigation sidebar and the content, and a little
    // breathing room under row titles.
    if let Some(display) = gtk::gdk::Display::default() {
        let css = gtk::CssProvider::new();
        css.load_from_data(
            ".sidebar-pane { border-right: 1px solid alpha(@window_fg_color, 0.10); }\n\
             .sidebar-pane list.navigation-sidebar { padding: 6px 8px; }\n\
             .sidebar-pane list.navigation-sidebar > row { border-radius: 10px; margin: 1px 0; }\n\
             .page-intro { margin: 4px 0 10px 0; }\n\
             .stat-grid { margin: 0 0 6px 0; }\n\
             .stat-card {\n\
                 background-color: alpha(@window_fg_color, 0.06);\n\
                 border: 1px solid alpha(@window_fg_color, 0.10);\n\
                 border-radius: 12px;\n\
                 padding: 12px 14px;\n\
             }\n\
             .stat-value { font-size: 20px; font-weight: 800; }\n\
             .stat-label { font-size: 12px; opacity: 0.72; }\n\
             preferencesgroup row { padding-top: 4px; padding-bottom: 4px; }\n\
             preferencesgroup row:not(:last-child) {\n\
                 border-bottom: 1px solid alpha(@window_fg_color, 0.08);\n\
             }\n\
             row .subtitle { margin-top: 3px; }\n",
        );
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&split));
    window.set_content(Some(&toasts));

    let app = Rc::new(App {
        window: window.clone(),
        toasts,
        split,
        stack,
        sidebar: sidebar.clone(),
        content_title: content_title.clone(),
        path,
        text: RefCell::new(text),
        rows: RefCell::new(Vec::new()),
        runner,
    });

    for page in Page::ALL {
        sidebar.append(&sidebar_row(page));
    }

    {
        let weak = Rc::downgrade(&app);
        let preview_button = preview_button.clone();
        let save_button = save_button.clone();
        sidebar.connect_row_selected(move |_, row| {
            let Some(row) = row else {
                return;
            };
            let Some(page) = Page::from_name(&row.widget_name()) else {
                return;
            };
            let Some(app) = weak.upgrade() else {
                return;
            };
            app.stack.set_visible_child_name(page.name());
            app.content_title.set_text(page.title());
            app.split.set_show_content(true);

            // Only the compositor catalog and the permanent HDR rows are
            // edited in place; other pages have their own explicit actions.
            let edits_config = matches!(page, Page::Compositor | Page::Displays);
            preview_button.set_visible(edits_config);
            save_button.set_visible(edits_config);
        });
    }

    {
        let weak = Rc::downgrade(&app);
        search.connect_search_changed(move |search| {
            let query = search.text().to_lowercase();
            let Some(app) = weak.upgrade() else {
                return;
            };
            let mut child = app.sidebar.first_child();
            while let Some(row) = child {
                let name = row.widget_name();
                let visible = Page::from_name(&name)
                    .map(|page| {
                        query.is_empty()
                            || page.title().to_lowercase().contains(&query)
                            || page.search_terms().contains(&query)
                    })
                    .unwrap_or(true);
                row.set_visible(visible);
                child = row.next_sibling();
            }
        });
    }

    {
        let weak = Rc::downgrade(&app);
        reload_button.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.reload();
            }
        });
    }
    {
        let weak = Rc::downgrade(&app);
        save_button.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.save();
            }
        });
    }
    {
        let weak = Rc::downgrade(&app);
        preview_button.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.preview();
            }
        });
    }

    {
        let about = gio::SimpleAction::new("about", None);
        let weak = Rc::downgrade(&app);
        about.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.select_page(Page::About);
            }
        });
        window.add_action(&about);
        application.set_accels_for_action("win.about", &["F1"]);
    }

    app.rebuild();
    app.select_page(Page::Overview);
    app
}

/// Application entry point shared by `main` and the headless test.
pub fn run() -> gtk::glib::ExitCode {
    let application = adw::Application::builder()
        .application_id("org.loom.Settings")
        .build();
    application.connect_startup(|_| {
        // Explicitly follow the desktop's light/dark preference. libadwaita's
        // default is `ColorScheme::Default`, which does exactly that; keeping
        // the call here documents the intent and works on older libadwaita.
        let _manager = adw::StyleManager::default();
    });
    application.connect_activate(build_ui);
    application.run()
}

fn build_ui(application: &adw::Application) {
    let path = configured_path();
    let text = read(&path).unwrap_or_default();
    let runner: Rc<dyn CommandRunner> = Rc::new(SystemRunner);
    let app = build_application(application, path, text, runner);
    app.window.present();
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use loom_settings::integrations::{CommandResult, ScriptedRunner};

    use super::*;

    fn fixture(name: &str) -> serde_json::Value {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
    }

    fn response(program: &str, args: &[&str], fixture_name: &str) -> CommandResult {
        CommandResult::output(
            program,
            args.to_vec(),
            0,
            fixture(fixture_name).to_string(),
            "",
        )
    }

    fn display_available() -> bool {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    fn ensure_gtk() -> bool {
        if !display_available() {
            return false;
        }
        if gtk::is_initialized() {
            return true;
        }
        gtk::init().is_ok() && adw::init().is_ok()
    }

    fn page_count(app: &App) -> usize {
        let mut count = 0;
        let mut child = app.stack.first_child();
        while let Some(current) = child {
            count += 1;
            child = current.next_sibling();
        }
        count
    }

    fn sidebar_count(app: &App) -> usize {
        let mut count = 0;
        let mut child = app.sidebar.first_child();
        while let Some(current) = child {
            count += 1;
            child = current.next_sibling();
        }
        count
    }

    #[test]
    fn page_catalog_is_complete_and_unique() {
        let mut names = std::collections::BTreeSet::new();
        for page in Page::ALL {
            assert!(names.insert(page.name()), "duplicate page {}", page.name());
            assert!(!page.title().is_empty());
            assert!(!page.icon().is_empty());
        }
        assert_eq!(names.len(), 10);
        assert_eq!(Page::from_name("compositor"), Some(Page::Compositor));
        assert_eq!(Page::from_name("nope"), None);
    }

    #[test]
    fn headless_control_center_builds_every_page() {
        // GTK is not initialized twice per process; this is the only widget test.
        if !ensure_gtk() {
            return;
        }

        let runner = ScriptedRunner::new()
            .on(
                "niri",
                &["msg", "--json", "version"],
                response("niri", &["msg", "--json", "version"], "niri-version.json"),
            )
            .on(
                "niri",
                &["msg", "--json", "capabilities"],
                response(
                    "niri",
                    &["msg", "--json", "capabilities"],
                    "niri-capabilities.json",
                ),
            )
            .on(
                "niri",
                &["msg", "--json", "outputs"],
                response("niri", &["msg", "--json", "outputs"], "niri-outputs.json"),
            )
            .on(
                "loom-monitor",
                &["--version"],
                CommandResult::output("loom-monitor", ["--version"], 0, "1\n", ""),
            )
            .on(
                "loom-monitor",
                &["--json", "list"],
                response("loom-monitor", &["--json", "list"], "monitor-list.json"),
            )
            .on(
                "loom-monitor",
                &["--json", "position"],
                response(
                    "loom-monitor",
                    &["--json", "position"],
                    "monitor-position.json",
                ),
            )
            .on(
                "loom-session",
                &["--version"],
                CommandResult::output("loom-session", ["--version"], 0, "1\n", ""),
            )
            .on(
                "loom-session",
                &["list"],
                response("loom-session", &["list"], "session-list.json"),
            )
            .on(
                "loom-session",
                &["history", "list"],
                response("loom-session", &["history", "list"], "session-history.json"),
            )
            .on(
                "loom-wallpaper",
                &["--version"],
                CommandResult::output("loom-wallpaper", ["--version"], 0, "1\n", ""),
            )
            .on(
                "loom-wallpaper",
                &["status"],
                response("loom-wallpaper", &["status"], "wallpaper-status.json"),
            )
            .on(
                "loom-wallpaper",
                &["scene", "list"],
                response(
                    "loom-wallpaper",
                    &["scene", "list"],
                    "wallpaper-scene-list.json",
                ),
            )
            .on(
                "loom-audio",
                &["--version"],
                CommandResult::output("loom-audio", ["--version"], 0, "1\n", ""),
            )
            .on(
                "loom-audio",
                &["status"],
                response("loom-audio", &["status"], "audio-status.json"),
            )
            .on(
                "loom-audio",
                &["sinks"],
                response("loom-audio", &["sinks"], "audio-sinks.json"),
            )
            .on(
                "loom-audio",
                &["sources"],
                response("loom-audio", &["sources"], "audio-sources.json"),
            )
            .on(
                "loom-sidepanels",
                &["--version"],
                CommandResult::output("loom-sidepanels", ["--version"], 0, "1\n", ""),
            )
            .on(
                "loomctl",
                &["--version"],
                CommandResult::output("loomctl", ["--version"], 0, "1\n", ""),
            )
            .on(
                "loomctl",
                &["status"],
                response("loomctl", &["status"], "companion-status.json"),
            )
            .on(
                "loomctl",
                &["describe"],
                response("loomctl", &["describe"], "companion-describe.json"),
            )
            .on(
                "loomctl",
                &["list"],
                response("loomctl", &["list"], "companion-list.json"),
            )
            .on(
                "loomctl",
                &["history", "20"],
                response("loomctl", &["history", "20"], "companion-history.json"),
            )
            .on(
                "loomctl",
                &["call", "idle", "status"],
                response(
                    "loomctl",
                    &["call", "idle", "status"],
                    "companion-idle-status.json",
                ),
            )
            .on(
                "loomctl",
                &["call", "rotate", "status"],
                response(
                    "loomctl",
                    &["call", "rotate", "status"],
                    "companion-rotate-status.json",
                ),
            )
            .on(
                "loomctl",
                &["call", "marks", "list"],
                response(
                    "loomctl",
                    &["call", "marks", "list"],
                    "companion-marks.json",
                ),
            );

        // A test-owned adw::Application never runs a main loop.
        let application = adw::Application::builder()
            .application_id("org.loom.Settings.Test")
            .build();
        // Emit startup before creating an ApplicationWindow; no activate
        // handler is connected, so this only satisfies GTK's lifecycle rule.
        let _ = application.register(None::<&gtk::gio::Cancellable>);
        application.connect_activate(|_| {});
        application.activate();
        let path = std::env::temp_dir().join("loom-settings-headless-config.kdl");
        let text = "layout {\n    gaps 12\n}\noutput \"eDP-1\" {\n    scale 1.5\n}\n".to_owned();
        let app = build_application(
            &application,
            path,
            text,
            Rc::new(runner) as Rc<dyn CommandRunner>,
        );

        assert_eq!(page_count(&app), Page::ALL.len());
        assert_eq!(sidebar_count(&app), Page::ALL.len());
        assert!(!app.rows.borrow().is_empty(), "catalog rows were not built");
        assert_eq!(
            app.stack.visible_child_name().as_deref(),
            Some(Page::Overview.name())
        );

        // Loading a config must not mark rows dirty, and every row must read
        // back as a non-empty KDL value.
        let pending = settings::pending_edits(&app.rows.borrow());
        assert_eq!(pending.changed, 0, "loading a config marked rows dirty");
        assert!(pending.errors.is_empty());

        // Every page must be present by name, not just in the sidebar.
        for page in Page::ALL {
            assert!(
                app.stack.child_by_name(page.name()).is_some(),
                "missing page {}",
                page.name()
            );
        }

        // Switching pages updates the title and visible child.
        let switched = Rc::new(Cell::new(false));
        let switched_for_page = switched.clone();
        let page = Page::Compositor;
        app.stack.connect_visible_child_name_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some(page.name()) {
                switched_for_page.set(true);
            }
        });
        app.select_page(Page::Compositor);
        assert_eq!(
            app.stack.visible_child_name().as_deref(),
            Some(Page::Compositor.name())
        );
        assert!(switched.get(), "page switch did not notify");
        assert_eq!(app.content_title.text(), Page::Compositor.title());

        // Rebuilding (as Save/Reload do) keeps ten pages and no duplicate rows.
        let rows_before = app.rows.borrow().len();
        app.rebuild();
        assert_eq!(page_count(&app), Page::ALL.len());
        assert_eq!(app.rows.borrow().len(), rows_before);
    }
}
