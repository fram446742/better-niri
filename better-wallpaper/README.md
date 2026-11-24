# better-wallpaper

Per-workspace wallpaper scenes for niri. Part of the Better Desktop ecosystem;
lives inside this repository so no new repository is created (excluded from
niri's Cargo workspace).

## Features

- Named **scenes** mapping workspace names (plus an optional `default` image) to
  image paths.
- Two backends:
  - **native** — one built-in `zwlr_layer_shell_v1` background surface per
    output, drawn with `wl_shm`. No `swaybg`/`swww` needed. Supports PNG and
    JPEG, per-output images, HiDPI scale and output rotation, and reloads an
    image when its file changes.
  - **command** — the original behaviour: an external command template per
    output (`swaybg`/`ssww` style) with `{image}`, `{workspace}` and `{output}`
    placeholders.
- `auto` (the default) prefers the native backend and degrades to the command
  template when the compositor cannot host layer-shell backgrounds.
- `apply` sets the wallpaper once; `watch` follows niri's event stream and
  re-applies on workspace focus changes.
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

Backends are selected with `--backend=auto|native|command` on `apply`/`watch`,
or with `$BETTER_WALLPAPER_BACKEND`:

```sh
# Native renderer on every output, failing loudly if unavailable.
better-wallpaper watch --backend=native

# External commands (the pre-native behaviour).
better-wallpaper watch --backend=command

# Equivalent to the default: native when supported, command otherwise.
export BETTER_WALLPAPER_BACKEND=auto
better-wallpaper watch
```

Example with `swww` (command backend):

```sh
export BETTER_WALLPAPER_COMMAND='swww img {image} --outputs {output}'
better-wallpaper watch --backend=command
```

`--socket PATH` overrides `$NIRI_SOCKET`. `--dry-run` queries niri and prints
the image resolved for each output without creating surfaces or spawning
anything (`apply --dry-run` with `--backend=command` keeps printing the
single-workspace result).

## Native backend

Each output gets one layer-shell surface on the **background** layer, anchored
to all four edges with an exclusive zone of `0`, so it covers the output without
reserving space for windows. The configured image is scaled to **cover** the
surface and centered (aspect-fill: no letterboxing, edges cropped); the buffer
is allocated at `logical size × output scale` and advertised with
`wl_surface.set_buffer_scale`, and the compositor applies output rotation.

Requirements: a Wayland session whose compositor implements
`zwlr_layer_shell_v1` (niri does), plus `wl_compositor` and `wl_shm`. `probe`
checks those globals before anything is drawn; `status` reports the result:

```console
$ better-wallpaper status
{
  "backend": {
    "requested": "auto",
    "native_available": true,
    "native_error": null
  },
  ...
}
```

Behaviour:

| Situation | Result |
| --- | --- |
| `--backend=native` but no layer-shell | error explaining why, suggests `--backend=command` |
| `--backend=auto` but no layer-shell | warning, command backend is used |
| New output / monitor plugged in | background surface created for it |
| Output mode or scale change | buffer is re-rendered at the new geometry |
| Output removed | surface is dropped; the process keeps running |
| Focused workspace changes (`watch`) | each output re-resolves its workspace image |
| Output has no named workspace | scene `default` is used |
| Image file modified | re-decoded and re-rendered within a second (mtime+size) |
| niri IPC drops or restarts | event watcher reconnects every second |
| Compositor disconnects | session is rebuilt every second until it is back |
| Image missing or undecodable | dark fallback colour, retried when the file changes |

`apply` with the native backend renders every output and **keeps running** so
the surfaces stay mapped (like `swaybg`); use `watch` to also follow workspace
focus. Set `BETTER_WALLPAPER_DEBUG=1` to log surface creation, configure sizes
and buffer commits.

Limitations (native backend):

- No transitions or animations between images; a change is an immediate swap.
  The design has no animation loop, so adding one would need a frame-driven
  cross-fade, not just a bigger buffer.
- Images are decoded and scaled on the CPU when they change, which can take a
  moment for large photos on very high-resolution outputs; steady-state CPU use
  is idle (no frame callbacks, one timer tick per second).
- The active scene is read at startup; after `scene set`/`scene use`, restart
  `watch` to pick it up (same as the command backend).
- Per-output images come from the scene of each output's active workspace; there
  is no per-output override file yet.
- Wayland only. Under X11 or a compositor without layer-shell, use
  `--backend=command`.

## Resolution

For an output whose active workspace is named `main`: exact
`wallpapers["main"]`, else the scene `default`, else nothing (the native backend
paints a dark fallback). Unnamed workspaces only use the default. The command
backend resolves for the globally focused workspace; the native backend resolves
per output.

## Test

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

The 30 unit tests cover scene resolution and JSON parsing, aspect-fill geometry,
rotation/scale, image reload stamps, rendering to XRGB8888, backend selection and
degradation, and the command watcher/`watch` IPC loop against a fake niri socket.
No test needs a running compositor: Wayland is only probed by `status`, and the
native session is exercised manually.
