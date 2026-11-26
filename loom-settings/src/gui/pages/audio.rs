//! Audio: backend status, mixer, default sink and routing rules.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use loom_settings::integrations::{self, AudioDeviceKind};

use crate::gui::{App, format, widgets};

struct MixerRow {
    kind: AudioDeviceKind,
    name: String,
    initial_volume: f64,
    initial_mute: bool,
    scale: gtk::Scale,
    mute: gtk::Switch,
}

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();

    let status_group = widgets::group(
        "Backend",
        Some("loom-audio picks a native or command backend; this page only reports and routes."),
    );
    match integrations::audio_status(app.runner.as_ref()) {
        Ok(status) => {
            status_group.add(&widgets::info_row(
                "Backend",
                &format!(
                    "{} (requested {}){}",
                    status.active_backend.as_deref().unwrap_or("unavailable"),
                    status.requested_backend,
                    if status.degraded { ", degraded" } else { "" }
                ),
            ));
            if let Some(description) = &status.description {
                status_group.add(&widgets::info_row("Details", description));
            }
            if let Some(reason) = &status.reason {
                status_group.add(&widgets::info_row("Reason", reason));
            }
            for attempt in &status.attempts {
                status_group.add(&widgets::info_row("Probe", attempt));
            }
            status_group.add(&widgets::info_row(
                "Default sink",
                status.current_default.as_deref().unwrap_or("(none)"),
            ));
            status_group.add(&widgets::info_row(
                "Rules",
                &format!(
                    "{} rule(s); configured default {}",
                    status.rules,
                    status.configured_default.as_deref().unwrap_or("(none)")
                ),
            ));
        }
        Err(err) => {
            let row = widgets::info_row("Backend unavailable", &err.to_string());
            row.add_css_class("error");
            status_group.add(&row);
        }
    }
    page.add(&status_group);

    let devices = match integrations::audio_devices(app.runner.as_ref()) {
        Ok(devices) => devices,
        Err(err) => {
            let group = widgets::group("Mixer", Some("Sinks and sources reported by loom-audio."));
            let row = widgets::info_row("Mixer unavailable", &err.to_string());
            row.add_css_class("error");
            group.add(&row);
            page.add(&group);
            return page.upcast();
        }
    };

    let mixer = widgets::group(
        "Mixer",
        Some("Move a slider or toggle mute; nothing is sent until Preview or Apply."),
    );
    mixer.add(&widgets::info_row(
        "Devices",
        &format!(
            "{} sink(s), {} source(s) via {}",
            devices.sinks.len(),
            devices.sources.len(),
            devices.backend
        ),
    ));
    let rows: Rc<RefCell<Vec<MixerRow>>> = Rc::new(RefCell::new(Vec::new()));
    let append = |kind: AudioDeviceKind,
                  name: String,
                  description: String,
                  volume: Option<f64>,
                  mute: Option<bool>| {
        let row = adw::ActionRow::new();
        row.set_title(&name);
        row.set_subtitle(&description);
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        scale.set_draw_value(true);
        scale.set_digits(2);
        scale.set_hexpand(true);
        scale.set_width_request(180);
        scale.set_value(volume.unwrap_or(1.0));
        scale.set_tooltip_text(Some("Volume (0.00–1.00)"));
        let mute_switch = gtk::Switch::new();
        mute_switch.set_active(mute.unwrap_or(false));
        mute_switch.set_valign(gtk::Align::Center);
        mute_switch.set_tooltip_text(Some("Mute"));
        row.add_suffix(&scale);
        row.add_suffix(&mute_switch);
        mixer.add(&row);
        rows.borrow_mut().push(MixerRow {
            kind,
            name,
            initial_volume: volume.unwrap_or(1.0),
            initial_mute: mute.unwrap_or(false),
            scale,
            mute: mute_switch,
        });
    };
    for sink in &devices.sinks {
        append(
            AudioDeviceKind::Sink,
            sink.name.clone(),
            sink.description.clone(),
            sink.volume,
            sink.mute,
        );
    }
    for source in &devices.sources {
        append(
            AudioDeviceKind::Source,
            source.name.clone(),
            source.description.clone(),
            source.volume,
            source.mute,
        );
    }
    if rows.borrow().is_empty() {
        mixer.add(&widgets::info_row(
            "No devices",
            "The backend reported no sinks or sources.",
        ));
    }

    let changes = {
        let rows = rows.clone();
        Rc::new(move || {
            let mut changes = Vec::new();
            for row in rows.borrow().iter() {
                let volume = row.scale.value();
                if (volume - row.initial_volume).abs() > 0.005 {
                    changes.push((row.kind, row.name.clone(), Some(volume), None));
                }
                if row.mute.is_active() != row.initial_mute {
                    changes.push((row.kind, row.name.clone(), None, Some(row.mute.is_active())));
                }
            }
            changes
        })
    };

    {
        let app = app.clone();
        let changes = changes.clone();
        mixer.add(&widgets::button_row(
            "Preview mixer changes",
            "Print the loom-audio dry-run result without changing volumes.",
            "Preview",
            false,
            move || {
                let changes = changes();
                if changes.is_empty() {
                    app.toast("No mixer changes");
                    return;
                }
                let mut lines = Vec::new();
                for (kind, name, volume, mute) in changes {
                    let result = match (volume, mute) {
                        (Some(volume), _) => integrations::audio_set_volume(
                            app.runner.as_ref(),
                            kind,
                            &name,
                            volume,
                            true,
                        ),
                        (None, Some(mute)) => integrations::audio_set_mute(
                            app.runner.as_ref(),
                            kind,
                            &name,
                            mute,
                            true,
                        ),
                        (None, None) => continue,
                    };
                    match result {
                        Ok(value) => lines.push(format!("{name}: {}", format::pretty(&value))),
                        Err(err) => lines.push(format!("{name}: {err}")),
                    }
                }
                app.report("Mixer dry run", &lines.join("\n"));
            },
        ));
    }
    {
        let app = app.clone();
        let changes = changes.clone();
        let rows = rows.clone();
        mixer.add(&widgets::button_row(
            "Apply mixer changes",
            "Run the previewed loom-audio volume and mute commands.",
            "Apply",
            true,
            move || {
                let changes = changes();
                if changes.is_empty() {
                    app.toast("No mixer changes");
                    return;
                }
                let mut lines = Vec::new();
                let mut failed = false;
                for (kind, name, volume, mute) in changes {
                    let result = match (volume, mute) {
                        (Some(volume), _) => integrations::audio_set_volume(
                            app.runner.as_ref(),
                            kind,
                            &name,
                            volume,
                            false,
                        ),
                        (None, Some(mute)) => integrations::audio_set_mute(
                            app.runner.as_ref(),
                            kind,
                            &name,
                            mute,
                            false,
                        ),
                        (None, None) => continue,
                    };
                    match result {
                        Ok(value) => lines.push(format!("{name}: {}", format::pretty(&value))),
                        Err(err) => {
                            failed = true;
                            lines.push(format!("{name}: {err}"));
                        }
                    }
                }
                if !failed {
                    for row in rows.borrow_mut().iter_mut() {
                        row.initial_volume = row.scale.value();
                        row.initial_mute = row.mute.is_active();
                    }
                }
                app.report(
                    if failed {
                        "Mixer applied with errors"
                    } else {
                        "Mixer applied"
                    },
                    &lines.join("\n"),
                );
            },
        ));
    }
    page.add(&mixer);

    let default_group = widgets::group(
        "Default sink",
        Some("Changes loom-audio's configured default sink (explicit apply)."),
    );
    let sinks = integrations::audio_sinks(app.runner.as_ref())
        .map(|sinks| {
            sinks
                .sinks
                .into_iter()
                .map(|sink| sink.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if sinks.is_empty() {
        default_group.add(&widgets::info_row(
            "No sinks",
            "No output devices were reported.",
        ));
    } else {
        let combo = adw::ComboRow::new();
        combo.set_title("Sink");
        combo.set_subtitle("Used by the routing rules and the default-sink fallback.");
        let model = gtk::StringList::new(&sinks.iter().map(String::as_str).collect::<Vec<_>>());
        combo.set_model(Some(&model));
        let items = Rc::new(sinks);
        {
            let app = app.clone();
            let selected_combo = combo.clone();
            let items = items.clone();
            let button = gtk::Button::with_label("Set default");
            button.set_valign(gtk::Align::Center);
            button.connect_clicked(move |_| {
                let Some(sink) = widgets::selected(&selected_combo, &items) else {
                    return;
                };
                let app = app.clone();
                app.clone().confirm(
                    "Set the configured default sink?",
                    &format!("loom-audio will store `{sink}` as its default sink."),
                    "Set default",
                    move || match integrations::audio_set_default(app.runner.as_ref(), &sink) {
                        Ok(value) => app.toast(format!("Default sink: {}", format::pretty(&value))),
                        Err(err) => app.report("Set default failed", &err.to_string()),
                    },
                );
            });
            combo.add_suffix(&button);
        }
        default_group.add(&combo);
    }
    page.add(&default_group);

    let routing = widgets::group(
        "Routing rules and restore",
        Some("Apply and restore are previewed first; loom-audio keeps its own rollback history."),
    );
    match integrations::audio_rules(app.runner.as_ref()) {
        Ok(rules) if !rules.is_empty() => {
            for rule in rules {
                let subtitle = format!(
                    "sink {} · priority {} · volume {} · mute {}",
                    rule.sink,
                    rule.priority,
                    rule.volume
                        .map(|volume| format!("{volume:.2}"))
                        .unwrap_or_else(|| "unchanged".to_owned()),
                    rule.mute
                        .map(|mute| mute.to_string())
                        .unwrap_or_else(|| "unchanged".to_owned())
                );
                routing.add(&widgets::info_row(&rule.app, &subtitle));
            }
        }
        Ok(_) => routing.add(&widgets::info_row(
            "No rules",
            "No per-app routing rules are configured.",
        )),
        Err(err) => routing.add(&widgets::info_row("Rules unavailable", &err.to_string())),
    }
    {
        let app = app.clone();
        routing.add(&widgets::button_row(
            "Preview rule apply",
            "Route matching streams without changing anything.",
            "Preview",
            false,
            move || match integrations::audio_apply_preview(app.runner.as_ref()) {
                Ok(value) => app.report("Audio apply preview", &format::pretty(&value)),
                Err(err) => app.report("Preview failed", &err.to_string()),
            },
        ));
    }
    {
        let app = app.clone();
        routing.add(&widgets::button_row(
            "Apply routing rules",
            "Preview, then route matching streams.",
            "Apply",
            true,
            move || {
                let preview = match integrations::audio_apply_preview(app.runner.as_ref()) {
                    Ok(value) => format::pretty(&value),
                    Err(err) => {
                        app.report("Preview failed", &err.to_string());
                        return;
                    }
                };
                match integrations::audio_apply(app.runner.as_ref()) {
                    Ok(value) => app.report(
                        "Audio rules applied",
                        &format!("{preview}\n\nApplied:\n{}", format::pretty(&value)),
                    ),
                    Err(err) => app.report("Apply failed", &format!("{preview}\n\nerror: {err}")),
                }
            },
        ));
    }
    {
        let app = app.clone();
        routing.add(&widgets::button_row(
            "Preview stream restore",
            "Show which streams loom-audio would move back.",
            "Preview",
            false,
            move || match integrations::audio_restore_preview(app.runner.as_ref()) {
                Ok(value) => app.report("Audio restore preview", &format::pretty(&value)),
                Err(err) => app.report("Preview failed", &err.to_string()),
            },
        ));
    }
    {
        let app = app.clone();
        routing.add(&widgets::button_row(
            "Restore streams",
            "Preview, then move streams back.",
            "Restore",
            false,
            move || {
                let preview = match integrations::audio_restore_preview(app.runner.as_ref()) {
                    Ok(value) => format::pretty(&value),
                    Err(err) => {
                        app.report("Preview failed", &err.to_string());
                        return;
                    }
                };
                match integrations::audio_restore(app.runner.as_ref()) {
                    Ok(value) => app.report(
                        "Streams restored",
                        &format!("{preview}\n\nRestored:\n{}", format::pretty(&value)),
                    ),
                    Err(err) => app.report("Restore failed", &format!("{preview}\n\nerror: {err}")),
                }
            },
        ));
    }
    page.add(&routing);

    page.upcast()
}
