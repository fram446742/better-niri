# better-sidepanels

Persistent layer-shell side panels for niri. Part of the Better Desktop
ecosystem; lives inside this repository so no new repository is created
(excluded from niri's Cargo workspace).

## What it does

One panel per output, on the configured side, with an **exclusive zone** so
tiled windows do not overlap it. A panel is a vertical list of rows:

- **Label** — static text.
- **Command** — a shell command run on left click.
- **Module** — a shell command whose first output line is shown right-aligned
  and refreshed every `interval_secs`.

The panel is rendered with cairo into a shared-memory buffer (ARGB32), so text
uses the system font and the panel scales with the surface size.

## Usage

```sh
better-sidepanels --init          # write the default config
better-sidepanels --check         # print the loaded config and exit
better-sidepanels                 # run the panel
```

Run it from niri's `spawn-at-startup`:

```kdl
spawn-at-startup "better-sidepanels"
```

Config path: `$BETTER_SIDEPANELS_CONFIG`, then
`$XDG_CONFIG_HOME/better-sidepanels/config.json`.

## Configuration

```json
{
  "side": "right",
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
    "row": "#1a1f26"
  },
  "rows": [
    { "label": "Niri", "command": "niri msg version" },
    { "label": "Clock", "module": { "command": "date +%H:%M", "interval_secs": 5 } },
    { "label": "Volume", "module": { "command": "wpctl get-volume @DEFAULT_AUDIO_SINK@", "interval_secs": 10 } }
  ]
}
```

Colours accept `#rgb`, `#rrggbb` and `#rrggbbaa`. Missing keys fall back to the
defaults above.

## Layout and behaviour

- Header with the panel title and an accent rule.
- Rows stack from the top; clicking a row with a `command` spawns it detached.
- Hover highlights the row.
- The accent stripe sits on the panel's outer edge (left panel → left edge).
- Modules refresh on their own interval even when the panel is idle; a module
  that fails is displayed blank.

## Test

```sh
cargo test
cargo clippy --all-targets
```

The library tests cover config parsing/defaults, layout and hit testing, module
refresh/throttling, click commands, colour parsing, and the cairo renderer
(background/rows/accent/hover/module output) without needing a compositor.
