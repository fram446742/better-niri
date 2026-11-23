# better-settings

A small GTK4 frontend and a scriptable CLI for niri's KDL config. Part of the
Better Desktop ecosystem; lives inside this repository so no new repository is
created (excluded from niri's Cargo workspace).

## Why it edits text

Most config GUIs re-serialize the file, which drops comments, ordering and
hand-written structure. `better-settings` scans the existing text, records where
each `key value` lives inside its block, and rewrites only the lines it changes.
Everything else stays byte-identical, so the config remains reviewable and
versionable.

## GUI

```sh
cargo build --features gui      # or just `cargo build` (gui is the default)
./target/debug/better-settings
```

The window lists a catalog of settings (layout, input, focus ring, outputs,
screenshots, animations, cursor). Values present in the config are loaded;
changing a row and pressing **Save** patches only those lines. **Reload** re-reads
the file.

Config path: `$BETTER_SETTINGS_CONFIG`, then `$NIRI_CONFIG`, then
`$XDG_CONFIG_HOME/niri/config.kdl`, then `~/.config/niri/config.kdl`.

Every save copies the previous file to `<name>.bak-<epoch>` (with a numeric
suffix if the same second is saved twice) and writes through a temporary file.

## CLI

```sh
better-settingsctl list
better-settingsctl get layout.gaps
better-settingsctl set layout.gaps 24
better-settingsctl set layout.gaps 24 --dry-run
better-settingsctl set prefer-no-csd true
better-settingsctl set 'output "eDP-1".scale' 2
better-settingsctl diff          # print the config with catalog values re-applied
```

Values are validated against the catalog (`float` ranges, integer ranges,
booleans, choices) before anything is written. Paths that are not in the catalog
are written as raw strings.

## Supported syntax

- Blocks and nested blocks: `input { keyboard { … } }`.
- Block arguments: `output "eDP-1" { … }`; the same key in different outputs
  is edited independently.
- Bare flags: `tap`, `natural-scroll`, `prefer-no-csd`; setting them to `false`
  removes the line.
- `//` and `/* … */` comments are never treated as settings and are preserved.
- Inline child blocks (`presets { … }` on one line) are left untouched.

Missing keys are inserted into their block; missing blocks are created (nested
as needed) at the end of the file or inside the deepest existing parent.

## Test

```sh
cargo test                     # model, no GTK needed
cargo test --no-default-features
cargo clippy --all-targets
```
