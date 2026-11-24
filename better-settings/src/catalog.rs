//! The settings catalog: which niri keys the frontend and CLI know about.
//!
//! [`CATALOG`] holds global settings. [`catalog_for`] expands it with one copy
//! of each output-specific setting per `output "<name>"` block found in the
//! config, so the GUI can edit outputs that actually exist.

use crate::{PatchError, Segment, scan};

/// One entry in the settings catalog.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Float {
        min: f64,
        max: f64,
    },
    Int {
        min: i64,
        max: i64,
    },
    Bool,
    /// A bare flag: present means enabled.
    Flag,
    Text,
    Choice(&'static [&'static str]),
    /// A raw KDL fragment, written without quoting (for example
    /// `x=0 y=5` or `proportion 0.5`).
    Raw,
}

#[derive(Debug, Clone, Copy)]
pub struct SettingSpec {
    pub path: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub section: &'static str,
}

/// A setting path resolved against a concrete config (output names filled in).
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedSetting {
    pub path: String,
    pub label: String,
    pub help: &'static str,
    pub kind: Kind,
    pub section: &'static str,
}

/// Settings that are valid for any niri config, independent of outputs.
pub const CATALOG: &[SettingSpec] = &[
    // ---------------------------------------------------------------- layout
    SettingSpec {
        path: "layout.gaps",
        label: "Gaps",
        help: "Logical pixels between windows (fractional values allowed).",
        kind: Kind::Float {
            min: -65535.0,
            max: 65535.0,
        },
        section: "Layout",
    },
    SettingSpec {
        path: "layout.main-axis",
        label: "Main axis",
        help: "Direction columns are laid out and scrolled.",
        kind: Kind::Choice(&["horizontal", "vertical"]),
        section: "Layout",
    },
    SettingSpec {
        path: "layout.center-focused-column",
        label: "Center focused column",
        help: "Whether the focused column is centered.",
        kind: Kind::Choice(&["never", "always", "on-overflow"]),
        section: "Layout",
    },
    SettingSpec {
        path: "layout.always-center-single-column",
        label: "Always center single column",
        help: "Center a lone column regardless of center-focused-column.",
        kind: Kind::Flag,
        section: "Layout",
    },
    SettingSpec {
        path: "layout.empty-workspace-above-first",
        label: "Empty workspace above first",
        help: "Start the workspace list with an empty workspace.",
        kind: Kind::Flag,
        section: "Layout",
    },
    SettingSpec {
        path: "layout.focused-window-above-floating",
        label: "Focused window above floating",
        help: "Raise a focused tiled window above floating windows.",
        kind: Kind::Flag,
        section: "Layout",
    },
    SettingSpec {
        path: "layout.default-column-display",
        label: "Default column display",
        help: "Display mode for new columns.",
        kind: Kind::Choice(&["normal", "tabbed"]),
        section: "Layout",
    },
    SettingSpec {
        path: "layout.background-color",
        label: "Background color",
        help: "Workspace background color, for example `#003300`.",
        kind: Kind::Text,
        section: "Layout",
    },
    SettingSpec {
        path: "layout.fullscreen-backdrop-color",
        label: "Fullscreen backdrop color",
        help: "Backdrop color behind fullscreen windows.",
        kind: Kind::Text,
        section: "Layout",
    },
    SettingSpec {
        path: "layout.default-column-width",
        label: "Default column width",
        help: "Raw size: `proportion 0.5`, `fixed 640`, or empty to let windows decide.",
        kind: Kind::Raw,
        section: "Layout",
    },
    SettingSpec {
        path: "layout.preset-column-widths",
        label: "Preset column widths",
        help: "Comma-separated sizes, for example `proportion 0.5, fixed 640`.",
        kind: Kind::Raw,
        section: "Layout",
    },
    SettingSpec {
        path: "layout.struts.left",
        label: "Strut left",
        help: "Reserved space at the left edge; negative emulates outer gaps.",
        kind: Kind::Float {
            min: -65535.0,
            max: 65535.0,
        },
        section: "Layout / Struts",
    },
    SettingSpec {
        path: "layout.struts.right",
        label: "Strut right",
        help: "Reserved space at the right edge.",
        kind: Kind::Float {
            min: -65535.0,
            max: 65535.0,
        },
        section: "Layout / Struts",
    },
    SettingSpec {
        path: "layout.struts.top",
        label: "Strut top",
        help: "Reserved space at the top edge.",
        kind: Kind::Float {
            min: -65535.0,
            max: 65535.0,
        },
        section: "Layout / Struts",
    },
    SettingSpec {
        path: "layout.struts.bottom",
        label: "Strut bottom",
        help: "Reserved space at the bottom edge.",
        kind: Kind::Float {
            min: -65535.0,
            max: 65535.0,
        },
        section: "Layout / Struts",
    },
    SettingSpec {
        path: "layout.focus-ring.on",
        label: "Focus ring enabled",
        help: "Draw the focus ring.",
        kind: Kind::Flag,
        section: "Layout / Focus ring",
    },
    SettingSpec {
        path: "layout.focus-ring.off",
        label: "Focus ring disabled",
        help: "Disable the focus ring (prefer setting `on` to false).",
        kind: Kind::Flag,
        section: "Layout / Focus ring",
    },
    SettingSpec {
        path: "layout.focus-ring.width",
        label: "Focus ring width",
        help: "Thickness of the focus ring in logical pixels.",
        kind: Kind::Int { min: 0, max: 64 },
        section: "Layout / Focus ring",
    },
    SettingSpec {
        path: "layout.focus-ring.active-color",
        label: "Focus ring active color",
        help: "Color of the focused window's ring.",
        kind: Kind::Text,
        section: "Layout / Focus ring",
    },
    SettingSpec {
        path: "layout.focus-ring.inactive-color",
        label: "Focus ring inactive color",
        help: "Color of unfocused windows' rings.",
        kind: Kind::Text,
        section: "Layout / Focus ring",
    },
    SettingSpec {
        path: "layout.focus-ring.urgent-color",
        label: "Focus ring urgent color",
        help: "Color of urgent windows' rings.",
        kind: Kind::Text,
        section: "Layout / Focus ring",
    },
    SettingSpec {
        path: "layout.border.on",
        label: "Border enabled",
        help: "Draw borders around windows.",
        kind: Kind::Flag,
        section: "Layout / Border",
    },
    SettingSpec {
        path: "layout.border.off",
        label: "Border disabled",
        help: "Disable borders (prefer setting `on` to false).",
        kind: Kind::Flag,
        section: "Layout / Border",
    },
    SettingSpec {
        path: "layout.border.width",
        label: "Border width",
        help: "Thickness of window borders in logical pixels.",
        kind: Kind::Int { min: 0, max: 64 },
        section: "Layout / Border",
    },
    SettingSpec {
        path: "layout.border.active-color",
        label: "Border active color",
        help: "Color of the focused window's border.",
        kind: Kind::Text,
        section: "Layout / Border",
    },
    SettingSpec {
        path: "layout.border.inactive-color",
        label: "Border inactive color",
        help: "Color of unfocused windows' borders.",
        kind: Kind::Text,
        section: "Layout / Border",
    },
    SettingSpec {
        path: "layout.border.urgent-color",
        label: "Border urgent color",
        help: "Color of urgent windows' borders.",
        kind: Kind::Text,
        section: "Layout / Border",
    },
    SettingSpec {
        path: "layout.shadow.on",
        label: "Shadow enabled",
        help: "Draw window shadows.",
        kind: Kind::Flag,
        section: "Layout / Shadow",
    },
    SettingSpec {
        path: "layout.shadow.off",
        label: "Shadow disabled",
        help: "Disable window shadows (prefer setting `on` to false).",
        kind: Kind::Flag,
        section: "Layout / Shadow",
    },
    SettingSpec {
        path: "layout.shadow.softness",
        label: "Shadow softness",
        help: "Blur radius of the shadow.",
        kind: Kind::Float {
            min: 0.0,
            max: 1024.0,
        },
        section: "Layout / Shadow",
    },
    SettingSpec {
        path: "layout.shadow.spread",
        label: "Shadow spread",
        help: "Shadow size added around the window.",
        kind: Kind::Float {
            min: -1024.0,
            max: 1024.0,
        },
        section: "Layout / Shadow",
    },
    SettingSpec {
        path: "layout.shadow.offset",
        label: "Shadow offset",
        help: "Raw offset, for example `x=0 y=5`.",
        kind: Kind::Raw,
        section: "Layout / Shadow",
    },
    SettingSpec {
        path: "layout.shadow.color",
        label: "Shadow color",
        help: "Shadow color, for example `#00000070`.",
        kind: Kind::Text,
        section: "Layout / Shadow",
    },
    SettingSpec {
        path: "layout.shadow.draw-behind-window",
        label: "Shadow behind window",
        help: "Draw the shadow behind translucent windows.",
        kind: Kind::Bool,
        section: "Layout / Shadow",
    },
    SettingSpec {
        path: "layout.insert-hint.on",
        label: "Insert hint enabled",
        help: "Show the column insert hint.",
        kind: Kind::Flag,
        section: "Layout / Insert hint",
    },
    SettingSpec {
        path: "layout.insert-hint.off",
        label: "Insert hint disabled",
        help: "Disable the column insert hint.",
        kind: Kind::Flag,
        section: "Layout / Insert hint",
    },
    SettingSpec {
        path: "layout.insert-hint.color",
        label: "Insert hint color",
        help: "Color of the insert hint, for example `#ffc87f80`.",
        kind: Kind::Text,
        section: "Layout / Insert hint",
    },
    // ----------------------------------------------------------------- input
    SettingSpec {
        path: "input.keyboard.repeat-delay",
        label: "Keyboard repeat delay",
        help: "Milliseconds before key repeat starts.",
        kind: Kind::Int { min: 0, max: 2000 },
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.repeat-rate",
        label: "Keyboard repeat rate",
        help: "Repeats per second.",
        kind: Kind::Int { min: 1, max: 500 },
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.track-layout",
        label: "Track layout",
        help: "Whether the active layout follows the focused window.",
        kind: Kind::Choice(&["global", "window"]),
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.numlock",
        label: "Numlock",
        help: "Enable numlock on startup.",
        kind: Kind::Flag,
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.xkb.layout",
        label: "Keyboard layout",
        help: "XKB layout, for example `us` or `us,de`.",
        kind: Kind::Text,
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.xkb.variant",
        label: "Keyboard variant",
        help: "XKB variant, for example `colemak_dh_ortho`.",
        kind: Kind::Text,
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.xkb.options",
        label: "Keyboard options",
        help: "XKB options, for example `compose:ralt,ctrl:nocaps`.",
        kind: Kind::Text,
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.xkb.model",
        label: "Keyboard model",
        help: "XKB model (usually empty).",
        kind: Kind::Text,
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.xkb.rules",
        label: "Keyboard rules",
        help: "XKB rules file (usually empty).",
        kind: Kind::Text,
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.keyboard.xkb.file",
        label: "Keyboard keymap file",
        help: "Path to a complete .xkb keymap; overrides other xkb options.",
        kind: Kind::Text,
        section: "Input / Keyboard",
    },
    SettingSpec {
        path: "input.touchpad.tap",
        label: "Touchpad tap to click",
        help: "Enable tap-to-click.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.dwt",
        label: "Disable while typing",
        help: "Ignore touchpad input while typing.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.dwtp",
        label: "Disable while trackpointing",
        help: "Ignore touchpad input while using the trackpoint.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.drag",
        label: "Touchpad drag",
        help: "Enable drag with the touchpad.",
        kind: Kind::Bool,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.drag-lock",
        label: "Touchpad drag lock",
        help: "Keep dragging after lifting the finger.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.natural-scroll",
        label: "Touchpad natural scroll",
        help: "Scroll direction follows finger movement.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.accel-speed",
        label: "Touchpad acceleration",
        help: "Acceleration speed from -1 to 1.",
        kind: Kind::Float {
            min: -1.0,
            max: 1.0,
        },
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.accel-profile",
        label: "Touchpad acceleration profile",
        help: "Adaptive or flat acceleration.",
        kind: Kind::Choice(&["adaptive", "flat"]),
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.scroll-method",
        label: "Touchpad scroll method",
        help: "How the touchpad scrolls.",
        kind: Kind::Choice(&["no-scroll", "two-finger", "edge", "on-button-down"]),
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.scroll-button",
        label: "Touchpad scroll button",
        help: "Evdev button used for button scrolling.",
        kind: Kind::Int {
            min: 0,
            max: 0xffff,
        },
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.scroll-button-lock",
        label: "Touchpad scroll button lock",
        help: "Keep button scrolling active until pressed again.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.tap-button-map",
        label: "Touchpad tap button map",
        help: "Button order for tap-to-click.",
        kind: Kind::Choice(&["left-right-middle", "left-middle-right"]),
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.click-method",
        label: "Touchpad click method",
        help: "Clickfinger or button areas.",
        kind: Kind::Choice(&["clickfinger", "button-areas"]),
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.left-handed",
        label: "Touchpad left handed",
        help: "Swap primary and secondary buttons.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.disabled-on-external-mouse",
        label: "Disable touchpad with mouse",
        help: "Disable the touchpad while an external mouse is connected.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.middle-emulation",
        label: "Touchpad middle emulation",
        help: "Emulate the middle button with left+right clicks.",
        kind: Kind::Flag,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.pinch-sensitivity",
        label: "Touchpad pinch sensitivity",
        help: "Multiplier for pinch gestures.",
        kind: Kind::Float {
            min: 0.0,
            max: 100.0,
        },
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.touchpad.scroll-factor",
        label: "Touchpad scroll factor",
        help: "Raw factor, for example `vertical=1.0 horizontal=-2.0`.",
        kind: Kind::Raw,
        section: "Input / Touchpad",
    },
    SettingSpec {
        path: "input.mouse.off",
        label: "Mouse disabled",
        help: "Disable all mice.",
        kind: Kind::Flag,
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.natural-scroll",
        label: "Mouse natural scroll",
        help: "Reverse the scroll direction.",
        kind: Kind::Flag,
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.accel-speed",
        label: "Mouse acceleration",
        help: "Acceleration speed from -1 to 1.",
        kind: Kind::Float {
            min: -1.0,
            max: 1.0,
        },
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.accel-profile",
        label: "Mouse acceleration profile",
        help: "Adaptive or flat acceleration.",
        kind: Kind::Choice(&["adaptive", "flat"]),
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.scroll-method",
        label: "Mouse scroll method",
        help: "How the mouse scrolls.",
        kind: Kind::Choice(&["no-scroll", "two-finger", "edge", "on-button-down"]),
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.scroll-button",
        label: "Mouse scroll button",
        help: "Evdev button used for button scrolling.",
        kind: Kind::Int {
            min: 0,
            max: 0xffff,
        },
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.scroll-button-lock",
        label: "Mouse scroll button lock",
        help: "Keep button scrolling active until pressed again.",
        kind: Kind::Flag,
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.left-handed",
        label: "Mouse left handed",
        help: "Swap primary and secondary buttons.",
        kind: Kind::Flag,
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.mouse.middle-emulation",
        label: "Mouse middle emulation",
        help: "Emulate the middle button with left+right clicks.",
        kind: Kind::Flag,
        section: "Input / Mouse",
    },
    SettingSpec {
        path: "input.trackpoint.off",
        label: "Trackpoint disabled",
        help: "Disable the trackpoint.",
        kind: Kind::Flag,
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.trackpoint.natural-scroll",
        label: "Trackpoint natural scroll",
        help: "Reverse the scroll direction.",
        kind: Kind::Flag,
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.trackpoint.accel-speed",
        label: "Trackpoint acceleration",
        help: "Acceleration speed from -1 to 1.",
        kind: Kind::Float {
            min: -1.0,
            max: 1.0,
        },
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.trackpoint.accel-profile",
        label: "Trackpoint acceleration profile",
        help: "Adaptive or flat acceleration.",
        kind: Kind::Choice(&["adaptive", "flat"]),
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.trackpoint.scroll-method",
        label: "Trackpoint scroll method",
        help: "How the trackpoint scrolls.",
        kind: Kind::Choice(&["no-scroll", "two-finger", "edge", "on-button-down"]),
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.trackpoint.scroll-button",
        label: "Trackpoint scroll button",
        help: "Evdev button used for button scrolling.",
        kind: Kind::Int {
            min: 0,
            max: 0xffff,
        },
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.trackpoint.left-handed",
        label: "Trackpoint left handed",
        help: "Swap primary and secondary buttons.",
        kind: Kind::Flag,
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.trackpoint.middle-emulation",
        label: "Trackpoint middle emulation",
        help: "Emulate the middle button with left+right clicks.",
        kind: Kind::Flag,
        section: "Input / Trackpoint",
    },
    SettingSpec {
        path: "input.touch.off",
        label: "Touch disabled",
        help: "Disable touch input.",
        kind: Kind::Flag,
        section: "Input / Touch",
    },
    SettingSpec {
        path: "input.touch.map-to-output",
        label: "Touch mapped to output",
        help: "Map touch input to one output, for example `eDP-1`.",
        kind: Kind::Text,
        section: "Input / Touch",
    },
    SettingSpec {
        path: "input.tablet.off",
        label: "Tablet disabled",
        help: "Disable tablet input.",
        kind: Kind::Flag,
        section: "Input / Tablet",
    },
    SettingSpec {
        path: "input.tablet.map-to-output",
        label: "Tablet mapped to output",
        help: "Map tablet input to one output.",
        kind: Kind::Text,
        section: "Input / Tablet",
    },
    SettingSpec {
        path: "input.tablet.map-to-focused-output",
        label: "Tablet map to focused output",
        help: "Map tablet input to the focused output.",
        kind: Kind::Flag,
        section: "Input / Tablet",
    },
    SettingSpec {
        path: "input.tablet.left-handed",
        label: "Tablet left handed",
        help: "Swap tablet buttons or orientation.",
        kind: Kind::Flag,
        section: "Input / Tablet",
    },
    SettingSpec {
        path: "input.tablet.calibration-matrix",
        label: "Tablet calibration matrix",
        help: "Raw six-number calibration matrix.",
        kind: Kind::Raw,
        section: "Input / Tablet",
    },
    SettingSpec {
        path: "input.disable-power-key-handling",
        label: "Disable power key handling",
        help: "Let logind handle the power key.",
        kind: Kind::Flag,
        section: "Input / Global",
    },
    SettingSpec {
        path: "input.warp-mouse-to-focus",
        label: "Warp mouse to focus",
        help: "Move the cursor to the newly focused window.",
        kind: Kind::Flag,
        section: "Input / Global",
    },
    SettingSpec {
        path: "input.focus-follows-mouse",
        label: "Focus follows mouse",
        help: "Focus windows under the cursor; may take raw arguments.",
        kind: Kind::Flag,
        section: "Input / Global",
    },
    SettingSpec {
        path: "input.confine-mouse-to-fullscreen",
        label: "Confine mouse to fullscreen",
        help: "Keep the cursor inside fullscreen windows.",
        kind: Kind::Flag,
        section: "Input / Global",
    },
    SettingSpec {
        path: "input.workspace-auto-back-and-forth",
        label: "Workspace auto back and forth",
        help: "Repeating a workspace switch returns to the previous workspace.",
        kind: Kind::Flag,
        section: "Input / Global",
    },
    SettingSpec {
        path: "input.mod-key",
        label: "Mod key",
        help: "Key used for the `Mod` modifier, for example `Super`.",
        kind: Kind::Text,
        section: "Input / Global",
    },
    SettingSpec {
        path: "input.mod-key-nested",
        label: "Mod key (nested)",
        help: "Key used for `Mod` when niri runs nested.",
        kind: Kind::Text,
        section: "Input / Global",
    },
    // ------------------------------------------------------------- animations
    SettingSpec {
        path: "animations.off",
        label: "Animations disabled",
        help: "Turn off all animations.",
        kind: Kind::Flag,
        section: "Animations",
    },
    SettingSpec {
        path: "animations.slowdown",
        label: "Animation slowdown",
        help: "Multiplier for animation durations.",
        kind: Kind::Float {
            min: 0.1,
            max: 10.0,
        },
        section: "Animations",
    },
    SettingSpec {
        path: "animations.window-open.duration-ms",
        label: "Window open duration",
        help: "Opening animation duration in milliseconds.",
        kind: Kind::Int {
            min: 0,
            max: 10_000,
        },
        section: "Animations",
    },
    SettingSpec {
        path: "animations.window-open.curve",
        label: "Window open curve",
        help: "Easing curve, for example `ease-out-expo`.",
        kind: Kind::Text,
        section: "Animations",
    },
    SettingSpec {
        path: "animations.window-close.duration-ms",
        label: "Window close duration",
        help: "Closing animation duration in milliseconds.",
        kind: Kind::Int {
            min: 0,
            max: 10_000,
        },
        section: "Animations",
    },
    SettingSpec {
        path: "animations.window-close.curve",
        label: "Window close curve",
        help: "Easing curve for closing windows.",
        kind: Kind::Text,
        section: "Animations",
    },
    SettingSpec {
        path: "animations.focus-ring.duration-ms",
        label: "Focus ring duration",
        help: "Focus ring animation duration in milliseconds.",
        kind: Kind::Int {
            min: 0,
            max: 10_000,
        },
        section: "Animations",
    },
    // ---------------------------------------------------------------- gestures
    SettingSpec {
        path: "gestures.dnd-edge-view-scroll.trigger-width",
        label: "DnD edge scroll width",
        help: "Edge area that scrolls the view while dragging.",
        kind: Kind::Int {
            min: 0,
            max: 10_000,
        },
        section: "Gestures",
    },
    SettingSpec {
        path: "gestures.dnd-edge-view-scroll.delay-ms",
        label: "DnD edge scroll delay",
        help: "Delay before edge scrolling starts.",
        kind: Kind::Int {
            min: 0,
            max: 10_000,
        },
        section: "Gestures",
    },
    SettingSpec {
        path: "gestures.dnd-edge-view-scroll.max-speed",
        label: "DnD edge scroll speed",
        help: "Maximum edge scroll speed in logical pixels per second.",
        kind: Kind::Int {
            min: 0,
            max: 100_000,
        },
        section: "Gestures",
    },
    SettingSpec {
        path: "gestures.dnd-edge-workspace-switch.trigger-height",
        label: "DnD workspace edge height",
        help: "Edge area that switches workspaces while dragging.",
        kind: Kind::Int {
            min: 0,
            max: 10_000,
        },
        section: "Gestures",
    },
    SettingSpec {
        path: "gestures.dnd-edge-workspace-switch.delay-ms",
        label: "DnD workspace switch delay",
        help: "Delay before edge workspace switching starts.",
        kind: Kind::Int {
            min: 0,
            max: 10_000,
        },
        section: "Gestures",
    },
    SettingSpec {
        path: "gestures.dnd-edge-workspace-switch.max-speed",
        label: "DnD workspace switch speed",
        help: "Maximum workspace switch speed.",
        kind: Kind::Int {
            min: 0,
            max: 100_000,
        },
        section: "Gestures",
    },
    SettingSpec {
        path: "gestures.hot-corners.off",
        label: "Hot corners disabled",
        help: "Disable hot corners.",
        kind: Kind::Flag,
        section: "Gestures / Hot corners",
    },
    SettingSpec {
        path: "gestures.hot-corners.top-left",
        label: "Hot corner top left",
        help: "Enable the top-left overview hot corner.",
        kind: Kind::Flag,
        section: "Gestures / Hot corners",
    },
    SettingSpec {
        path: "gestures.hot-corners.top-right",
        label: "Hot corner top right",
        help: "Enable the top-right overview hot corner.",
        kind: Kind::Flag,
        section: "Gestures / Hot corners",
    },
    SettingSpec {
        path: "gestures.hot-corners.bottom-left",
        label: "Hot corner bottom left",
        help: "Enable the bottom-left hot corner.",
        kind: Kind::Flag,
        section: "Gestures / Hot corners",
    },
    SettingSpec {
        path: "gestures.hot-corners.bottom-right",
        label: "Hot corner bottom right",
        help: "Enable the bottom-right hot corner.",
        kind: Kind::Flag,
        section: "Gestures / Hot corners",
    },
    // ---------------------------------------------------------------- overview
    SettingSpec {
        path: "overview.zoom",
        label: "Overview zoom",
        help: "Zoom level of the overview.",
        kind: Kind::Float { min: 0.0, max: 1.0 },
        section: "Overview",
    },
    SettingSpec {
        path: "overview.backdrop-color",
        label: "Overview backdrop color",
        help: "Backdrop color behind the overview.",
        kind: Kind::Text,
        section: "Overview",
    },
    SettingSpec {
        path: "overview.workspace-shadow.off",
        label: "Overview shadow disabled",
        help: "Disable workspace shadows in the overview.",
        kind: Kind::Flag,
        section: "Overview",
    },
    SettingSpec {
        path: "overview.workspace-shadow.softness",
        label: "Overview shadow softness",
        help: "Blur radius of workspace shadows.",
        kind: Kind::Float {
            min: 0.0,
            max: 1024.0,
        },
        section: "Overview",
    },
    SettingSpec {
        path: "overview.workspace-shadow.spread",
        label: "Overview shadow spread",
        help: "Spread of workspace shadows.",
        kind: Kind::Float {
            min: -1024.0,
            max: 1024.0,
        },
        section: "Overview",
    },
    SettingSpec {
        path: "overview.workspace-shadow.offset",
        label: "Overview shadow offset",
        help: "Raw offset, for example `x=0 y=10`.",
        kind: Kind::Raw,
        section: "Overview",
    },
    SettingSpec {
        path: "overview.workspace-shadow.color",
        label: "Overview shadow color",
        help: "Workspace shadow color.",
        kind: Kind::Text,
        section: "Overview",
    },
    // ----------------------------------------------------------------- cursor
    SettingSpec {
        path: "cursor.xcursor-theme",
        label: "Cursor theme",
        help: "XCURSOR_THEME for niri and spawned clients.",
        kind: Kind::Text,
        section: "Cursor",
    },
    SettingSpec {
        path: "cursor.xcursor-size",
        label: "Cursor size",
        help: "XCURSOR_SIZE in pixels.",
        kind: Kind::Int { min: 1, max: 512 },
        section: "Cursor",
    },
    SettingSpec {
        path: "cursor.hide-when-typing",
        label: "Hide cursor when typing",
        help: "Hide the cursor while typing.",
        kind: Kind::Flag,
        section: "Cursor",
    },
    SettingSpec {
        path: "cursor.hide-after-inactive-ms",
        label: "Hide cursor after",
        help: "Milliseconds of inactivity before hiding the cursor.",
        kind: Kind::Int {
            min: 0,
            max: 60_000,
        },
        section: "Cursor",
    },
    // -------------------------------------------------------------- clipboard
    SettingSpec {
        path: "clipboard.disable-primary",
        label: "Disable primary clipboard",
        help: "Disable the primary selection clipboard.",
        kind: Kind::Flag,
        section: "Clipboard",
    },
    // ---------------------------------------------------------------- misc
    SettingSpec {
        path: "prefer-no-csd",
        label: "Prefer no client-side decorations",
        help: "Ask clients to use server-side decorations.",
        kind: Kind::Flag,
        section: "Miscellaneous",
    },
    SettingSpec {
        path: "screenshot-path",
        label: "Screenshot path",
        help: "Where screenshots are saved (supports ~ and strftime).",
        kind: Kind::Text,
        section: "Miscellaneous",
    },
    SettingSpec {
        path: "hotkey-overlay.skip-at-startup",
        label: "Skip hotkey overlay at startup",
        help: "Do not show the important-hotkeys popup on startup.",
        kind: Kind::Flag,
        section: "Miscellaneous",
    },
    SettingSpec {
        path: "hotkey-overlay.hide-not-bound",
        label: "Hide unbound hotkeys",
        help: "Only list bound keys in the hotkey overlay.",
        kind: Kind::Flag,
        section: "Miscellaneous",
    },
    SettingSpec {
        path: "config-notification.disable-failed",
        label: "Disable failed config notification",
        help: "Do not show a notification when the config fails to load.",
        kind: Kind::Flag,
        section: "Miscellaneous",
    },
];

/// Settings that are repeated per `output "<name>"` block.
#[derive(Debug, Clone, Copy)]
pub struct OutputTemplate {
    pub suffix: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub section: &'static str,
}

pub const OUTPUT_TEMPLATES: &[OutputTemplate] = &[
    OutputTemplate {
        suffix: "off",
        label: "Output off",
        help: "Turn this output off.",
        kind: Kind::Flag,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "create-virtual",
        label: "Create virtual output",
        help: "Create a headless output with this name if it is missing.",
        kind: Kind::Flag,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "mode",
        label: "Mode",
        help: "Resolution and refresh rate, for example `1920x1080@120.030`.",
        kind: Kind::Text,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "scale",
        label: "Scale",
        help: "Output scale factor.",
        kind: Kind::Float {
            min: 0.0,
            max: 10.0,
        },
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "transform",
        label: "Transform",
        help: "Rotation or flip applied to the output.",
        kind: Kind::Choice(&[
            "normal",
            "90",
            "180",
            "270",
            "flipped",
            "flipped-90",
            "flipped-180",
            "flipped-270",
        ]),
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "position",
        label: "Position",
        help: "Raw position, for example `x=1280 y=0`.",
        kind: Kind::Raw,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "variable-refresh-rate",
        label: "Variable refresh rate",
        help: "Enable VRR (add `on-demand=true` in the file if desired).",
        kind: Kind::Flag,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "focus-at-startup",
        label: "Focus at startup",
        help: "Focus this output when niri starts.",
        kind: Kind::Flag,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "isolated",
        label: "Isolated output",
        help: "Isolate this output from the rest of the workspace set.",
        kind: Kind::Flag,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "drm-lease",
        label: "DRM lease",
        help: "Lease this output to another process.",
        kind: Kind::Flag,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "backdrop-color",
        label: "Backdrop color",
        help: "Color behind fullscreen windows on this output.",
        kind: Kind::Text,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "background-color",
        label: "Background color (deprecated)",
        help: "Deprecated per-output background color; use layout.background-color.",
        kind: Kind::Text,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "max-bpc",
        label: "Maximum bits per channel",
        help: "Maximum bits per color channel.",
        kind: Kind::Int { min: 6, max: 16 },
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "hdr",
        label: "HDR",
        help: "Enable HDR signalling on this output.",
        kind: Kind::Flag,
        section: "Outputs",
    },
    OutputTemplate {
        suffix: "hot-corners.off",
        label: "Hot corners disabled",
        help: "Disable hot corners on this output.",
        kind: Kind::Flag,
        section: "Outputs / Layout",
    },
    OutputTemplate {
        suffix: "layout.gaps",
        label: "Gaps override",
        help: "Per-output gaps.",
        kind: Kind::Float {
            min: -65535.0,
            max: 65535.0,
        },
        section: "Outputs / Layout",
    },
    OutputTemplate {
        suffix: "layout.center-focused-column",
        label: "Center focused column override",
        help: "Per-output center-focused-column.",
        kind: Kind::Choice(&["never", "always", "on-overflow"]),
        section: "Outputs / Layout",
    },
    OutputTemplate {
        suffix: "layout.always-center-single-column",
        label: "Always center single column override",
        help: "Per-output always-center-single-column.",
        kind: Kind::Flag,
        section: "Outputs / Layout",
    },
    OutputTemplate {
        suffix: "layout.default-column-width",
        label: "Default column width override",
        help: "Raw size: `proportion 0.5`, `fixed 640`, or empty.",
        kind: Kind::Raw,
        section: "Outputs / Layout",
    },
];

/// Output names mentioned by `output "<name>"` blocks, in file order.
pub fn output_names(text: &str) -> Result<Vec<String>, PatchError> {
    let scan = scan(text)?;
    let mut names = Vec::new();
    for block in &scan.blocks {
        if block.path.len() == 1 && block.path[0].name == "output" && !block.path[0].args.is_empty()
        {
            let name = block.path[0].args.join(" ");
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    Ok(names)
}

/// The static catalog plus one set of output settings per configured output.
pub fn catalog_for(text: &str) -> Vec<OwnedSetting> {
    let mut settings: Vec<OwnedSetting> = CATALOG
        .iter()
        .map(|spec| OwnedSetting {
            path: spec.path.to_owned(),
            label: spec.label.to_owned(),
            help: spec.help,
            kind: spec.kind,
            section: spec.section,
        })
        .collect();

    let Ok(names) = output_names(text) else {
        return settings;
    };
    for name in names {
        let output = Segment::with_arg("output", name.as_str());
        let prefix = output.to_string();
        for template in OUTPUT_TEMPLATES {
            let path = format!("{prefix}.{}", template.suffix);
            if settings.iter().any(|setting| setting.path == path) {
                continue;
            }
            settings.push(OwnedSetting {
                path: path.clone(),
                label: format!("{}: {}", name, template.label),
                help: template.help,
                kind: template.kind,
                section: template.section,
            });
        }
    }
    settings
}

/// Finds a catalog entry by exact path.
pub fn find(text: &str, path: &str) -> Option<OwnedSetting> {
    catalog_for(text)
        .into_iter()
        .find(|setting| setting.path == path)
}

/// Validates a raw value against a setting kind, normalizing it for KDL.
pub fn normalize(kind: Kind, raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    match kind {
        Kind::Float { min, max } => {
            let value: f64 = raw
                .parse()
                .map_err(|_| format!("`{raw}` is not a number"))?;
            if !(min..=max).contains(&value) {
                return Err(format!("{value} is outside {min}..={max}"));
            }
            Ok(format!("{value}"))
        }
        Kind::Int { min, max } => {
            let value: i64 = raw
                .parse()
                .map_err(|_| format!("`{raw}` is not an integer"))?;
            if !(min..=max).contains(&value) {
                return Err(format!("{value} is outside {min}..={max}"));
            }
            Ok(value.to_string())
        }
        Kind::Flag | Kind::Bool => match raw {
            "true" | "yes" | "on" | "1" => Ok("true".to_owned()),
            "false" | "no" | "off" | "0" => Ok("false".to_owned()),
            other => Err(format!("`{other}` is not a boolean")),
        },
        Kind::Text => Ok(raw.to_owned()),
        Kind::Choice(choices) => {
            if choices.contains(&raw) {
                Ok(raw.to_owned())
            } else {
                Err(format!("`{raw}` must be one of {}", choices.join(", ")))
            }
        }
        Kind::Raw => Ok(raw.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_path;

    #[test]
    fn catalog_paths_parse_and_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for spec in CATALOG {
            let path = parse_path(spec.path).unwrap();
            assert!(!path.is_empty());
            assert!(seen.insert(spec.path), "duplicate path {}", spec.path);
            assert!(!spec.label.is_empty(), "{} has no label", spec.path);
            assert!(!spec.section.is_empty(), "{} has no section", spec.path);
        }
    }

    #[test]
    fn every_requested_section_is_covered() {
        let sections: std::collections::BTreeSet<_> =
            CATALOG.iter().map(|spec| spec.section).collect();
        for wanted in [
            "Layout",
            "Input / Keyboard",
            "Input / Touchpad",
            "Input / Mouse",
            "Input / Trackpoint",
            "Input / Touch",
            "Animations",
            "Gestures",
            "Overview",
            "Cursor",
            "Clipboard",
            "Miscellaneous",
        ] {
            assert!(sections.contains(wanted), "missing section {wanted}");
        }
    }

    #[test]
    fn output_templates_expand_per_output() {
        let text = "output \"eDP-1\" {\n    scale 1.5\n}\noutput \"HDMI-A-1\" {\n}\n";
        let settings = catalog_for(text);
        assert!(
            settings
                .iter()
                .any(|setting| setting.path == "output \"eDP-1\".scale")
        );
        assert!(
            settings
                .iter()
                .any(|setting| setting.path == "output \"HDMI-A-1\".transform")
        );
        assert!(
            settings
                .iter()
                .any(|setting| setting.path == "output \"eDP-1\".layout.gaps")
        );
        assert_eq!(output_names(text).unwrap(), vec!["eDP-1", "HDMI-A-1"]);
    }

    #[test]
    fn normalize_handles_raw_and_choices() {
        assert_eq!(normalize(Kind::Raw, " x=0 y=5 ").unwrap(), "x=0 y=5");
        assert_eq!(
            normalize(Kind::Choice(&["normal", "tabbed"]), "tabbed").unwrap(),
            "tabbed"
        );
        assert!(normalize(Kind::Choice(&["normal", "tabbed"]), "other").is_err());
    }
}
