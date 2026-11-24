# Pending-feature checks (Vulkan/portal/companions)

Run these **inside** a niri session on tty3 with `NIRI_RENDERER=vulkan`
(switch with Ctrl+Alt+F3, log in, start niri, open a terminal).

## Isolated config

`isolated-session.sh [DIR]` clones `~/.config/niri` into `/tmp/niri-test-session`
(override with `NIRI_TEST_DIR`) and validates it. Run the test session with
`./target/debug/niri -c "$NIRI_TEST_DIR"` (the **elite** build; stock
`/usr/bin/niri` does not accept a directory and fails with "Is a directory").
The scripts default to the repo's `target/debug/niri` via `NIRI_BIN`. `HDR_OUTPUT=DP-1` adds an `hdr mode="on"` block to the copy.

## Debug files

`run-test-session.sh` starts the isolated session with
`RUST_LOG=niri=debug`, using `NIRI_LOG` (default `/tmp/niri-test-session.log`).
`drive.sh` snapshots IPC state into `/tmp/pending-drive/`. `hdr.sh` writes the
clip probe, the full mpv log and a combined HDR summary
(`/tmp/pending-hdr-summary.txt`, `/tmp/pending-hdr-mpv.log`). All scripts also
tee to `/tmp/pending-<name>.log`, so the whole run can be analyzed afterwards.

## drive.sh — windows, clipping, workspaces, overview, screenshots

```sh
./scripts/pending-checks/drive.sh          # uses `niri` from PATH
```

Spawns terminals, moves/floats/closes windows, switches workspaces, toggles the
overview/carousel and triggers screenshot captures. It ends with a visual
checklist. Expected on Vulkan today: windows, rounded-corner clipping, borders,
shadows and decoration shaders work; window/global/scoped shader effects,
blur/xray, liquid glass, carousel panel contents, magnified screenshot capture
and screen transitions still fall back/skip.

## hdr.sh — HDR output

Needs: an HDR-capable output (your DP-1 EDID advertises PQ/BT2020), `mpv` and
`ffmpeg` with libx265 + zscale. The script generates a 20 s HDR10 clip with
ffmpeg when `$HDR_CLIP` does not exist, then plays it with HDR passthrough.

```sh
# defaults to /home/franc/Downloads/hdr10+test_lake_2021_02_01.mp4
HDR_CLIP=/path/to/hdr10.mp4 ./scripts/pending-checks/hdr.sh
```

Check the HDR badge in mpv and that highlights are not clipped. **HDR output on
the Vulkan renderer is not wired yet**: this run should be compared against
`NIRI_RENDERER=auto` (GLES) to see the current gap. For a real HDR test also
enable HDR on the output in the niri config.

## portal.sh — better-portal

Needs: `better-portal` built
(`cargo build --manifest-path better-portal/Cargo.toml`), a session bus, and
interactive selection for screenshot/region calls.

```sh
./scripts/pending-checks/portal.sh
```

Calls Screenshot, PickColor and ScreenCast.CreateSession over D-Bus. A full
ScreenCast Start/stream needs a real PipeWire client (OBS or the
`xdg-desktop-portal` test app); the portal proxies niri's capture, so niri must
be running.

## companion.sh — rules, parking, swallow, run-or-raise

Needs: `better-companion` built
(`cargo build --manifest-path better-companion/Cargo.toml`) and a running niri
session for compositor/park/swallow.

```sh
./scripts/pending-checks/companion.sh
```

Starts a temporary daemon on `/tmp/better-companion-test.sock` and exercises
rules, parking, window find/run-or-raise and swallow status. Swallowing itself
needs a child launched from the terminal after enabling it.

## Manual checks that need a real key press

- Screenshot UI magnifier: open your screenshot bind, inspect the magnifier if
  configured, press Escape. (No `wtype`/`ydotool` installed here.)
- Screen transitions: configure `animations` and switch workspaces; the script
  does the switching, you check the animation.
- Blur/xray/liquid glass/carousel: add the corresponding `window-rule`/
  `overview`/shader config, then run `drive.sh` and watch the documented
  expected degradation on Vulkan.
