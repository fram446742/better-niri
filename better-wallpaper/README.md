# better-wallpaper

Per-workspace wallpaper scenes for niri. Part of the Better Desktop ecosystem;
lives inside this repository so no new repository is created (excluded from
niri's Cargo workspace).

## Features

- Named **scenes** mapping workspace names (plus an optional `default` image) to
  image paths.
- `apply` sets the wallpaper on the currently focused workspace once.
- `watch` follows niri's event stream and re-applies on workspace focus changes.
- One wallpaper process per output: switching workspace replaces the previous
  process after a short grace period, so a just-spawned command is not lost.
- External command template with `{image}`, `{workspace}` and `{output}`
  placeholders, from `$BETTER_WALLPAPER_COMMAND` (default `swaybg -i {image}`).
- JSON state under `$XDG_STATE_HOME/better-wallpaper/scenes.json` (override with
  `BETTER_WALLPAPER_STATE`).

## Usage

```sh
export NIRI_SOCKET=...        # normally set inside niri

better-wallpaper scene set work main ~/Pictures/main.png
better-wallpaper scene set work default ~/Pictures/default.png
better-wallpaper scene use work
better-wallpaper status
better-wallpaper apply --dry-run
better-wallpaper watch --dry-run
better-wallpaper watch        # keep running, e.g. from niri's spawn-at-startup
```

Example with `swww`:

```sh
export BETTER_WALLPAPER_COMMAND='swww img {image} --outputs {output}'
better-wallpaper watch
```

`--socket PATH` overrides `$NIRI_SOCKET`.

## Resolution

For a focused workspace named `main`: exact `wallpapers["main"]`, else the
scene `default`, else nothing. Unnamed workspaces only use the default.

## Test

```sh
cargo test
cargo clippy --all-targets
```
