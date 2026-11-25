//! The `better-settingsctl` command implementation, kept in the library so it
//! can be tested without a process or a display.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::catalog::{Kind, catalog_for, find, normalize};
use crate::model::{MODEL_VERSION, Model, export_model, import_model};
use crate::nodes::{
    Bind, RuleKind, RuleSpec, add_preset_column_width, add_rule, add_startup, binds, environment,
    remove_bind, remove_preset_column_width, remove_rule, remove_startup, rules, set_bind,
    spawn_at_startup, validate,
};
use crate::{Edit, config_path, current, patch, remove_key, save, unified_diff};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOutput {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    fn ok(stdout: String) -> Self {
        Self {
            code: 0,
            stdout,
            stderr: String::new(),
        }
    }

    fn with_code(code: u8, stdout: String) -> Self {
        Self {
            code,
            stdout,
            stderr: String::new(),
        }
    }

    fn error(message: impl std::fmt::Display) -> Self {
        Self {
            code: 1,
            stdout: String::new(),
            stderr: format!("better-settingsctl: {message}\n"),
        }
    }
}

fn usage() -> &'static str {
    "better-settingsctl [--config PATH] [--dry-run] [--raw] <command>\n\n\
     Commands:\n\
       list                         List settings, binds, rules and startup commands\n\
       get <path>                   Print the current value of one setting\n\
       set <path> <value>           Patch one setting in place\n\
       unset <path>                 Remove one setting\n\
       diff                         Print the config with catalog values re-applied\n\
       add-bind <key> --action ACTION [--action ACTION]...\n\
                                    Add or replace a bind; supports --repeat=false,\n\
                                    --cooldown-ms N, --allow-when-locked[=BOOL],\n\
                                    --allow-inhibiting[=BOOL], --hotkey-overlay-title TITLE\n\
       remove-bind <key>            Remove a bind\n\
       add-window-rule --match MATCH [--exclude MATCH]... [--property LINE]...\n\
                                    Append a window rule\n\
       remove-window-rule <index>   Remove the zero-based window rule\n\
       add-layer-rule --match MATCH [--exclude MATCH]... [--property LINE]...\n\
                                    Append a layer rule\n\
       remove-layer-rule <index>    Remove the zero-based layer rule\n\
       add-preset-width <size>      Append `proportion 0.5` / `fixed 640`\n\
       remove-preset-width <index>  Remove a preset column width\n\
       validate                     Structural and catalog round-trip check\n\
       export                       Print the JSON model\n\
       import <file|->              Apply a JSON model (`-` reads stdin)\n\n\
     Companion app integrations (read-only unless --apply is given):\n\
       integrations                 Which companion CLIs are installed and their versions\n\
       overview                     Aggregate compositor, tool, portal and saved-state JSON\n\
       compositor status|reload [--apply]\n\
                                    niri version/capabilities and config reload\n\
       displays list|show|preview   better-monitor profiles and dry-run plans\n\
       displays apply|restore [--apply]\n\
                                    Preview, then apply or restore with --apply\n\
       displays confirm|cancel --apply\n\
                                    Resolve a pending safe apply\n\
       sessions list|show|explain|activate [--apply]\n\
                                    better-session contexts and restores\n\
       sessions history|capture     Recorded snapshots; capture needs --apply\n\
       wallpaper status|scenes|use|apply [--apply]\n\
                                    better-wallpaper scenes and applies\n\
       audio status|sinks|rules|default|apply|restore [--apply]\n\
                                    better-audio state, routing and restore\n\
       sidepanels status|validate|layouts|check|path|open [--config PATH]\n\
                                    Read, validate, preview or open the JSON config\n\
       companion describe|status|list|marks|history|explain\n\
                                    companionctl registry, status, marks and matches\n\
       companion rules|idle|rotate [--apply]\n\
                                    Run rules or change an automation policy\n\
       companion run <ns> <action> [--apply]\n\
                                    Dry-run an action; --apply calls it\n\
       portal status                better-portal unit and portals.conf hint\n\
       noctalia status              Noctalia plugin source status\n\n\
     Mutating commands print a preview and refuse to run without --apply.\n\
     Every command accepts --dry-run; mutations also include a unified diff in\n\
     their JSON output. Config defaults to $BETTER_SETTINGS_CONFIG, then\n\
     $NIRI_CONFIG, then $XDG_CONFIG_HOME/niri/config.kdl, then\n\
     ~/.config/niri/config.kdl. Writes keep the previous file as <name>.bak-<epoch>."
}

struct Globals {
    config: PathBuf,
    dry_run: bool,
    raw: bool,
    rest: Vec<String>,
}

fn parse_globals(args: Vec<String>) -> Result<Globals, String> {
    let mut config = std::env::var_os("BETTER_SETTINGS_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("NIRI_CONFIG").map(PathBuf::from))
        .unwrap_or_else(config_path);
    let mut dry_run = false;
    let mut raw = false;
    let mut rest = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => {
                config = PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--config needs a path".to_owned())?,
                );
            }
            "--dry-run" => dry_run = true,
            "--raw" => raw = true,
            "--help" | "-h" => {
                rest.push("--help".to_owned());
            }
            _ => rest.push(arg),
        }
    }
    Ok(Globals {
        config,
        dry_run,
        raw,
        rest,
    })
}

fn read_config(path: &PathBuf) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Float { .. } => "float",
        Kind::Int { .. } => "int",
        Kind::Bool => "bool",
        Kind::Flag => "flag",
        Kind::Text => "text",
        Kind::Choice(_) => "choice",
        Kind::Raw => "raw",
    }
}

fn mutation(
    config: &PathBuf,
    before: &str,
    after: &str,
    dry_run: bool,
    path: &str,
) -> Result<String, String> {
    if before == after {
        return Ok(serde_json::to_string_pretty(&json!({
            "changed": false,
            "path": path,
        }))
        .unwrap());
    }
    let backup = save(config, after, dry_run)
        .map_err(|err| format!("cannot write {}: {err}", config.display()))?;
    let label = config
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.kdl".to_owned());
    let diff = unified_diff(before, after, &format!("a/{label}"), &format!("b/{label}"));
    Ok(serde_json::to_string_pretty(&json!({
        "changed": true,
        "dry_run": dry_run,
        "path": path,
        "config": config,
        "backup": backup,
        "text": after,
        "diff": diff,
    }))
    .unwrap())
}

fn require<'a>(args: &'a [String], index: usize, message: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| message.to_owned())
}

fn take_value(args: &[String], index: &mut usize, name: &str) -> Result<String, String> {
    let arg = args
        .get(*index)
        .ok_or_else(|| format!("{name} needs a value"))?;
    if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
        *index += 1;
        return Ok(value.to_owned());
    }
    let value = args
        .get(*index + 1)
        .ok_or_else(|| format!("{name} needs a value"))?
        .clone();
    *index += 2;
    Ok(value)
}

fn parse_bool_option(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(format!("{name} expects a boolean, got `{value}`")),
    }
}

fn parse_add_bind(args: &[String]) -> Result<Bind, String> {
    let key = require(args, 1, "add-bind needs a key")?.to_owned();
    let mut actions = Vec::new();
    let mut bind = Bind::new(key, Vec::new());
    let mut index = 2;
    while index < args.len() {
        let arg = args[index].clone();
        if let Some(value) = arg.strip_prefix("--action=") {
            actions.push(value.to_owned());
            index += 1;
        } else if arg == "--action" {
            actions.push(require(args, index + 1, "--action needs a value")?.to_owned());
            index += 2;
        } else if let Some(value) = arg.strip_prefix("--repeat=") {
            bind.repeat = Some(parse_bool_option("--repeat", value)?);
            index += 1;
        } else if arg == "--repeat" {
            if let Some(next) = args.get(index + 1)
                && !next.starts_with("--")
            {
                bind.repeat = Some(parse_bool_option("--repeat", next)?);
                index += 2;
                continue;
            }
            bind.repeat = Some(true);
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--cooldown-ms=") {
            bind.cooldown_ms = Some(
                value
                    .parse()
                    .map_err(|_| format!("--cooldown-ms expects an integer, got `{value}`"))?,
            );
            index += 1;
        } else if arg == "--cooldown-ms" {
            let mut cursor = index;
            let value = take_value(args, &mut cursor, "--cooldown-ms")?;
            bind.cooldown_ms = Some(
                value
                    .parse()
                    .map_err(|_| format!("--cooldown-ms expects an integer, got `{value}`"))?,
            );
            index = cursor;
        } else if let Some(value) = arg.strip_prefix("--allow-when-locked=") {
            bind.allow_when_locked = Some(parse_bool_option("--allow-when-locked", value)?);
            index += 1;
        } else if arg == "--allow-when-locked" {
            bind.allow_when_locked = Some(true);
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--allow-inhibiting=") {
            bind.allow_inhibiting = Some(parse_bool_option("--allow-inhibiting", value)?);
            index += 1;
        } else if arg == "--allow-inhibiting" {
            bind.allow_inhibiting = Some(true);
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--hotkey-overlay-title=") {
            bind.hotkey_overlay_title = Some(value.to_owned());
            index += 1;
        } else if arg == "--hotkey-overlay-title" {
            let mut cursor = index;
            bind.hotkey_overlay_title =
                Some(take_value(args, &mut cursor, "--hotkey-overlay-title")?);
            index = cursor;
        } else if arg.starts_with("--") {
            return Err(format!("unknown add-bind option `{arg}`"));
        } else {
            actions.push(arg);
            index += 1;
        }
    }
    bind.actions = actions;
    Ok(bind)
}

fn parse_rule(args: &[String], kind: RuleKind) -> Result<RuleSpec, String> {
    let mut matches = Vec::new();
    let mut properties = Vec::new();
    let mut index = 1;
    while index < args.len() {
        let arg = args[index].clone();
        if let Some(value) = arg.strip_prefix("--match=") {
            matches.push(value.to_owned());
            index += 1;
        } else if arg == "--match" {
            matches.push(require(args, index + 1, "--match needs a value")?.to_owned());
            index += 2;
        } else if let Some(value) = arg.strip_prefix("--exclude=") {
            matches.push(format!("exclude {value}"));
            index += 1;
        } else if arg == "--exclude" {
            let value = require(args, index + 1, "--exclude needs a value")?;
            matches.push(format!("exclude {value}"));
            index += 2;
        } else if let Some(value) = arg.strip_prefix("--property=") {
            properties.push(value.to_owned());
            index += 1;
        } else if arg == "--property" {
            properties.push(require(args, index + 1, "--property needs a value")?.to_owned());
            index += 2;
        } else {
            return Err(format!("unknown rule option `{arg}`"));
        }
    }
    if matches.is_empty() && properties.is_empty() {
        return Err("rule needs at least one --match or --property".to_owned());
    }
    Ok(match kind {
        RuleKind::Window => RuleSpec::window(matches, properties),
        RuleKind::Layer => RuleSpec::layer(matches, properties),
    })
}

fn list_command(text: &str) -> Result<String, String> {
    let mut settings = Vec::new();
    for spec in catalog_for(text) {
        let value = current(text, &spec.path).map_err(|err| err.to_string())?;
        settings.push(json!({
            "path": spec.path,
            "label": spec.label,
            "section": spec.section,
            "kind": kind_name(spec.kind),
            "value": value,
        }));
    }
    let rules_json = |kind: RuleKind| -> Value {
        rules(text, kind)
            .into_iter()
            .enumerate()
            .map(|(index, rule)| {
                json!({
                    "index": index,
                    "summary": rule.summary(),
                    "lines": rule.lines,
                })
            })
            .collect()
    };
    let env: Vec<Value> = environment(text)
        .into_iter()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
    serde_json::to_string_pretty(&json!({
        "settings": settings,
        "binds": binds(text),
        "window_rules": rules_json(RuleKind::Window),
        "layer_rules": rules_json(RuleKind::Layer),
        "spawn_at_startup": spawn_at_startup(text),
        "environment": env,
    }))
    .map_err(|err| err.to_string())
}

fn set_command(
    globals: &Globals,
    text: &str,
    path: &str,
    raw_value: &str,
) -> Result<String, String> {
    let spec = find(text, path);
    let edit = match spec {
        Some(spec) if spec.kind == Kind::Flag => {
            let value = normalize(Kind::Flag, raw_value)?;
            Edit::flag(path.to_owned(), value == "true")
        }
        Some(spec) if spec.kind == Kind::Raw => Edit::raw(path.to_owned(), raw_value.trim()),
        Some(spec) => Edit::new(path.to_owned(), normalize(spec.kind, raw_value)?),
        None if globals.raw => Edit::raw(path.to_owned(), raw_value.trim()),
        None => Edit::new(path.to_owned(), raw_value),
    };
    let patched = patch(text, &edit).map_err(|err| err.to_string())?;
    mutation(&globals.config, text, &patched, globals.dry_run, path)
}

fn command(globals: &Globals, text: &str, args: &[String]) -> Result<(String, u8), String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage().to_owned());
    };
    match command {
        "--help" | "-h" => Ok((format!("{}\n", usage()), 0)),
        "list" => Ok((format!("{}\n", list_command(text)?), 0)),
        "get" => {
            let path = require(args, 1, "get needs a setting path")?;
            match current(text, path).map_err(|err| err.to_string())? {
                Some(value) => Ok((format!("{value}\n"), 0)),
                None => Err(format!(
                    "`{path}` is not set in {}",
                    globals.config.display()
                )),
            }
        }
        "set" => {
            let path = require(args, 1, "set needs a setting path")?;
            let value = require(args, 2, "set needs a value")?;
            Ok((format!("{}\n", set_command(globals, text, path, value)?), 0))
        }
        "unset" => {
            let path = require(args, 1, "unset needs a setting path")?;
            let patched = remove_key(text, path).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(&globals.config, text, &patched, globals.dry_run, path)?
                ),
                0,
            ))
        }
        "diff" => {
            let mut patched = text.to_owned();
            for spec in catalog_for(text) {
                let Ok(Some(value)) = current(text, &spec.path) else {
                    continue;
                };
                if spec.kind == Kind::Flag || spec.kind == Kind::Raw {
                    continue;
                }
                let edit = Edit::new(spec.path.clone(), value);
                patched = patch(&patched, &edit).map_err(|err| err.to_string())?;
            }
            Ok((patched, 0))
        }
        "add-bind" => {
            let bind = parse_add_bind(args)?;
            let patched = set_bind(text, &bind).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(&globals.config, text, &patched, globals.dry_run, &bind.key)?
                ),
                0,
            ))
        }
        "remove-bind" => {
            let key = require(args, 1, "remove-bind needs a key")?;
            let patched = remove_bind(text, key).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(&globals.config, text, &patched, globals.dry_run, key)?
                ),
                0,
            ))
        }
        "add-window-rule" | "add-layer-rule" => {
            let kind = if command == "add-window-rule" {
                RuleKind::Window
            } else {
                RuleKind::Layer
            };
            let rule = parse_rule(args, kind)?;
            let patched = add_rule(text, &rule).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(
                        &globals.config,
                        text,
                        &patched,
                        globals.dry_run,
                        kind.node_name(),
                    )?
                ),
                0,
            ))
        }
        "remove-window-rule" | "remove-layer-rule" => {
            let kind = if command == "remove-window-rule" {
                RuleKind::Window
            } else {
                RuleKind::Layer
            };
            let index: usize = require(args, 1, "remove rule needs an index")?
                .parse()
                .map_err(|_| "rule index must be a non-negative integer".to_owned())?;
            let patched = remove_rule(text, kind, index).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(
                        &globals.config,
                        text,
                        &patched,
                        globals.dry_run,
                        kind.node_name(),
                    )?
                ),
                0,
            ))
        }
        "add-preset-width" => {
            let value = require(args, 1, "add-preset-width needs a size")?;
            let patched = add_preset_column_width(text, value).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(
                        &globals.config,
                        text,
                        &patched,
                        globals.dry_run,
                        "layout.preset-column-widths",
                    )?
                ),
                0,
            ))
        }
        "remove-preset-width" => {
            let index: usize = require(args, 1, "remove-preset-width needs an index")?
                .parse()
                .map_err(|_| "index must be a non-negative integer".to_owned())?;
            let patched = remove_preset_column_width(text, index).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(
                        &globals.config,
                        text,
                        &patched,
                        globals.dry_run,
                        "layout.preset-column-widths",
                    )?
                ),
                0,
            ))
        }
        "validate" => {
            let report = validate(text);
            let output = serde_json::to_string_pretty(&json!({
                "ok": report.ok(),
                "blocks": report.blocks,
                "keys": report.keys,
                "errors": report.errors,
            }))
            .unwrap();
            let code = u8::from(!report.ok());
            Ok((format!("{output}\n"), code))
        }
        "export" => {
            let model = export_model(text);
            Ok((
                format!(
                    "{}\n",
                    serde_json::to_string_pretty(&model).map_err(|err| err.to_string())?
                ),
                0,
            ))
        }
        "import" => {
            let source = require(args, 1, "import needs a file or -")?;
            let json_text = if source == "-" {
                let mut buffer = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)
                    .map_err(|err| format!("cannot read stdin: {err}"))?;
                buffer
            } else {
                std::fs::read_to_string(source)
                    .map_err(|err| format!("cannot read {source}: {err}"))?
            };
            let model: Model = serde_json::from_str(&json_text)
                .map_err(|err| format!("invalid model JSON: {err}"))?;
            if model.version > MODEL_VERSION {
                return Err(format!(
                    "model version {} is newer than supported version {MODEL_VERSION}",
                    model.version
                ));
            }
            let patched = import_model(text, &model).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(&globals.config, text, &patched, globals.dry_run, "model")?
                ),
                0,
            ))
        }
        "add-startup" => {
            let command: Vec<String> = args[1..].to_vec();
            let patched = add_startup(text, &command).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(
                        &globals.config,
                        text,
                        &patched,
                        globals.dry_run,
                        "spawn-at-startup",
                    )?
                ),
                0,
            ))
        }
        "remove-startup" => {
            let index: usize = require(args, 1, "remove-startup needs an index")?
                .parse()
                .map_err(|_| "index must be a non-negative integer".to_owned())?;
            let patched = remove_startup(text, index).map_err(|err| err.to_string())?;
            Ok((
                format!(
                    "{}\n",
                    mutation(
                        &globals.config,
                        text,
                        &patched,
                        globals.dry_run,
                        "spawn-at-startup",
                    )?
                ),
                0,
            ))
        }
        other => Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
}

/// Runs one CLI invocation without touching process globals.
pub fn run(args: Vec<String>) -> CliOutput {
    // Companion integrations do not read niri's config, so dispatch them
    // before config resolution. Their own flags (for example sidepanels
    // `--config`) must not be consumed by the niri config parser.
    if let Some(section) = args.first()
        && crate::integrations::is_section(section)
    {
        return match crate::integrations::dispatch(&crate::integrations::SystemRunner, &args) {
            Ok(stdout) => CliOutput::ok(format!("{stdout}\n")),
            Err(err) => CliOutput::error(err),
        };
    }
    let globals = match parse_globals(args) {
        Ok(globals) => globals,
        Err(err) => return CliOutput::error(err),
    };
    let text = match read_config(&globals.config) {
        Ok(text) => text,
        Err(err) => return CliOutput::error(err),
    };
    match command(&globals, &text, &globals.rest) {
        Ok((stdout, code)) => {
            if code == 0 {
                CliOutput::ok(stdout)
            } else {
                CliOutput::with_code(code, stdout)
            }
        }
        Err(err) => CliOutput::error(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(name: &str, contents: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("better-settings-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.kdl");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn run_on(path: &std::path::Path, args: &[&str]) -> CliOutput {
        let mut all = vec!["--config".to_owned(), path.display().to_string()];
        all.extend(args.iter().map(|arg| arg.to_string()));
        run(all)
    }

    #[test]
    fn help_lists_every_command() {
        let output = run(vec!["--help".to_owned()]);
        assert_eq!(output.code, 0);
        for command in [
            "get",
            "set",
            "unset",
            "add-bind",
            "remove-bind",
            "add-window-rule",
            "remove-window-rule",
            "list",
            "validate",
            "export",
            "import",
            "integrations",
            "overview",
            "compositor",
            "displays",
            "sessions",
            "wallpaper",
            "audio",
            "sidepanels",
            "portal",
            "noctalia",
            "companion",
        ] {
            assert!(output.stdout.contains(command), "missing {command}");
        }
    }

    #[test]
    fn set_and_get_round_trip_and_dry_run_does_not_write() {
        let path = temp_config("set", "layout {\n    gaps 16\n}\n");
        let dry = run_on(&path, &["set", "layout.gaps", "24", "--dry-run"]);
        assert_eq!(dry.code, 0, "{}", dry.stderr);
        let parsed: Value = serde_json::from_str(&dry.stdout).unwrap();
        assert_eq!(parsed["dry_run"], true);
        assert!(parsed["diff"].as_str().unwrap().contains("+    gaps 24"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "layout {\n    gaps 16\n}\n"
        );

        let written = run_on(&path, &["set", "layout.gaps", "24"]);
        assert_eq!(written.code, 0, "{}", written.stderr);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "layout {\n    gaps 24\n}\n"
        );
        let got = run_on(&path, &["get", "layout.gaps"]);
        assert_eq!(got.stdout, "24\n");
    }

    #[test]
    fn unset_removes_a_flag() {
        let path = temp_config("unset", "layout {\n    gaps 16\n}\nprefer-no-csd\n");
        let output = run_on(&path, &["unset", "prefer-no-csd"]);
        assert_eq!(output.code, 0, "{}", output.stderr);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "layout {\n    gaps 16\n}\n"
        );
    }

    #[test]
    fn bind_commands_parse_flags_and_actions() {
        let path = temp_config("bind", "binds {\n}\n");
        let output = run_on(
            &path,
            &[
                "add-bind",
                "Mod+T",
                "--action",
                "spawn \"foot\"",
                "--action",
                "focus-column-right",
                "--repeat=false",
                "--cooldown-ms",
                "500",
            ],
        );
        assert_eq!(output.code, 0, "{}", output.stderr);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("Mod+T repeat=false cooldown-ms=500 {"),
            "{text}"
        );
        assert!(text.contains("spawn \"foot\";"));
        assert!(text.contains("focus-column-right;"));
        let removed = run_on(&path, &["remove-bind", "Mod+T"]);
        assert_eq!(removed.code, 0, "{}", removed.stderr);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "binds {\n}\n");
    }

    #[test]
    fn rule_and_startup_commands() {
        let path = temp_config("rules", "layout {\n    gaps 16\n}\n");
        let added = run_on(
            &path,
            &[
                "add-window-rule",
                "--match",
                "app-id=\"Alacritty\"",
                "--property",
                "open-floating true",
            ],
        );
        assert_eq!(added.code, 0, "{}", added.stderr);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("window-rule {\n    match app-id=\"Alacritty\""));
        let removed = run_on(&path, &["remove-window-rule", "0"]);
        assert_eq!(removed.code, 0, "{}", removed.stderr);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("window-rule")
        );

        let startup = run_on(&path, &["add-startup", "waybar"]);
        assert_eq!(startup.code, 0, "{}", startup.stderr);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("spawn-at-startup \"waybar\"")
        );
        let removed = run_on(&path, &["remove-startup", "0"]);
        assert_eq!(removed.code, 0, "{}", removed.stderr);
    }

    #[test]
    fn list_validate_and_export_are_deterministic() {
        let path = temp_config(
            "list",
            "layout {\n    gaps 16\n}\noutput \"eDP-1\" {\n    scale 1.5\n}\n",
        );
        let first = run_on(&path, &["list"]);
        let second = run_on(&path, &["list"]);
        assert_eq!(first.code, 0);
        assert_eq!(first.stdout, second.stdout);
        assert!(first.stdout.contains("output \\\"eDP-1\\\".scale"));

        let valid = run_on(&path, &["validate"]);
        assert_eq!(valid.code, 0, "{}", valid.stdout);
        let report: Value = serde_json::from_str(&valid.stdout).unwrap();
        assert_eq!(report["ok"], true);

        let exported = run_on(&path, &["export"]);
        let model: Model = serde_json::from_str(&exported.stdout).unwrap();
        assert_eq!(model.version, MODEL_VERSION);
    }

    #[test]
    fn validate_reports_malformed_kdl_with_nonzero_exit() {
        let path = temp_config("broken", "layout {\n    gaps 16\n");
        let output = run_on(&path, &["validate"]);
        assert_eq!(output.code, 1);
        let report: Value = serde_json::from_str(&output.stdout).unwrap();
        assert_eq!(report["ok"], false);
        assert!(!report["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn import_applies_a_json_model() {
        let path = temp_config("import", "layout {\n    gaps 16\n}\n");
        let model = Model {
            binds: vec![Bind::new("Mod+Return", vec!["spawn \"foot\"".into()])],
            ..Model::default()
        };
        let file = path.with_file_name("model.json");
        std::fs::write(&file, serde_json::to_string(&model).unwrap()).unwrap();
        let output = run_on(&path, &["import", &file.display().to_string()]);
        assert_eq!(output.code, 0, "{}", output.stderr);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Mod+Return")
        );
    }

    #[test]
    fn unknown_commands_and_options_are_errors() {
        let path = temp_config("errors", "");
        assert_eq!(run_on(&path, &["nope"]).code, 1);
        assert_eq!(run_on(&path, &["add-bind", "Mod+X"]).code, 1);
        assert_eq!(
            run_on(&path, &["add-bind", "Mod+X", "--action", "x", "--bogus"]).code,
            1
        );
    }
}
