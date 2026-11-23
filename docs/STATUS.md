# Project status

Last updated: 2026-11-23.

## Safe baseline (`main`)

`main` is niri upstream `02ee0585` plus documentation, the upstream drift
script, and a pin to the pristine better-smithay fork (`a38bb709`, identical to
Smithay upstream). It has no behavioural changes.

## Integration branch: `integration/elite-base`

This is now the main integration line. It keeps niri upstream history as its
base and merges niri-elite as the feature base, then reconciles with current
upstream.

### better-smithay `integration/elite-base` @ `2e8e9edb`

- First parent: `MEKCCK/smithay` `66b0670` (`elite-color-management`):
  stabilised `wp-color-management-v1`, connector colour state, GLES colour
  transforms, DRM async page flips, `wp_tearing_control_v1`, per-client seat
  filtering.
- Second parent: the losnoco lineage (`dba9452b`): Vulkan renderer, DRM colour
  pipelines/colourop, SurfaceBarrier, tone-mapping helpers, Nvidia colour
  guards.
- Verified: `cargo check -p smithay` with niri's GLES feature set and with
  `--all-features` (Vulkan) both pass.
- Not hardware-tested.

### better-niri `integration/elite-base` @ `bbd46e08`

Brings in the full niri-elite feature set:

- grid overview, window minimisation, magnifier, pick-colour overlay;
- agent seat, automation socket and AT-SPI tooling;
- built-in XDG portal backend, native screen-cast picker, SHM fallback;
- SVG cursors, shake-to-enlarge, liquid glass, hot-corner actions;
- HDR / colour management, tearing and `wp_content_type_v1`;
- input bind extensions (press/release, bare modifiers, raw/ASCII keysyms);
- per-feature visual tests merged with upstream's tests.

Added on top of the elite base:

- per-keyboard configuration (core input): named `keyboard "device" { ... }`
  blocks, inheritance from the unnamed block, keymap/repeat/Num Lock/locale1/IPC
  updates on device switches, with headless tests.
- virtual outputs (core outputs): TTY/headless create/remove over IPC/CLI,
  declarative `create-virtual`, per-output mode/refresh handling.
- isolated outputs (core outputs): compositor chrome is kept off them, the
  overview is suppressed, and `power-off-monitors skip-isolated=true` keeps them
  lit while the rest sleep.

Reconciled with upstream `02ee0585`:

- focus flags on move actions, `pass_through` binds, DPMS frame-callback guards;
- ext-image-copy capture and the newer damage-tracker capture paths;
- stationary view position for tile IPC, station/gesture restriction;
- HDR screencopy uses `CaptureBuffer` and the native portal `node_sink` path.

Added on top of the elite base after the initial merge:

- KDE output order (`kde_output_order_v1`) and KDE colour picker D-Bus interface.
- Global post-process shaders (`global-shader`): multi-pass chains, feedback
  (`niri_prev`/`niri_buffer`), `niri_screen`/`niri_source`, region/cursor mode,
  redraw intelligence and black-seeded first frame.
- Scoped shaders: `region-shader` blocks and per-window `window-rule { shader }`
  chains with content-addressed compile cache and watched-file hot reload.
- Named `window-shaders`/`output-shaders` presets, per-output shader rules,
  `toggle-window-shader`/`cycle-window-shader` and
  `toggle-output-shader`/`cycle-output-shader` actions.
- Focus-ring shaders: animated wax/rainbow ring, per-window watched-file
  decoration shaders, padding envelopes, hot reload.
- Overview runtime zoom presets and `overview-zoom-cycle/in/out` actions.
- Layer-rule background-effect `ignore-opacity` (alpha-mask surface sampling).
- Hidden workspaces (`hidden`, `toggle-workspace-visibility`, hidden-aware
  focus/IPC/unhide rules) and sticky windows (`open-sticky`,
  `toggle-window-sticky`, monitor-level sticky space), plus
  `float-above-fullscreen`.
- Overview preset-based zoom cycling with `overview-zoom-cycle/in/out` and
  automatic overview open/close, plus the `overview-zoom` animation.
- Decoration light spill from focus-ring/border shaders.
- `kde` Cargo feature (default on) gating all KDE interop.
- Headless/virtual outputs compile configured global and scoped shaders at
  startup.
- Consolidated multi-monitor carousel, phases 1-2: `overview.consolidated-carousel`
  config, focused-output overview scoping, centered-output state and rotation,
  participant selection, card geometry with host clamping, scale-normalized
  rotating sibling rendering with edge fades, arrow/shift-scroll rotation, and
  the reveal/lens zoom bands and the lens pass that fits the centered output's
  overview into the host. The cover-flow panel-stack redesign (phase 3) is still
  pending.

Verified on this branch:

| Check | Result |
| --- | --- |
| `cargo check --workspace` | pass |
| `cargo test --workspace` | pass, 752 tests (1 order-dependent EGL golden test ignored) |
| `cargo clippy --workspace --all-targets` | pass (no errors; warnings remain in elite code) |
| `cargo fmt --check` | pass |
| `cargo build --release` | not re-run after the merge |

**Not verified:** any hardware behaviour (HDR, VRR, tearing, explicit sync,
pointer warp, direct scanout, DPMS, multi-GPU, screencasting). Do not merge to
`main` until the hardware matrix in `TESTING.md` has real results.

## What is published

- `fram446742/better-smithay` branch `integration/elite-base` (`2e8e9edb`).
- `fram446742/better-smithay` branch `integration/hdr-color` (`344be37b`, the
  previous losnoco-base experiment; kept for comparison).
- `fram446742/better-niri` `main` remains the safe baseline; the elite branch is
  pushed separately, never force-updated.

## Architecture decision (2026-11-23)

The module boundary is now documented and drives the next integrations:

- **Core**: presentation/HDR/colour, sync/tearing/FIFO, input (including
  per-keyboard), layout/window primitives, outputs, protocols, config/IPC.
- **Built-in modules**: `effects` (all shader scopes + liquid glass presets),
  `navigation` (grid/carousel/zoom/magnifier), `cursor` (SVG/shake), `agent`
  (off by default), `kde`, `vulkan`.
- **Companions**: better-companion (absorbs nsticky's staging/scratchpad
  workflow), better-session, better-sidepanels, better-shot, better-monitor,
  better-audio, better-wallpaper, better-settings, better-portal (separate
  portal daemon); shell integration stays Noctalia-only.

See `docs/MODULES.md` and `docs/ECOSYSTEM.md`. A per-keyboard branch
(`integration/biri-per-keyboard`) is parked with the config/tests commits
applied; it is core input work and resumes next.

## What is still open

Ordered by plan value. Each item names the upstream references so it can be
picked up without re-auditing.

1. **Hardware validation** of the current branch on AMD/Intel/Nvidia and
   multi-monitor (HDR, VRR, tearing, explicit sync, DPCM, multi-GPU,
   screencasting). The running compositor must stay untouched; use the
   headless/visual-test harness.
2. **Carousel cover-flow redesign** — biri `fbf672f4`…`54887e66`
   (`carousel_ring`/rotation, damage-gated `PanelSource`s `f8dcd26d`,
   `0d13d718`, cover-flow stack `bb818388`, concave panels `f0603a62`,
   pull-back `6d4d5542`, click-to-rotate `38a9513e`, lens focus-jump
   `54887e66`). The panel shader (`424ec5fb`) is already in place.
3. **Portrait / vertical main axis** — biri `b30284c7` (rewrites the scrolling
   axis; large).
4. **Vulkan renderer on the niri side** — better-smithay has the Vulkan
   renderer; niri still wires only GLES.
5. **KDE screenshot2** — `kde/main` `src/dbus/kwin_screenshot2.rs` needs
   Naxdy's capture abstraction; colour picker and output order are already
   integrated.
6. **Ecosystem** — `better-portal` (separate daemon), `better-companion`
   (workspace with `companion-core`, `-parking`, `-automation`, `-hooks`,
   `-system`, `companionctl`), and the companions listed in `ECOSYSTEM.md`.
   Companion repos start only after Phase 1.
7. **Remaining built-in module features** — Cargo features for `effects`,
   `navigation`, `cursor` and `vulkan` (the `kde` feature is done), so a
   minimal build can drop them; the code is currently always compiled with
   `kde` gated.
8. **Multi-action binds** — deferred cross-cutting refactor touching ~59
   bind/config/IPC/UI conflict points; DWT is already integrated separately.
