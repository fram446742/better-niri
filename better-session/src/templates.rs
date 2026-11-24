//! Built-in context templates.
//!
//! Templates are shapes, not app prescriptions: every app id and launch
//! command is a placeholder (`{{code_app}}`, `{{code_cmd}}`, ...) that the user
//! fills in with `template apply --set key=value`. Nothing here guesses that a
//! particular desktop uses Slack, Firefox or VS Code.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::{Context, now_secs};

/// A built-in template.
#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    /// Placeholder names without the `{{ }}` delimiters.
    pub placeholders: &'static [&'static str],
    blueprint: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "development",
        description: "Three workspaces: code, web and terminal, one placeholder app each.",
        placeholders: &[
            "code_app",
            "code_cmd",
            "browser_app",
            "browser_cmd",
            "terminal_app",
            "terminal_cmd",
        ],
        blueprint: include_str!("templates/development.json"),
    },
    Template {
        name: "gaming",
        description: "Game workspace plus a chat workspace, one placeholder app each.",
        placeholders: &["game_app", "game_cmd", "chat_app", "chat_cmd"],
        blueprint: include_str!("templates/gaming.json"),
    },
    Template {
        name: "work",
        description: "Mail, meeting and documents workspaces, one placeholder app each.",
        placeholders: &[
            "mail_app",
            "mail_cmd",
            "meeting_app",
            "meeting_cmd",
            "docs_app",
            "docs_cmd",
        ],
        blueprint: include_str!("templates/work.json"),
    },
];

/// All built-in templates, in stable name order.
pub fn list() -> &'static [Template] {
    TEMPLATES
}

/// Finds a built-in template by name (case-insensitive).
pub fn get(name: &str) -> Option<&'static Template> {
    TEMPLATES
        .iter()
        .find(|template| template.name.eq_ignore_ascii_case(name))
}

/// Renders a template into a context.
///
/// All placeholders must be supplied. Unresolved placeholders are reported
/// together so one command can fix them all.
pub fn render(name: &str, values: &BTreeMap<String, String>) -> Result<Context, String> {
    let template = get(name).ok_or_else(|| format!("unknown template `{name}`"))?;
    let mut raw = template.blueprint.to_owned();
    for placeholder in template.placeholders {
        if let Some(value) = values.get(*placeholder) {
            raw = raw.replace(&format!("{{{{{placeholder}}}}}"), value);
        }
    }
    let unresolved = unresolved_placeholders(&raw);
    if !unresolved.is_empty() {
        let names: Vec<String> = unresolved.into_iter().collect();
        return Err(format!(
            "template `{}` needs values for: {} (use --set key=value)",
            template.name,
            names.join(", ")
        ));
    }

    #[derive(serde::Deserialize)]
    struct Blueprint {
        #[serde(default)]
        command_map: BTreeMap<String, String>,
        #[serde(default)]
        workspaces: Vec<crate::WorkspaceSnapshot>,
        #[serde(default)]
        windows: Vec<crate::WindowSnapshot>,
        #[serde(default)]
        hooks: crate::Hooks,
    }

    let blueprint: Blueprint = serde_json::from_str(&raw)
        .map_err(|err| format!("invalid built-in template `{}`: {err}", template.name))?;
    let timestamp = now_secs();
    let mut context = Context {
        created_at: timestamp,
        updated_at: timestamp,
        command_map: blueprint.command_map,
        workspaces: blueprint.workspaces,
        windows: blueprint.windows,
        hooks: blueprint.hooks,
        template: Some(template.name.to_owned()),
        ..Context::default()
    };
    context.workspaces.sort_by_key(crate::workspace_sort_key);
    context.windows.sort_by_key(crate::window_sort_key);
    Ok(context)
}

/// Resolves `--set key=value` pairs for a template.
pub fn values_from_sets(
    template: &Template,
    sets: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    for placeholder in template.placeholders {
        let env_key = format!(
            "BETTER_SESSION_TEMPLATE_{}",
            placeholder.to_ascii_uppercase()
        );
        if let Some(value) = std::env::var_os(env_key) {
            values.insert(
                (*placeholder).to_owned(),
                value.to_string_lossy().into_owned(),
            );
        }
    }
    for set in sets {
        let (key, value) = set
            .split_once('=')
            .ok_or_else(|| format!("--set needs key=value, got `{set}`"))?;
        let key = key.trim();
        if !template.placeholders.contains(&key) {
            return Err(format!(
                "template `{}` has no placeholder `{key}` (known: {})",
                template.name,
                template.placeholders.join(", ")
            ));
        }
        values.insert(key.to_owned(), value.to_owned());
    }
    Ok(values)
}

/// Placeholders still present in a rendered blueprint.
pub fn unresolved_placeholders(raw: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = raw;
    while let Some(start) = rest.find("{{") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("}}") else {
            break;
        };
        let name = rest[..end].trim();
        if !name.is_empty() {
            found.insert(name.to_owned());
        }
        rest = &rest[end + 2..];
    }
    found
}

/// JSON description of every template, for `template list --json`.
pub fn describe_all() -> Value {
    Value::Array(
        TEMPLATES
            .iter()
            .map(|template| {
                json!({
                    "name": template.name,
                    "description": template.description,
                    "placeholders": template.placeholders,
                })
            })
            .collect(),
    )
}

/// Raw blueprint for `template show`.
pub fn raw(name: &str) -> Option<Value> {
    let template = get(name)?;
    serde_json::from_str(template.blueprint).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn every_template_is_listed_and_has_a_description() {
        assert_eq!(list().len(), 3);
        for template in list() {
            assert!(get(template.name).is_some());
            assert!(!template.description.is_empty());
            assert!(!template.placeholders.is_empty());
        }
    }

    #[test]
    fn development_template_renders_placeholders() {
        let context = render(
            "development",
            &values(&[
                ("code_app", "my-editor"),
                ("code_cmd", "my-editor"),
                ("browser_app", "my-browser"),
                ("browser_cmd", "my-browser"),
                ("terminal_app", "my-terminal"),
                ("terminal_cmd", "my-terminal"),
            ]),
        )
        .unwrap();
        assert_eq!(context.template.as_deref(), Some("development"));
        assert_eq!(context.workspaces.len(), 3);
        assert_eq!(context.windows.len(), 3);
        assert_eq!(context.command_map["my-editor"], "my-editor");
        assert!(
            context
                .windows
                .iter()
                .all(|window| !window.app_id.contains("{{"))
        );
    }

    #[test]
    fn missing_placeholders_are_reported_together() {
        let error = render("work", &values(&[("mail_app", "mail")])).unwrap_err();
        assert!(error.contains("mail_cmd"), "{error}");
        assert!(error.contains("docs_app"), "{error}");
    }

    #[test]
    fn sets_validate_placeholder_names() {
        let template = get("gaming").unwrap();
        let error = values_from_sets(template, &["nope=value".to_owned()]).unwrap_err();
        assert!(error.contains("no placeholder `nope`"));
        let parsed = values_from_sets(template, &["game_app=game".to_owned()]).unwrap();
        assert_eq!(parsed["game_app"], "game");
    }

    #[test]
    fn unknown_templates_are_rejected() {
        assert!(render("nope", &BTreeMap::new()).is_err());
        assert!(raw("nope").is_none());
    }
}
