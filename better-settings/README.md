# better-settings

A GTK4 frontend and a scriptable CLI for niri's KDL config. Part of the Better
Desktop ecosystem; lives inside this repository so no new repository is created
(excluded from niri's Cargo workspace).

## Why it edits text

Most config GUIs re-serialize the file, which drops comments, ordering and
hand-written structure. `better-settings` scans the existing text, records where
each node lives, and rewrites only the bytes it changes. Comments, blank lines,
indentation, ordering, line endings (LF/CRLF) and unrelated settings stay
identical, so the config remains reviewable and versionable.

## Coverage

The catalog covers the settings a full niri config most often edits. Paths are
shown relative to the config root; output settings are generated once per
`output "<name>"` block found in the file.

| Section | Keys |
| --- | --- |
| Layout | `gaps`, `main-axis`, `center-focused-column`, `always-center-single-column`, `empty-workspace-above-first`, `focused-window-above-floating`, `default-column-display`, `background-color`, `fullscreen-backdrop-color`, `default-column-width`, `preset-column-widths` |
| Layout / Struts | `struts.left/right/top/bottom` |
| Layout / Focus ring | `focus-ring.on/off/width/active-color/inactive-color/urgent-color` |
| Layout / Border | `border.on/off/width/active-color/inactive-color/urgent-color` |
| Layout / Shadow | `shadow.on/off/softness/spread/offset/color/draw-behind-window` |
| Layout / Insert hint | `insert-hint.on/off/color` |
| Input / Keyboard | `repeat-delay`, `repeat-rate`, `track-layout`, `numlock`, `xkb.layout/variant/options/model/rules/file` |
| Input / Touchpad | `tap`, `dwt`, `dwtp`, `drag`, `drag-lock`, `natural-scroll`, `accel-speed`, `accel-profile`, `scroll-method`, `scroll-button`, `scroll-button-lock`, `tap-button-map`, `click-method`, `left-handed`, `disabled-on-external-mouse`, `middle-emulation`, `pinch-sensitivity`, `scroll-factor` |
| Input / Mouse | `off`, `natural-scroll`, `accel-speed`, `accel-profile`, `scroll-method`, `scroll-button`, `scroll-button-lock`, `left-handed`, `middle-emulation` |
| Input / Trackpoint | `off`, `natural-scroll`, `accel-speed`, `accel-profile`, `scroll-method`, `scroll-button`, `left-handed`, `middle-emulation` |
| Input / Touch | `off`, `map-to-output` |
| Input / Tablet | `off`, `map-to-output`, `map-to-focused-output`, `left-handed`, `calibration-matrix` |
| Input / Global | `disable-power-key-handling`, `warp-mouse-to-focus`, `focus-follows-mouse`, `confine-mouse-to-fullscreen`, `workspace-auto-back-and-forth`, `mod-key`, `mod-key-nested` |
| Outputs | `off`, `create-virtual`, `mode`, `scale`, `transform`, `position`, `variable-refresh-rate`, `focus-at-startup`, `isolated`, `drm-lease`, `backdrop-color`, `background-color`, `max-bpc`, `hdr`, `hot-corners.off`, `layout.gaps`, `layout.center-focused-column`, `layout.always-center-single-column`, `layout.default-column-width` |
| Binds | list, add/replace, remove; multi-action binds and `repeat`, `cooldown-ms`, `allow-when-locked`, `allow-inhibiting`, `hotkey-overlay-title` |
| Window rules | list, add, remove (matches, excludes and raw property/nested-block lines) |
| Layer rules | list, add, remove |
| Startup | `spawn-at-startup` list (add/remove) |
| Environment | arbitrary `environment` variables, including `null` to unset |
| Animations | `off`, `slowdown`, `window-open`, `window-close`, `focus-ring` timings/curves |
| Gestures | `dnd-edge-view-scroll`, `dnd-edge-workspace-switch`, `hot-corners` |
| Overview | `zoom`, `backdrop-color`, `workspace-shadow` |
| Cursor | `xcursor-theme`, `xcursor-size`, `hide-when-typing`, `hide-after-inactive-ms` |
| Clipboard | `disable-primary` |
| Miscellaneous | `prefer-no-csd`, `screenshot-path`, `hotkey-overlay`, `config-notification` |

Settings not modeled by the catalog can still be written with the CLI using
`--raw` (for a KDL fragment) or the default quoting rules.

## GUI

```sh
cargo build --features gui      # or just `cargo build` (gui is the default)
./target/debug/better-settings
```

The window groups settings into sections (layout, input, output, animations,
gestures, overview, cursor, clipboard, miscellaneous), one section per output
found in the file, and shows read-only summaries for binds, window/layer rules,
startup commands, environment variables and column widths. Values present in
the config are loaded; changing a row and pressing **Save** patches only those
lines. **Reload** re-reads the file, rebuilds the output sections, and reports
structural/round-trip validation errors.

Config path: `$BETTER_SETTINGS_CONFIG`, then `$NIRI_CONFIG`, then
`$XDG_CONFIG_HOME/niri/config.kdl`, then `~/.config/niri/config.kdl`.

Every save copies the previous file to `<name>.bak-<epoch>` (with a numeric
suffix if the same second is saved twice), writes through a temporary file, and
refuses to save a config that fails validation.

## CLI

```sh
# Read
better-settingsctl list
better-settingsctl get layout.gaps
better-settingsctl get 'output "eDP-1".scale'
better-settingsctl get layout.preset-column-widths

# Single values (--dry-run prints a diff and writes nothing)
better-settingsctl set layout.gaps 24
better-settingsctl set 'output "eDP-1".transform' 90
better-settingsctl set 'output "eDP-1".position' 'x=1280 y=0' --raw
better-settingsctl set prefer-no-csd true
better-settingsctl set prefer-no-csd false        # removes the flag line
better-settingsctl set layout.default-column-width 'proportion 0.5'
better-settingsctl set layout.preset-column-widths 'proportion 0.5, fixed 640'
better-settingsctl unset layout.struts.left
better-settingsctl set layout.gaps 24 --dry-run

# Binds (add is also replace when the key already exists)
better-settingsctl add-bind Mod+Return --action 'spawn "foot"'
better-settingsctl add-bind Mod+T --action 'spawn "alacritty"' \
    --repeat=false --cooldown-ms 500 --allow-when-locked
better-settingsctl add-bind Mod+G \
    --action focus-column-right --action consume-or-expel-window-left
better-settingsctl remove-bind Mod+Return

# Rules (zero-based index for removal, in file order)
better-settingsctl add-window-rule --match 'app-id="Alacritty"' \
    --property 'open-floating true'
better-settingsctl add-window-rule --match 'title="Picture-in-Picture"' \
    --exclude 'app-id="keepassxc"' --property 'open-floating true'
better-settingsctl remove-window-rule 0
better-settingsctl add-layer-rule --match 'namespace="waybar"' \
    --property 'opacity 0.9'

# Startup and presets
better-settingsctl add-startup waybar
better-settingsctl remove-startup 0
better-settingsctl add-preset-width 'proportion 0.25'
better-settingsctl remove-preset-width 0

# Checks and snapshots
better-settingsctl validate
better-settingsctl diff          # print the config with catalog values re-applied
better-settingsctl export > niri-model.json
better-settingsctl import niri-model.json --dry-run
better-settingsctl import niri-model.json
```

Every command accepts `--config PATH`, `--dry-run` and `--raw`. Mutations print
deterministic JSON containing `changed`, `dry_run`, the patched `text`, the
backup path (or `null`) and a unified `diff`.

`validate` exits non-zero when the KDL is malformed (unclosed block, unexpected
`}`, unterminated string or block comment) or when a catalog value would not
round-trip byte-for-byte.

## JSON model (`export`/`import`)

`export` prints a snapshot; `import` applies it and replaces the managed lists
(binds, window rules, layer rules, startup commands) with the model's lists.
Settings are applied through the text-preserving patcher, and environment
entries are set one by one.

```json
{
  "version": 1,
  "settings": [
    { "path": "layout.gaps", "value": "16", "flag": false, "raw": false },
    { "path": "prefer-no-csd", "value": "true", "flag": true, "raw": false },
    { "path": "output \"eDP-1\".position", "value": "x=1280 y=0", "flag": false, "raw": true }
  ],
  "binds": [
    {
      "key": "Mod+Return",
      "actions": ["spawn \"foot\""],
      "repeat": false,
      "cooldown_ms": 500
    }
  ],
  "window_rules": [
    { "lines": ["match app-id=\"Alacritty\"", "open-floating true"] }
  ],
  "layer_rules": [
    { "lines": ["match namespace=\"waybar\"", "opacity 0.9"] }
  ],
  "spawn_at_startup": [["waybar"]],
  "environment": { "QT_QPA_PLATFORM": "wayland", "DISPLAY": null }
}
```

`version` must be `1` (or lower). `import -` reads the model from stdin.
`--dry-run` previews the diff without writing anything. A flag with properties
(for example `variable-refresh-rate on-demand=true`) is stored with `flag` and
`raw` both true and its property text as `value`, so import preserves it.

## Supported syntax

- Blocks and nested blocks: `input { keyboard { … } }`.
- Block arguments: `output "eDP-1" { … }`, named `keyboard "…"` blocks.
- Multi-argument values: `position x=1280 y=0`, `scroll-factor vertical=1.0
  horizontal=-2.0`, `mode custom=true "…"`.
- Bare flags: `tap`, `natural-scroll`, `prefer-no-csd`; setting them to `false`
  removes the line. `true` on an existing flag with properties (for example
  `variable-refresh-rate on-demand=true`) is a no-op rather than a rewrite.
- Inline blocks are parsed for binds and for the block-valued layout settings
  (`default-column-width { proportion 0.5; }`), and are expanded to a multi-line
  block only when their value actually changes.
- `//` and `/* … */` comments are never treated as settings and are preserved.
- Missing keys are inserted into their block; missing blocks are created outer
  before inner, at the end of the file or inside the deepest existing parent.

## Safety

- Only the target line/node span is rewritten; unrelated bytes stay identical,
  including CRLF line endings and a missing final newline.
- Writes go through a temporary file plus rename, after copying the old file to
  `<name>.bak-<epoch>[-N]`.
- `--dry-run` never creates a backup and never writes.
- The GUI validates before saving and shows parse/round-trip errors; the CLI
  `validate` command does the same and exits non-zero on failure.

## Test

```sh
cargo test                     # model/CLI; GTK is only linked in the GUI binary
cargo test --no-default-features
cargo fmt --check
cargo clippy --all-targets
```
