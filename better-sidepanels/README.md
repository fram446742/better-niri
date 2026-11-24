# better-sidepanels

Persistent layer-shell panels for niri: a side dock, a top/bottom bar, or several
of them on every output. Part of the Better Desktop ecosystem; lives inside this
repository so no new repository is created (excluded from niri's Cargo
workspace).

## What it does

Each panel is a **layer-shell surface** on the top layer with an **exclusive
zone**, so tiled windows do not overlap it. Panels can be placed on `left`,
`right`, `top` or `bottom`, and one output can have several panels.

A panel is a list of rows:

- **Label** — static text with an optional `icon` prefix.
- **Separator** — a divider line, not clickable.
- **Module** — a built-in module (clock, workspaces, …) or a custom command
  whose first output line is shown and refreshed every `interval_secs`.
- **Actions** — per-row click and scroll-up/scroll-down commands or niri IPC
  actions. Module chips (workspace numbers) are individually clickable.

Everything is rendered with cairo into a shared-memory buffer (ARGB32), using a
configurable font, colours and background opacity.

## Usage

```sh
better-sidepanels --init          # write the default config
better-sidepanels --validate      # parse + validate, print problems and exit
better-sidepanels --check         # print the loaded config and exit
better-sidepanels                 # run the panels
```

Run it from niri's `spawn-at-startup`:

```kdl
spawn-at-startup "better-sidepanels"
```

Config path: `$BETTER_SIDEPANELS_CONFIG`, then
`$XDG_CONFIG_HOME/better-sidepanels/config.json`.

The `workspaces` and `window-title` modules talk to niri over
`$NIRI_SOCKET` (falling back to `$XDG_RUNTIME_DIR/niri.sock`). Without it those
rows stay blank; every other module still works.

## Modules

| `kind`         | Shows                                                   | Source                                                        |
| -------------- | ------------------------------------------------------- | ------------------------------------------------------------- |
| `clock`        | Date/time, `format` is a strftime pattern               | system clock + local UTC offset (`libc`)                      |
| `workspaces`   | Workspace chips: active highlighted, occupied brighter  | niri IPC `Workspaces`                                         |
| `window-title` | Focused window app and/or title, truncated              | niri IPC `FocusedWindow`                                      |
| `media`        | Player, artist and title                                | `playerctl metadata` (degrades to blank if missing)           |
| `battery`      | Capacity and status                                     | `/sys/class/power_supply/*`                                   |
| `cpu`          | Busy percentage since the previous sample               | `/proc/stat`                                                  |
| `memory`       | Used/total GiB and percentage                           | `/proc/meminfo`                                               |
| `network`      | Interface state and rx/tx rates                         | `/sys/class/net/<if>/…`                                       |
| `volume`       | Sink volume and mute state                              | `pactl`, falling back to `wpctl`                              |
| `custom`       | First line of a shell command                           | `sh -c`                                                       |

Module fields:

```json
{ "kind": "clock", "interval_secs": 1, "format": "%a %H:%M" }
{ "kind": "workspaces", "interval_secs": 1 }
{ "kind": "window-title", "interval_secs": 1, "format": "{app}: {title}", "max_chars": 40 }
{ "kind": "media", "interval_secs": 2 }
{ "kind": "battery", "interval_secs": 30, "format": "{percent}% {status}" }
{ "kind": "cpu", "interval_secs": 2 }
{ "kind": "memory", "interval_secs": 5 }
{ "kind": "network", "interval_secs": 2, "interface": "wlan0" }
{ "kind": "volume", "interval_secs": 5 }
{ "kind": "custom", "command": "date +%s", "interval_secs": 10 }
```

- `kind` defaults to `custom`, so v1 command modules keep working.
- `interval_secs` defaults to `30`. A value of `0` refreshes on every tick
  (250 ms).
- `max_chars` truncates the value with `...` (default 64 characters).
- Clock formats support `%H %M %S %I %p %d %e %m %Y %y %a %A %b %B %j %s %F %T`
  and `%%`. Names are English.
- `window-title` placeholders are `{app}` and `{title}`. If one side is missing
  the separator is trimmed instead of showing `-`.
- `battery` placeholders are `{percent}` and `{status}`
  (`charging`, `discharging`, `full`, `idle`, …).
- `network` picks `interface` when set, otherwise the first non-`lo` interface
  that is `up`. Rates are per-refresh deltas.
- `media` prefixes `[paused]` / `[stopped]`; artist or title may be empty.
- `volume` parses the first percentage from `pactl get-sink-volume`, plus
  `pactl get-sink-mute`; if `pactl` fails it tries `wpctl get-volume`.
- `cpu` and `network` are blank for one refresh while their first sample is
  collected.
- A module that fails, or a missing tool, renders blank rather than an error.

**Tray / status-notifier is not implemented.** Hosting SNI icons requires a
D-Bus client, `org.kde.StatusNotifierWatcher` registration and icon pixmap
handling — a new protocol stack this crate deliberately does not take on. A
`custom` module can query a specific notifier CLI, but it cannot enumerate
arbitrary tray icons.

## Configuration

The file is versioned JSON (v2). A v1 file (a bare panel object, as written by
older `--init`) is still accepted and wrapped in a single panel.

```json
{
  "version": 2,
  "outputs": {
    "DP-1": {
      "width": 300,
      "theme": { "bg": "#101418", "accent": "#f97b4f", "opacity": 0.92 }
    }
  },
  "panels": [
    {
      "side": "left",
      "width": 280,
      "margin": 10,
      "padding": 12,
      "row_height": 34,
      "title": "WORK",
      "theme": {
        "bg": "#101418",
        "fg": "#f0f0f0",
        "dim": "#888888",
        "accent": "#f97b4f",
        "row": "#1a1f26",
        "font": "Sans",
        "opacity": 1.0
      },
      "rows": [
        { "label": "Workspaces", "module": { "kind": "workspaces", "interval_secs": 1 } },
        {
          "label": "Window",
          "module": {
            "kind": "window-title",
            "interval_secs": 1,
            "format": "{app}: {title}",
            "max_chars": 40
          }
        },
        { "separator": true },
        { "label": "Media", "module": { "kind": "media", "interval_secs": 2 } },
        { "separator": true },
        { "label": "CPU", "module": { "kind": "cpu", "interval_secs": 2 } },
        { "label": "Memory", "module": { "kind": "memory", "interval_secs": 5 } },
        {
          "label": "Battery",
          "module": { "kind": "battery", "interval_secs": 30, "format": "{percent}% {status}" }
        },
        {
          "label": "Network",
          "module": { "kind": "network", "interval_secs": 2, "interface": "wlan0" }
        },
        {
          "label": "Volume",
          "module": { "kind": "volume", "interval_secs": 5 },
          "click": "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle",
          "scroll_up": "wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+",
          "scroll_down": "wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"
        },
        { "label": "Clock", "icon": "⏱", "module": { "kind": "clock", "interval_secs": 1, "format": "%a %H:%M" } }
      ]
    },
    {
      "side": "top",
      "output": "*",
      "margin": 4,
      "padding": 6,
      "row_height": 26,
      "theme": { "bg": "#101418cc", "font": "Sans", "opacity": 0.95 },
      "rows": [
        { "label": "Workspaces", "module": { "kind": "workspaces", "interval_secs": 1 } },
        { "label": "Window", "module": { "kind": "window-title", "interval_secs": 1 } },
        {
          "label": "Clock",
          "module": { "kind": "clock", "interval_secs": 1, "format": "%H:%M" },
          "click": "gnome-calendar"
        }
      ]
    }
  ]
}
```

### Panel fields

| Field        | Default    | Meaning                                                             |
| ------------ | ---------- | ------------------------------------------------------------------- |
| `output`     | all        | Output connector name, `"*"` or omitted for every output            |
| `side`       | `"left"`   | `left`, `right`, `top`, `bottom`                                    |
| `width`      | `260`      | Width of vertical panels; also the row-width basis                   |
| `margin`     | `8`        | Gap between the panel and the output edge (exclusive zone includes it) |
| `padding`    | `12`       | Inner padding                                                       |
| `row_height` | `34`       | Height of each row                                                  |
| `title`      | `"BETTER"` | Header text; vertical panels only                                   |
| `theme`      | see above  | Colours, `font`, `opacity`                                          |
| `rows`       | —          | List of rows                                                        |

### Row fields

| Field         | Meaning                                                                 |
| ------------- | ----------------------------------------------------------------------- |
| `label`       | Row text                                                                |
| `icon`        | Optional prefix glyph, e.g. a Nerd Font icon                            |
| `module`      | Module object (see above)                                               |
| `separator`   | `true` draws a divider row instead of a label                           |
| `command`     | v1 click command; kept for compatibility                                |
| `click`       | Click action (string command or niri action object); wins over `command` |
| `scroll_up`   | Action for wheel up / two-finger scroll up                              |
| `scroll_down` | Action for wheel down / scroll down                                     |

### Version 1 files

```json
{
  "side": "right",
  "rows": [{ "label": "Clock", "module": { "command": "date +%H:%M", "interval_secs": 5 } }]
}
```

This loads as a one-panel config with `version: 1`. `--init` writes v2.

### Colours

Colours accept `#rgb`, `#rrggbb` and `#rrggbbaa`. `theme.opacity` (0.0–1.0)
multiplies the background alpha, on top of any alpha in `theme.bg`.

## Interaction

- **Left click** a row runs its `click` action (or legacy `command`).
- **Scroll** a row runs `scroll_up` / `scroll_down`.
- **Workspaces** rows need no extra config: each workspace chip focuses that
  workspace (by stable id) on click, and scrolling cycles
  `focus-workspace-up` / `focus-workspace-down`. Explicit `scroll_up` /
  `scroll_down` on the row override cycling.
- Hovering highlights the row.
- A bare-string action is a shell command; an object is a niri IPC action:

```json
"click": { "niri": "focus-workspace", "index": 2 }
"click": { "niri": "focus-workspace", "id": 42 }
"click": { "niri": "focus-workspace", "name": "web" }
"click": { "niri": "focus-workspace-down" }
"scroll_up": { "niri": "focus-workspace-up" }
"click": "niri msg action focus-workspace 2"
```

Commands are spawned detached with `sh -c`; the daemon does not wait for them.
Panels do not take keyboard focus (`KeyboardInteractivity::None`).

## Layout

- `left` / `right` panels stack rows top to bottom under a header and reserve
  `width + margin` as their exclusive zone.
- `top` / `bottom` panels lay rows out left to right and are `padding * 2 +
  row_height` tall; they reserve `bar_height + margin`. The header title is
  skipped on horizontal bars.
- Multiple panels per output are supported by adding entries to `panels`
  (`left` dock + `top` bar, etc.). Two panels on the *same* edge should use
  different `margin` values; the compositor stacks their exclusive zones.
- `separator` rows render as a dim rule and are not clickable.
- Per-output overrides in `outputs` can change size, padding, title, side and
  individual theme fields for one connector without duplicating a panel.

## Reload

The config file is watched (mtime + size, polled every 250 ms). On a valid
change:

- if the panel structure (output, edge, count) is unchanged, panels update in
  place and module output refreshes — the layer surfaces are **not** recreated;
- if panels were added, removed or moved to another edge, surfaces are recreated.

An invalid reload is rejected and the previous config keeps running; run
`better-sidepanels --validate` to see every problem at once. There is no
`SIGHUP` handler; edit the file (or restart the process) instead.

## Security notes

- Module output is **display-only**. Window titles, app ids, media metadata and
  workspace names are never interpolated into a shell command, so a hostile
  title cannot execute code. There is no `{}` expansion of module values into
  actions.
- Commands come only from the config and run with your user's privileges via
  `sh -c`. Treat the config as executable code; prefer absolute paths.
- Workspace clicks send only a numeric workspace id in a fixed JSON request;
  display names are never sent to a shell.
- The niri client sends only `Workspaces`, `FocusedWindow` and
  `FocusWorkspace*` requests, with a 500 ms socket timeout.
- Module readers parse `/proc` and `/sys` entries defensively and treat every
  parse failure as blank output.

## Limitations

- JSON only; no TOML parser.
- No tray/status-notifier host (see Modules).
- `playerctl`, `pactl`/`wpctl` are optional; their rows simply stay blank.
- Top/bottom panels have no header title.
- Module polling is per panel: several identical panels on many outputs repeat
  the same reads (and niri requests).
- `battery` uses the first `Battery` entry; `network` uses one interface;
  `volume` uses the default sink.
- Clock month/weekday names are English, and the UTC offset is sampled per
  refresh (DST changes are picked up, but the panel does not know the timezone
  database directly).
- No buffer scale / font DPI setting: text is rendered at logical pixels.
- With `KeyboardInteractivity::None` the panel cannot receive keyboard shortcuts.
- Niri's IPC socket is the only IPC integration; there is no event-stream
  subscription, so modules poll on their intervals.

## Test

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

The 45 library tests cover config v1/v2 parsing, defaults, per-output overrides,
validation, layout (vertical and horizontal), hit testing and workspace chip
clicks, scroll action mapping, clock formatting, `/proc/stat` and
`/proc/meminfo` deltas, battery/network/volume/media parsing and fallbacks,
window-title truncation, reload watching, security (untrusted titles never reach
commands) and cairo renderer smoke tests for all four sides — no compositor or
real niri instance required.
