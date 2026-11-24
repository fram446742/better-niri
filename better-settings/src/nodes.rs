//! Node-level editing for constructs that are not a single `key value` line:
//! `binds`, `window-rule`/`layer-rule`, `spawn-at-startup`, `environment`, and
//! the two layout settings written as blocks (`default-column-width` and
//! `preset-column-widths`).
//!
//! Everything here is line-oriented and text-preserving: helpers only replace
//! the span of the node they own, leaving comments, ordering and unrelated
//! bytes untouched.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::catalog::{Kind, catalog_for};
use crate::{
    Edit, PatchError, Segment, current, indent_of, join_lines_with, patch, quote_if_needed, scan,
    tokens,
};

/// One KDL node found by [`nodes`].
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub name: String,
    /// Non-property arguments, unquoted.
    pub args: Vec<String>,
    /// `key=value` properties, unquoted.
    pub properties: Vec<(String, String)>,
    pub depth: usize,
    pub parent: Option<usize>,
    pub start_line: usize,
    pub end_line: usize,
    pub block: bool,
    /// True when the node's block closes on its header line.
    pub inline: bool,
    pub closed: bool,
    /// Original header text without the opening brace.
    pub raw_header: String,
}

impl Node {
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Returns two length-preserving views of `line`:
///
/// - the first blanks comments *and* string contents, for brace/`;` scanning;
/// - the second blanks only comments, so header text can be sliced from the original while trailing
///   comments are ignored.
fn blank_code(line: &str, in_block_comment: &mut bool) -> (String, String, bool) {
    let mut code = String::with_capacity(line.len());
    let mut content = String::with_capacity(line.len());
    let mut index = 0;
    let mut unterminated = false;
    while index < line.len() {
        if *in_block_comment {
            if line[index..].starts_with("*/") {
                code.push_str("  ");
                content.push_str("  ");
                index += 2;
                *in_block_comment = false;
            } else {
                let ch = line[index..].chars().next().unwrap();
                let spaces = " ".repeat(ch.len_utf8());
                code.push_str(&spaces);
                content.push_str(&spaces);
                index += ch.len_utf8();
            }
            continue;
        }
        if line[index..].starts_with("//") {
            let spaces = " ".repeat(line.len() - index);
            code.push_str(&spaces);
            content.push_str(&spaces);
            break;
        }
        if line[index..].starts_with("/*") {
            code.push_str("  ");
            content.push_str("  ");
            index += 2;
            *in_block_comment = true;
            continue;
        }
        let ch = line[index..].chars().next().unwrap();
        if ch != '"' {
            code.push(ch);
            content.push(ch);
            index += ch.len_utf8();
            continue;
        }
        // String: blank it fully in `code`; keep it verbatim in `content`.
        code.push(' ');
        content.push('"');
        index += 1;
        let mut closed = false;
        while index < line.len() {
            let inner = line[index..].chars().next().unwrap();
            if inner == '\\' {
                code.push_str(&" ".repeat(inner.len_utf8()));
                content.push(inner);
                index += inner.len_utf8();
                if index < line.len() {
                    let escaped = line[index..].chars().next().unwrap();
                    code.push_str(&" ".repeat(escaped.len_utf8()));
                    content.push(escaped);
                    index += escaped.len_utf8();
                }
                continue;
            }
            if inner == '"' {
                code.push(' ');
                content.push('"');
                index += 1;
                closed = true;
                break;
            }
            code.push_str(&" ".repeat(inner.len_utf8()));
            content.push(inner);
            index += inner.len_utf8();
        }
        if !closed {
            unterminated = true;
        }
    }
    (code, content, unterminated)
}

fn trim_bounds(code: &str, start: usize, end: usize) -> (usize, usize) {
    let slice = &code[start..end];
    let trimmed = slice.trim_start();
    let leading = slice.len() - trimmed.len();
    let trimmed = trimmed.trim_end();
    (start + leading, start + leading + trimmed.len())
}

/// Reads one token, keeping quoted spaces and unescaping quoted characters.
/// Returns the token and the output index where its first quoted section began.
fn read_token(raw: &str, index: &mut usize) -> (String, Option<usize>) {
    let mut out = String::new();
    let mut quote_start = None;
    let mut quoted = false;
    while *index < raw.len() {
        let ch = raw[*index..].chars().next().unwrap();
        if quoted {
            if ch == '\\' {
                *index += 1;
                if *index < raw.len() {
                    let escaped = raw[*index..].chars().next().unwrap();
                    out.push(escaped);
                    *index += escaped.len_utf8();
                }
                continue;
            }
            if ch == '"' {
                quoted = false;
            } else {
                out.push(ch);
            }
            *index += ch.len_utf8();
            continue;
        }
        if ch == '"' {
            quoted = true;
            quote_start.get_or_insert(out.len());
            *index += 1;
            continue;
        }
        if ch.is_whitespace() {
            break;
        }
        out.push(ch);
        *index += ch.len_utf8();
    }
    (out, quote_start)
}

fn parse_header(raw: &str) -> (String, Vec<String>, Vec<(String, String)>) {
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < raw.len() {
        while index < raw.len() && raw[index..].chars().next().unwrap().is_whitespace() {
            index += raw[index..].chars().next().unwrap().len_utf8();
        }
        if index >= raw.len() {
            break;
        }
        let (token, quote_start) = read_token(raw, &mut index);
        if !token.is_empty() {
            tokens.push((token, quote_start));
        }
    }
    let mut tokens = tokens.into_iter();
    let name = tokens.next().map(|(token, _)| token).unwrap_or_default();
    let mut args = Vec::new();
    let mut properties = Vec::new();
    for (token, quote_start) in tokens {
        let property = token
            .find('=')
            .filter(|equals| quote_start.is_none_or(|quote| *equals < quote));
        match property {
            Some(equals) => {
                properties.push((token[..equals].to_owned(), token[equals + 1..].to_owned()))
            }
            None => args.push(token),
        }
    }
    (name, args, properties)
}

fn make_node(
    raw_header: &str,
    depth: usize,
    parent: Option<usize>,
    start_line: usize,
    end_line: usize,
    block: bool,
) -> Node {
    let raw_header = raw_header.trim().to_owned();
    let (name, args, properties) = parse_header(&raw_header);
    Node {
        name,
        args,
        properties,
        depth,
        parent,
        start_line,
        end_line,
        block,
        inline: block && start_line == end_line,
        closed: !block,
        raw_header,
    }
}

/// Parses all nodes (including inline blocks) in `text`.
pub fn nodes(text: &str) -> Vec<Node> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut stack: Vec<Option<usize>> = Vec::new();
    let mut in_block_comment = false;

    for (line_index, line) in lines.iter().enumerate() {
        let (code, content, _) = blank_code(line, &mut in_block_comment);
        let mut segment = 0;
        let mut index = 0;
        while index < code.len() {
            let ch = code[index..].chars().next().unwrap();
            match ch {
                '{' => {
                    let (start, end) = trim_bounds(&content, segment, index);
                    if start < end {
                        let parent = stack.iter().rev().find_map(|slot| *slot);
                        let node = make_node(
                            &line[start..end],
                            stack.len(),
                            parent,
                            line_index,
                            usize::MAX,
                            true,
                        );
                        found.push(node);
                        stack.push(Some(found.len() - 1));
                    } else {
                        stack.push(None);
                    }
                    index += 1;
                    segment = index;
                }
                '}' => {
                    if let Some(Some(node)) = stack.pop() {
                        found[node].end_line = line_index;
                        found[node].closed = true;
                        found[node].inline = found[node].start_line == line_index;
                    }
                    index += 1;
                    segment = index;
                }
                ';' => {
                    let (start, end) = trim_bounds(&content, segment, index);
                    if start < end {
                        let parent = stack.iter().rev().find_map(|slot| *slot);
                        found.push(make_node(
                            &line[start..end],
                            stack.len(),
                            parent,
                            line_index,
                            line_index,
                            false,
                        ));
                    }
                    index += 1;
                    segment = index;
                }
                other => index += other.len_utf8(),
            }
        }
        let (start, end) = trim_bounds(&content, segment, code.len());
        if start < end {
            let parent = stack.iter().rev().find_map(|slot| *slot);
            found.push(make_node(
                &line[start..end],
                stack.len(),
                parent,
                line_index,
                line_index,
                false,
            ));
        }
    }

    let last_line = lines.len().saturating_sub(1);
    for node in stack.into_iter().flatten() {
        if found[node].end_line == usize::MAX {
            found[node].end_line = last_line;
        }
    }
    for node in &mut found {
        if node.end_line == usize::MAX {
            node.end_line = node.start_line;
        }
    }
    found
}

fn node_path(found: &[Node], mut index: usize) -> Vec<Segment> {
    let mut path = Vec::new();
    loop {
        let node = &found[index];
        path.push(Segment {
            name: node.name.clone(),
            args: node.args.clone(),
        });
        match node.parent {
            Some(parent) => index = parent,
            None => break,
        }
    }
    path.reverse();
    path
}

fn find_node(found: &[Node], path: &[Segment]) -> Option<usize> {
    found
        .iter()
        .enumerate()
        .position(|(index, _)| node_path(found, index) == path)
}

fn split_text(text: &str) -> (Vec<String>, bool) {
    (
        text.lines().map(str::to_owned).collect(),
        text.ends_with('\n'),
    )
}

fn replace_span(
    text: &str,
    start: usize,
    end: usize,
    replacement: &[String],
) -> Result<String, PatchError> {
    let (mut lines, trailing) = split_text(text);
    if start > end || end >= lines.len() {
        return Err(PatchError::Syntax(format!(
            "line span {start}..={end} is outside the file"
        )));
    }
    lines.splice(start..=end, replacement.iter().cloned());
    Ok(join_lines_with(lines, trailing, crate::line_ending(text)))
}

fn remove_span_with_blank(text: &str, start: usize, end: usize) -> Result<String, PatchError> {
    let (lines, _) = split_text(text);
    let start = if start > 0
        && lines
            .get(start - 1)
            .is_some_and(|line| line.trim().is_empty())
    {
        start - 1
    } else {
        start
    };
    replace_span(text, start, end, &[])
}

fn insert_lines(text: &str, at: usize, inserted: &[String]) -> String {
    let (mut lines, trailing) = split_text(text);
    let at = at.min(lines.len());
    for (offset, line) in inserted.iter().enumerate() {
        lines.insert(at + offset, line.clone());
    }
    join_lines_with(lines, trailing, crate::line_ending(text))
}

fn append_lines(text: &str, inserted: &[String]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.extend(inserted.iter().cloned());
    let trailing = text.is_empty() || text.ends_with('\n');
    join_lines_with(lines, trailing, crate::line_ending(text))
}

fn line_indent(line: &str) -> usize {
    indent_of(line)
}

fn quote_always(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn child_indent(text: &str, found: &[Node], index: usize, fallback: usize) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    found
        .iter()
        .filter(|node| node.parent == Some(index))
        .map(|node| node.start_line)
        .min()
        .and_then(|line| lines.get(line))
        .map_or(fallback, |line| line_indent(line))
}

// ------------------------------------------------------------------- binds

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bind {
    pub key: String,
    pub actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_when_locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_inhibiting: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey_overlay_title: Option<String>,
}

impl Bind {
    pub fn new(key: impl Into<String>, actions: Vec<String>) -> Self {
        Self {
            key: key.into(),
            actions,
            repeat: None,
            cooldown_ms: None,
            allow_when_locked: None,
            allow_inhibiting: None,
            hotkey_overlay_title: None,
        }
    }

    fn header(&self) -> String {
        let mut header = self.key.clone();
        if let Some(repeat) = self.repeat {
            header.push_str(if repeat {
                " repeat=true"
            } else {
                " repeat=false"
            });
        }
        if let Some(cooldown) = self.cooldown_ms {
            header.push_str(&format!(" cooldown-ms={cooldown}"));
        }
        if let Some(allow) = self.allow_when_locked {
            header.push_str(if allow {
                " allow-when-locked=true"
            } else {
                " allow-when-locked=false"
            });
        }
        if let Some(allow) = self.allow_inhibiting {
            header.push_str(if allow {
                " allow-inhibiting=true"
            } else {
                " allow-inhibiting=false"
            });
        }
        if let Some(title) = &self.hotkey_overlay_title {
            header.push_str(&format!(" hotkey-overlay-title={}", quote_if_needed(title)));
        }
        header
    }

    fn lines(&self, indent: usize) -> Vec<String> {
        let pad = " ".repeat(indent);
        let header = self.header();
        let mut lines = Vec::new();
        if self.actions.len() == 1 {
            let action = self.actions[0].trim().trim_end_matches(';').trim();
            lines.push(format!("{pad}{header} {{ {action}; }}"));
        } else {
            lines.push(format!("{pad}{header} {{"));
            for action in &self.actions {
                lines.push(format!(
                    "{pad}    {};",
                    action.trim().trim_end_matches(';').trim()
                ));
            }
            lines.push(format!("{pad}}}"));
        }
        lines
    }

    /// One-line summary used by the GUI and `list`.
    pub fn summary(&self) -> String {
        format!("{} → {}", self.key, self.actions.join("; "))
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// Keeps the action's original spelling (quoting, numeric arguments, …).
fn serialize_node(node: &Node) -> String {
    node.raw_header.trim().to_owned()
}

/// All binds in file order.
pub fn binds(text: &str) -> Vec<Bind> {
    let found = nodes(text);
    let Some(binds_index) = found
        .iter()
        .position(|node| node.depth == 0 && node.name == "binds" && node.block && node.closed)
    else {
        return Vec::new();
    };

    found
        .iter()
        .enumerate()
        .filter(|(_, node)| node.parent == Some(binds_index) && node.block)
        .map(|(index, node)| {
            let actions = found
                .iter()
                .enumerate()
                .filter(|(_, child)| child.parent == Some(index))
                .map(|(_, child)| serialize_node(child))
                .collect();
            Bind {
                key: node.name.clone(),
                actions,
                repeat: node.property("repeat").and_then(parse_bool),
                cooldown_ms: node
                    .property("cooldown-ms")
                    .and_then(|value| value.parse().ok()),
                allow_when_locked: node.property("allow-when-locked").and_then(parse_bool),
                allow_inhibiting: node.property("allow-inhibiting").and_then(parse_bool),
                hotkey_overlay_title: node
                    .property("hotkey-overlay-title")
                    .filter(|value| *value != "null")
                    .map(str::to_owned),
            }
        })
        .collect()
}

fn validate_bind(bind: &Bind) -> Result<(), PatchError> {
    if bind.key.trim().is_empty() {
        return Err(PatchError::Syntax("bind needs a key".to_owned()));
    }
    if bind.actions.is_empty() {
        return Err(PatchError::Syntax(format!(
            "bind `{}` needs at least one action",
            bind.key
        )));
    }
    if bind.actions.iter().any(|action| action.trim().is_empty()) {
        return Err(PatchError::Syntax(format!(
            "bind `{}` has an empty action",
            bind.key
        )));
    }
    Ok(())
}

/// Adds or replaces a bind, preserving everything outside its span.
pub fn set_bind(text: &str, bind: &Bind) -> Result<String, PatchError> {
    validate_bind(bind)?;
    let found = nodes(text);
    if let Some(binds_index) = found
        .iter()
        .position(|node| node.depth == 0 && node.name == "binds" && node.block && node.closed)
    {
        if let Some((_, node)) = found
            .iter()
            .enumerate()
            .find(|(_, node)| node.parent == Some(binds_index) && node.name == bind.key)
        {
            let indent = text
                .lines()
                .nth(node.start_line)
                .map_or(node.depth * 4, line_indent);
            return replace_span(text, node.start_line, node.end_line, &bind.lines(indent));
        }
        let close = found[binds_index].end_line;
        let indent = child_indent(text, &found, binds_index, 4);
        return Ok(insert_lines(text, close, &bind.lines(indent)));
    }

    // No binds block: create one at the end of the file.
    let mut inserted = vec!["binds {".to_owned()];
    inserted.extend(bind.lines(4));
    inserted.push("}".to_owned());
    Ok(append_lines(text, &inserted))
}

/// Removes a bind by key. Missing keys are a no-op.
pub fn remove_bind(text: &str, key: &str) -> Result<String, PatchError> {
    let found = nodes(text);
    let Some(binds_index) = found
        .iter()
        .position(|node| node.depth == 0 && node.name == "binds" && node.block && node.closed)
    else {
        return Ok(text.to_owned());
    };
    let Some(node) = found
        .iter()
        .find(|node| node.parent == Some(binds_index) && node.name == key)
    else {
        return Ok(text.to_owned());
    };
    replace_span(text, node.start_line, node.end_line, &[])
}

// ------------------------------------------------------------------- rules

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleKind {
    #[default]
    Window,
    Layer,
}

impl RuleKind {
    pub fn node_name(self) -> &'static str {
        match self {
            Self::Window => "window-rule",
            Self::Layer => "layer-rule",
        }
    }
}

/// A rule's body, as dedented raw KDL lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSpec {
    #[serde(skip)]
    pub kind: RuleKind,
    #[serde(default)]
    pub lines: Vec<String>,
}

impl RuleSpec {
    pub fn window(matches: Vec<String>, properties: Vec<String>) -> Self {
        let mut lines = matches
            .into_iter()
            .map(|line| normalize_match_line(&line))
            .collect::<Vec<_>>();
        lines.extend(properties);
        Self {
            kind: RuleKind::Window,
            lines,
        }
    }

    pub fn layer(matches: Vec<String>, properties: Vec<String>) -> Self {
        let mut lines = matches
            .into_iter()
            .map(|line| normalize_match_line(&line))
            .collect::<Vec<_>>();
        lines.extend(properties);
        Self {
            kind: RuleKind::Layer,
            lines,
        }
    }

    pub fn matches(&self) -> impl Iterator<Item = &str> {
        self.lines
            .iter()
            .map(String::as_str)
            .filter(|line| line.starts_with("match ") || line.starts_with("exclude "))
    }

    pub fn summary(&self) -> String {
        if self.lines.is_empty() {
            return "(empty rule)".to_owned();
        }
        self.lines.join("; ")
    }
}

fn normalize_match_line(line: &str) -> String {
    let line = line.trim();
    if line.starts_with("match ") || line.starts_with("exclude ") {
        line.to_owned()
    } else {
        format!("match {line}")
    }
}

/// All rules of `kind`, in file order.
pub fn rules(text: &str, kind: RuleKind) -> Vec<RuleSpec> {
    let found = nodes(text);
    let all_lines: Vec<&str> = text.lines().collect();
    found
        .iter()
        .filter(|node| node.depth == 0 && node.name == kind.node_name() && node.block)
        .map(|node| {
            let mut body: Vec<String> = all_lines
                .iter()
                .take(node.end_line)
                .skip(node.start_line + 1)
                .map(|line| line.to_string())
                .collect();
            let indent = body
                .iter()
                .filter(|line| !line.trim().is_empty())
                .map(|line| line_indent(line))
                .min()
                .unwrap_or(0);
            for line in &mut body {
                if line.len() >= indent {
                    line.drain(..indent);
                }
                while line.ends_with(' ') {
                    line.pop();
                }
            }
            while body.last().is_some_and(|line| line.is_empty()) {
                body.pop();
            }
            RuleSpec { kind, lines: body }
        })
        .collect()
}

/// Appends a rule at the end of the file (later rules win in niri).
pub fn add_rule(text: &str, rule: &RuleSpec) -> Result<String, PatchError> {
    let name = rule.kind.node_name();
    let mut inserted = vec![format!("{name} {{")];
    for line in &rule.lines {
        if line.is_empty() {
            inserted.push(String::new());
        } else {
            inserted.push(format!("    {line}"));
        }
    }
    inserted.push("}".to_owned());
    Ok(append_lines(text, &inserted))
}

/// Removes the `index`-th rule of `kind` (zero based).
pub fn remove_rule(text: &str, kind: RuleKind, index: usize) -> Result<String, PatchError> {
    let found = nodes(text);
    let Some(node) = found
        .iter()
        .filter(|node| node.depth == 0 && node.name == kind.node_name() && node.block)
        .nth(index)
    else {
        return Err(PatchError::Syntax(format!(
            "no {} at index {index}",
            kind.node_name()
        )));
    };
    remove_span_with_blank(text, node.start_line, node.end_line)
}

// --------------------------------------------------------------- startup

/// `spawn-at-startup` commands in file order, one vector per node.
pub fn spawn_at_startup(text: &str) -> Vec<Vec<String>> {
    nodes(text)
        .into_iter()
        .filter(|node| node.depth == 0 && node.name == "spawn-at-startup" && !node.block)
        .map(|node| node.args)
        .collect()
}

/// Appends a `spawn-at-startup` line.
pub fn add_startup(text: &str, command: &[String]) -> Result<String, PatchError> {
    if command.is_empty() {
        return Err(PatchError::Syntax(
            "spawn-at-startup needs a command".to_owned(),
        ));
    }
    let mut line = "spawn-at-startup".to_owned();
    for arg in command {
        line.push(' ');
        line.push_str(&quote_always(arg));
    }
    Ok(append_lines(text, &[line]))
}

/// Removes the `index`-th `spawn-at-startup` node.
pub fn remove_startup(text: &str, index: usize) -> Result<String, PatchError> {
    let found = nodes(text);
    let Some(node) = found
        .iter()
        .filter(|node| node.depth == 0 && node.name == "spawn-at-startup" && !node.block)
        .nth(index)
    else {
        return Err(PatchError::Syntax(format!(
            "no spawn-at-startup at index {index}"
        )));
    };
    remove_span_with_blank(text, node.start_line, node.end_line)
}

// ------------------------------------------------------------- environment

/// Environment variables set by the `environment` block.
pub fn environment(text: &str) -> BTreeMap<String, Option<String>> {
    let mut out = BTreeMap::new();
    let Ok(scan) = scan(text) else {
        return out;
    };
    for key in &scan.keys {
        if key.path.len() == 2 && key.path[0].name == "environment" && key.path[1].args.is_empty() {
            let value = (key.value != "null").then(|| key.value.clone());
            out.insert(key.path[1].name.clone(), value);
        }
    }
    out
}

// ------------------------------------------- block-valued layout settings

fn layout_node(found: &[Node], parent: &[Segment]) -> Option<usize> {
    find_node(found, parent)
}

fn ensure_parent(text: &str, parent: &[Segment]) -> Result<String, PatchError> {
    let found = nodes(text);
    if layout_node(&found, parent).is_some() {
        return Ok(text.to_owned());
    }
    let placeholder = "__better_settings_placeholder__";
    let mut path = parent.to_vec();
    path.push(Segment::new(placeholder));
    let path_text = path
        .iter()
        .map(Segment::to_string)
        .collect::<Vec<_>>()
        .join(".");
    let expanded = patch(text, &Edit::new(path_text.clone(), "1"))?;
    crate::remove_key(&expanded, &path_text)
}

fn set_block(
    text: &str,
    parent: &[Segment],
    name: &str,
    sizes: &[String],
    remove_when_empty: bool,
) -> Result<String, PatchError> {
    let text = ensure_parent(text, parent)?;
    let found = nodes(&text);
    let Some(parent_index) = layout_node(&found, parent) else {
        return Err(PatchError::Syntax(format!(
            "cannot create `{}`",
            parent
                .iter()
                .map(Segment::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )));
    };
    let target = found
        .iter()
        .enumerate()
        .find(|(_, node)| node.parent == Some(parent_index) && node.name == name)
        .map(|(index, _)| index);

    if sizes.is_empty() && remove_when_empty {
        if let Some(index) = target {
            let node = &found[index];
            return remove_span_with_blank(&text, node.start_line, node.end_line);
        }
        return Ok(text);
    }

    if let Some(index) = target {
        let existing: Vec<String> = found
            .iter()
            .filter(|node| node.parent == Some(index))
            .filter_map(|node| {
                let amount = node.args.first()?;
                matches!(node.name.as_str(), "proportion" | "fixed")
                    .then(|| format!("{} {amount}", node.name))
            })
            .collect();
        if existing == sizes {
            return Ok(text);
        }
    }

    let lines: Vec<&str> = text.lines().collect();
    let indent = target
        .and_then(|index| lines.get(found[index].start_line))
        .map_or_else(
            || child_indent(&text, &found, parent_index, 4),
            |line| line_indent(line),
        );
    let block = if sizes.is_empty() {
        // `default-column-width {}` is meaningful: windows choose their size.
        vec![format!("{}{name} {{}}", " ".repeat(indent))]
    } else {
        let mut block = vec![format!("{}{name} {{", " ".repeat(indent))];
        for size in sizes {
            block.push(format!("{}{size}", " ".repeat(indent + 4)));
        }
        block.push(format!("{}}}", " ".repeat(indent)));
        block
    };

    match target {
        Some(index) => {
            let node = &found[index];
            replace_span(&text, node.start_line, node.end_line, &block)
        }
        None => {
            let close = found[parent_index].end_line;
            if !found[parent_index].closed {
                return Err(PatchError::MissingClosingBrace(
                    parent
                        .iter()
                        .map(Segment::to_string)
                        .collect::<Vec<_>>()
                        .join("."),
                ));
            }
            Ok(insert_lines(&text, close, &block))
        }
    }
}

/// The global `layout.default-column-width` value (`proportion 0.5`, `fixed 640`, empty).
pub fn default_column_width(text: &str) -> Option<String> {
    default_column_width_at(text, &[Segment::new("layout")]).unwrap_or(None)
}

pub(crate) fn default_column_width_at(
    text: &str,
    parent: &[Segment],
) -> Result<Option<String>, PatchError> {
    let found = nodes(text);
    let Some(parent_index) = layout_node(&found, parent) else {
        return Ok(None);
    };
    let Some(index) = found.iter().enumerate().position(|(_, node)| {
        node.parent == Some(parent_index) && node.name == "default-column-width"
    }) else {
        return Ok(None);
    };
    if !found[index].block {
        return Ok(None);
    }
    let sizes = found
        .iter()
        .enumerate()
        .filter(|(_, node)| node.parent == Some(index))
        .filter_map(|(_, node)| {
            let amount = node.args.first()?;
            matches!(node.name.as_str(), "proportion" | "fixed")
                .then(|| format!("{} {amount}", node.name))
        })
        .collect::<Vec<_>>();
    Ok(Some(sizes.first().cloned().unwrap_or_default()))
}

/// Sets (or clears) the global `layout.default-column-width` block.
pub fn set_default_column_width(text: &str, value: &str) -> Result<String, PatchError> {
    set_default_column_width_at(text, &[Segment::new("layout")], value)
}

pub(crate) fn set_default_column_width_at(
    text: &str,
    parent: &[Segment],
    value: &str,
) -> Result<String, PatchError> {
    let value = value.trim();
    let sizes = if value.is_empty() {
        Vec::new()
    } else {
        crate::split_size_list(value)?
    };
    set_block(text, parent, "default-column-width", &sizes, false)
}

/// Removes a block-valued setting from `parent`.
pub(crate) fn remove_block_at(
    text: &str,
    parent: &[Segment],
    name: &str,
) -> Result<String, PatchError> {
    let found = nodes(text);
    let Some(parent_index) = layout_node(&found, parent) else {
        return Ok(text.to_owned());
    };
    let Some(node) = found
        .iter()
        .find(|node| node.parent == Some(parent_index) && node.name == name)
    else {
        return Ok(text.to_owned());
    };
    remove_span_with_blank(text, node.start_line, node.end_line)
}

/// The global `layout.preset-column-widths` list.
pub fn preset_column_widths(text: &str) -> Vec<String> {
    preset_column_widths_at(text, &[Segment::new("layout")]).unwrap_or_default()
}

pub(crate) fn preset_column_widths_at(
    text: &str,
    parent: &[Segment],
) -> Result<Vec<String>, PatchError> {
    let found = nodes(text);
    let Some(parent_index) = layout_node(&found, parent) else {
        return Ok(Vec::new());
    };
    let Some(index) = found.iter().enumerate().position(|(_, node)| {
        node.parent == Some(parent_index) && node.name == "preset-column-widths"
    }) else {
        return Ok(Vec::new());
    };
    Ok(found
        .iter()
        .filter(|node| node.parent == Some(index))
        .filter_map(|node| {
            let amount = node.args.first()?;
            matches!(node.name.as_str(), "proportion" | "fixed")
                .then(|| format!("{} {amount}", node.name))
        })
        .collect())
}

/// Adds one preset column width to the global layout.
pub fn add_preset_column_width(text: &str, value: &str) -> Result<String, PatchError> {
    let mut sizes = preset_column_widths(text);
    let normalized = crate::split_size_list(value)?;
    if normalized.is_empty() {
        return Err(PatchError::Syntax("empty preset size".to_owned()));
    }
    sizes.extend(normalized);
    set_preset_column_widths_at(text, &[Segment::new("layout")], &sizes)
}

/// Removes the `index`-th preset column width from the global layout.
pub fn remove_preset_column_width(text: &str, index: usize) -> Result<String, PatchError> {
    let mut sizes = preset_column_widths(text);
    if index >= sizes.len() {
        return Err(PatchError::Syntax(format!(
            "no preset column width at index {index}"
        )));
    }
    sizes.remove(index);
    set_preset_column_widths_at(text, &[Segment::new("layout")], &sizes)
}

/// Replaces the global `layout.preset-column-widths` block.
pub fn set_preset_column_widths(text: &str, sizes: &[String]) -> Result<String, PatchError> {
    set_preset_column_widths_at(text, &[Segment::new("layout")], sizes)
}

pub(crate) fn set_preset_column_widths_at(
    text: &str,
    parent: &[Segment],
    sizes: &[String],
) -> Result<String, PatchError> {
    set_block(text, parent, "preset-column-widths", sizes, true)
}

// ---------------------------------------------------------------- validate

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    pub blocks: usize,
    pub keys: usize,
    pub errors: Vec<String>,
}

impl Validation {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

impl fmt::Display for Validation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ok() {
            write!(formatter, "ok: {} blocks, {} keys", self.blocks, self.keys)
        } else {
            write!(formatter, "{}", self.errors.join("\n"))
        }
    }
}

/// Structural validation plus a catalog round-trip check.
pub fn validate(text: &str) -> Validation {
    let mut errors = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut in_block_comment = false;
    let mut depth: i64 = 0;

    for (line_index, line) in text.lines().enumerate() {
        let (code, content, unterminated) = blank_code(line, &mut in_block_comment);
        if unterminated {
            errors.push(format!("line {}: unterminated string", line_index + 1));
        }
        let mut index = 0;
        let mut segment = 0;
        while index < code.len() {
            let ch = code[index..].chars().next().unwrap();
            match ch {
                '{' => {
                    let (start, end) = trim_bounds(&content, segment, index);
                    let header = if start < end {
                        tokens(&line[start..end])
                            .first()
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    stack.push(header);
                    depth += 1;
                    index += 1;
                    segment = index;
                }
                '}' => {
                    depth -= 1;
                    if depth < 0 {
                        errors.push(format!("line {}: unexpected closing brace", line_index + 1));
                        depth = 0;
                    }
                    stack.pop();
                    index += 1;
                    segment = index;
                }
                ';' => {
                    index += 1;
                    segment = index;
                }
                other => index += other.len_utf8(),
            }
        }
    }
    if in_block_comment {
        errors.push("unterminated block comment".to_owned());
    }
    if depth > 0 {
        let names: Vec<_> = stack
            .iter()
            .filter(|name| !name.is_empty())
            .cloned()
            .collect();
        if names.is_empty() {
            errors.push("unclosed block".to_owned());
        } else {
            errors.push(format!("unclosed block `{}`", names.join(" > ")));
        }
    }

    let (blocks, keys) = match scan(text) {
        Ok(scan) => (scan.blocks.len(), scan.keys.len()),
        Err(err) => {
            errors.push(err.to_string());
            (0, 0)
        }
    };

    if errors.is_empty() {
        for spec in catalog_for(text) {
            if let Some(segment) = spec.path.rsplit('.').next()
                && (segment == "default-column-width" || segment == "preset-column-widths")
            {
                continue;
            }
            let Ok(Some(value)) = current(text, &spec.path) else {
                continue;
            };
            if spec.kind == Kind::Flag && value != "true" {
                continue;
            }
            let edit = if spec.kind == Kind::Flag {
                Edit::flag(spec.path.clone(), true)
            } else if spec.kind == Kind::Raw {
                Edit::raw(spec.path.clone(), value)
            } else {
                Edit::new(spec.path.clone(), value)
            };
            match patch(text, &edit) {
                Ok(patched) if patched == text => {}
                Ok(_) => errors.push(format!("round-trip changed `{}`", spec.path)),
                Err(err) => errors.push(format!("{}: {err}", spec.path)),
            }
        }
    }

    Validation {
        blocks,
        keys,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"input {
    keyboard {
        repeat-delay 300 // fast
    }
}

layout {
    gaps 16
    default-column-width { proportion 0.5; }
    preset-column-widths {
        proportion 0.33333
        proportion 0.5
    }
}

binds {
    Mod+Left { focus-column-left; }
    Mod+T repeat=false cooldown-ms=500 {
        spawn "alacritty";
        focus-column-right;
    }
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
    fn parses_binds_with_flags_and_multi_action() {
        let parsed = binds(CONFIG);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].key, "Mod+Left");
        assert_eq!(parsed[0].actions, vec!["focus-column-left"]);
        assert_eq!(parsed[1].key, "Mod+T");
        assert_eq!(parsed[1].repeat, Some(false));
        assert_eq!(parsed[1].cooldown_ms, Some(500));
        assert_eq!(
            parsed[1].actions,
            vec!["spawn \"alacritty\"", "focus-column-right"]
        );
    }

    #[test]
    fn adds_and_removes_binds_without_touching_neighbours() {
        let bind = Bind {
            cooldown_ms: Some(250),
            repeat: Some(false),
            allow_when_locked: Some(true),
            ..Bind::new("Mod+Shift+Q", vec!["spawn \"foot\"".into()])
        };
        let patched = set_bind(CONFIG, &bind).unwrap();
        assert!(
            patched.contains(
                "    Mod+Shift+Q repeat=false cooldown-ms=250 allow-when-locked=true { spawn \"foot\"; }"
            ),
            "{patched}"
        );
        assert!(patched.contains("Mod+Left { focus-column-left; }"));
        assert!(patched.contains("// fast"));
        let removed = remove_bind(&patched, "Mod+Shift+Q").unwrap();
        assert_eq!(removed, CONFIG);
    }

    #[test]
    fn adding_an_existing_bind_replaces_it() {
        let bind = Bind::new("Mod+Left", vec!["focus-column-right".into()]);
        let patched = set_bind(CONFIG, &bind).unwrap();
        assert!(patched.contains("Mod+Left { focus-column-right; }"));
        assert!(!patched.contains("focus-column-left"));
    }

    #[test]
    fn rules_round_trip_and_remove_by_index() {
        let rules_now = rules(CONFIG, RuleKind::Window);
        assert_eq!(rules_now.len(), 1);
        assert_eq!(
            rules_now[0].lines,
            vec!["match app-id=\"Alacritty\"", "open-floating true"]
        );
        let added = add_rule(
            CONFIG,
            &RuleSpec::window(
                vec!["app-id=\"firefox\"".into()],
                vec!["open-maximized true".into()],
            ),
        )
        .unwrap();
        assert!(
            added.contains(
                "window-rule {\n    match app-id=\"firefox\"\n    open-maximized true\n}"
            )
        );
        let removed = remove_rule(&added, RuleKind::Window, 1).unwrap();
        assert_eq!(removed, CONFIG);
    }

    #[test]
    fn startup_and_environment_are_lists() {
        assert_eq!(spawn_at_startup(CONFIG), vec![vec!["waybar".to_owned()]]);
        let added = add_startup(
            CONFIG,
            &["alacritty".into(), "--class".into(), "term".into()],
        )
        .unwrap();
        assert!(added.contains("spawn-at-startup \"alacritty\" \"--class\" \"term\""));
        let removed = remove_startup(&added, 1).unwrap();
        assert_eq!(removed, CONFIG);

        let env = environment(CONFIG);
        assert_eq!(
            env.get("QT_QPA_PLATFORM"),
            Some(&Some("wayland".to_owned()))
        );
    }

    #[test]
    fn preset_and_default_widths_are_editable() {
        assert_eq!(
            default_column_width(CONFIG).as_deref(),
            Some("proportion 0.5")
        );
        assert_eq!(
            preset_column_widths(CONFIG),
            vec!["proportion 0.33333", "proportion 0.5"]
        );
        let patched = set_default_column_width(CONFIG, "fixed 640").unwrap();
        assert!(patched.contains("default-column-width {\n        fixed 640\n    }"));
        assert!(patched.contains("preset-column-widths {"));
        let added = add_preset_column_width(CONFIG, "fixed 800").unwrap();
        assert!(added.contains("fixed 800"));
        let removed = remove_preset_column_width(&added, 2).unwrap();
        assert_eq!(removed, CONFIG);
    }

    #[test]
    fn validate_accepts_the_sample_and_rejects_broken_text() {
        assert!(validate(CONFIG).ok(), "{:?}", validate(CONFIG).errors);
        assert!(!validate("layout {\n    gaps 16\n").ok());
        assert!(!validate("layout {\n    gaps \"oops\n}\n").ok());
        assert!(!validate("}\n").ok());
    }
}
