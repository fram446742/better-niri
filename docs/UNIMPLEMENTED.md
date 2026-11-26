# Remaining reference-fork gaps — analysis and plan

This file records the items still not implemented after the 2026-11-26 port
round, why each one is not a small local change, and the concrete path to land
it. Everything listed as “landed” below is in the tree and covered by tests
unless a hardware note says otherwise.

## Landed in this round

- niri-spicy: `hdr { peak-luminance N }` (metadata, tone-map peak and
  advertised luminances), per-output `allow-tearing` (`always`/`never`/
  `fullscreen`), initial `xdg_toplevel::State::Suspended` while locked or
  powered off, and asynchronous NVIDIA frame-fence waits before KMS queueing.
- demoji: focused-workspace-only active decorations; `overview { focus-ring
  { … } shadow { … } }` overrides.
- biri: sticky windows in focus switching/centering, pointer warp on carousel
  confirmation, hidden-workspace child placement.
- Naxdy: `relative-to "cursor"` floating spawn, blur `brightness`/`contrast`
  (alpha masking and saturation already existed), cursor-warp tweaks, real tile
  groups (auto-group, expel-to-column, tab titles/indicator, group-aware
  movement, flattened under tabbed columns) with the reference animation tests,
  layer open/close fades with Vulkan-aware closing snapshots, and the
  `org.kde.KWin.ScreenShot2` fd interface.
- Companion: `better-sidepanels-stack` (full zgavin/niri-sidepanels port with
  104 tests), portal recording, SNI tray, native video wallpapers and
  layer-shell overlay pins (`pin --layer`, live grim-verified).

## 1. HDR scanout colour pipeline, post-blend encode and HDR cursor plane

**Missing:** `niri-spicy`’s per-element `ScanoutColorTransform` map,
`post-blend` PQ encode offload, hardware cursor/overlay planes on HDR, and the
LUT-accelerated cursor encoder. Our TTY path still composites the cursor on
HDR and has no `scanout_color_transforms` call at all.

**Why it is not a local port:** the three features are one pipeline. The
post-blend and cursor-plane changes call `DrmCompositor::use_color_transforms`,
`use_post_blend_encode` and `set_cursor_buffer_transform`, and their inputs are
the per-surface transforms produced by `Niri::scanout_color_transforms`. That
function depends on Smithay’s plane color-pipeline discovery and on the
`ContentColor` conversion helpers; porting only the last two commits would
compile against an empty transform map and change nothing. It is also
hardware-behavioural: RADV validates the code paths, but NVIDIA is the target
of the post-blend path and the fence work, and we cannot validate KMS colour
programming on this machine.

**Plan:**
1. Port `ContentColor` / `get_surface_description` / `scanout_color_transforms`
   from niri-spicy (`96c7e4a7^` range) and wire the map into the TTY
   `render_frame` call plus `use_color_transforms`.
2. Port the composition-side gamut conversion (`Convert non-sRGB container
   primaries during composition`) and its readback tests.
3. Add `blend::hdr_post_blend_encode` + `post_blend_linear_transform` and the
   `SrgbToPqEncoder` (including `new_linear`), then the two TTY integrations
   (`use_post_blend_encode`, deferred gamma in `set_gamma`/vblank).
4. Switch the HDR cursor gate to the `disable-cursor-plane-on-hdr` opt-out and
   pass the linear cursor encoder.
5. Verify with `cargo test --workspace`, a RADV TTY pass (`--check-vulkan`,
   HDR fullscreen, direct scanout transition log) and an NVIDIA KMS run with
   explicit sync and a moving cursor. Do not claim hardware support until the
   NVIDIA run exists.

The reference commits are preserved in the local `niri-spicy` checkout:
`96c7e4a7`, `31b38918`/`fdf7d06b`, `2a052fa9`, `1bfaa833`.

## 4. KDE Screenshot2 — landed

`org.kde.KWin.ScreenShot2` is served from its own connection at
`/org/kde/KWin/ScreenShot2` alongside the old `org.kde.kwin.Screenshot`
interface. It implements `CaptureWorkspace`, `CaptureActiveScreen`,
`CaptureScreen`, `CaptureActiveWindow`, `CaptureArea` and `CaptureInteractive`
on top of niri's screenshot pipeline: capture replies with the encoded PNG in
memory (no file is written), which is decoded to RGBA8888 and written into the
client's pipe after the metadata reply, as KWin does. Metadata includes
`type`/`width`/`height`/`stride`/`scale`/`format`, plus `screen` or `windowId`
where they apply.

Interactive screen picks use a new `PickOutputGrab`; window picks reuse the
existing `PickWindowGrab`. Validation against a real Plasma portal or Spectacle
is still pending because no Plasma session is installed here.

## 5. better-shot true layer-shell pins — landed

`better-shot pin PATH --layer` now spawns `better-shot pin-view PATH`, which
renders a click-through `zwlr_layer_shell_v1` overlay (top-right, empty input
region) and stays alive until `pin unpin` sends SIGTERM. The PID is stored in
the registry (`pid`/`pid_alive` in JSON); `--layer` is opt-in and the
`xdg-open` fallback remains. Live-validated on the running niri session with a
red test image (grim pixel readback). 132 tests pass.

## 6. Piri plugin ABI/ecosystem

**Missing:** Piri-compatible compiled plugins and its plugin lifecycle.

**Why we do not implement it:** Piri is an in-process Rust `Plugin` trait with
a single-binary registry, not an ABI. “Compatibility” would mean vendoring Piri
and running better-companion inside it, replacing our UDS/JSON boundary; the
plugin ecosystem would still be source-compiled per binary, so it gives users
no installable-plugin model. Our earlier deviation record (§26.11 of the
expansion plan) documents the technical reason.

**What we can do:** if Piri plugin reuse becomes a hard requirement, add a
separate `piri-compat` crate that embeds Piri and exposes its plugins as
better-companion adapters (Piri is MIT; attribution preserved). Do not make it
the core daemon.

## 7. Hardware-only validations

Multi-GPU scan-out, suspend/resume resource cleanup, the new NVIDIA fence
path, VRR/tearing under a real client and HDR mixed contrast need a DRM seat.
The code paths exist and the non-hardware tests pass; a failure here is not
reported as validated until the hardware matrix in `docs/TESTING.md` is run.
