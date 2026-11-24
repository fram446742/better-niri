//! Black-box coverage for the expanded model: every catalog entry writes and
//! clears, nested blocks appear in valid order, and edits preserve bytes.

use std::path::PathBuf;

use better_settings::{
    Bind, CATALOG, Edit, Kind, Model, RuleKind, RuleSpec, add_preset_column_width, add_rule,
    add_startup, binds, catalog_for, current, default_column_width, environment, export_model,
    import_model, nodes, patch, patch_many, preset_column_widths, remove_bind,
    remove_preset_column_width, remove_rule, remove_startup, rules, save, set_bind,
    set_default_column_width, spawn_at_startup, unified_diff, validate,
};

fn sample(kind: Kind, path: &str) -> String {
    match kind {
        Kind::Float { min, .. } => format!("{}", min.max(0.5)),
        Kind::Int { min, .. } => format!("{}", min.max(10)),
        Kind::Bool | Kind::Flag => "true".to_owned(),
        Kind::Text => "sample value".to_owned(),
        Kind::Choice(choices) => choices[0].to_owned(),
        Kind::Raw if path.ends_with("default-column-width") => "proportion 0.5".to_owned(),
        Kind::Raw if path.ends_with("preset-column-widths") => {
            "proportion 0.5, fixed 640".to_owned()
        }
        Kind::Raw => "x=0 y=5".to_owned(),
    }
}

fn edit_for(kind: Kind, path: &str, value: &str) -> Edit {
    match kind {
        Kind::Flag => Edit::flag(path, value != "false"),
        Kind::Raw => Edit::raw(path, value),
        _ => Edit::new(path, value),
    }
}

#[test]
fn every_catalog_setting_writes_reads_and_clears() {
    for spec in CATALOG {
        let value = sample(spec.kind, spec.path);
        let edit = edit_for(spec.kind, spec.path, &value);
        let patched = patch("", &edit).unwrap_or_else(|err| {
            panic!(
                "{} failed to write: {err}\n{patched}",
                spec.path,
                patched = ""
            )
        });
        let got = current(&patched, spec.path).unwrap();
        assert!(got.is_some(), "{} did not read back:\n{patched}", spec.path);
        if spec.kind == Kind::Raw && !spec.path.ends_with("preset-column-widths") {
            assert_eq!(
                got.as_deref().map(str::trim),
                Some(value.as_str()),
                "{} raw value",
                spec.path
            );
        }
        // Re-applying the same edit is a byte-identical no-op.
        assert_eq!(
            patch(&patched, &edit).unwrap(),
            patched,
            "{} is not idempotent",
            spec.path
        );
        if spec.kind == Kind::Flag {
            let cleared = patch(&patched, &Edit::flag(spec.path, false)).unwrap();
            assert_eq!(
                current(&cleared, spec.path).unwrap(),
                None,
                "{} did not clear",
                spec.path
            );
        }
    }
}

#[test]
fn every_output_setting_writes_reads_and_clears() {
    let text = "output \"eDP-1\" {\n    scale 1.5\n}\n";
    let settings: Vec<_> = catalog_for(text)
        .into_iter()
        .filter(|spec| spec.path.starts_with("output \"eDP-1\"."))
        .collect();
    assert!(settings.len() >= 18, "expected output coverage");
    for spec in settings {
        let value = sample(spec.kind, &spec.path);
        let edit = edit_for(spec.kind, &spec.path, &value);
        let patched = patch(text, &edit).unwrap();
        assert!(
            current(&patched, &spec.path).unwrap().is_some(),
            "{} did not read back:\n{patched}",
            spec.path
        );
        assert_eq!(
            patch(&patched, &edit).unwrap(),
            patched,
            "{} is not idempotent",
            spec.path
        );
        if spec.kind == Kind::Flag {
            let cleared = patch(&patched, &Edit::flag(spec.path.clone(), false)).unwrap();
            assert_eq!(current(&cleared, &spec.path).unwrap(), None);
        }
    }
}

#[test]
fn quoting_keeps_kdl_tokens_safe() {
    // Numbers and identifiers stay bare.
    assert_eq!(better_settings::quote_if_needed("1.5"), "1.5");
    assert_eq!(better_settings::quote_if_needed("-16"), "-16");
    assert_eq!(better_settings::quote_if_needed("two-finger"), "two-finger");
    assert_eq!(
        better_settings::quote_if_needed("ease-out-expo"),
        "ease-out-expo"
    );
    // Paths, colors, modes and anything with spaces are quoted.
    assert_eq!(
        better_settings::quote_if_needed("~/Pictures/Screenshots"),
        "\"~/Pictures/Screenshots\""
    );
    assert_eq!(better_settings::quote_if_needed("#003300"), "\"#003300\"");
    assert_eq!(
        better_settings::quote_if_needed("1920x1080@120.030"),
        "\"1920x1080@120.030\""
    );
    assert_eq!(better_settings::quote_if_needed("a:b"), "\"a:b\"");
}

#[test]
fn nested_blocks_are_created_outer_before_inner() {
    let patched = patch("", &Edit::new("input.touchpad.dwt", "true")).unwrap();
    assert_eq!(
        patched,
        "input {\n    touchpad {\n        dwt true\n    }\n}"
    );

    let patched = patch(
        "layout {\n    gaps 16\n}\n",
        &Edit::new("layout.focus-ring.width", "4"),
    )
    .unwrap();
    assert_eq!(
        patched,
        "layout {\n    gaps 16\n    focus-ring {\n        width 4\n    }\n}\n"
    );

    let patched = patch("", &Edit::new("output \"DP-2\".layout.gaps", "8")).unwrap();
    assert_eq!(
        patched,
        "output \"DP-2\" {\n    layout {\n        gaps 8\n    }\n}"
    );
}

#[test]
fn editing_preserves_comments_blank_lines_and_line_endings() {
    let config = "// top comment\nlayout {\n    /* inner */\n    gaps 16 // tail\n\n    struts {\n        left 4\n    }\n}\n\nbinds {\n    Mod+Left { focus-column-left; }\n}\n";
    let patched = patch(config, &Edit::new("layout.gaps", "24")).unwrap();
    assert!(patched.contains("// top comment"));
    assert!(patched.contains("/* inner */"));
    assert!(patched.contains("gaps 24 // tail"));
    assert!(patched.contains("Mod+Left { focus-column-left; }"));
    let before: Vec<_> = config
        .lines()
        .filter(|line| !line.contains("gaps"))
        .collect();
    let after: Vec<_> = patched
        .lines()
        .filter(|line| !line.contains("gaps"))
        .collect();
    assert_eq!(before, after);

    let crlf = "layout {\r\n    gaps 16\r\n}\r\n";
    let patched = patch(crlf, &Edit::new("layout.gaps", "24")).unwrap();
    assert_eq!(patched, "layout {\r\n    gaps 24\r\n}\r\n");
}

#[test]
fn no_op_patches_are_byte_identical() {
    let config = "layout {\n    gaps 16\n    default-column-width { proportion 0.5; }\n}\n\noutput \"eDP-1\" {\n    scale 1.5\n    variable-refresh-rate on-demand=true\n}\n\nspawn-at-startup \"waybar\"\n";
    for spec in catalog_for(config) {
        let Some(value) = current(config, &spec.path).unwrap() else {
            continue;
        };
        let edit = match spec.kind {
            Kind::Flag => Edit::flag(spec.path.clone(), true),
            Kind::Raw => Edit::raw(spec.path.clone(), value),
            _ => Edit::new(spec.path.clone(), value),
        };
        assert_eq!(
            patch(config, &edit).unwrap(),
            config,
            "{} changed on a no-op patch",
            spec.path
        );
    }
}

#[test]
fn bind_round_trip_with_all_flags_and_several_actions() {
    let bind = Bind {
        repeat: Some(false),
        cooldown_ms: Some(750),
        allow_when_locked: Some(true),
        allow_inhibiting: Some(false),
        hotkey_overlay_title: Some("Toggle \"dark\" mode".to_owned()),
        ..Bind::new(
            "Mod+Shift+D",
            vec![
                "spawn \"some-script\" \"--flag\"".to_owned(),
                "focus-column-right".to_owned(),
            ],
        )
    };
    let text = "binds {\n    Mod+Left { focus-column-left; }\n}\n";
    let patched = set_bind(text, &bind).unwrap();
    let parsed = binds(&patched);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[1], bind);
    assert_eq!(remove_bind(&patched, "Mod+Shift+D").unwrap(), text);

    // Creating the binds block is also supported.
    let created = set_bind("layout {\n    gaps 16\n}\n", &bind).unwrap();
    assert!(created.contains("binds {"));
    assert_eq!(binds(&created), vec![bind]);
}

#[test]
fn rule_and_startup_lists_round_trip() {
    let text = "layout {\n    gaps 16\n}\n";
    let rule = RuleSpec::window(
        vec!["app-id=\"Alacritty\"".to_owned()],
        vec!["open-floating true".to_owned(), "opacity 0.95".to_owned()],
    );
    let patched = add_rule(text, &rule).unwrap();
    assert_eq!(rules(&patched, RuleKind::Window), vec![rule]);
    assert_eq!(remove_rule(&patched, RuleKind::Window, 0).unwrap(), text);

    let layer = RuleSpec::layer(
        vec!["namespace=\"waybar\"".to_owned()],
        vec!["opacity 0.9".to_owned()],
    );
    let patched = add_rule(text, &layer).unwrap();
    assert_eq!(rules(&patched, RuleKind::Layer), vec![layer]);

    let patched = add_startup(text, &["waybar".to_owned(), "-s".to_owned()]).unwrap();
    assert_eq!(
        spawn_at_startup(&patched),
        vec![vec!["waybar".to_owned(), "-s".to_owned()]]
    );
    assert_eq!(remove_startup(&patched, 0).unwrap(), text);
}

#[test]
fn column_width_settings_round_trip() {
    let text = "layout {\n    gaps 16\n}\n";
    let patched = set_default_column_width(text, "fixed 640").unwrap();
    assert_eq!(default_column_width(&patched).as_deref(), Some("fixed 640"));
    let patched = add_preset_column_width(&patched, "proportion 0.5").unwrap();
    let patched = add_preset_column_width(&patched, "fixed 800").unwrap();
    assert_eq!(
        preset_column_widths(&patched),
        vec!["proportion 0.5", "fixed 800"]
    );
    let patched = remove_preset_column_width(&patched, 0).unwrap();
    assert_eq!(preset_column_widths(&patched), vec!["fixed 800"]);

    // An empty default-column-width block is meaningful and must not be
    // mistaken for "remove the setting".
    let empty = set_default_column_width(text, "").unwrap();
    assert!(empty.contains("default-column-width {}"), "{empty}");
    assert_eq!(default_column_width(&empty).as_deref(), Some(""));
    let removed = better_settings::remove_key(&empty, "layout.default-column-width").unwrap();
    assert_eq!(removed, text);
}

#[test]
fn export_import_json_is_deterministic() {
    let config = "layout {\n    gaps 16\n}\n\nbinds {\n    Mod+Left { focus-column-left; }\n}\n\nwindow-rule {\n    match app-id=\"Alacritty\"\n    open-floating true\n}\n\nspawn-at-startup \"waybar\"\nenvironment {\n    QT_QPA_PLATFORM \"wayland\"\n}\n";
    let model = export_model(config);
    let first = serde_json::to_string_pretty(&model).unwrap();
    let second = serde_json::to_string_pretty(&export_model(config)).unwrap();
    assert_eq!(first, second);
    let parsed: Model = serde_json::from_str(&first).unwrap();
    let rebuilt = import_model("", &parsed).unwrap();
    let again = export_model(&rebuilt);
    assert_eq!(again.settings, model.settings);
    assert_eq!(again.binds, model.binds);
    assert_eq!(again.window_rules, model.window_rules);
    assert_eq!(again.spawn_at_startup, model.spawn_at_startup);
    assert_eq!(environment(&rebuilt), model.environment);
}

#[test]
fn validation_reports_malformed_configs() {
    for broken in [
        "layout {\n    gaps 16\n",
        "layout {\n    gaps \"unterminated\n}\n",
        "}\n",
        "/* never closed\nlayout {\n}\n",
    ] {
        let report = validate(broken);
        assert!(!report.ok(), "expected failure for {broken:?}");
        assert!(!report.errors.is_empty());
    }
    let good = "layout {\n    gaps 16\n}\n";
    assert!(validate(good).ok());
}

#[test]
fn save_dry_run_does_not_write_and_real_save_backs_up() {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "better-settings-integration-{}-save",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.kdl");
    std::fs::write(&path, "layout {\n    gaps 16\n}\n").unwrap();

    assert!(
        save(&path, "layout {\n    gaps 24\n}\n", true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "layout {\n    gaps 16\n}\n"
    );

    let backup = save(&path, "layout {\n    gaps 24\n}\n", false)
        .unwrap()
        .expect("backup");
    assert!(backup.exists());
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "layout {\n    gaps 16\n}\n"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "layout {\n    gaps 24\n}\n"
    );
}

#[test]
fn diff_preview_is_empty_for_equal_text_and_shows_changes() {
    assert_eq!(unified_diff("a\n", "a\n", "a", "b"), "");
    let diff = unified_diff(
        "layout {\n    gaps 16\n}\n",
        "layout {\n    gaps 24\n}\n",
        "a/config.kdl",
        "b/config.kdl",
    );
    assert!(diff.starts_with("--- a/config.kdl\n+++ b/config.kdl\n"));
    assert!(diff.contains("-    gaps 16\n+    gaps 24\n"));
}

#[test]
fn node_scanner_handles_inline_and_multiline_blocks() {
    let text = "layout {\n    default-column-width { proportion 0.5; }\n    preset-column-widths {\n        proportion 0.33333\n    }\n}\nbinds { Mod+Left { focus-column-left; } }\n";
    let all = nodes(text);
    assert!(
        all.iter()
            .any(|node| node.name == "default-column-width" && node.inline)
    );
    assert!(
        all.iter()
            .any(|node| node.name == "preset-column-widths" && node.block)
    );
    assert!(
        all.iter()
            .any(|node| node.name == "Mod+Left" && node.inline)
    );
    assert!(all.iter().any(|node| node.name == "focus-column-left"));
}

#[test]
fn patch_many_keeps_the_config_valid_and_idempotent() {
    let config = "// keep me\nlayout {\n    gaps 16\n}\noutput \"eDP-1\" {\n    scale 1.5\n}\n";
    let edits = [
        Edit::new("layout.gaps", "24"),
        Edit::new("layout.border.width", "2"),
        Edit::new("output \"eDP-1\".transform", "90"),
        Edit::new("input.touchpad.natural-scroll", "true"),
        Edit::flag("prefer-no-csd", true),
    ];
    let patched = patch_many(config, &edits).unwrap();
    assert!(validate(&patched).ok(), "{:?}", validate(&patched).errors);
    assert_eq!(patch_many(&patched, &edits).unwrap(), patched);
    assert!(patched.contains("// keep me"));
}
