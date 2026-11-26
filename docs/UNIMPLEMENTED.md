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
  (alpha masking and saturation already existed), cursor-warp tweaks.
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

## 2. Window groups / tabbed tiles (Naxdy)

**Missing:** real tile groups: auto-grouping new windows, expel-to-new-column,
tab titles/indicator, click-to-activate, group-aware movement and animations.
We only have upstream’s `ToggleColumnTabbedDisplay`.

**Why it is not a small port:** it replaces the column/tile ownership model
(`Column { tiles }` becomes group-aware), touches layout, movement, input,
rendering and window rules, and the reference is a long-lived rebase fork whose
commits assume its own surrounding code. A partial port would either duplicate
the tab strip or break tabbed columns; it is a multi-thousand-line refactor
with a high regression risk and no way to validate visually here.

**Plan:** land it as an isolated feature branch in four steps: (a) introduce a
`Group` layer inside `Column` with no behavior change, (b) port group
add/expel/tab-title rules with unit tests on the layout model, (c) port tab
strip rendering + click routing, (d) port movement/animation integration. Keep
`ToggleColumnTabbedDisplay` as the compatibility path until the group model is
proven. Reference: `Naxdy/niri` commits under `feat(tile/group*)` and
`feat(tile/tabs*)`.

## 3. Layer-surface fade in/out animations (Naxdy)

**Missing:** `animations { layer-open … layer-close … }` and the fade itself.

**Why it is not a two-line port:** fade-out must keep a layer surface alive
after `wl_surface` unmap, render it to an offscreen snapshot, and composite
that snapshot with alpha until the animation ends. The reference refactors
`closing_window` into `closing_element` to support both windows and layers.
Our render path is Vulkan-aware and the existing closing-window code is
GLES-snapshot oriented, so this needs a backend-agnostic closing-element type
plus output-render integration.

**Plan:** (1) extract a generic `ClosingElement` snapshot type from
`layout/closing_window.rs`, (2) use it in `MappedLayer::render` for fade-in
(`OffscreenBuffer` + alpha), (3) on `layer_destroyed`, move the mapped layer
into a per-output closing list instead of dropping it, (4) advance/remove in
`advance_animations`, (5) add config parsing and tests. Reference:
`Naxdy/niri` `4df357ee`, `230e5705`.

## 4. KDE Screenshot2 `CaptureWorkspace` / Plasma portal

**Missing:** the newer KDE Screenshot2 fd-based interface with
`CaptureWorkspace`. The old `org.kde.kwin.Screenshot` interface we implement
already captures all outputs through `screenshotFullscreen`, so Spectacle’s
main path works, but Plasma’s portal talks to the newer interface.

**Why it is not a method alias:** Screenshot2 passes a writable pipe fd and
returns image metadata; it needs a screenshot-output adapter around our
`ScreenshotPathReplySender` pipeline and a second D-Bus object/name setup.
That is a contained port but it should be validated against a real Plasma
portal, which is not installed here.

**Plan:** port `kwin_screenshot2.rs` from `Naxdy/niri`, adapt
`ScreenshotOutput` to write into the supplied fd, expose `captureWorkspace` as
`ScreenshotTarget::AllOutputs`, and add a D-Bus smoke test with a private bus
(no compositor). Validate with Spectacle + the Plasma portal afterwards.

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
