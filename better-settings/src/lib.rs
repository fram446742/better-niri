//! `better-settings`: text-preserving settings for niri's KDL config.
//!
//! The model never re-serializes the config. It scans the original text,
//! remembers where each `key value` lives inside its block, and rewrites only
//! the lines it needs to change. Comments, blank lines, indentation and
//! unrelated settings survive untouched, which keeps diffs minimal and lets
//! users keep hand-written config files in version control.
//!
//! Two entry points share this model:
//!
//! - `better-settings` (GTK4, feature `gui`) renders a small settings window.
//! - `better-settingsctl` applies single settings from a script.

use std::fmt;
use std::path::{Path, PathBuf};

pub mod arrange;
pub mod catalog;
pub mod cli;
pub mod diff;
pub mod integrations;
pub mod model;
pub mod nodes;

pub use arrange::{
    Arrangement, DisplayBox, DragState, GRID, MAX_COORDINATE, SNAP_THRESHOLD, SnapResult, Viewport,
};
pub use catalog::{
    CATALOG, Kind, OUTPUT_TEMPLATES, OutputTemplate, OwnedSetting, SettingSpec, catalog_for, find,
    normalize, output_names,
};
pub use diff::unified_diff;
pub use model::{Model, SettingValue, export_model, import_model};
pub use nodes::{
    Bind, Node, RuleKind, RuleSpec, Validation, add_preset_column_width, add_rule, add_startup,
    binds, default_column_width, environment, nodes, preset_column_widths, remove_bind,
    remove_preset_column_width, remove_rule, remove_startup, rules, set_bind,
    set_default_column_width, spawn_at_startup, validate,
};

/// One segment of a KDL path: a node name plus optional arguments.
///
/// `output "eDP-1"` → `Segment { name: "output", args: ["eDP-1"] }`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Segment {
    pub name: String,
    pub args: Vec<String>,
}

impl Segment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: Vec::new(),
        }
    }

    pub fn with_arg(name: impl Into<String>, arg: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: vec![arg.into()],
        }
    }

    /// Parses `name`, `name "arg"` or `name arg`.
    pub fn parse(text: &str) -> Result<Self, PatchError> {
        let tokens = tokens(text);
        let Some((name, rest)) = tokens.split_first() else {
            return Err(PatchError::Syntax(format!("empty path segment `{text}`")));
        };
        Ok(Self {
            name: name.clone(),
            args: rest.to_vec(),
        })
    }
}

impl fmt::Display for Segment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.name)?;
        for arg in &self.args {
            write!(
                formatter,
                " \"{}\"",
                arg.replace('\\', "\\\\").replace('"', "\\\"")
            )?;
        }
        Ok(())
    }
}

/// Parses a dotted path such as `layout.gaps` or `output "eDP-1".scale`.
///
/// Dots inside quoted arguments are preserved.
pub fn parse_path(text: &str) -> Result<Vec<Segment>, PatchError> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = false;
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote => {
                current.push(ch);
                escaped = true;
            }
            '"' => {
                quote = !quote;
                current.push(ch);
            }
            '.' if !quote => {
                let segment = current.trim();
                if segment.is_empty() {
                    return Err(PatchError::Syntax(format!(
                        "empty path segment in `{text}`"
                    )));
                }
                segments.push(Segment::parse(segment)?);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let segment = current.trim();
    if segment.is_empty() {
        return Err(PatchError::Syntax(format!(
            "empty path segment in `{text}`"
        )));
    }
    segments.push(Segment::parse(segment)?);
    Ok(segments)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    Syntax(String),
    Unterminated(String),
    MissingClosingBrace(String),
}

impl fmt::Display for PatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(message) => write!(formatter, "{message}"),
            Self::Unterminated(message) => write!(formatter, "unterminated string: {message}"),
            Self::MissingClosingBrace(path) => {
                write!(formatter, "block `{path}` has no closing brace")
            }
        }
    }
}

impl std::error::Error for PatchError {}

/// A block (`layout { … }`) found in the config.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockInfo {
    pub path: Vec<Segment>,
    pub header_line: usize,
    pub close_line: Option<usize>,
    pub header_indent: usize,
    pub key_indent: Option<usize>,
}

/// A `key value` line found in the config.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyLine {
    /// Full path including the key as its last segment.
    pub path: Vec<Segment>,
    pub line: usize,
    pub value: String,
    pub indent: usize,
    /// Text after the value on the same line (`;`, comments, …).
    pub tail: String,
    /// True for bare flags such as `prefer-no-csd`; their value is
    /// `true` while the line exists.
    pub flag: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Scan {
    pub blocks: Vec<BlockInfo>,
    pub keys: Vec<KeyLine>,
}

impl Scan {
    pub fn block(&self, path: &[Segment]) -> Option<&BlockInfo> {
        self.blocks.iter().find(|block| block.path == path)
    }

    pub fn key(&self, path: &[Segment]) -> Option<&KeyLine> {
        self.keys.iter().find(|key| key.path == path)
    }

    pub fn value(&self, path: &[Segment]) -> Option<&str> {
        self.key(path).map(|key| key.value.as_str())
    }
}

/// Replaces every comment with spaces so indices match the original line.
fn strip_comments(line: &str, in_block_comment: &mut bool) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < bytes.len() {
        if *in_block_comment {
            if bytes[index..].starts_with(b"*/") {
                out.push_str("  ");
                index += 2;
                *in_block_comment = false;
            } else {
                out.push(' ');
                index += char_len(line, index);
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            out.push_str("  ");
            index += 2;
            *in_block_comment = true;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            out.extend(std::iter::repeat_n(' ', line.len() - index));
            break;
        }
        let ch = line[index..].chars().next().unwrap();
        if ch == '"' {
            // Copy the string verbatim so comment markers inside it survive.
            out.push('"');
            index += 1;
            while index < bytes.len() {
                let inner = line[index..].chars().next().unwrap();
                out.push(inner);
                index += inner.len_utf8();
                if inner == '\\' && index < bytes.len() {
                    let escaped = line[index..].chars().next().unwrap();
                    out.push(escaped);
                    index += escaped.len_utf8();
                    continue;
                }
                if inner == '"' {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn char_len(line: &str, byte_index: usize) -> usize {
    line[byte_index..].chars().next().map_or(1, char::len_utf8)
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Splits `name "arg"` / `name arg` into tokens, unquoting strings.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote = false;
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote => {
                escaped = true;
                current.push(ch);
            }
            '"' => {
                quote = !quote;
                // Keep quotes out of the token value.
                if quote {
                    // Starting a new quoted token: flush any bare prefix.
                    if !current.trim().is_empty() {
                        out.push(current.trim().to_owned());
                        current.clear();
                    }
                } else {
                    out.push(current.clone());
                    current.clear();
                }
            }
            ch if ch.is_whitespace() && !quote => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// Quotes a value unless it is a plain KDL number or identifier.
///
/// Deliberately conservative: paths (`~/…`, `a/b`), colors (`#rrggbb`),
/// modes (`1920x1080@120.030`) and anything with spaces are quoted, because
/// they are not valid bare KDL identifiers.
pub fn quote_if_needed(value: &str) -> String {
    let number = value.parse::<f64>().is_ok_and(|number| number.is_finite())
        && value
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E'));
    let mut chars = value.chars();
    let identifier = !value.is_empty()
        && chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if number || identifier {
        value.to_owned()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Scans `text` and records blocks and key lines.
pub fn scan(text: &str) -> Result<Scan, PatchError> {
    let mut scan = Scan::default();
    let mut stack: Vec<Segment> = Vec::new();
    let mut in_block_comment = false;
    let mut key_indents: Vec<Option<usize>> = Vec::new();

    for (index, original) in text.lines().enumerate() {
        let analysis = strip_comments(original, &mut in_block_comment);
        let trimmed = analysis.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Closing braces (one or more per line).
        let closers = trimmed.chars().take_while(|ch| *ch == '}').count();
        for _ in 0..closers {
            if let Some(popped) = stack.pop() {
                let mut path = stack.clone();
                path.push(popped);
                if let Some(block) = scan
                    .blocks
                    .iter_mut()
                    .rev()
                    .find(|block| block.path == path)
                {
                    block.close_line = Some(index);
                }
                key_indents.pop();
            }
        }
        if closers > 0 && trimmed.chars().all(|ch| ch == '}' || ch.is_whitespace()) {
            continue;
        }

        if let Some(open) = trimmed.find('{') {
            let header = trimmed[..open].trim();
            let rest = trimmed[open + 1..].trim();
            if rest.is_empty() {
                let parsed = tokens(header);
                let Some((name, args)) = parsed.split_first() else {
                    continue;
                };
                let segment = Segment {
                    name: name.clone(),
                    args: args.to_vec(),
                };
                let mut path = stack.clone();
                path.push(segment.clone());
                scan.blocks.push(BlockInfo {
                    path,
                    header_line: index,
                    close_line: None,
                    header_indent: indent_of(original),
                    key_indent: None,
                });
                stack.push(segment);
                key_indents.push(None);
                continue;
            }
            // Inline children (`presets { … }` on one line) stay opaque.
            continue;
        }

        let (key, value, tail, flag) = match split_key_line(&analysis, original) {
            Some((key, value, tail)) => (key, value, tail, false),
            None => match split_flag(&analysis, original) {
                Some((key, tail)) => (key, "true".to_owned(), tail, true),
                None => continue,
            },
        };
        let indent = indent_of(original);
        if stack.is_empty() {
            // Top-level settings live in the root "block".
            scan.keys.push(KeyLine {
                path: vec![Segment::new(key.clone())],
                line: index,
                value,
                indent,
                tail,
                flag,
            });
            continue;
        }
        if let Some(slot) = key_indents.last_mut()
            && slot.is_none()
        {
            *slot = Some(indent);
        }
        if let Some(block) = scan
            .blocks
            .iter_mut()
            .rev()
            .find(|block| block.path == stack)
            && block.key_indent.is_none()
        {
            block.key_indent = Some(indent);
        }
        let mut path = stack.clone();
        path.push(Segment::new(key));
        scan.keys.push(KeyLine {
            path,
            line: index,
            value,
            indent,
            tail,
            flag,
        });
    }

    Ok(scan)
}

/// Splits a key line into `(key, value, tail)` using the comment-free view for
/// positions and the original text for content.
fn split_key_line(analysis: &str, original: &str) -> Option<(String, String, String)> {
    let trimmed_start = analysis.len() - analysis.trim_start().len();
    let body = &analysis[trimmed_start..];
    let mut name_end = 0;
    for (index, ch) in body.char_indices() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            name_end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if name_end == 0 {
        return None;
    }
    let key = &body[..name_end];
    let after = &body[name_end..];
    let after_start = trimmed_start + name_end;
    if after.trim().is_empty() {
        // Flag without a value (`prefer-no-csd`).
        return None;
    }
    // Find where the value starts.
    let value_offset = after.len() - after.trim_start().len();
    let value_start = after_start + value_offset;
    // The value may have several tokens (`position x=0 y=5`,
    // `scroll-factor vertical=1.0 horizontal=-2.0`). Take everything up to a
    // terminating semicolon or trailing comment.
    let mut value_end = analysis.len();
    while value_end > value_start && analysis.as_bytes()[value_end - 1].is_ascii_whitespace() {
        value_end -= 1;
    }
    if value_end > value_start && analysis.as_bytes()[value_end - 1] == b';' {
        value_end -= 1;
        while value_end > value_start && analysis.as_bytes()[value_end - 1].is_ascii_whitespace() {
            value_end -= 1;
        }
    }
    let raw_value = &original[value_start..value_end];
    let value = unquote_single(raw_value).unwrap_or_else(|| raw_value.to_owned());
    // `tail` keeps the semicolon and any trailing comment. It must be read
    // from the original line: `analysis` has comments blanked out.
    let tail = original
        .get(value_end..)
        .map(str::trim_end)
        .unwrap_or_default()
        .to_owned();
    Some((key.to_owned(), value, tail))
}

/// Unquotes `raw` only when the whole value is one quoted string.
fn unquote_single(raw: &str) -> Option<String> {
    if !raw.starts_with('"') {
        return None;
    }
    let mut escaped = false;
    for (index, ch) in raw.char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => {
                return (index + ch.len_utf8() == raw.len())
                    .then(|| raw[1..index].replace("\\\"", "\"").replace("\\\\", "\\"));
            }
            _ => {}
        }
    }
    None
}

/// Recognizes a bare flag line (`prefer-no-csd`, `tap`) and its trailing text.
fn split_flag(analysis: &str, original: &str) -> Option<(String, String)> {
    let trimmed = analysis.trim_start();
    let start = analysis.len() - trimmed.len();
    let mut end = 0;
    for (index, ch) in trimmed.char_indices() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let rest = trimmed[end..].trim();
    if !rest.is_empty() && !rest.starts_with(';') {
        return None;
    }
    let key = &trimmed[..end];
    let tail = original
        .get(start + end..)
        .map(str::trim_end)
        .unwrap_or_default()
        .to_owned();
    Some((key.to_owned(), tail))
}

/// The current value at `path`, if the config sets it.
pub fn current(text: &str, path: &str) -> Result<Option<String>, PatchError> {
    let path = parse_path(path)?;
    if let Some((parent, kind)) = special_block(&path) {
        return match kind {
            "default-column-width" => nodes::default_column_width_at(text, parent),
            "preset-column-widths" => nodes::preset_column_widths_at(text, parent)
                .map(|sizes| (!sizes.is_empty()).then(|| sizes.join(", "))),
            _ => Ok(None),
        };
    }
    Ok(scan(text)?.value(&path).map(str::to_owned))
}

/// Detects the two layout settings that are KDL blocks rather than key/value
/// lines (`default-column-width` and `preset-column-widths`).
fn special_block(path: &[Segment]) -> Option<(&[Segment], &str)> {
    let last = path.last()?;
    if last.name != "default-column-width" && last.name != "preset-column-widths" {
        return None;
    }
    let parent = &path[..path.len() - 1];
    if parent.last().map(|segment| segment.name.as_str()) != Some("layout") {
        return None;
    }
    Some((parent, last.name.as_str()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub path: String,
    pub value: String,
    /// When set, the setting is a bare flag and `value` is a boolean.
    pub flag: bool,
    /// When set, `value` is written verbatim instead of being quoted.
    pub raw: bool,
}

impl Edit {
    pub fn new(path: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            value: value.into(),
            flag: false,
            raw: false,
        }
    }

    pub fn flag(path: impl Into<String>, enabled: bool) -> Self {
        Self {
            path: path.into(),
            value: if enabled { "true" } else { "false" }.to_owned(),
            flag: true,
            raw: false,
        }
    }

    pub fn raw(path: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            value: value.into(),
            flag: false,
            raw: true,
        }
    }
}

/// Applies one edit to `text`, returning the new text.
///
/// Only the target line (or the insertion point for a missing key/block) is
/// rewritten; everything else, including comments, is preserved byte for byte.
pub fn patch(text: &str, edit: &Edit) -> Result<String, PatchError> {
    let path = parse_path(&edit.path)?;
    if let Some((parent, kind)) = special_block(&path) {
        if edit.flag {
            return Err(PatchError::Syntax(format!(
                "`{}` is a block and cannot be set as a flag",
                edit.path
            )));
        }
        return match kind {
            "default-column-width" => {
                nodes::set_default_column_width_at(text, parent, edit.value.trim())
            }
            "preset-column-widths" => {
                let sizes = split_size_list(&edit.value)?;
                nodes::set_preset_column_widths_at(text, parent, &sizes)
            }
            _ => Ok(text.to_owned()),
        };
    }
    let value = if edit.flag {
        normalize(Kind::Flag, &edit.value).map_err(PatchError::Syntax)?
    } else if edit.raw {
        edit.value.trim().to_owned()
    } else {
        quote_if_needed(&edit.value)
    };
    let scan = scan(text)?;
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let trailing_newline = text.ends_with('\n');

    if let Some(key) = scan.key(&path) {
        if edit.flag {
            // A present flag is already enabled; keep any properties it has
            // (for example `variable-refresh-rate on-demand=true`).
            if value == "true" {
                return Ok(text.to_owned());
            }
            lines.remove(key.line);
            return Ok(join_lines_with(lines, trailing_newline, line_ending(text)));
        }
        if key.value == edit.value {
            return Ok(text.to_owned());
        }
        let key_name = &path.last().unwrap().name;
        lines[key.line] = format!("{}{} {value}{}", " ".repeat(key.indent), key_name, key.tail);
        return Ok(join_lines_with(lines, trailing_newline, line_ending(text)));
    }
    // A flag that is not present is already disabled.
    if edit.flag && value == "false" {
        return Ok(text.to_owned());
    }
    if path.len() == 1 {
        let key = &path[0].name;
        let mut line = " ".repeat(0) + key;
        if !edit.flag {
            line.push(' ');
            line.push_str(&value);
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(line);
        return Ok(join_lines_with(lines, trailing_newline, line_ending(text)));
    }

    let (blocks, key) = (&path[..path.len() - 1], path.last().unwrap());
    // Longest existing block prefix of the requested path.
    let mut prefix_len = blocks.len();
    while prefix_len > 0 && scan.block(&blocks[..prefix_len]).is_none() {
        prefix_len -= 1;
    }

    let (insert_at, base_indent) = if prefix_len == 0 {
        (lines.len(), 0usize)
    } else {
        let block = scan.block(&blocks[..prefix_len]).unwrap();
        let close = block.close_line.ok_or_else(|| {
            PatchError::MissingClosingBrace(
                blocks[..prefix_len]
                    .iter()
                    .map(Segment::to_string)
                    .collect::<Vec<_>>()
                    .join("."),
            )
        })?;
        (close, block.key_indent.unwrap_or(block.header_indent + 4))
    };

    let mut block_lines = Vec::new();
    for (depth, segment) in blocks[prefix_len..].iter().enumerate() {
        block_lines.push(format!(
            "{}{} {{",
            " ".repeat(base_indent + depth * 4),
            segment
        ));
    }
    let key_indent = base_indent + (blocks.len() - prefix_len) * 4;
    block_lines.push(if edit.flag {
        format!("{}{key}", " ".repeat(key_indent))
    } else {
        format!("{}{key} {value}", " ".repeat(key_indent))
    });
    for depth in (0..blocks.len() - prefix_len).rev() {
        block_lines.push(format!("{}}}", " ".repeat(base_indent + depth * 4)));
    }

    let mut inserted = block_lines;
    if prefix_len == 0 && !lines.is_empty() {
        inserted.insert(0, String::new());
    }
    for (offset, line) in inserted.into_iter().enumerate() {
        lines.insert(insert_at + offset, line);
    }
    Ok(join_lines_with(lines, trailing_newline, line_ending(text)))
}

/// Splits a comma-separated list of preset sizes and validates each entry.
pub(crate) fn split_size_list(value: &str) -> Result<Vec<String>, PatchError> {
    let mut sizes = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let tokens = tokens(part);
        let valid = match tokens.as_slice() {
            [kind, amount] if kind == "proportion" => amount.parse::<f64>().is_ok(),
            [kind, amount] if kind == "fixed" => amount.parse::<i64>().is_ok(),
            _ => false,
        };
        if !valid {
            return Err(PatchError::Syntax(format!(
                "`{part}` must be `proportion <0..1>` or `fixed <pixels>`"
            )));
        }
        sizes.push(format!("{} {}", tokens[0], tokens[1]));
    }
    Ok(sizes)
}

/// The dominant line ending of `text`; edits keep it so files do not churn.
pub(crate) fn line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

pub(crate) fn join_lines_with(
    lines: Vec<String>,
    trailing_newline: bool,
    separator: &str,
) -> String {
    let mut text = lines.join(separator);
    if trailing_newline {
        text.push_str(separator);
    }
    text
}

/// Removes the key line at `path`, leaving every other byte untouched.
///
/// Removing a key that is not present is a no-op.
pub fn remove_key(text: &str, path: &str) -> Result<String, PatchError> {
    let path = parse_path(path)?;
    if let Some((parent, kind)) = special_block(&path) {
        return match kind {
            "default-column-width" => nodes::remove_block_at(text, parent, kind),
            "preset-column-widths" => nodes::remove_block_at(text, parent, kind),
            _ => Ok(text.to_owned()),
        };
    }
    let scan = scan(text)?;
    let Some(key) = scan.key(&path) else {
        return Ok(text.to_owned());
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    lines.remove(key.line);
    Ok(join_lines_with(
        lines,
        text.ends_with('\n'),
        line_ending(text),
    ))
}

/// Applies several edits in order.
pub fn patch_many(text: &str, edits: &[Edit]) -> Result<String, PatchError> {
    let mut current = text.to_owned();
    for edit in edits {
        current = patch(&current, edit)?;
    }
    Ok(current)
}

/// The niri config path: `$NIRI_CONFIG`, then
/// `$XDG_CONFIG_HOME/niri/config.kdl`, then `~/.config/niri/config.kdl`.
pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NIRI_CONFIG") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_default();
    base.join("niri/config.kdl")
}

/// Writes `text` to `path`, copying the previous file to
/// `<name>.bak-<epoch>`. With `dry_run`, nothing is written.
pub fn save(path: &Path, text: &str, dry_run: bool) -> std::io::Result<Option<PathBuf>> {
    if dry_run {
        return Ok(None);
    }
    let backup = if path.exists() {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.kdl".to_owned());
        // Two saves in the same second must not overwrite each other.
        let mut backup = path.with_file_name(format!("{file_name}.bak-{epoch}"));
        let mut counter = 1;
        while backup.exists() {
            backup = path.with_file_name(format!("{file_name}.bak-{epoch}-{counter}"));
            counter += 1;
        }
        std::fs::copy(path, &backup)?;
        Some(backup)
    } else {
        None
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.kdl".to_owned())
    ));
    std::fs::write(&temp, text)?;
    std::fs::rename(temp, path)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"// niri config
input {
    keyboard {
        repeat-delay 300 // fast
        repeat-rate 40
    }

    touchpad {
        tap
        natural-scroll
    }
}

layout {
    gaps 16
    center-focused-column "never"
}

output "eDP-1" {
    scale 1.5
}

output "HDMI-A-1" {
    scale 1
}

prefer-no-csd
"#;

    fn value_of(text: &str, path: &str) -> Option<String> {
        current(text, path).unwrap()
    }

    #[test]
    fn scans_nested_blocks_and_outputs() {
        let scan = scan(CONFIG).unwrap();
        assert_eq!(value_of(CONFIG, "layout.gaps").as_deref(), Some("16"));
        assert_eq!(
            value_of(CONFIG, "input.keyboard.repeat-delay").as_deref(),
            Some("300")
        );
        assert_eq!(
            value_of(CONFIG, "output \"eDP-1\".scale").as_deref(),
            Some("1.5")
        );
        assert_eq!(
            value_of(CONFIG, "output \"HDMI-A-1\".scale").as_deref(),
            Some("1")
        );
        assert!(
            scan.block(&parse_path("output \"eDP-1\"").unwrap())
                .is_some()
        );
        // Bare flags are reported as `true` while the line is present.
        assert_eq!(
            value_of(CONFIG, "input.touchpad.tap").as_deref(),
            Some("true")
        );
    }

    #[test]
    fn replaces_a_value_and_keeps_comments() {
        let edit = Edit::new("input.keyboard.repeat-delay", "250");
        let patched = patch(CONFIG, &edit).unwrap();
        assert!(patched.contains("        repeat-delay 250 // fast"));
        assert!(patched.contains("// niri config"));
        assert_eq!(
            value_of(&patched, "input.keyboard.repeat-delay").as_deref(),
            Some("250")
        );
        // Everything else is byte-identical.
        let before: Vec<_> = CONFIG
            .lines()
            .filter(|l| !l.contains("repeat-delay"))
            .collect();
        let after: Vec<_> = patched
            .lines()
            .filter(|l| !l.contains("repeat-delay"))
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn edits_only_the_addressed_output() {
        let edit = Edit::new("output \"eDP-1\".scale", "2");
        let patched = patch(CONFIG, &edit).unwrap();
        assert_eq!(
            value_of(&patched, "output \"eDP-1\".scale").as_deref(),
            Some("2")
        );
        assert_eq!(
            value_of(&patched, "output \"HDMI-A-1\".scale").as_deref(),
            Some("1")
        );
    }

    #[test]
    fn no_op_when_value_is_already_set() {
        let edit = Edit::new("layout.gaps", "16");
        assert_eq!(patch(CONFIG, &edit).unwrap(), CONFIG);
    }

    #[test]
    fn inserts_missing_key_into_existing_block() {
        let edit = Edit::new("layout.empty-workspace-above-first", "true");
        let patched = patch(CONFIG, &edit).unwrap();
        assert!(patched.contains("    empty-workspace-above-first true\n}"));
        assert_eq!(
            value_of(&patched, "layout.empty-workspace-above-first").as_deref(),
            Some("true")
        );
        // The closing brace position is preserved after the new line.
        assert!(patched.contains(
            "    center-focused-column \"never\"\n    empty-workspace-above-first true\n}"
        ));
    }

    #[test]
    fn inserts_missing_nested_block() {
        let edit = Edit::new("input.touchpad.dwt", "true");
        let patched = patch(CONFIG, &edit).unwrap();
        assert!(patched.contains("        dwt true\n    }"));
        assert_eq!(
            value_of(&patched, "input.touchpad.dwt").as_deref(),
            Some("true")
        );
    }

    #[test]
    fn creates_missing_root_block() {
        let text = "layout {\n    gaps 16\n}\n";
        let edit = Edit::new("animations.slowdown", "2.5");
        let patched = patch(text, &edit).unwrap();
        assert!(patched.ends_with("\n\nanimations {\n    slowdown 2.5\n}\n"));
        assert_eq!(
            value_of(&patched, "animations.slowdown").as_deref(),
            Some("2.5")
        );
    }

    #[test]
    fn creates_missing_output_block() {
        let text = "layout {\n    gaps 16\n}\n";
        let edit = Edit::new("output \"DP-2\".scale", "1.25");
        let patched = patch(text, &edit).unwrap();
        assert!(patched.contains("output \"DP-2\" {\n    scale 1.25\n}"));
        assert_eq!(
            value_of(&patched, "output \"DP-2\".scale").as_deref(),
            Some("1.25")
        );
    }

    #[test]
    fn quotes_values_that_need_it() {
        let edit = Edit::new("screenshot-path", "~/Pictures/Screenshots");
        let patched = patch(CONFIG, &edit).unwrap();
        assert!(patched.contains("screenshot-path \"~/Pictures/Screenshots\""));
        assert_eq!(
            value_of(&patched, "screenshot-path").as_deref(),
            Some("~/Pictures/Screenshots")
        );
    }

    #[test]
    fn ignores_braces_and_keys_inside_comments() {
        let text = r#"// layout { gaps 999 }
layout {
    /* gaps 1 */
    gaps 8
}
"#;
        let scan = scan(text).unwrap();
        assert_eq!(scan.value(&parse_path("layout.gaps").unwrap()), Some("8"));
        let patched = patch(text, &Edit::new("layout.gaps", "10")).unwrap();
        assert!(patched.contains("    gaps 10\n"));
        assert!(patched.contains("// layout { gaps 999 }"));
        assert!(patched.contains("/* gaps 1 */"));
    }

    #[test]
    fn patch_many_applies_all_edits() {
        let edits = [
            Edit::new("layout.gaps", "24"),
            Edit::new("input.keyboard.repeat-rate", "60"),
            Edit::new("cursor.hide-after-ms", "2000"),
        ];
        let patched = patch_many(CONFIG, &edits).unwrap();
        assert_eq!(value_of(&patched, "layout.gaps").as_deref(), Some("24"));
        assert_eq!(
            value_of(&patched, "input.keyboard.repeat-rate").as_deref(),
            Some("60")
        );
        assert_eq!(
            value_of(&patched, "cursor.hide-after-ms").as_deref(),
            Some("2000")
        );
        // Re-applying is a no-op.
        assert_eq!(patch_many(&patched, &edits).unwrap(), patched);
    }

    #[test]
    fn save_creates_a_backup_and_writes_atomically() {
        let dir = std::env::temp_dir().join(format!("better-settings-{}-save", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.kdl");
        std::fs::write(&path, "layout {\n    gaps 16\n}\n").unwrap();

        let backup = save(&path, "layout {\n    gaps 24\n}\n", false)
            .unwrap()
            .expect("backup");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "layout {\n    gaps 16\n}\n"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "layout {\n    gaps 24\n}\n"
        );

        let dry = save(&path, "layout {\n    gaps 32\n}\n", true).unwrap();
        assert!(dry.is_none());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "layout {\n    gaps 24\n}\n"
        );
    }

    #[test]
    fn saves_in_the_same_second_keep_distinct_backups() {
        let dir =
            std::env::temp_dir().join(format!("better-settings-{}-backups", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.kdl");
        std::fs::write(&path, "gaps 1\n").unwrap();

        let first = save(&path, "gaps 2\n", false).unwrap().unwrap();
        let second = save(&path, "gaps 3\n", false).unwrap().unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "gaps 1\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "gaps 2\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "gaps 3\n");
    }

    #[test]
    fn normalize_validates_and_canonicalizes() {
        assert_eq!(normalize(Kind::Bool, "yes").unwrap(), "true".to_owned());
        assert_eq!(
            normalize(
                Kind::Float {
                    min: 0.0,
                    max: 10.0
                },
                "1.50"
            )
            .unwrap(),
            "1.5".to_owned()
        );
        assert_eq!(
            normalize(Kind::Choice(&["never", "always"]), "always").unwrap(),
            "always".to_owned()
        );
        assert!(normalize(Kind::Float { min: 0.0, max: 1.0 }, "5").is_err());
        assert!(normalize(Kind::Int { min: 0, max: 10 }, "1.5").is_err());
        assert!(normalize(Kind::Choice(&["a", "b"]), "c").is_err());
    }

    #[test]
    fn catalog_paths_parse_and_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in CATALOG {
            let path = parse_path(spec.path).unwrap();
            assert!(!path.is_empty());
            assert!(seen.insert(spec.path), "duplicate path {}", spec.path);
        }
    }
}
