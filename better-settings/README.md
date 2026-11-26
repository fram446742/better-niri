# better-settings

> Part of [Better Desktop](https://github.com/fram446742/better-desktop).

A GTK4/libadwaita control center and a scriptable CLI for niri's KDL config.
It is its own Cargo workspace with its own lockfile and build.

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
| Outputs | `off`, `create-virtual`, `mode`, `scale`, `transform`, `position`, `variable-refresh-rate`, `focus-at-startup`, `isolated`, `drm-lease`, `backdrop-color`, `background-color`, `max-bpc`, `hdr.mode`, `hdr.reference-luminance`, `hot-corners.off`, `layout.gaps`, `layout.center-focused-column`, `layout.always-center-single-column`, `layout.default-column-width` |
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

The window is a libadwaita control center: an `AdwApplicationWindow` with a
collapsible `AdwNavigationSplitView`, a `navigation-sidebar` page list, a header
bar with page-specific actions, toast feedback and a scrollable page per
section. Overview is a small dashboard (status cards plus grouped details);
Compositor and Displays use `AdwPreferencesPage` with grouped preference rows.
Light and dark follow the system through `AdwStyleManager`; the minimum window
is 760×540. `F1` opens About from any page, and the sidebar has a search field.
The version is shown at the bottom of the sidebar.

Preview and Save only appear on Compositor and Displays, the two pages that
edit the KDL file in place. Every other page has explicit actions on the page
itself, so the header never offers a save that would do nothing.

No screenshots are checked in (the widget tree is built at run time from the
catalog, so a bitmap would go stale). The layout is:

```
┌─────────────────────────────────────────────────────────────────────┐
│ Better Settings                          Preview  Reload  [ Save ] │
├──────────────────┬──────────────────────────────────────────────────┤
│ Search pages     │ Compositor                                       │
│ ◉ Overview       │ Layout                                           │
│   Compositor     │  Gaps                    12                      │
│   Displays       │  Center focused column   never                   │
│   Sessions       │ Input                                            │
│   Wallpaper      │  Tap                     (switch)                │
│   Audio          │ Outputs                                          │
│   Side panels    │  eDP-1: Scale            1.50                    │
│   Automation     │ Binds (42)                                       │
│   Services       │  Mod+Return · spawn "foot"                       │
│   About          │                                                  │
└──────────────────┴──────────────────────────────────────────────────┘
```

### Pages

| Page | What it controls |
| --- | --- |
| Overview | compositor version and renderer capability, output/HDR counts, validation result, saved profile/context/scene counts, portal and companion status, config path; quick actions to validate, open and reload the config |
| Compositor | the complete catalog in Layout, Input, Outputs, Animations, Cursor and Misc groups, plus searchable read-only Binds, window/layer rules, Startup, Environment and column-width groups; Preview shows the unified diff, Save patches only changed lines |
| Displays | better-monitor profile selection with preview/apply/restore/confirm/cancel, the drag-and-snap arrangement editor, runtime HDR through `niri msg`, and permanent HDR mode/reference luminance in the catalog |
| Sessions | context list with Explain/Activate (preview first), capture-current-as-context with confirmation, and better-session history |
| Wallpaper | active scene and backend, scene selection/use, preview/apply, and workspace/session bindings from the scene map |
| Audio | backend status, per-sink/source volume and mute with preview/apply, default sink, routing rules and stream apply/restore |
| Side panels | config summary, validation, config path and open action, named layouts with a read-only `--check` preview, and panel details |
| Automation | companion daemon/status and registry, rules explain/apply, idle and rotation policy status plus enable/disable, window marks, run history, and a custom dry-run/apply action |
| Services | installed companion CLIs and versions, better-portal unit and `portals.conf` selection hint, Noctalia plugin source status, and a copyable diagnostics report |
| About | versions, docs links, the editing/safety model, F1 and colour-scheme notes |

### Dependencies

The `gui` feature links GTK 4 and libadwaita. The crate is `libadwaita 0.9.2`
with the `v1_5` and `gtk_v4_10` features; it targets a system libadwaita 1.10
(`pkg-config --modversion libadwaita-1`). `cargo build --no-default-features`
builds the CLI only and needs neither GUI library.

Values present in the config are loaded; changing a row and pressing **Save**
patches only those lines. **Preview** shows the diff without writing. **Reload**
re-reads the file, rebuilds every page, and reports structural/round-trip
validation errors. Companion pages use the same explicit split: their own dry
run where the CLI supports one, otherwise a confirmation dialog. Another app's
config is never rewritten; see [Other apps (integrations)](#other-apps-integrations).

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

## Other apps (integrations)

`better-settings` is also the unified frontend for the companion apps, but it
never owns their data. It reads each app's JSON output or config file, shows it
in the GTK window, and shells out to the app's own CLI for changes. No
companion settings are copied into niri's config.

| App | Read | Safe actions |
| --- | --- | --- |
| `niri` | `msg --json version`, `capabilities`, `outputs` | runtime HDR mode; reload config |
| `better-monitor` | `list`/`show` profiles, `position` geometry | dry-run preview, apply, restore, confirm/cancel a pending apply; drag/reposition the arrangement editor and apply it |
| `better-session` | `list`/`show` contexts (workspaces, windows, outputs), `history list` | `explain` preview, restore/activate, explicit capture |
| `better-wallpaper` | `status` (active scene, scenes, backend) and `scene list` | use a scene, dry-run and real apply |
| `better-audio` | `status`, `sinks`, `sources`, `rules` | set the default sink; mixer volume/mute with preview/apply; dry-run and real apply/restore |
| `better-sidepanels` | `config.json` (`--config`, `$XDG_CONFIG_HOME`), named layouts | `--validate`, read-only `--check --profile <layout>`, open in `$VISUAL`/`$EDITOR` (never rewritten) |
| `better-companion` | `companionctl describe`/`status`/`list`/`history`, `explain rules|windows`, `call idle|rotate status`, `call marks list` | dry-run an action with `--dry-run`; rules run, idle/rotate enable/disable and real calls require confirmation/`--apply` |
| `better-portal` | `systemctl --user is-active/is-enabled better-portal`, `portals.conf` selection hint | none (read-only) |
| Noctalia plugins | plugin source directory and manifests (or `--version`) | none (read-only) |
| all | tool discovery | lists which CLIs are installed; missing tools render a note instead of failing |

```sh
better-settingsctl integrations                 # installed CLIs and versions
better-settingsctl overview                     # aggregate status JSON
better-settingsctl compositor status            # niri version, renderer capability, HDR
better-settingsctl compositor reload --apply
better-settingsctl displays list
better-settingsctl displays preview desk        # always a dry run
better-settingsctl displays apply desk          # preview only
better-settingsctl displays apply desk --apply  # explicit mutation
better-settingsctl displays positions
better-settingsctl displays arrange '{"eDP-1":[0,0],"DP-1":[1920,0]}' --apply
better-settingsctl displays restore --apply
better-settingsctl displays confirm --apply
better-settingsctl sessions list
better-settingsctl sessions history
better-settingsctl sessions explain work
better-settingsctl sessions capture work --apply
better-settingsctl sessions activate work --apply
better-settingsctl wallpaper status
better-settingsctl wallpaper use night --apply
better-settingsctl wallpaper apply --apply
better-settingsctl audio status
better-settingsctl audio default alsa_output.speakers --apply
better-settingsctl audio volume sink alsa_output.speakers 0.5 --apply
better-settingsctl audio mute source alsa_input.usb-mic on --apply
better-settingsctl audio apply --apply
better-settingsctl sidepanels status
better-settingsctl sidepanels layouts
better-settingsctl sidepanels check work
better-settingsctl sidepanels validate --config ~/.config/better-sidepanels/config.json
better-settingsctl sidepanels open
better-settingsctl companion describe
better-settingsctl companion history 20
better-settingsctl companion explain rules
better-settingsctl companion rules --apply
better-settingsctl companion idle status
better-settingsctl companion idle enable --apply
better-settingsctl companion rotate status
better-settingsctl companion marks
better-settingsctl companion run parking park              # dry run
better-settingsctl companion run parking park --apply      # real call
better-settingsctl portal status
better-settingsctl noctalia status
```

Mutating commands print the tool's own preview and refuse to act without
`--apply`. The GTK window has the same split: read-only groups plus explicit
buttons, with previews shown in a scrollable report dialog. Header **Preview**
shows the config diff, **Reload** re-reads everything and **Save** patches only
changed rows. Sidebar pages are the CLI sections; the CLI remains the scriptable
surface for everything the window can do.

Tool paths can be redirected for wrappers or tests:
`BETTER_SETTINGS_BETTER_MONITOR`, `BETTER_SETTINGS_BETTER_SESSION`,
`BETTER_SETTINGS_BETTER_WALLPAPER`, `BETTER_SETTINGS_BETTER_AUDIO`,
`BETTER_SETTINGS_BETTER_SIDEPANELS` and `BETTER_SETTINGS_COMPANIONCTL`. The
sidepanels path follows `BETTER_SIDEPANELS_CONFIG` and `--config`; `niri` can be
redirected with `BETTER_SETTINGS_NIRI`, the portal selection file with
`BETTER_SETTINGS_PORTALS_CONF`, and the Noctalia plugin source with
`BETTER_SETTINGS_NOCTALIA_PLUGINS`.

### Displays arrangement editor

The Displays page reads `better-monitor position` and draws every enabled
output as a rectangle proportional to its logical size. Drag a rectangle to
move it; the editor snaps its edges/centre to other outputs and then to a 10 px
grid, and shows the candidate logical coordinates. The X/Y spin buttons edit
the selected output directly.

`Preview arrangement` validates the model and runs
`better-monitor arrange --plan <json> --dry-run`; `Apply arrangement` refuses
overlaps or an empty/disabled layout before sending anything, previews, then
applies. `Reset` re-reads the live positions. The plan is
`{"outputs": {"<output>": [x, y]}}`, exactly what better-monitor accepts.

### Audio mixer

The Audio page lists sinks and sources with their current volume and mute
state. Sliders and mute switches only change local state until `Apply mixer`
is pressed; `Preview mixer` sends the same changes with `--dry-run`. Every
change is a `better-audio volume|mute <kind> <device> <value>` command, and the
page shows the active backend. If `better-audio` is missing, the page renders a
note and no controls.

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
cargo test                     # model, CLI, integrations and widget tests
cargo test --no-default-features   # CLI only, no GTK/libadwaita
cargo fmt --check
cargo clippy --all-targets
xvfb-run -a cargo test         # runs the headless GTK construction test
```

Integration tests use JSON fixtures captured from the real CLIs
(`tests/fixtures/`) and an in-memory `ScriptedRunner`, so they need neither the
apps installed nor a compositor. The widget test builds the whole libadwaita
window (all ten pages, sidebar and catalog rows) when a display is available;
without `DISPLAY`/`WAYLAND_DISPLAY` it skips itself and the model/integration
tests still cover the data path.
