//! User configuration file and hot reload.
//!
//! `$XDG_CONFIG_HOME/better-companion/config.json` (or `--config PATH`) is the
//! optional declarative layer over the mutable state files. Every section is
//! optional and additive:
//!
//! ```json
//! {
//!   "rules": {
//!     "browser": {
//!       "match": { "app_id": "firefox" },
//!       "steps": [{ "namespace": "parker", "action": "park" }]
//!     }
//!   },
//!   "hooks": { "workspace-activated": ["notify-send switched"] },
//!   "automation": {
//!     "focus": [{ "namespace": "system", "action": "ping" }]
//!   },
//!   "adapters": { "audio": { "enabled": false } }
//! }
//! ```
//!
//! The file is only ever read, never written. Runtime actions work exactly as
//! before when it is missing. `config.validate` reports every error without
//! applying anything; `config.reload` validates first and only then merges the
//! listed entries into the live contexts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use companion_adapters::ADAPTER_NAMESPACES;
use companion_core::{ActionInfo, Context, ParamInfo, Step};
use companion_rules::{Rule, WORKSPACE_EVENTS, validate_rule};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One config file. All sections are optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub rules: BTreeMap<String, Rule>,
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub automation: BTreeMap<String, Vec<Step>>,
    #[serde(default)]
    pub adapters: BTreeMap<String, Value>,
}

impl ConfigFile {
    /// Validates the file without touching any live context.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, rule) in &self.rules {
            if name.trim().is_empty() {
                errors.push("rule name must not be empty".to_owned());
            }
            if rule.steps.is_empty() {
                errors.push(format!("rule `{name}` needs at least one step"));
            }
            for step in &rule.steps {
                if step.namespace.trim().is_empty() || step.action.trim().is_empty() {
                    errors.push(format!("rule `{name}` has a step without namespace/action"));
                }
            }
            if let Err(error) = validate_rule(rule) {
                errors.push(format!("rule `{name}`: {error}"));
            }
        }
        for (event, commands) in &self.hooks {
            if event.trim().is_empty() {
                errors.push("hook event must not be empty".to_owned());
            }
            if commands.iter().any(|command| command.trim().is_empty()) {
                errors.push(format!("hook `{event}` contains an empty command"));
            }
        }
        for (name, steps) in &self.automation {
            if name.trim().is_empty() {
                errors.push("automation name must not be empty".to_owned());
            }
            if steps.is_empty() {
                errors.push(format!(
                    "automation sequence `{name}` needs at least one step"
                ));
            }
            for step in steps {
                if step.namespace.trim().is_empty() || step.action.trim().is_empty() {
                    errors.push(format!(
                        "automation sequence `{name}` has a step without namespace/action"
                    ));
                }
            }
        }
        for namespace in self.adapters.keys() {
            if !ADAPTER_NAMESPACES.contains(&namespace.as_str()) {
                errors.push(format!(
                    "unknown adapter `{namespace}`; expected one of {}",
                    ADAPTER_NAMESPACES.join(", ")
                ));
            }
        }
        errors
    }

    /// Number of entries in each section.
    pub fn counts(&self) -> Value {
        json!({
            "rules": self.rules.len(),
            "hooks": self.hooks.len(),
            "automation": self.automation.len(),
            "adapters": self.adapters.len(),
        })
    }
}

/// The live contexts a config file can update.
pub trait ConfigTarget: Send + Sync {
    fn validate_adapter(&self, namespace: &str, patch: &Value) -> Result<(), String>;
    fn apply_rules(&self, rules: &BTreeMap<String, Rule>) -> Result<usize, String>;
    fn apply_hooks(&self, hooks: &BTreeMap<String, Vec<String>>) -> Result<usize, String>;
    fn apply_automation(&self, sequences: &BTreeMap<String, Vec<Step>>) -> Result<usize, String>;
    fn apply_adapter(&self, namespace: &str, patch: &Value) -> Result<(), String>;
}

impl<T: ConfigTarget + ?Sized> ConfigTarget for Arc<T> {
    fn validate_adapter(&self, namespace: &str, patch: &Value) -> Result<(), String> {
        (**self).validate_adapter(namespace, patch)
    }

    fn apply_rules(&self, rules: &BTreeMap<String, Rule>) -> Result<usize, String> {
        (**self).apply_rules(rules)
    }

    fn apply_hooks(&self, hooks: &BTreeMap<String, Vec<String>>) -> Result<usize, String> {
        (**self).apply_hooks(hooks)
    }

    fn apply_automation(&self, sequences: &BTreeMap<String, Vec<Step>>) -> Result<usize, String> {
        (**self).apply_automation(sequences)
    }

    fn apply_adapter(&self, namespace: &str, patch: &Value) -> Result<(), String> {
        (**self).apply_adapter(namespace, patch)
    }
}

/// The `config.*` context.
pub struct Config {
    path: PathBuf,
    target: Box<dyn ConfigTarget>,
}

/// Result of a load/validate/apply pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReloadReport {
    pub path: String,
    pub exists: bool,
    pub reloaded: bool,
    pub rules: usize,
    pub hooks: usize,
    pub automation: usize,
    pub adapters: usize,
}

impl Config {
    pub fn new(path: impl Into<PathBuf>, target: Box<dyn ConfigTarget>) -> Self {
        Self {
            path: path.into(),
            target,
        }
    }

    /// Reads and parses the file. Missing is `Ok(None)`.
    pub fn read(path: &Path) -> Result<Option<ConfigFile>, String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let config: ConfigFile = serde_json::from_slice(&bytes)
                    .map_err(|err| format!("invalid config {}: {err}", path.display()))?;
                Ok(Some(config))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("cannot read {}: {err}", path.display())),
        }
    }

    /// Validates a parsed file against the live contexts.
    pub fn validate_with_target(&self, config: &ConfigFile) -> Vec<String> {
        let mut errors = config.validate();
        for (namespace, patch) in &config.adapters {
            if ADAPTER_NAMESPACES.contains(&namespace.as_str())
                && let Err(error) = self.target.validate_adapter(namespace, patch)
            {
                errors.push(format!("adapter `{namespace}`: {error}"));
            }
        }
        errors
    }

    /// Reads, validates and applies the file. Missing files are a no-op.
    pub fn apply(&self) -> Result<ReloadReport, String> {
        let Some(config) = Self::read(&self.path)? else {
            return Ok(ReloadReport {
                path: self.path.display().to_string(),
                exists: false,
                reloaded: false,
                rules: 0,
                hooks: 0,
                automation: 0,
                adapters: 0,
            });
        };
        let errors = self.validate_with_target(&config);
        if !errors.is_empty() {
            return Err(format!(
                "config {} is invalid:\n- {}",
                self.path.display(),
                errors.join("\n- ")
            ));
        }
        let rules = self.target.apply_rules(&config.rules)?;
        let hooks = self.target.apply_hooks(&config.hooks)?;
        let automation = self.target.apply_automation(&config.automation)?;
        for (namespace, patch) in &config.adapters {
            self.target.apply_adapter(namespace, patch)?;
        }
        Ok(ReloadReport {
            path: self.path.display().to_string(),
            exists: true,
            reloaded: true,
            rules,
            hooks,
            automation,
            adapters: config.adapters.len(),
        })
    }

    fn show(&self) -> Value {
        match Self::read(&self.path) {
            Ok(Some(config)) => json!({
                "path": self.path.display().to_string(),
                "exists": true,
                "config": config,
                "sections": config.counts(),
            }),
            Ok(None) => json!({
                "path": self.path.display().to_string(),
                "exists": false,
                "config": Value::Null,
                "sections": {"rules": 0, "hooks": 0, "automation": 0, "adapters": 0},
            }),
            Err(error) => json!({
                "path": self.path.display().to_string(),
                "exists": true,
                "config": Value::Null,
                "error": error,
            }),
        }
    }

    fn validate(&self, params: &Value) -> Value {
        let supplied = params.get("config").filter(|value| !value.is_null());
        let (config, exists) = match supplied {
            Some(value) => match serde_json::from_value::<ConfigFile>(value.clone()) {
                Ok(config) => (Some(config), true),
                Err(error) => {
                    return json!({
                        "path": self.path.display().to_string(),
                        "exists": true,
                        "valid": false,
                        "errors": [format!("invalid config object: {error}")],
                    });
                }
            },
            None => match Self::read(&self.path) {
                Ok(config) => (config, self.path.exists()),
                Err(error) => {
                    return json!({
                        "path": self.path.display().to_string(),
                        "exists": true,
                        "valid": false,
                        "errors": [error],
                    });
                }
            },
        };
        let Some(config) = config else {
            return json!({
                "path": self.path.display().to_string(),
                "exists": false,
                "valid": true,
                "errors": [],
                "sections": {"rules": 0, "hooks": 0, "automation": 0, "adapters": 0},
            });
        };
        let errors = self.validate_with_target(&config);
        json!({
            "path": self.path.display().to_string(),
            "exists": exists,
            "valid": errors.is_empty(),
            "errors": errors,
            "sections": config.counts(),
        })
    }
}

impl Context for Config {
    fn namespace(&self) -> &'static str {
        "config"
    }

    fn actions(&self) -> Vec<ActionInfo> {
        vec![
            ActionInfo::new(
                "show",
                "Show the config path, file contents and section counts",
            ),
            ActionInfo::new(
                "validate",
                "Validate the file, or a supplied `config` object, without applying it",
            )
            .with_params([ParamInfo::object(
                "config",
                false,
                "config object to validate instead of the file",
            )]),
            ActionInfo::new(
                "reload",
                "Re-read, validate and apply rules, hooks, automation and adapter defaults",
            ),
        ]
    }

    fn call(&self, action: &str, params: &Value) -> Result<Value, String> {
        match action {
            "show" => Ok(self.show()),
            "validate" => Ok(self.validate(params)),
            "reload" => serde_json::to_value(self.apply()?)
                .map_err(|err| format!("cannot encode reload report: {err}")),
            other => Err(format!("unknown action `config.{other}`")),
        }
    }
}

/// Default config path helper.
pub fn default_path() -> PathBuf {
    companion_core::config_path()
}

/// Known workspace triggers (re-exported for documentation/tests).
pub const WORKSPACE_RULE_EVENTS: &[&str] = WORKSPACE_EVENTS;

/// Applies a config file at daemon startup when it exists.
pub fn apply_startup(config: &Config) -> Result<ReloadReport, String> {
    config.apply()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use companion_adapters::Adapters;
    use companion_automation::Automation;
    use companion_hooks::Hooks;
    use companion_parking::Parking;
    use companion_rules::Rules;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-config-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[derive(Default)]
    struct Recorder {
        rules: Mutex<usize>,
        hooks: Mutex<usize>,
        automation: Mutex<usize>,
        adapters: Mutex<usize>,
    }

    impl ConfigTarget for Recorder {
        fn validate_adapter(&self, namespace: &str, patch: &Value) -> Result<(), String> {
            if namespace == "session" && patch.get("nope").is_some() {
                return Err("unknown field `nope`".to_owned());
            }
            Ok(())
        }

        fn apply_rules(&self, rules: &BTreeMap<String, Rule>) -> Result<usize, String> {
            *self.rules.lock().unwrap() = rules.len();
            Ok(rules.len())
        }

        fn apply_hooks(&self, hooks: &BTreeMap<String, Vec<String>>) -> Result<usize, String> {
            *self.hooks.lock().unwrap() = hooks.len();
            Ok(hooks.len())
        }

        fn apply_automation(
            &self,
            sequences: &BTreeMap<String, Vec<Step>>,
        ) -> Result<usize, String> {
            *self.automation.lock().unwrap() = sequences.len();
            Ok(sequences.len())
        }

        fn apply_adapter(&self, _namespace: &str, _patch: &Value) -> Result<(), String> {
            *self.adapters.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn rule() -> Value {
        json!({
            "match": { "app_id": "firefox" },
            "steps": [{ "namespace": "system", "action": "ping" }]
        })
    }

    #[test]
    fn missing_config_is_valid_and_actions_still_work() {
        let dir = temp_dir("missing");
        let target = Arc::new(Recorder::default());
        let config = Config::new(dir.join("config.json"), Box::new(target.clone()));
        assert!(config.call("show", &Value::Null).unwrap()["exists"] == false);
        assert!(config.call("validate", &Value::Null).unwrap()["valid"] == true);
        let report = config.call("reload", &Value::Null).unwrap();
        assert_eq!(report["reloaded"], false);
        assert_eq!(*target.rules.lock().unwrap(), 0);
    }

    #[test]
    fn validate_reports_errors_without_applying() {
        let dir = temp_dir("validate");
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "rules": { "bad": { "match": { "title_regex": "[" }, "steps": [{ "namespace": "system", "action": "ping" }] } },
                "unknown": {}
            }))
            .unwrap(),
        )
        .unwrap();
        let target = Arc::new(Recorder::default());
        let config = Config::new(&path, Box::new(target.clone()));
        let result = config.call("validate", &Value::Null).unwrap();
        assert_eq!(result["valid"], false);
        assert!(!result["errors"].as_array().unwrap().is_empty());
        assert_eq!(*target.rules.lock().unwrap(), 0);
    }

    #[test]
    fn valid_file_applies_every_section_and_is_never_written() {
        let dir = temp_dir("apply");
        let path = dir.join("config.json");
        let body = serde_json::to_vec_pretty(&json!({
            "rules": { "browser": rule() },
            "hooks": { "workspace-activated": ["notify-send hi"] },
            "automation": { "focus": [{ "namespace": "system", "action": "ping" }] },
            "adapters": { "session": { "enabled": false } }
        }))
        .unwrap();
        std::fs::write(&path, &body).unwrap();
        let before = std::fs::read(&path).unwrap();
        let target = Arc::new(Recorder::default());
        let config = Config::new(&path, Box::new(target.clone()));
        let report = config.call("reload", &Value::Null).unwrap();
        assert_eq!(report["reloaded"], true);
        assert_eq!(report["rules"], 1);
        assert_eq!(report["hooks"], 1);
        assert_eq!(report["automation"], 1);
        assert_eq!(report["adapters"], 1);
        assert_eq!(*target.rules.lock().unwrap(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn adapter_defaults_are_validated_before_anything_is_applied() {
        let dir = temp_dir("adapter-error");
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "rules": { "browser": rule() },
                "adapters": { "session": { "nope": true } }
            }))
            .unwrap(),
        )
        .unwrap();
        let target = Arc::new(Recorder::default());
        let config = Config::new(&path, Box::new(target.clone()));
        let error = config.call("reload", &Value::Null).unwrap_err();
        assert!(error.contains("nope"), "{error}");
        assert_eq!(*target.rules.lock().unwrap(), 0);
    }

    #[test]
    fn real_targets_apply_rules_hooks_automation_and_adapter_state() {
        let dir = temp_dir("real-targets");
        let rules = Arc::new(
            Rules::load(
                dir.join("rules.json"),
                Parking::load(dir.join("parking.json")).unwrap(),
            )
            .unwrap(),
        );
        let hooks = Arc::new(Hooks::load(dir.join("hooks.json")).unwrap());
        let automation = Arc::new(Automation::load(dir.join("automation.json")).unwrap());
        let adapters = Adapters::load(&dir).unwrap();

        struct RealTarget {
            rules: Arc<Rules>,
            hooks: Arc<Hooks>,
            automation: Arc<Automation>,
            adapters: Adapters,
        }
        impl ConfigTarget for RealTarget {
            fn validate_adapter(&self, namespace: &str, patch: &Value) -> Result<(), String> {
                self.adapters.validate_config(namespace, patch)
            }
            fn apply_rules(&self, rules: &BTreeMap<String, Rule>) -> Result<usize, String> {
                self.rules.apply_config(rules)
            }
            fn apply_hooks(&self, hooks: &BTreeMap<String, Vec<String>>) -> Result<usize, String> {
                self.hooks.apply_config(hooks)
            }
            fn apply_automation(
                &self,
                sequences: &BTreeMap<String, Vec<Step>>,
            ) -> Result<usize, String> {
                self.automation.apply_config(sequences)
            }
            fn apply_adapter(&self, namespace: &str, patch: &Value) -> Result<(), String> {
                self.adapters.apply_config(namespace, patch)
            }
        }

        let path = dir.join("config.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "rules": { "browser": rule() },
                "hooks": { "workspace-activated": ["notify-send hi"] },
                "automation": { "focus": [{ "namespace": "system", "action": "ping" }] },
                "adapters": { "audio": { "enabled": true, "dedup_ms": 0 } }
            }))
            .unwrap(),
        )
        .unwrap();
        let config = Config::new(
            &path,
            Box::new(RealTarget {
                rules: rules.clone(),
                hooks: hooks.clone(),
                automation: automation.clone(),
                adapters: adapters.clone(),
            }),
        );
        config.apply().unwrap();
        assert!(rules.call("list", &Value::Null).unwrap()["rules"]["browser"].is_object());
        assert_eq!(
            hooks.call("list", &Value::Null).unwrap()["hooks"]["workspace-activated"][0],
            "notify-send hi"
        );
        assert_eq!(
            automation.call("list", &Value::Null).unwrap()["names"][0],
            "focus"
        );
        assert_eq!(
            adapters.context().call("status", &Value::Null).unwrap()["adapters"]["audio"]["enabled"],
            true
        );
    }
}
