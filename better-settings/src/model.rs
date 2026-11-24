//! JSON export/import of the whole editable model.
//!
//! The model is a snapshot, not a re-serialization of the file: settings are
//! stored as paths plus raw values, binds/rules/startup commands as lists, and
//! environment variables as a map. Importing replaces the managed lists and
//! applies every setting through the text-preserving patcher.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{Kind, catalog_for};
use crate::nodes::{
    Bind, RuleKind, RuleSpec, add_rule, binds, environment, remove_bind, remove_rule,
    remove_startup, rules, set_bind, spawn_at_startup,
};
use crate::{Edit, PatchError, current, patch, quote_if_needed};

pub const MODEL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingValue {
    pub path: String,
    pub value: String,
    #[serde(default)]
    pub flag: bool,
    #[serde(default)]
    pub raw: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub version: u32,
    #[serde(default)]
    pub settings: Vec<SettingValue>,
    #[serde(default)]
    pub binds: Vec<Bind>,
    #[serde(default)]
    pub window_rules: Vec<RuleSpec>,
    #[serde(default)]
    pub layer_rules: Vec<RuleSpec>,
    #[serde(default)]
    pub spawn_at_startup: Vec<Vec<String>>,
    #[serde(default)]
    pub environment: BTreeMap<String, Option<String>>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            version: MODEL_VERSION,
            settings: Vec::new(),
            binds: Vec::new(),
            window_rules: Vec::new(),
            layer_rules: Vec::new(),
            spawn_at_startup: Vec::new(),
            environment: BTreeMap::new(),
        }
    }
}

/// Captures every catalog value and managed list currently present in `text`.
pub fn export_model(text: &str) -> Model {
    let mut model = Model::default();

    for spec in catalog_for(text) {
        let Ok(Some(value)) = current(text, &spec.path) else {
            continue;
        };
        let (flag, raw) = match spec.kind {
            Kind::Flag => (true, false),
            Kind::Raw => (false, true),
            _ => (false, false),
        };
        model.settings.push(SettingValue {
            path: spec.path,
            value: if flag { "true".to_owned() } else { value },
            flag,
            raw,
        });
    }
    model
        .settings
        .sort_by(|left, right| left.path.cmp(&right.path));

    model.binds = binds(text);
    let mut window_rules = rules(text, RuleKind::Window);
    for rule in &mut window_rules {
        rule.kind = RuleKind::Window;
    }
    model.window_rules = window_rules;
    let mut layer_rules = rules(text, RuleKind::Layer);
    for rule in &mut layer_rules {
        rule.kind = RuleKind::Layer;
    }
    model.layer_rules = layer_rules;
    model.spawn_at_startup = spawn_at_startup(text);
    model.environment = environment(text);
    model
}

/// Applies `model` onto `text`, replacing managed lists and settings.
pub fn import_model(text: &str, model: &Model) -> Result<String, PatchError> {
    let mut out = text.to_owned();

    // Replace lists: remove the existing entries first, then append the model.
    for bind in binds(&out).into_iter().rev() {
        out = remove_bind(&out, &bind.key)?;
    }
    for kind in [RuleKind::Window, RuleKind::Layer] {
        while !rules(&out, kind).is_empty() {
            out = remove_rule(&out, kind, 0)?;
        }
    }
    while !spawn_at_startup(&out).is_empty() {
        out = remove_startup(&out, 0)?;
    }

    for setting in &model.settings {
        let edit = if setting.flag {
            Edit::flag(setting.path.clone(), setting.value != "false")
        } else if setting.raw {
            Edit::raw(setting.path.clone(), setting.value.clone())
        } else {
            Edit::new(setting.path.clone(), setting.value.clone())
        };
        out = patch(&out, &edit)?;
    }

    for bind in &model.binds {
        out = set_bind(&out, bind)?;
    }
    for rule in &model.window_rules {
        out = add_rule(&out, rule)?;
    }
    for rule in &model.layer_rules {
        out = add_rule(&out, rule)?;
    }
    for command in &model.spawn_at_startup {
        out = crate::nodes::add_startup(&out, command)?;
    }
    for (name, value) in &model.environment {
        let path = format!("environment.{}", quote_if_needed(name));
        let edit = match value {
            Some(value) => Edit::new(path, value.clone()),
            None => Edit::raw(path, "null"),
        };
        out = patch(&out, &edit)?;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"layout {
    gaps 16
}

output "eDP-1" {
    scale 1.5
}

binds {
    Mod+Left { focus-column-left; }
}

window-rule {
    match app-id="Alacritty"
    open-floating true
}

spawn-at-startup "waybar"
environment {
    QT_QPA_PLATFORM "wayland"
}
"#;

    #[test]
    fn export_captures_settings_and_lists() {
        let model = export_model(CONFIG);
        assert!(
            model
                .settings
                .iter()
                .any(|setting| { setting.path == "layout.gaps" && setting.value == "16" })
        );
        assert!(
            model.settings.iter().any(|setting| {
                setting.path == "output \"eDP-1\".scale" && setting.value == "1.5"
            })
        );
        assert_eq!(model.binds.len(), 1);
        assert_eq!(model.window_rules.len(), 1);
        assert_eq!(model.spawn_at_startup, vec![vec!["waybar".to_owned()]]);
        assert_eq!(
            model.environment.get("QT_QPA_PLATFORM"),
            Some(&Some("wayland".to_owned()))
        );
    }

    #[test]
    fn import_recreates_values_and_replaces_lists() {
        let model = export_model(CONFIG);
        let rebuilt = import_model("", &model).unwrap();
        let second = export_model(&rebuilt);
        assert_eq!(second.settings, model.settings);
        assert_eq!(second.binds, model.binds);
        assert_eq!(second.window_rules, model.window_rules);
        assert_eq!(second.spawn_at_startup, model.spawn_at_startup);
        assert_eq!(second.environment, model.environment);
    }

    #[test]
    fn import_replaces_existing_lists() {
        let model = Model {
            binds: vec![Bind::new("Mod+Return", vec!["spawn \"foot\"".into()])],
            window_rules: vec![RuleSpec::window(
                vec!["app-id=\"foot\"".into()],
                vec!["open-floating true".into()],
            )],
            ..Model::default()
        };
        let imported = import_model(CONFIG, &model).unwrap();
        let exported = export_model(&imported);
        assert_eq!(exported.binds.len(), 1);
        assert_eq!(exported.binds[0].key, "Mod+Return");
        assert_eq!(exported.window_rules.len(), 1);
        assert_eq!(exported.spawn_at_startup, Vec::<Vec<String>>::new());
    }

    #[test]
    fn json_round_trips() {
        let model = export_model(CONFIG);
        let json = serde_json::to_string_pretty(&model).unwrap();
        let parsed: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, model);
        assert!(json.contains("\"window_rules\""));
        assert!(json.contains("\"spawn_at_startup\""));
    }
}
