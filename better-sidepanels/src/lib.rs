//! `better-sidepanels`: persistent layer-shell side panels for niri.
//!
//! The library holds everything that can be tested without a compositor:
//! configuration, module refresh, layout/hit testing and the cairo software
//! renderer. `main.rs` is the thin Wayland layer-shell daemon on top.
//!
//! Panels are a vertical list of rows. A row has a label, an optional click
//! command, and an optional module (a shell command whose first output line is
//! shown on the right and refreshed every `interval_secs`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface};
use serde::{Deserialize, Serialize};

pub const HEADER_HEIGHT: u32 = 46;
pub const ROW_GAP: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Parses `#rgb`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let hex = text.trim().trim_start_matches('#');
        let digits: Vec<u8> = hex
            .chars()
            .map(|ch| ch.to_digit(16).map(|digit| digit as u8))
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| format!("`{text}` is not a hex colour"))?;
        match digits.len() {
            3 => Ok(Self {
                r: digits[0] * 17,
                g: digits[1] * 17,
                b: digits[2] * 17,
                a: 255,
            }),
            6 | 8 => {
                let byte = |index: usize| digits[index] * 16 + digits[index + 1];
                Ok(Self {
                    r: byte(0),
                    g: byte(2),
                    b: byte(4),
                    a: if digits.len() == 8 { byte(6) } else { 255 },
                })
            }
            _ => Err(format!("`{text}` must be #rgb, #rrggbb or #rrggbbaa")),
        }
    }

    fn cairo(self) -> (f64, f64, f64, f64) {
        (
            self.r as f64 / 255.,
            self.g as f64 / 255.,
            self.b as f64 / 255.,
            self.a as f64 / 255.,
        )
    }

    fn with_alpha(self, alpha: f64) -> (f64, f64, f64, f64) {
        let (r, g, b, a) = self.cairo();
        (r, g, b, a * alpha)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default = "default_bg")]
    pub bg: String,
    #[serde(default = "default_fg")]
    pub fg: String,
    #[serde(default = "default_dim")]
    pub dim: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default = "default_row")]
    pub row: String,
}

fn default_bg() -> String {
    "#14161b".to_owned()
}
fn default_fg() -> String {
    "#e6e6e6".to_owned()
}
fn default_dim() -> String {
    "#9aa0a6".to_owned()
}
fn default_accent() -> String {
    "#4f9cf9".to_owned()
}
fn default_row() -> String {
    "#1d2027".to_owned()
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: default_bg(),
            fg: default_fg(),
            dim: default_dim(),
            accent: default_accent(),
            row: default_row(),
        }
    }
}

impl Theme {
    pub fn bg(&self) -> Result<Rgba, String> {
        Rgba::parse(&self.bg)
    }
    pub fn fg(&self) -> Result<Rgba, String> {
        Rgba::parse(&self.fg)
    }
    pub fn dim(&self) -> Result<Rgba, String> {
        Rgba::parse(&self.dim)
    }
    pub fn accent(&self) -> Result<Rgba, String> {
        Rgba::parse(&self.accent)
    }
    pub fn row(&self) -> Result<Rgba, String> {
        Rgba::parse(&self.row)
    }

    fn colors(&self) -> Result<Colors, String> {
        Ok(Colors {
            bg: self.bg()?,
            fg: self.fg()?,
            dim: self.dim()?,
            accent: self.accent()?,
            row: self.row()?,
        })
    }
}

struct Colors {
    bg: Rgba,
    fg: Rgba,
    dim: Rgba,
    accent: Rgba,
    row: Rgba,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleSpec {
    pub command: String,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
}

fn default_interval() -> u64 {
    30
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleSpec>,
}

impl Row {
    pub fn label(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: None,
            module: None,
        }
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_module(mut self, command: impl Into<String>, interval_secs: u64) -> Self {
        self.module = Some(ModuleSpec {
            command: command.into(),
            interval_secs,
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelConfig {
    #[serde(default)]
    pub side: Side,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_margin")]
    pub margin: u32,
    #[serde(default = "default_padding")]
    pub padding: u32,
    #[serde(default = "default_row_height")]
    pub row_height: u32,
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub rows: Vec<Row>,
}

fn default_width() -> u32 {
    260
}
fn default_margin() -> u32 {
    8
}
fn default_padding() -> u32 {
    12
}
fn default_row_height() -> u32 {
    34
}
fn default_title() -> String {
    "BETTER".to_owned()
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            side: Side::Left,
            width: default_width(),
            margin: default_margin(),
            padding: default_padding(),
            row_height: default_row_height(),
            title: default_title(),
            theme: Theme::default(),
            rows: vec![
                Row::label("Workspaces").with_command("niri msg workspaces"),
                Row::label("Clock").with_module("date +%H:%M", 10),
            ],
        }
    }
}

impl PanelConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|err| format!("cannot parse {}: {err}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(format!("cannot read {}: {err}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| format!("cannot serialize config: {err}"))?;
        std::fs::write(path, bytes).map_err(|err| format!("cannot write {}: {err}", path.display()))
    }

    /// Panel width including margins; used as the exclusive zone.
    pub fn exclusive_zone(&self) -> u32 {
        self.width + self.margin
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_default();
    base.join("better-sidepanels/config.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x as f64
            && x < (self.x + self.width) as f64
            && y >= self.y as f64
            && y < (self.y + self.height) as f64
    }
}

/// Runtime panel state: config plus refreshed module output.
#[derive(Debug, Clone)]
pub struct Panel {
    pub config: PanelConfig,
    outputs: Vec<String>,
    updated: Vec<Option<Instant>>,
}

impl Panel {
    pub fn new(config: PanelConfig) -> Self {
        let outputs = vec![String::new(); config.rows.len()];
        let updated = vec![None; config.rows.len()];
        Self {
            config,
            outputs,
            updated,
        }
    }

    pub fn output(&self, index: usize) -> Option<&str> {
        self.outputs.get(index).map(String::as_str)
    }

    /// Runs every module whose interval elapsed. Returns true if anything
    /// changed (so the caller can redraw).
    pub fn refresh(&mut self) -> bool {
        self.refresh_at(Instant::now())
    }

    pub fn refresh_at(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for (index, row) in self.config.rows.iter().enumerate() {
            let Some(module) = &row.module else {
                continue;
            };
            let due = match self.updated[index] {
                None => true,
                Some(last) => now.duration_since(last) >= Duration::from_secs(module.interval_secs),
            };
            if !due {
                continue;
            }
            let output = run_module(&module.command);
            if self.outputs[index] != output {
                self.outputs[index] = output;
                changed = true;
            }
            self.updated[index] = Some(now);
        }
        changed
    }

    /// Runs every module immediately (startup and config reload).
    pub fn refresh_all(&mut self) -> bool {
        for index in 0..self.config.rows.len() {
            self.updated[index] = None;
        }
        self.refresh()
    }

    /// Row rectangles in panel-local coordinates.
    pub fn row_rects(&self) -> Vec<Rect> {
        let inset = self.config.padding;
        let width = self.config.width.saturating_sub(inset * 2);
        let height = self.config.row_height;
        self.config
            .rows
            .iter()
            .enumerate()
            .map(|(index, _)| Rect {
                x: inset,
                y: HEADER_HEIGHT + index as u32 * (height + ROW_GAP),
                width,
                height,
            })
            .collect()
    }

    pub fn hit(&self, x: f64, y: f64) -> Option<usize> {
        self.row_rects().iter().position(|rect| rect.contains(x, y))
    }

    /// Executes a row's click command, if it has one.
    pub fn activate(&self, index: usize) -> Result<Option<String>, String> {
        let row = self
            .config
            .rows
            .get(index)
            .ok_or_else(|| format!("row {index} does not exist"))?;
        match &row.command {
            Some(command) => {
                Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .spawn()
                    .map_err(|err| format!("cannot run `{command}`: {err}"))?;
                Ok(Some(command.clone()))
            }
            None => Ok(None),
        }
    }

    /// Renders the panel into an ARGB32 buffer.
    pub fn render(&self, width: u32, height: u32) -> Result<PanelImage, String> {
        self.render_with(width, height, None)
    }

    pub fn render_with(
        &self,
        width: u32,
        height: u32,
        hovered: Option<usize>,
    ) -> Result<PanelImage, String> {
        if width == 0 || height == 0 {
            return Err("cannot render an empty panel".to_owned());
        }
        let colors = self.config.theme.colors()?;
        let stride = width as i32 * 4;
        let surface = ImageSurface::create_for_data(
            vec![0u8; (width * height * 4) as usize],
            Format::ARgb32,
            width as i32,
            height as i32,
            stride,
        )
        .map_err(|err| format!("cannot create surface: {err}"))?;

        {
            let ctx = Context::new(&surface).map_err(|err| format!("cairo: {err}"))?;
            let (r, g, b, a) = colors.bg.cairo();
            ctx.set_source_rgba(r, g, b, a);
            ctx.paint().map_err(|err| format!("cairo: {err}"))?;

            let padding = self.config.padding as f64;
            // Header title.
            ctx.set_source_rgba_tuple(colors.fg.cairo());
            ctx.select_font_face("Sans", FontSlant::Normal, FontWeight::Bold);
            ctx.set_font_size(15.);
            ctx.move_to(padding, padding + 16.);
            ctx.show_text(&self.config.title)
                .map_err(|err| format!("cairo: {err}"))?;

            // Accent line under the header.
            ctx.set_source_rgba_tuple(colors.accent.cairo());
            ctx.rectangle(0., HEADER_HEIGHT as f64 - 1., width as f64, 1.);
            ctx.fill().map_err(|err| format!("cairo: {err}"))?;

            for (index, rect) in self.row_rects().iter().enumerate() {
                self.paint_row(&ctx, index, rect, hovered == Some(index), &colors)?;
            }
        }

        let data = surface
            .take_data()
            .map_err(|_| "cannot take surface data".to_owned())?;
        Ok(PanelImage {
            width,
            height,
            stride: stride as usize,
            data: data.as_ref().to_vec(),
        })
    }

    fn paint_row(
        &self,
        ctx: &Context,
        index: usize,
        rect: &Rect,
        hovered: bool,
        colors: &Colors,
    ) -> Result<(), String> {
        let x = rect.x as f64;
        let y = rect.y as f64;
        let w = rect.width as f64;
        let h = rect.height as f64;

        let row_color = if hovered { colors.accent } else { colors.row };
        let alpha = if hovered { 0.35 } else { 1.0 };
        ctx.set_source_rgba_tuple(row_color.with_alpha(alpha));
        ctx.rectangle(x, y, w, h);
        ctx.fill().map_err(|err| format!("cairo: {err}"))?;

        // Accent stripe on the panel's outer edge.
        let stripe = 3.;
        let stripe_x = match self.config.side {
            Side::Left => x,
            Side::Right => x + w - stripe,
        };
        ctx.set_source_rgba_tuple(colors.accent.cairo());
        ctx.rectangle(stripe_x, y, stripe, h);
        ctx.fill().map_err(|err| format!("cairo: {err}"))?;

        let row = &self.config.rows[index];
        let text_x = x + 12.;
        let baseline = y + h / 2. + 5.;

        ctx.set_source_rgba_tuple(colors.fg.cairo());
        ctx.select_font_face("Sans", FontSlant::Normal, FontWeight::Normal);
        ctx.set_font_size(13.);
        let available = w - 24. - if row.module.is_some() { 80. } else { 0. };
        let label = truncate(ctx, &row.label, available.max(20.))?;
        ctx.move_to(text_x, baseline);
        ctx.show_text(&label)
            .map_err(|err| format!("cairo: {err}"))?;

        if row.module.is_some() {
            if let Some(output) = self.outputs.get(index).filter(|out| !out.is_empty()) {
                ctx.set_source_rgba_tuple(colors.dim.cairo());
                ctx.set_font_size(12.);
                let text = truncate(ctx, output, 90.)?;
                let extents = ctx
                    .text_extents(&text)
                    .map_err(|err| format!("cairo: {err}"))?;
                ctx.move_to(x + w - 12. - extents.width(), baseline);
                ctx.show_text(&text)
                    .map_err(|err| format!("cairo: {err}"))?;
            }
        }
        Ok(())
    }
}

trait CairoSource {
    fn set_source_rgba_tuple(&self, color: (f64, f64, f64, f64));
}

impl CairoSource for Context {
    fn set_source_rgba_tuple(&self, color: (f64, f64, f64, f64)) {
        self.set_source_rgba(color.0, color.1, color.2, color.3);
    }
}

fn truncate(ctx: &Context, text: &str, max_width: f64) -> Result<String, String> {
    let extents = ctx
        .text_extents(text)
        .map_err(|err| format!("cairo: {err}"))?;
    if extents.width() <= max_width {
        return Ok(text.to_owned());
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push_str("...");
        let extents = ctx
            .text_extents(&candidate)
            .map_err(|err| format!("cairo: {err}"))?;
        if extents.width() > max_width {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    Ok(out)
}

fn run_module(command: &str) -> String {
    let output = Command::new("sh").arg("-c").arg(command).output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(48)
            .collect(),
        _ => String::new(),
    }
}

/// A rendered panel: ARGB32 bytes, row-major, `stride` bytes per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelImage {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub data: Vec<u8>,
}

impl PanelImage {
    /// Pixel as `[r, g, b, a]` (the buffer is little-endian ARGB32).
    pub fn rgba(&self, x: u32, y: u32) -> [u8; 4] {
        let index = y as usize * self.stride + x as usize * 4;
        let pixel = &self.data[index..index + 4];
        [pixel[2], pixel[1], pixel[0], pixel[3]]
    }

    pub fn is_solid(&self, width: u32, height: u32) -> bool {
        let first = self.rgba(0, 0);
        (0..width).all(|x| (0..height).all(|y| self.rgba(x, y) == first))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("better-sidepanels-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn panel() -> Panel {
        let config = PanelConfig {
            rows: vec![
                Row::label("Workspaces").with_command("true"),
                Row::label("Volume").with_module("echo 42%", 1),
                Row::label("Network"),
            ],
            ..PanelConfig::default()
        };
        Panel::new(config)
    }

    #[test]
    fn parses_hex_colours() {
        assert_eq!(Rgba::parse("#fff").unwrap(), Rgba::rgb(255, 255, 255));
        assert_eq!(Rgba::parse("1d2027").unwrap(), Rgba::rgb(0x1d, 0x20, 0x27));
        assert_eq!(
            Rgba::parse("#11223380").unwrap(),
            Rgba {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 0x80
            }
        );
        assert!(Rgba::parse("#12").is_err());
        assert!(Rgba::parse("#zzzzzz").is_err());
    }

    #[test]
    fn config_round_trips_and_uses_defaults() {
        let dir = temp_dir("config");
        let path = dir.join("config.json");
        let config = PanelConfig {
            side: Side::Right,
            width: 300,
            rows: vec![Row::label("Custom")],
            ..PanelConfig::default()
        };
        config.save(&path).unwrap();

        let loaded = PanelConfig::load(&path).unwrap();
        assert_eq!(loaded, config);

        std::fs::write(&path, r#"{"rows":[{"label":"Only"}]}"#).unwrap();
        let partial = PanelConfig::load(&path).unwrap();
        assert_eq!(partial.width, default_width());
        assert_eq!(partial.side, Side::Left);
        assert_eq!(partial.rows[0].label, "Only");

        let missing = PanelConfig::load(&dir.join("missing.json")).unwrap();
        assert_eq!(missing, PanelConfig::default());
    }

    #[test]
    fn exclusive_zone_covers_panel_and_margin() {
        let config = PanelConfig {
            width: 240,
            margin: 10,
            ..PanelConfig::default()
        };
        assert_eq!(config.exclusive_zone(), 250);
    }

    #[test]
    fn layout_stacks_rows_and_hit_tests_them() {
        let panel = panel();
        let rects = panel.row_rects();
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0].y, HEADER_HEIGHT);
        assert_eq!(rects[1].y, HEADER_HEIGHT + 34 + ROW_GAP);
        assert_eq!(rects[0].x, 12);
        assert_eq!(rects[0].width, 260 - 24);

        let rect = rects[1];
        let inside = panel.hit(rect.x as f64 + 1., rect.y as f64 + 1.);
        assert_eq!(inside, Some(1));
        assert_eq!(panel.hit(0., 0.), None);
        assert_eq!(panel.hit(rect.x as f64 + rect.width as f64 + 1., 60.), None);
    }

    #[test]
    fn module_output_is_refreshed_and_throttled() {
        let dir = temp_dir("module");
        let counter = dir.join("counter");
        let command = format!("echo run >> {}; echo hello", counter.display());
        let config = PanelConfig {
            rows: vec![Row::label("A").with_module(command, 60)],
            ..PanelConfig::default()
        };
        let mut panel = Panel::new(config);
        let start = Instant::now();
        assert!(panel.refresh_at(start));
        assert_eq!(panel.output(0), Some("hello"));
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            1
        );

        // Within the interval nothing runs again.
        assert!(!panel.refresh_at(start + Duration::from_secs(10)));
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            1
        );

        // Past the interval it runs again, even if the value is unchanged.
        panel.refresh_at(start + Duration::from_secs(61));
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().lines().count(),
            2
        );
    }

    #[test]
    fn failing_module_is_blank() {
        let config = PanelConfig {
            rows: vec![Row::label("A").with_module("exit 3", 60)],
            ..PanelConfig::default()
        };
        let mut panel = Panel::new(config);
        panel.refresh();
        assert_eq!(panel.output(0), Some(""));
    }

    #[test]
    fn activate_runs_commands_only_for_rows_with_one() {
        let dir = temp_dir("activate");
        let marker = dir.join("ran");
        let config = PanelConfig {
            rows: vec![
                Row::label("Run").with_command(format!("touch {}", marker.display())),
                Row::label("Inert"),
            ],
            ..PanelConfig::default()
        };
        let panel = Panel::new(config);
        assert_eq!(panel.activate(1).unwrap(), None);
        let command = panel.activate(0).unwrap().unwrap();
        assert!(command.contains("touch"));
        // Wait for the detached command.
        for _ in 0..50 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(marker.exists());
        assert!(panel.activate(9).is_err());
    }

    #[test]
    fn renders_background_header_and_rows() {
        let panel = panel();
        let image = panel.render(260, 200).unwrap();
        assert_eq!(image.width, 260);
        assert_eq!(image.height, 200);
        assert_eq!(image.data.len(), 260 * 200 * 4);
        // Background pixel matches the theme.
        assert_eq!(image.rgba(250, 190), [0x14, 0x16, 0x1b, 0xff]);
        // Row background matches the row colour.
        let rect = panel.row_rects()[0];
        assert_eq!(
            image.rgba(rect.x + 30, rect.y + 5),
            [0x1d, 0x20, 0x27, 0xff]
        );
        // Text and the accent stripe introduce other colours.
        assert!(!image.is_solid(260, 200));
        let accent = image.rgba(rect.x + 1, rect.y + 5);
        assert_eq!(accent, [0x4f, 0x9c, 0xf9, 0xff]);
    }

    #[test]
    fn hover_changes_the_row_and_right_side_flips_the_stripe() {
        let panel = panel();
        let plain = panel.render(260, 200).unwrap();
        let hovered = panel.render_with(260, 200, Some(1)).unwrap();
        let rect = panel.row_rects()[1];
        assert_ne!(
            plain.rgba(rect.x + 60, rect.y + 10),
            hovered.rgba(rect.x + 60, rect.y + 10)
        );

        let mut config = panel.config.clone();
        config.side = Side::Right;
        let right = Panel::new(config).render(260, 200).unwrap();
        let stripe = right.rgba(rect.x + rect.width - 1, rect.y + 5);
        assert_eq!(stripe, [0x4f, 0x9c, 0xf9, 0xff]);
        let left_of_stripe = right.rgba(rect.x, rect.y + 5);
        assert_eq!(left_of_stripe, [0x1d, 0x20, 0x27, 0xff]);
    }

    #[test]
    fn module_output_is_drawn() {
        let config = PanelConfig {
            rows: vec![Row::label("Vol").with_module("echo 42%", 60)],
            ..PanelConfig::default()
        };
        let mut panel = Panel::new(config);
        panel.refresh();
        let blank = {
            let mut copy = panel.clone();
            copy.outputs[0] = String::new();
            copy.render(260, 120).unwrap()
        };
        let filled = panel.render(260, 120).unwrap();
        assert_ne!(blank.data, filled.data);
    }

    #[test]
    fn rejects_empty_render_size() {
        assert!(panel().render(0, 10).is_err());
        assert!(panel().render(10, 0).is_err());
    }
}
