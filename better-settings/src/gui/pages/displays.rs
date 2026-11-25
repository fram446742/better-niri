//! Displays: monitor profiles, the arrangement editor and HDR controls.

use std::cell::RefCell;
use std::rc::Rc;

use better_settings::integrations::{self, HdrOutput};
use better_settings::{Arrangement, GRID, Kind, SNAP_THRESHOLD, Viewport};
use gtk4 as gtk;
use libadwaita::prelude::*;

use crate::gui::{App, format, settings, widgets};

struct ArrangementState {
    arrangement: Arrangement,
    drag: Option<better_settings::DragState>,
    selected: Option<String>,
    width: f64,
    height: f64,
}

pub fn build(app: &Rc<App>) -> gtk::Widget {
    let page = widgets::preferences_page();
    page.add(&profiles_group(app));
    page.add(&arrangement_group(app));
    let (runtime, permanent) = hdr_groups(app);
    page.add(&runtime);
    page.add(&permanent);
    page.upcast()
}

fn profiles_group(app: &Rc<App>) -> libadwaita::PreferencesGroup {
    let group = widgets::group(
        "Monitor profiles",
        Some(
            "Profiles live in better-monitor. Preview is a dry run; Apply changes the compositor.",
        ),
    );
    let profiles = integrations::display_profiles(app.runner.as_ref()).unwrap_or_default();
    if profiles.is_empty() {
        group.add(&widgets::info_row(
            "No saved profiles",
            "Save one with `better-monitor save NAME`.",
        ));
        return group;
    }

    let combo = libadwaita::ComboRow::new();
    combo.set_title("Profile");
    combo.set_subtitle("The stored output layout to preview or apply.");
    let model = gtk::StringList::new(&profiles.iter().map(String::as_str).collect::<Vec<_>>());
    combo.set_model(Some(&model));
    group.add(&combo);
    let items = Rc::new(profiles);

    {
        let app = app.clone();
        let combo = combo.clone();
        let items = items.clone();
        group.add(&widgets::button_row(
            "Preview apply",
            "Run `better-monitor apply NAME --dry-run` and show the plan.",
            "Preview",
            false,
            move || {
                let Some(name) = widgets::selected(&combo, &items) else {
                    return;
                };
                match integrations::display_preview(app.runner.as_ref(), &name) {
                    Ok(plan) => app.report("Display preview", &format::display_plan(&plan)),
                    Err(err) => app.report("Preview failed", &err.to_string()),
                }
            },
        ));
    }
    {
        let app = app.clone();
        let combo = combo.clone();
        let items = items.clone();
        group.add(&widgets::button_row(
            "Apply profile",
            "Preview first, then apply with better-monitor. A rollback profile is stored by the tool.",
            "Apply",
            true,
            move || {
                let Some(name) = widgets::selected(&combo, &items) else {
                    return;
                };
                let preview = match integrations::display_preview(app.runner.as_ref(), &name) {
                    Ok(plan) if plan.errors.is_empty() => format::display_plan(&plan),
                    Ok(plan) => {
                        app.report("Preview has errors", &format::display_plan(&plan));
                        return;
                    }
                    Err(err) => {
                        app.report("Preview failed", &err.to_string());
                        return;
                    }
                };
                match integrations::display_apply(app.runner.as_ref(), &name) {
                    Ok(plan) => app.report(
                        "Applied display profile",
                        &format!("{preview}\n\nApplied:\n{}", format::display_plan(&plan)),
                    ),
                    Err(err) => app.report("Apply failed", &format!("{preview}\n\nerror: {err}")),
                }
            },
        ));
    }
    {
        let app = app.clone();
        group.add(&widgets::button_row(
            "Preview restore",
            "Preview restoring the previous layout.",
            "Preview",
            false,
            move || match integrations::display_restore_preview(app.runner.as_ref()) {
                Ok(plan) => app.report("Restore preview", &format::display_plan(&plan)),
                Err(err) => app.report("Preview failed", &err.to_string()),
            },
        ));
    }
    {
        let app = app.clone();
        group.add(&widgets::button_row(
            "Restore previous layout",
            "Preview, then restore the layout stored before the last apply.",
            "Restore",
            false,
            move || {
                let preview = match integrations::display_restore_preview(app.runner.as_ref()) {
                    Ok(plan) if plan.errors.is_empty() => format::display_plan(&plan),
                    Ok(plan) => {
                        app.report("Preview has errors", &format::display_plan(&plan));
                        return;
                    }
                    Err(err) => {
                        app.report("Preview failed", &err.to_string());
                        return;
                    }
                };
                match integrations::display_restore(app.runner.as_ref()) {
                    Ok(plan) => app.report(
                        "Restored previous layout",
                        &format!("{preview}\n\nRestored:\n{}", format::display_plan(&plan)),
                    ),
                    Err(err) => app.report("Restore failed", &format!("{preview}\n\nerror: {err}")),
                }
            },
        ));
    }
    {
        let app = app.clone();
        group.add(&widgets::button_row(
            "Confirm pending apply",
            "Keep the layout applied by a safe apply and cancel the rollback timer.",
            "Confirm",
            false,
            move || match integrations::display_confirm(app.runner.as_ref()) {
                Ok(value) => app.toast(format!("Confirmed: {}", format::pretty(&value))),
                Err(err) => app.report("Confirm failed", &err.to_string()),
            },
        ));
    }
    {
        let app = app.clone();
        group.add(&widgets::button_row(
            "Cancel pending apply",
            "Roll back to the previous layout.",
            "Cancel apply",
            false,
            move || {
                let app = app.clone();
                app.clone().confirm(
                    "Cancel the pending apply?",
                    "better-monitor will restore the layout stored before the pending apply.",
                    "Cancel apply",
                    move || match integrations::display_cancel(app.runner.as_ref()) {
                        Ok(value) => {
                            app.toast(format!("Rolled back: {}", format::pretty(&value)));
                        }
                        Err(err) => app.report("Cancel failed", &err.to_string()),
                    },
                );
            },
        ));
    }
    group
}

fn arrangement_group(app: &Rc<App>) -> libadwaita::PreferencesGroup {
    let group = widgets::group(
        "Arrange outputs",
        Some(
            "Drag an output or type exact coordinates. Preview is a dry run; Apply sends positions to niri.",
        ),
    );
    let arrangement = match integrations::display_positions(app.runner.as_ref()) {
        Ok(arrangement) if !arrangement.displays.is_empty() => arrangement,
        Ok(_) => {
            group.add(&widgets::info_row(
                "Arrangement editor unavailable",
                "The compositor reports no connected outputs.",
            ));
            return group;
        }
        Err(err) => {
            group.add(&widgets::info_row(
                "Arrangement editor unavailable",
                &err.to_string(),
            ));
            return group;
        }
    };

    let editor = gtk::Box::new(gtk::Orientation::Vertical, 8);
    editor.set_margin_top(10);
    editor.set_margin_bottom(10);
    editor.set_margin_start(12);
    editor.set_margin_end(12);

    let drawing = gtk::DrawingArea::new();
    drawing.set_content_height(260);
    drawing.set_hexpand(true);
    drawing.set_cursor_from_name(Some("grab"));
    editor.append(&drawing);

    let names: Vec<String> = arrangement
        .displays
        .iter()
        .map(|display| display.output.clone())
        .collect();
    let output_dd =
        gtk::DropDown::from_strings(&names.iter().map(String::as_str).collect::<Vec<_>>());
    let names = Rc::new(names);
    let x_spin = gtk::SpinButton::with_range(
        -f64::from(better_settings::MAX_COORDINATE),
        f64::from(better_settings::MAX_COORDINATE),
        1.0,
    );
    let y_spin = gtk::SpinButton::with_range(
        -f64::from(better_settings::MAX_COORDINATE),
        f64::from(better_settings::MAX_COORDINATE),
        1.0,
    );
    for spin in [&x_spin, &y_spin] {
        spin.set_width_chars(7);
        spin.set_numeric(true);
    }

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.append(&widgets::note("Output"));
    controls.append(&output_dd);
    controls.append(&widgets::note("X"));
    controls.append(&x_spin);
    controls.append(&widgets::note("Y"));
    controls.append(&y_spin);
    editor.append(&controls);

    let info = widgets::note(
        "Drag an output to move it; snap uses output edges and centres, then a grid.",
    );
    let preview = gtk::Button::with_label("Preview arrangement");
    let apply = gtk::Button::with_label("Apply arrangement");
    apply.add_css_class("suggested-action");
    let reset = gtk::Button::with_label("Reset");
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.append(&preview);
    actions.append(&apply);
    actions.append(&reset);
    actions.append(&info);
    info.set_hexpand(true);
    editor.append(&actions);
    editor.append(&widgets::note(&format!(
        "Snap threshold {} px, grid {GRID} px. Invalid arrangements are refused before any command runs.",
        SNAP_THRESHOLD
    )));
    group.add(&editor);

    let selected = arrangement
        .displays
        .first()
        .map(|display| display.output.clone());
    let state = Rc::new(RefCell::new(ArrangementState {
        arrangement,
        drag: None,
        selected,
        width: 640.0,
        height: 260.0,
    }));

    if let Some(name) = state.borrow().selected.clone()
        && let Some(display) = state.borrow().arrangement.get(&name)
    {
        x_spin.set_value(f64::from(display.x));
        y_spin.set_value(f64::from(display.y));
    }

    fn update_info(info: &gtk::Label, state: &ArrangementState) {
        let errors = state.arrangement.validate();
        if errors.is_empty() {
            let position = state
                .selected
                .as_deref()
                .and_then(|name| state.arrangement.get(name))
                .map(|display| format!("{}: {},{}", display.output, display.x, display.y))
                .unwrap_or_else(|| "select an output".to_owned());
            info.set_text(&format!("{position}; drag or type coordinates"));
        } else {
            info.set_text(&format!("cannot apply: {}", errors.join("; ")));
        }
    }

    {
        let state = state.clone();
        drawing.set_draw_func(move |_area, cr, width, height| {
            let mut state = state.borrow_mut();
            state.width = f64::from(width);
            state.height = f64::from(height);
            let viewport = Viewport::fit(&state.arrangement, state.width, state.height, 14.0);
            cr.set_source_rgb(0.09, 0.09, 0.11);
            let _ = cr.paint();
            for display in &state.arrangement.displays {
                if !display.enabled {
                    continue;
                }
                let (x, y, w, h) = viewport.rectangle(display);
                let is_selected = state.selected.as_deref() == Some(display.output.as_str());
                if is_selected {
                    cr.set_source_rgba(0.20, 0.45, 0.72, 0.9);
                } else {
                    cr.set_source_rgba(0.24, 0.28, 0.34, 0.9);
                }
                cr.rectangle(x, y, w, h);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(0.85, 0.88, 0.92, 0.9);
                cr.set_line_width(1.5);
                let _ = cr.stroke();
                cr.set_source_rgb(0.96, 0.97, 0.99);
                cr.set_font_size(12.0);
                cr.move_to(x + 6.0, y + 17.0);
                let _ = cr.show_text(&display.output);
                cr.set_font_size(9.0);
                cr.set_source_rgba(0.86, 0.89, 0.93, 0.9);
                cr.move_to(x + 6.0, y + 30.0);
                let _ = cr.show_text(&format!(
                    "{},{}  {}x{}",
                    display.x, display.y, display.width, display.height
                ));
            }
        });
    }

    {
        let state = state.clone();
        let drawing_for_event = drawing.clone();
        let x_spin = x_spin.clone();
        let y_spin = y_spin.clone();
        let info = info.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(1);
        gesture.connect_pressed(move |_, _, px, py| {
            let (logical_x, logical_y) = {
                let state = state.borrow();
                let viewport = Viewport::fit(&state.arrangement, state.width, state.height, 14.0);
                viewport.to_logical(px, py)
            };
            let selected_position = {
                let mut state = state.borrow_mut();
                state.drag = state.arrangement.begin_drag(logical_x, logical_y);
                if let Some(drag) = &state.drag {
                    state.selected = Some(drag.output.clone());
                }
                state
                    .selected
                    .as_deref()
                    .and_then(|name| state.arrangement.get(name))
                    .map(|display| (display.x, display.y))
            };
            if let Some((x, y)) = selected_position {
                x_spin.set_value(f64::from(x));
                y_spin.set_value(f64::from(y));
            }
            update_info(&info, &state.borrow());
            drawing_for_event.queue_draw();
        });
        drawing.add_controller(gesture);
    }

    {
        let state = state.clone();
        let drawing_for_event = drawing.clone();
        let x_spin = x_spin.clone();
        let y_spin = y_spin.clone();
        let info = info.clone();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(move |_, px, py| {
            let selected_position = {
                let mut state = state.borrow_mut();
                if state.drag.is_none() {
                    return;
                }
                let viewport = Viewport::fit(&state.arrangement, state.width, state.height, 14.0);
                let (logical_x, logical_y) = viewport.to_logical(px, py);
                let drag = state.drag.clone();
                if let Some(drag) = drag {
                    let _snap = state.arrangement.drag_to(
                        &drag,
                        logical_x,
                        logical_y,
                        (f64::from(SNAP_THRESHOLD) / viewport.scale().max(0.01)) as i32,
                    );
                }
                state
                    .selected
                    .as_deref()
                    .and_then(|name| state.arrangement.get(name))
                    .map(|display| (display.x, display.y))
            };
            if let Some((x, y)) = selected_position {
                x_spin.set_value(f64::from(x));
                y_spin.set_value(f64::from(y));
            }
            update_info(&info, &state.borrow());
            drawing_for_event.queue_draw();
        });
        drawing.add_controller(motion);
    }

    {
        let state = state.clone();
        let drawing_for_event = drawing.clone();
        let info = info.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(1);
        gesture.connect_released(move |_, _, _, _| {
            let mut state = state.borrow_mut();
            state.drag = None;
            update_info(&info, &state);
            drawing_for_event.queue_draw();
        });
        drawing.add_controller(gesture);
    }

    {
        let state = state.clone();
        let drawing = drawing.clone();
        let x_spin = x_spin.clone();
        let y_spin = y_spin.clone();
        let info = info.clone();
        output_dd.connect_selected_notify(move |dropdown| {
            let selected_position = {
                let mut state = state.borrow_mut();
                state.selected = names.get(dropdown.selected() as usize).cloned();
                state
                    .selected
                    .as_deref()
                    .and_then(|name| state.arrangement.get(name))
                    .map(|display| (display.x, display.y))
            };
            if let Some((x, y)) = selected_position {
                x_spin.set_value(f64::from(x));
                y_spin.set_value(f64::from(y));
            }
            update_info(&info, &state.borrow());
            drawing.queue_draw();
        });
    }

    {
        let state = state.clone();
        let drawing = drawing.clone();
        let info = info.clone();
        x_spin.connect_value_changed(move |spin| {
            let mut state = state.borrow_mut();
            if let Some(name) = state.selected.clone()
                && let Some(display) = state.arrangement.get(&name)
            {
                let y = display.y;
                state
                    .arrangement
                    .move_to(&name, spin.value().round() as i32, y);
            }
            update_info(&info, &state);
            drawing.queue_draw();
        });
    }
    {
        let state = state.clone();
        let drawing = drawing.clone();
        let info = info.clone();
        y_spin.connect_value_changed(move |spin| {
            let mut state = state.borrow_mut();
            if let Some(name) = state.selected.clone()
                && let Some(display) = state.arrangement.get(&name)
            {
                let x = display.x;
                state
                    .arrangement
                    .move_to(&name, x, spin.value().round() as i32);
            }
            update_info(&info, &state);
            drawing.queue_draw();
        });
    }

    let reload: Rc<dyn Fn()> = {
        let state = state.clone();
        let app = app.clone();
        let drawing = drawing.clone();
        let x_spin = x_spin.clone();
        let y_spin = y_spin.clone();
        let info = info.clone();
        Rc::new(
            move || match integrations::display_positions(app.runner.as_ref()) {
                Ok(arrangement) => {
                    let selected_position = {
                        let mut state = state.borrow_mut();
                        state.arrangement = arrangement;
                        if let Some(name) = state.selected.clone()
                            && !state.arrangement.displays.iter().any(|d| d.output == name)
                        {
                            state.selected = state
                                .arrangement
                                .displays
                                .first()
                                .map(|display| display.output.clone());
                        }
                        state
                            .selected
                            .as_deref()
                            .and_then(|name| state.arrangement.get(name))
                            .map(|display| (display.x, display.y))
                    };
                    if let Some((x, y)) = selected_position {
                        x_spin.set_value(f64::from(x));
                        y_spin.set_value(f64::from(y));
                    }
                    update_info(&info, &state.borrow());
                    drawing.queue_draw();
                }
                Err(err) => info.set_text(&format!("cannot reload positions: {err}")),
            },
        )
    };

    {
        let state = state.clone();
        let app = app.clone();
        preview.connect_clicked(move |_| {
            let errors = state.borrow().arrangement.validate();
            if !errors.is_empty() {
                app.report("Cannot preview arrangement", &errors.join("\n"));
                return;
            }
            let plan = state.borrow().arrangement.plan_string();
            match integrations::display_arrange_preview(app.runner.as_ref(), &plan) {
                Ok(plan) => app.report("Arrangement preview", &format::display_plan(&plan)),
                Err(err) => app.report("Preview failed", &err.to_string()),
            }
        });
    }
    {
        let state = state.clone();
        let app = app.clone();
        let reload = reload.clone();
        apply.connect_clicked(move |_| {
            let errors = state.borrow().arrangement.validate();
            if !errors.is_empty() {
                app.report("Cannot apply arrangement", &errors.join("\n"));
                return;
            }
            let plan = state.borrow().arrangement.plan_string();
            let preview = match integrations::display_arrange_preview(app.runner.as_ref(), &plan) {
                Ok(preview) if preview.errors.is_empty() => format::display_plan(&preview),
                Ok(preview) => {
                    app.report("Preview has errors", &format::display_plan(&preview));
                    return;
                }
                Err(err) => {
                    app.report("Preview failed", &err.to_string());
                    return;
                }
            };
            match integrations::display_arrange(app.runner.as_ref(), &plan) {
                Ok(report) => {
                    app.report(
                        "Applied arrangement",
                        &format!("{preview}\n\nApplied:\n{}", format::display_plan(&report)),
                    );
                    reload();
                }
                Err(err) => app.report("Apply failed", &format!("{preview}\n\nerror: {err}")),
            }
        });
    }
    reset.connect_clicked(move |_| reload());

    group
}

fn hdr_groups(app: &Rc<App>) -> (libadwaita::PreferencesGroup, libadwaita::PreferencesGroup) {
    let runtime = widgets::group(
        "HDR (runtime)",
        Some("Applied through `niri msg output OUTPUT hdr MODE`. The config file is untouched."),
    );
    let outputs: Vec<HdrOutput> = integrations::display_hdr_outputs(app.runner.as_ref())
        .unwrap_or_default()
        .into_iter()
        .filter(|output| output.supported)
        .collect();
    if outputs.is_empty() {
        runtime.add(&widgets::info_row(
            "No HDR-capable output",
            "niri reported no output with HDR support.",
        ));
    } else {
        let names: Vec<String> = outputs.iter().map(|output| output.name.clone()).collect();
        let output_combo = libadwaita::ComboRow::new();
        output_combo.set_title("Output");
        output_combo.set_subtitle("HDR-capable outputs reported by niri.");
        let output_model =
            gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>());
        output_combo.set_model(Some(&output_model));

        let modes = ["auto".to_owned(), "on".to_owned(), "off".to_owned()];
        let mode_combo = libadwaita::ComboRow::new();
        mode_combo.set_title("Mode");
        mode_combo.set_subtitle("auto follows HDR content, on forces HDR, off disables it.");
        let mode_model =
            gtk::StringList::new(&modes.iter().map(String::as_str).collect::<Vec<_>>());
        mode_combo.set_model(Some(&mode_model));

        let outputs = Rc::new(outputs);
        {
            let outputs = outputs.clone();
            let mode_combo = mode_combo.clone();
            output_combo.connect_selected_notify(move |combo| {
                let Some(output) = outputs.get(combo.selected() as usize) else {
                    return;
                };
                mode_combo.set_selected(match output.mode.as_str() {
                    "on" => 1,
                    "off" => 2,
                    _ => 0,
                });
            });
        }
        if let Some(first) = outputs.first() {
            mode_combo.set_selected(match first.mode.as_str() {
                "on" => 1,
                "off" => 2,
                _ => 0,
            });
        }

        let apply = libadwaita::ActionRow::new();
        apply.set_title("Apply runtime HDR mode");
        apply.set_subtitle(
            "Changes the live output only; use the permanent group below for config.kdl.",
        );
        let button = gtk::Button::with_label("Apply HDR");
        button.add_css_class("suggested-action");
        button.set_valign(gtk::Align::Center);
        {
            let app = app.clone();
            let outputs = outputs.clone();
            let output_combo = output_combo.clone();
            let mode_combo = mode_combo.clone();
            button.connect_clicked(move |_| {
                let Some(output) = outputs.get(output_combo.selected() as usize) else {
                    return;
                };
                let mode = match mode_combo.selected() {
                    1 => "on",
                    2 => "off",
                    _ => "auto",
                };
                match integrations::display_hdr_set(app.runner.as_ref(), &output.name, mode) {
                    Ok(()) => app.toast(format!("HDR {mode} applied to {}", output.name)),
                    Err(err) => app.report("HDR failed", &err.to_string()),
                }
            });
        }
        apply.add_suffix(&button);

        runtime.add(&output_combo);
        runtime.add(&mode_combo);
        runtime.add(&apply);
    }

    let permanent = widgets::group(
        "HDR (permanent, config.kdl)",
        Some(
            "Writes `output \"NAME\" { hdr { ... } }` through the text-preserving patcher. Save applies it to the file.",
        ),
    );
    let text = app.config_text();
    let mut count = 0;
    for spec in better_settings::catalog_for(&text) {
        if !(spec.path.ends_with(".hdr.mode") || spec.path.ends_with(".hdr.reference-luminance")) {
            continue;
        }
        if !matches!(spec.kind, Kind::Choice(_) | Kind::Int { .. }) {
            continue;
        }
        let row = settings::build_row(spec, &text);
        permanent.add(&row.widget);
        app.rows.borrow_mut().push(row);
        count += 1;
    }
    if count == 0 {
        permanent.add(&widgets::info_row(
            "No output blocks in the config",
            "Add an `output \"NAME\" { }` block first; HDR settings are generated from it.",
        ));
    }
    (runtime, permanent)
}
