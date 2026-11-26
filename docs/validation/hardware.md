# Hardware validation pass

The remaining checks need a real DRM seat. Run them from a text console so the
script can start the isolated compositor directly; it does not touch the
desktop session's compositor or configuration.

## One command

Switch to a free VT (for example `Ctrl+Alt+F3`), log in as the same user and
run:

```sh
cd /home/franc/Projects/loom-niri/loom-niri
HDR_OUTPUT=DP-1 ./scripts/pending-checks/hardware-validation.sh
```

Omit `HDR_OUTPUT` for a first SDR-only run, or set `HW_OUTPUT=DP-1` to pin the
output. The script:

1. Rebuilds the isolated test config with `isolated-session.sh`.
2. Starts `target/debug/niri` on the current VT with `NIRI_RENDERER=vulkan`.
3. Checks the renderer log, `niri msg capabilities` (sticky, HDR, tearing,
   FIFO, explicit sync, pointer warp, commit-timing, SurfaceBarrier, capture),
   output enumeration, VRR toggle/read-back, 10-bit scanout formats, explicit
   sync and HDR signalling.
4. Captures a screenshot through IPC and verifies it is a PNG.
5. With `HDR_OUTPUT` set, prepares the HDR config and plays the HDR10 clip for
   `HDR_SECONDS` (default 20 s).
6. Stops the compositor and writes `PASS/FAIL/SKIP` lines to
   `/tmp/hardware-validation/summary.txt`, with the full log in
   `/tmp/hardware-validation/niri.log`.

`--check` only validates the prerequisites and prints the plan without starting
a compositor.

## Manual checklist printed by the script

These need eyes or a real client and are not asserted automatically:

- tearing with `allow-tearing true` (fullscreen video/game);
- a `wp_commit_timing_v1` client landing on its target presentation time;
- an output on the secondary GPU/render node;
- 90/270-degree output transitions;
- SDR desktop contrast under `hdr mode="on"` vs `"auto"`;
- closing/opening animations, popups, blur/xray in a live Vulkan session;
- portal screenshot/screencast through a real client.

## Recorded result (2026-11-25)

`HDR_OUTPUT=DP-1` on DP-1 (RX 9070 XT, RADV):

```
PASS Vulkan renderer selected
PASS renderer plan: preference=vulkan, path=Vulkan, render-tree-vulkan=true
PASS capabilities (sticky, hdr, tearing, fifo, explicit sync, pointer warp,
     commit-timing, surface barrier, capture)
PASS output DP-1 enumerated
PASS VRR toggled on and reported enabled
PASS 10-bit scanout formats kept (AB30)
PASS explicit sync enabled for the primary GPU
SKIP HDR mode=auto at startup (signals only with HDR content)
PASS multi-device setup: 2 DRM devices
PASS runtime HDR overrides: auto -> off -> auto
PASS screenshot captured through IPC (249160 bytes, PNG)
PASS HDR clip entered and left HDR signalling (hdr=true -> hdr=false)
```

The first rerun exposed and fixed a real bug: reconnecting an output for an HDR
mode change with an empty workspace panicked in `Monitor::new`. The layout now
keeps the no-output state empty and drops empty placeholders on reconnect, with
a dedicated test.

After the run, the summary and log stay under `/tmp/hardware-validation` so the
results can be reviewed without repeating the session.
