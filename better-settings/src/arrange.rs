//! Pure display-arrangement geometry shared by the GTK editor and tests.
//!
//! The model stores each connected output as a logical rectangle (position and
//! size in niri's logical pixels). Everything the editor does — picking a
//! display, dragging, snapping, validating and producing the `better-monitor`
//! plan — is pure arithmetic here, so it can be tested without a display
//! server. The GTK widget is a thin renderer over this module.

use serde::Serialize;
use serde_json::{Map, Value, json};

/// Largest absolute logical coordinate accepted, matching better-monitor.
pub const MAX_COORDINATE: i32 = 1_000_000;
/// Default snap distance in logical pixels.
pub const SNAP_THRESHOLD: i32 = 16;
/// Default grid step in logical pixels.
pub const GRID: i32 = 10;

/// One output in the arrangement editor.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DisplayBox {
    pub output: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub enabled: bool,
}

impl DisplayBox {
    pub fn rectangle(&self) -> (i32, i32, i32, i32) {
        (self.x, self.y, self.width, self.height)
    }
}

/// The connected displays and their current logical arrangement.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Arrangement {
    pub displays: Vec<DisplayBox>,
}

/// Smallest set of moves that makes the editor's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragState {
    pub output: String,
    /// Pointer position relative to the display's logical origin.
    pub grab_x: i32,
    pub grab_y: i32,
}

/// Result of snapping a dragged display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapResult {
    pub x: i32,
    pub y: i32,
    pub snapped_x: bool,
    pub snapped_y: bool,
}

impl Arrangement {
    /// Parses `better-monitor position --json` output (envelope or data).
    pub fn from_monitor_json(value: &Value) -> Result<Self, String> {
        let data = value.get("data").unwrap_or(value);
        let positions = data
            .get("positions")
            .or_else(|| data.as_array().map(|_| data))
            .ok_or_else(|| "position JSON has no `positions` array".to_owned())?;
        let positions = positions
            .as_array()
            .ok_or_else(|| "`positions` is not an array".to_owned())?;

        let mut displays = Vec::new();
        for entry in positions {
            let output = entry
                .get("output")
                .and_then(Value::as_str)
                .filter(|output| !output.is_empty())
                .ok_or_else(|| "a position entry has no output name".to_owned())?;
            let number = |key: &str| entry.get(key).and_then(Value::as_i64).unwrap_or(0);
            let x =
                i32::try_from(number("x")).map_err(|_| format!("{output}: x is out of range"))?;
            let y =
                i32::try_from(number("y")).map_err(|_| format!("{output}: y is out of range"))?;
            let width = i32::try_from(number("width")).unwrap_or(0).max(0);
            let height = i32::try_from(number("height")).unwrap_or(0).max(0);
            displays.push(DisplayBox {
                output: output.to_owned(),
                x,
                y,
                width,
                height,
                scale: entry.get("scale").and_then(Value::as_f64).unwrap_or(1.0),
                enabled: entry
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(width > 0 && height > 0),
            });
        }
        displays.sort_by(|a, b| a.output.cmp(&b.output));
        Ok(Self { displays })
    }

    pub fn get(&self, output: &str) -> Option<&DisplayBox> {
        self.displays
            .iter()
            .find(|display| display.output == output)
    }

    pub fn get_mut(&mut self, output: &str) -> Option<&mut DisplayBox> {
        self.displays
            .iter_mut()
            .find(|display| display.output == output)
    }

    /// Topmost display under a logical point. Later displays win, matching the
    /// paint order.
    pub fn pick(&self, x: i32, y: i32) -> Option<&DisplayBox> {
        self.displays.iter().rev().find(|display| {
            display.enabled
                && x >= display.x
                && x < display.x + display.width
                && y >= display.y
                && y < display.y + display.height
        })
    }

    pub fn begin_drag(&self, x: i32, y: i32) -> Option<DragState> {
        let display = self.pick(x, y)?;
        Some(DragState {
            output: display.output.clone(),
            grab_x: x - display.x,
            grab_y: y - display.y,
        })
    }

    /// Moves the dragged display to the pointer position, applying snapping.
    pub fn drag_to(
        &mut self,
        drag: &DragState,
        pointer_x: i32,
        pointer_y: i32,
        threshold: i32,
    ) -> Option<SnapResult> {
        let width = self.get(&drag.output)?.width;
        let height = self.get(&drag.output)?.height;
        let proposed_x = pointer_x - drag.grab_x;
        let proposed_y = pointer_y - drag.grab_y;
        let snap = self.snap_position(
            &drag.output,
            proposed_x,
            proposed_y,
            width,
            height,
            threshold,
        );
        let display = self.get_mut(&drag.output)?;
        display.x = snap.x;
        display.y = snap.y;
        Some(snap)
    }

    /// Snaps a proposed position to other displays' edges/centres and a grid.
    ///
    /// The display being moved is ignored. A non-grid alignment wins over the
    /// grid when both are within `threshold`; ties prefer the smaller move.
    #[allow(clippy::too_many_arguments)]
    pub fn snap_position(
        &self,
        moving: &str,
        proposed_x: i32,
        proposed_y: i32,
        width: i32,
        height: i32,
        threshold: i32,
    ) -> SnapResult {
        let threshold = threshold.max(0);
        let mut x_candidates: Vec<i32> = Vec::new();
        let mut y_candidates: Vec<i32> = Vec::new();
        for display in &self.displays {
            if display.output == moving || !display.enabled {
                continue;
            }
            x_candidates.extend([
                display.x,
                display.x + display.width,
                display.x - width,
                display.x + display.width - width,
                display.x + display.width / 2 - width / 2,
            ]);
            y_candidates.extend([
                display.y,
                display.y + display.height,
                display.y - height,
                display.y + display.height - height,
                display.y + display.height / 2 - height / 2,
            ]);
        }

        let grid_x = (f64::from(proposed_x) / f64::from(GRID)).round() as i32 * GRID;
        let grid_y = (f64::from(proposed_y) / f64::from(GRID)).round() as i32 * GRID;

        // Edges and centres take precedence over the grid, so dragging next to
        // another output aligns cleanly even when a grid line is closer.
        let (x, snapped_x) = match best_candidate(proposed_x, &x_candidates, threshold) {
            Some(candidate) => (candidate, true),
            None if (grid_x - proposed_x).abs() <= threshold => (grid_x, true),
            None => (proposed_x, false),
        };
        let (y, snapped_y) = match best_candidate(proposed_y, &y_candidates, threshold) {
            Some(candidate) => (candidate, true),
            None if (grid_y - proposed_y).abs() <= threshold => (grid_y, true),
            None => (proposed_y, false),
        };
        SnapResult {
            x,
            y,
            snapped_x,
            snapped_y,
        }
    }

    /// Moves one display without dragging, clamping to sane coordinates.
    pub fn move_to(&mut self, output: &str, x: i32, y: i32) -> bool {
        let Some(display) = self.get_mut(output) else {
            return false;
        };
        display.x = x.clamp(-MAX_COORDINATE, MAX_COORDINATE);
        display.y = y.clamp(-MAX_COORDINATE, MAX_COORDINATE);
        true
    }

    /// Outputs whose logical rectangles overlap (touching edges are fine).
    pub fn overlaps(&self) -> Vec<(String, String)> {
        let enabled: Vec<&DisplayBox> = self
            .displays
            .iter()
            .filter(|display| display.enabled && display.width > 0 && display.height > 0)
            .collect();
        let mut overlaps = Vec::new();
        for (index, first) in enabled.iter().enumerate() {
            for second in enabled.iter().skip(index + 1) {
                if intersects(first, second) {
                    overlaps.push((first.output.clone(), second.output.clone()));
                }
            }
        }
        overlaps
    }

    /// Validation shared by the preview/Apply path and the unit tests.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.displays.is_empty() {
            errors.push("no connected outputs to arrange".to_owned());
        }
        if !self.displays.iter().any(|display| display.enabled) {
            errors.push("at least one output must stay enabled".to_owned());
        }
        for (index, display) in self.displays.iter().enumerate() {
            if display.output.trim().is_empty() {
                errors.push(format!("display {index} has no output name"));
            }
            if i64::from(display.x).abs() > i64::from(MAX_COORDINATE)
                || i64::from(display.y).abs() > i64::from(MAX_COORDINATE)
            {
                errors.push(format!(
                    "output `{}` has an implausible position {},{}",
                    display.output, display.x, display.y
                ));
            }
            if display.width <= 0 || display.height <= 0 {
                errors.push(format!("output `{}` has no logical size", display.output));
            }
        }
        for (first, second) in self.overlaps() {
            errors.push(format!("outputs `{first}` and `{second}` overlap"));
        }
        errors
    }

    pub fn ok(&self) -> bool {
        self.validate().is_empty()
    }

    /// The plan passed to `better-monitor arrange --plan`.
    pub fn plan(&self) -> Value {
        let mut outputs = Map::new();
        for display in self.displays.iter().filter(|display| display.enabled) {
            outputs.insert(display.output.clone(), json!([display.x, display.y]));
        }
        json!({ "outputs": outputs })
    }

    pub fn plan_string(&self) -> String {
        self.plan().to_string()
    }

    /// Logical bounding box `(min_x, min_y, max_x, max_y)`.
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let enabled = self.displays.iter().filter(|display| display.enabled);
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for display in enabled {
            let right = display.x + display.width;
            let bottom = display.y + display.height;
            bounds = Some(match bounds {
                None => (display.x, display.y, right, bottom),
                Some((min_x, min_y, max_x, max_y)) => (
                    min_x.min(display.x),
                    min_y.min(display.y),
                    max_x.max(right),
                    max_y.max(bottom),
                ),
            });
        }
        bounds
    }
}

fn best_candidate(proposed: i32, candidates: &[i32], threshold: i32) -> Option<i32> {
    let mut best: Option<(i32, i32)> = None;
    for candidate in candidates {
        let distance = (candidate - proposed).abs();
        if distance > threshold {
            continue;
        }
        match best {
            Some((_, best_distance)) if best_distance <= distance => {}
            _ => best = Some((*candidate, distance)),
        }
    }
    best.map(|(candidate, _)| candidate)
}

fn intersects(first: &DisplayBox, second: &DisplayBox) -> bool {
    i64::from(first.x) < i64::from(second.x) + i64::from(second.width)
        && i64::from(second.x) < i64::from(first.x) + i64::from(first.width)
        && i64::from(first.y) < i64::from(second.y) + i64::from(second.height)
        && i64::from(second.y) < i64::from(first.y) + i64::from(first.height)
}

/// Maps logical coordinates into a GTK drawing area and back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    scale: f64,
    offset_x: f64,
    offset_y: f64,
}

impl Viewport {
    /// Fits the arrangement into `area_width` × `area_height` with padding.
    pub fn fit(arrangement: &Arrangement, area_width: f64, area_height: f64, padding: f64) -> Self {
        let (min_x, min_y, max_x, max_y) = arrangement.bounds().unwrap_or((0, 0, 1920, 1080));
        let logical_width = f64::from((max_x - min_x).max(1));
        let logical_height = f64::from((max_y - min_y).max(1));
        let available_width = (area_width - padding * 2.0).max(1.0);
        let available_height = (area_height - padding * 2.0).max(1.0);
        let scale = (available_width / logical_width)
            .min(available_height / logical_height)
            .max(0.0001);
        let offset_x =
            padding + (available_width - logical_width * scale) / 2.0 - f64::from(min_x) * scale;
        let offset_y =
            padding + (available_height - logical_height * scale) / 2.0 - f64::from(min_y) * scale;
        Self {
            scale,
            offset_x,
            offset_y,
        }
    }

    /// Screen rectangle `(x, y, width, height)` for a display.
    pub fn rectangle(&self, display: &DisplayBox) -> (f64, f64, f64, f64) {
        (
            f64::from(display.x) * self.scale + self.offset_x,
            f64::from(display.y) * self.scale + self.offset_y,
            f64::from(display.width) * self.scale,
            f64::from(display.height) * self.scale,
        )
    }

    /// Converts a widget coordinate to a logical position.
    pub fn to_logical(&self, screen_x: f64, screen_y: f64) -> (i32, i32) {
        (
            ((screen_x - self.offset_x) / self.scale).round() as i32,
            ((screen_y - self.offset_y) / self.scale).round() as i32,
        )
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arrangement() -> Arrangement {
        Arrangement {
            displays: vec![
                DisplayBox {
                    output: "eDP-1".to_owned(),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    scale: 1.0,
                    enabled: true,
                },
                DisplayBox {
                    output: "DP-1".to_owned(),
                    x: 1920,
                    y: 0,
                    width: 2560,
                    height: 1440,
                    scale: 1.0,
                    enabled: true,
                },
            ],
        }
    }

    #[test]
    fn parses_monitor_position_json() {
        let value = json!({
            "command": "position",
            "ok": true,
            "data": {
                "positions": [
                    {"output": "DP-1", "x": 1920, "y": 0, "width": 2560,
                     "height": 1440, "scale": 1.0, "transform": "Normal", "enabled": true},
                    {"output": "eDP-1", "x": 0, "y": 0, "width": 1920,
                     "height": 1080, "scale": 1.5, "transform": "Normal", "enabled": true}
                ]
            }
        });
        let arrangement = Arrangement::from_monitor_json(&value).unwrap();
        assert_eq!(arrangement.displays.len(), 2);
        assert_eq!(arrangement.displays[0].output, "DP-1");
        assert_eq!(arrangement.displays[1].x, 0);
        assert_eq!(arrangement.displays[1].scale, 1.5);
        assert!(arrangement.ok());

        let bare = json!({"positions": [{"output": "HDMI-1", "x": 0, "y": 0}]});
        let arrangement = Arrangement::from_monitor_json(&bare).unwrap();
        assert_eq!(arrangement.displays[0].width, 0);
        assert!(!arrangement.displays[0].enabled);
        assert!(Arrangement::from_monitor_json(&json!({})).is_err());
    }

    #[test]
    fn picks_and_drags_a_display_from_the_topmost_hit() {
        let mut arrangement = arrangement();
        assert_eq!(arrangement.pick(100, 100).unwrap().output, "eDP-1");
        assert_eq!(arrangement.pick(2000, 100).unwrap().output, "DP-1");
        assert!(arrangement.pick(5000, 5000).is_none());

        let drag = arrangement.begin_drag(2000, 100).unwrap();
        assert_eq!(drag.output, "DP-1");
        assert_eq!((drag.grab_x, drag.grab_y), (80, 100));
        let snap = arrangement
            .drag_to(&drag, 2000, 200, SNAP_THRESHOLD)
            .unwrap();
        assert_eq!(snap.x, 1920);
        assert_eq!(snap.y, 100);
        assert_eq!(arrangement.get("DP-1").unwrap().x, 1920);
    }

    #[test]
    fn snaps_to_edges_centres_and_the_grid() {
        let arrangement = arrangement();
        // DP-1's left edge snaps to eDP-1's right edge (1920).
        let snap = arrangement.snap_position("DP-1", 1910, 120, 2560, 1440, SNAP_THRESHOLD);
        assert_eq!(snap.x, 1920);
        assert!(snap.snapped_x);
        assert_eq!(snap.y, 120);

        // A small move snaps to the 10 px grid when nothing else is nearby.
        let snap = arrangement.snap_position("DP-1", 5003, 5007, 2560, 1440, SNAP_THRESHOLD);
        assert_eq!((snap.x, snap.y), (5000, 5010));
        assert!(snap.snapped_x && snap.snapped_y);

        // Top edges align to another display's top (0).
        let snap = arrangement.snap_position("DP-1", 5000, 6, 2560, 1440, SNAP_THRESHOLD);
        assert_eq!((snap.x, snap.y), (5000, 0));
        assert!(snap.snapped_y);

        // Outside the threshold nothing changes.
        let snap = arrangement.snap_position("DP-1", 4004, 4004, 2560, 1440, 2);
        assert_eq!((snap.x, snap.y), (4004, 4004));
        assert!(!snap.snapped_x && !snap.snapped_y);

        // Centres align when the displays are stacked around the same centre.
        let snap = arrangement.snap_position("DP-1", -315, -174, 2560, 1440, SNAP_THRESHOLD);
        assert_eq!((snap.x, snap.y), (-320, -180));
        assert!(snap.snapped_x && snap.snapped_y);
    }

    #[test]
    fn detects_overlaps_but_allows_touching_edges() {
        let mut arrangement = arrangement();
        assert!(arrangement.overlaps().is_empty());
        assert!(arrangement.ok());
        arrangement.move_to("DP-1", 1919, 0);
        assert_eq!(
            arrangement.overlaps(),
            vec![("eDP-1".to_owned(), "DP-1".to_owned())]
        );
        let errors = arrangement.validate();
        assert!(errors.iter().any(|error| error.contains("overlap")));
    }

    #[test]
    fn validation_rejects_empty_plans_and_disabled_outputs() {
        assert!(!Arrangement::default().ok());
        let mut arrangement = arrangement();
        for display in &mut arrangement.displays {
            display.enabled = false;
        }
        assert!(
            arrangement
                .validate()
                .iter()
                .any(|error| error.contains("at least one output"))
        );
    }

    #[test]
    fn plan_contains_only_enabled_outputs_in_logical_coordinates() {
        let mut arrangement = arrangement();
        arrangement.move_to("DP-1", 0, 1080);
        arrangement.displays[0].enabled = true;
        let plan = arrangement.plan();
        assert_eq!(plan["outputs"]["eDP-1"], json!([0, 0]));
        assert_eq!(plan["outputs"]["DP-1"], json!([0, 1080]));
        let text = arrangement.plan_string();
        let plan = better_settings_plan(&text);
        assert_eq!(plan["outputs"]["DP-1"][1], 1080);
    }

    fn better_settings_plan(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn viewport_round_trips_widget_and_logical_coordinates() {
        let arrangement = arrangement();
        let viewport = Viewport::fit(&arrangement, 800.0, 600.0, 20.0);
        let (x, y, width, height) = viewport.rectangle(&arrangement.displays[0]);
        assert!(x >= 0.0 && y >= 0.0);
        assert!(width > 0.0 && height > 0.0);
        let (logical_x, logical_y) = viewport.to_logical(x + width / 2.0, y + height / 2.0);
        assert_eq!(logical_x, 960);
        assert_eq!(logical_y, 540);
        assert!(viewport.scale() > 0.0);
    }

    #[test]
    fn moves_are_clamped_to_sane_coordinates() {
        let mut arrangement = arrangement();
        assert!(arrangement.move_to("DP-1", MAX_COORDINATE + 100, -MAX_COORDINATE - 100));
        let display = arrangement.get("DP-1").unwrap();
        assert_eq!(display.x, MAX_COORDINATE);
        assert_eq!(display.y, -MAX_COORDINATE);
        assert!(!arrangement.move_to("missing", 0, 0));
    }
}
