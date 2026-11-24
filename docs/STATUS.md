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
  updates on device switches, with headless tests. The first keyboard applies
  its config eagerly when it appears (hotplug included); later keyboards switch
  on first use, and unplugging the active keyboard reverts to the unnamed
  config.
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

- KDE output order (`kde_output_order_v1`), KDE colour picker and
  `org.kde.kwin.Screenshot` (fullscreen/area/window/window-under-cursor/
  interactive) D-Bus interfaces, all gated by the `kde` feature.
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
  automatic overview open/close, the `overview-zoom` animation, and a zoom
  target that persists across close (clamped at the carousel assembled zoom).
- Decoration light spill from focus-ring/border shaders.
- `kde` Cargo feature (default on) gating all KDE interop.
- Headless/virtual outputs compile configured global and scoped shaders at
  startup.
- Consolidated multi-monitor carousel, phases 1-2: `overview.consolidated-carousel`
  config, focused-output overview scoping, centered-output state and rotation,
  participant selection, card geometry with host clamping, scale-normalized
  rotating sibling rendering with edge fades, arrow/shift-scroll rotation, and
  the reveal/lens zoom bands, damage-gated per-output panel sources, and the
  cover-flow perspective panel stack, concave panels, pull-back choreography
  cancelled by user input, rotation from any overview zoom, and click-to-rotate
  side panels, and the Enter/click lens focus-jump to the real output.

Verified on this branch:

| Check | Result |
| --- | --- |
| `cargo check --workspace` | pass |
| `cargo test --workspace` | pass, 833 tests (1 order-dependent EGL golden test ignored) |
| `cargo check --workspace --no-default-features` | pass (workspace feature unification may keep defaults on for the `niri` lib) |
| `cargo test --workspace --no-default-features` | pass, 833 tests |
| `cargo check -p niri --no-default-features` | pass (true minimal build) |
| `cargo check -p niri --no-default-features --features vulkan` | pass |
| `niri --check-vulkan` | pass, probes two RADV devices; `NIRI_RENDERER=vulkan` resolves to `path: vulkan` |
| Vulkan on a real TTY | pass (2026-11-24): Vulkan renderer active, border/shadow SPIR-V compiled, DP-1 4K output created |
| `cargo test -p niri --no-default-features` | pass, 403 tests (effects/navigation tests compiled out) |
| `cargo test -p niri --no-default-features --features navigation` | pass, 631 tests |
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

See `docs/MODULES.md` and `docs/ECOSYSTEM.md`. Per-keyboard configuration is
done: `input { keyboard "Device Name" { … } }` blocks merge over the unnamed
block, the active keymap/repeat config is resolved per physical device, the
first keyboard's config is applied as soon as the device appears (so the first
keystroke is already correct), later keyboards switch on first use, and
unplugging the active keyboard reverts to the unnamed config. Layout changes
emit `KeyboardLayoutsChanged` and `niri msg keyboard-layouts` reflects the
active keymap.

## What is still open

Ordered by plan value. Each item names the upstream references so it can be
picked up without re-auditing.

1. **Hardware validation** of the current branch on AMD/Intel/Nvidia and
   multi-monitor (HDR, VRR, tearing, explicit sync, DPCM, multi-GPU,
   screencasting). The running compositor must stay untouched; use the
   headless/visual-test harness.
2. **Carousel — done.** Cover-flow redesign, lens, pull-back, click-to-rotate,
   lens focus-jump, plus the robustness/refinement series (exit/hotplug/refocus
   `da80bb8a`, `54366b6a`, ring reconciliation `a0428f1e`, sibling progress
   gating + lens workspace switching `c298638a`, lens workspace/column routing
   `920551ce`, `b2af6a07`, continuous placement `30dc96e1`, live-zoom/target-scale
   bake `186f66e9`, generation preservation `82cce136`, superseded-code cleanup
   and default-config docs `e79bd375`).
3. **Portrait / vertical main axis** — biri `b30284c7` (rewrites the scrolling
   axis; large).
4. **Vulkan renderer** — wired for the TTY backend (`TtyGpuManager`/`TtyRenderer`
   over GLES or Vulkan, `NIRI_RENDERER`). Borders and shadows now also render on
   Vulkan: their GLSL sources are compiled to SPIR-V through better-smithay's
   custom pixel pipeline at renderer init, together with the rounded-corner
   clipping texture program and the configured decoration shaders; a test
   compiles them on a real `VulkanRenderer`. Resize animations keep their animated stretch on GLES; on
   Vulkan the window renders normally during a resize (no red fallback, no
   stretch). Runtime validation on a real TTY passed on 2026-11-24 with the
   clipping/decoration build (Vulkan renderer active, border/shadow/clipping/
   decoration shaders compiled, 4K DP-1 output created, no renderer errors
   during a ~2.5 minute session).
   HDR fullscreen passthrough works on Vulkan (validated 2026-11-24: the DRM
   compositor came up with hdr=true, 10-bit formats, and "updated HDR
   signalling" while a fullscreen HDR10 clip played with mpv tone-mapping to
   the display's 1083-nit peak); SDR/HDR mixed compositing now runs on Vulkan
   too: HDR frames install the `texture_hdr` override for default texture draws
   (SDR greys/edges are PQ-encoded instead of raw sRGB, which fixes the rough
   text/antialiasing seen before), solid colours are sRGB→PQ encoded on the
   CPU, HDR-PQ content passes through on HDR frames and converts back with
   `texture_hdr_to_sdr` for SDR captures, and niri's own Vulkan programs read
   the frame blend state. Capture state is cleared after the output so
   screenshots/screencasts stay SDR; 10-bit scanout formats are unchanged.
   Screen capture protocols run on Vulkan too: wlr-screencopy (output/window/
   region, SHM and dmabuf), ext-image-copy-capture (SHM advertised, dmabuf
   accepted), and screencast frames (SHM advertised on Vulkan; dmabuf handled)
   render through the active renderer, with damage/cursor semantics unchanged.
   Transition captures are transform-aware (all eight transforms covered by a
   readback test), and popup/layer-popup background effects now draw on Vulkan
   instead of being gated on GLES. MRU title/scope/fade textures, the
   screencast picker transition/panel/preview bakes, the window opening
   animation and the tile alpha fade now bake through the generic offscreen
   path too; the custom open/close shaders remain GLES-only with a Vulkan
   texture fallback. The remaining `as_gles` uses are per-backend dispatch,
   GLES fast paths with a generic fallback, colour-management capture setup, or
   genuinely GLES-only raw shader programs. Textured `ShaderRenderElement`s now draw on
   Vulkan: the panel and decoration-light programs are compiled to SPIR-V and
   their sampler maps resolve `TtyOffscreen::Vulkan` textures. `OffscreenBuffer`
   is backend-agnostic (it stores a `TtyOffscreen`), so the carousel panel-source
   bake now runs on the active renderer and carousel panels draw on Vulkan; the
   decoration-light producer also runs on Vulkan: its emission prepass uses the
   backend-agnostic `OffscreenBuffer` and its dual-Kawase blur is the extracted
   `VulkanBlur` (`blur_vulkan.rs`), the same ladder the per-window effect uses. The per-window background effect does draw on
   Vulkan: `blur_down`/`blur_up` and the background composite
   (`clipped_surface.frag` + `postprocess.frag`, including the liquid-glass
   uniforms) are compiled for Vulkan and run through `render_custom_passes`
   over framebuffer captures; xray keeps its GLES path because its shared
   backdrop buffers are GLES offscreen textures. All of this is covered by
   real-device readback tests that skip when no Vulkan device exists.
5. **KDE screenshot2 — done.** `src/dbus/kwin_screenshot.rs` serves
   `org.kde.kwin.Screenshot` (fullscreen/area/window/window-under-cursor/
   interactive) on top of niri's capture pipeline, alongside the colour picker
   and output order.
6. **Ecosystem** — `better-companion` foundation is in `better-companion/`:
   a separate Cargo workspace (excluded from niri's) with `companion-core`
   (JSON-line UDS protocol + registry), `companion-parking`,
   `companion-automation` (registry-dispatched sequences),
   `companion-hooks`, `companion-system`, the `better-companion` daemon and
   `companionctl`; `companion-niri` adds a `compositor.*` bridge that performs
   typed niri actions and queries outputs/workspaces/windows over IPC, and
   subscribes to the event stream to auto-track `auto:workspaces` /
   `auto:windows` parking lots (activation, focus, close, minimize, urgency),
   and `parker.*` consumes them to park/restore a window or a whole workspace
   (minimize/unminimize through niri, snapshots persisted in the `parker` lot).
   Phase B is in: `rules.*` (declarative match + explain/apply/run through the
   registry), `windows.run-or-raise`/`singleton`/`find`, and opt-in terminal
   `swallow.*` (minimize the terminal while a launched child lives, restore on
   close, with an app-id allowlist and persisted pairs).
   Phase C ecosystem adapters are in: optional `session.*`, `monitor.*`,
   `audio.*` and `wallpaper.*` namespaces storing explicit commands and event
   hooks (execution only via explicit dispatch, disabled by default). The first
   companion apps are in: `better-monitor/` snapshots output profiles and
   applies them through niri IPC with `--dry-run`, `__previous` rollback and
   `restore`; `better-session/` saves workspaces/windows and restores them
   (matches by app id/title, moves/floats windows, focuses named workspaces,
   spawns configured missing apps, reports unmatched ones); `better-shot/`
   captures UI/output/window screenshots to generated paths with optional
   `wl-copy` and `satty` post-processing; `better-wallpaper/` maps named
   workspace scenes to images and applies them once or by following niri's
   event stream (`swaybg`/`swww` style command templates, one process per
   output); `better-settings/` edits niri's KDL config through a
   text-preserving patcher (comments/formatting kept, timestamped backups) with
   a GTK4 frontend plus a scriptable `better-settingsctl`; `better-audio/`
   routes playback streams by app rule to sinks through `pactl` (priority,
   glob/substring matching, dry-run, default sink, watcher);
   `better-sidepanels/` creates persistent layer-shell side panels per output
   (exclusive zone, click commands, module rows refreshed on an interval,
   cairo-rendered, configurable theme) with a testable model/renderer.
   Later rounds added workspace/output placement (`layout` target resolution,
   `windows.move-to-workspace`/`follow-to-workspace`/`move-to-output`/
   `focus-output`), cross-workspace swallowing with bounded pair storage and
   `swallow.reload`, and daemon resilience (tolerant event parsing, reconnects
   from a snapshot, bounded window cache, oversized-request rejection,
   idempotent raise/restore). `better-portal/` is installable on its own:
   `better.portal`, `portals.conf` snippet, D-Bus/systemd units, install and
   uninstall scripts, and a private-bus smoke test.
   The nsticky workflow is absorbed: `stage.*` (on the shared parking state),
   `scratchpad.*` named slots with persistence/reconcile, native
   `windows.toggle-sticky`/`set-sticky`/`list-sticky` and the `nsticky.*`
   compatibility namespace with a migration table. `better-shot/` now handles
   HDR capture (`--hdr=keep|sdr|tonemap`, PQ tone mapping for 16-bit sources),
   built-in `rect`/`text`/`arrow` annotations, PNG/JPEG/WebP output and JSON
   metadata sidecars.
   Idle and orientation are companion modules: `idle.*` (lock/DPMS/suspend/
   command timeouts, user/audio/fullscreen inhibitors, logind shell-out,
   idempotent ticks, explain) and `rotate.*` (iio-sensor-proxy or a configured
   sensor command, hysteresis, transform+position through `Request::Output`,
   lock/manual set), both inert by default. `better-session/` gained named
   contexts with outputs, hooks and ordered, idempotent restore that waits for
   late apps. `better-sidepanels/` gained built-in modules (clock, niri
   workspaces with per-chip click and scroll cycling, window title, media,
   battery, cpu, memory, network, volume, custom), safe click/scroll actions,
   panels on all four edges (several per output), config v2 with per-output
   overrides, validation and file-watch reload. Remaining in the `better-*`
   family: `better-audio/` has a native PipeWire backend (registry
   enumeration, port-link moves, metadata default sink, per-stream
   volume/mute) with `pactl`/`wpctl` fallbacks, backend selection and restore;
   `companion-adapters` now routes niri and companion events to the
   session/monitor/audio/wallpaper adapters with quoted templates, dedup,
   timeouts, concurrency limits and an opt-in failure policy.
   `better-wallpaper/` gained a native layer-shell backend (one background
   surface per output, cover-scaled `wl_shm` images, reload on file change,
   reconnect/hotplug) with `auto|native|command` selection and command
   fallback. Phase 8 packaging is in `packaging/`: Arch/AUR recipes for
   `niri-better` and `better-desktop`, pinned and `-git` variants, feature
   profiles (minimal/desktop/full), `verify.sh`, and `docs/PACKAGING.md` with
   the compatibility matrix. The Arch/AUR recipes are now split into
   `niri-better`, `better-portal`, `better-companion`, `better-monitor`,
   `better-session`, `better-shot`, `better-wallpaper`, `better-settings`,
   `better-audio`, `better-sidepanels` and the `better-desktop` meta package,
   with ownership/staging checks in `verify.sh`. Remaining: hardware validation
   (VRR/tearing/explicit sync/multi-GPU/HDR mixed on real panels). `better-settings/` now covers layout, input
   (keyboard/touchpad/mouse/trackpoint/touch/tablet), per-output settings,
   binds with flags and multi-action, window/layer rules, startup, environment,
   animations, gestures, overview, cursor, clipboard and screenshot path, with
   byte-preserving edits, dry-run diffs, export/import and a sectioned GTK UI.
   `better-monitor/` now has schema v2 (matching by name/make/model/serial,
   max BPC, HDR notes, VRR aliases), layout validation, ordered apply,
   partial-failure rollback, rules with priorities, a debounced watch mode and
   JSON reports; `better-shot/` added `record` with `wf-recorder`/command
   backends, region/window/output selection, codecs/containers, audio, HDR
   handling, PID state, `--stop` and JSON reports.
   Delivered since: session orchestration of monitor/audio/wallpaper/companion
   profiles with partial-success reports, history/snapshots, import/export and
   `.desktop`/Flatpak launches; monitor confirmation timer (`apply
   --confirm-timeout`, `confirm`, `cancel`); the shot editor
   (blur/pixelate/redaction/crop/zoom/spotlight/lines, annotation log with
   undo/redo, capture history, OCR and multi-output stitching); companion
   protocol v2 (request ids, error codes, UDS event subscriptions), window
   marks, workspace-triggered rules, automation history and an optional config
   file; and the Noctalia plugin source (manifest + Lua syntax checked, live
   shell check pending).
   App depth also landed: sidepanels auto-hide/peek, floating panels,
   app/workspace rules, named layouts, session state and SIGHUP reload;
   audio sources, named profiles, focus/workspace context rules, hotplug gating
   and `diagnose`; wallpaper cross-fades, playlists, time rules, per-session
   scenes and scene import/export. `better-settings/` now also exposes the
   other apps through their own CLIs/JSON (displays, sessions, wallpaper, audio,
   side panels, automation) with preview-then-apply, and degrades per tool when
   something is missing. The companion daemon ships a systemd user unit, and
   `scripts/check-workspaces.sh` plus a `better-workspaces` CI job run fmt/check/
   test for every workspace, and every companion binary answers `--version`.
   The optional GUI pieces are in too: better-monitor `position`/`arrange`
   (validated, dry-run, rollback), better-audio `volume`/`mute` for sinks and
   sources, a GTK display arrangement editor (drag, snap, overlap validation,
   preview/apply) and an audio mixer with sliders and mute toggles. Still open:
   nsticky delegating sticky to the compositor when the capability is present,
   and the live Noctalia/hardware checks. On the compositor side all planned core
   protocol ports (`wp-commit-timing-v1` and SurfaceBarrier/atomic transactions) are now wired;
   DPMS VRAM/GTT cleanup stays deferred until a leak is reproduced.
7. **Built-in module features** — `vulkan` is wired end to end for the TTY
   backend: the `vulkan` Cargo feature (default on) compiles better-smithay's
   `renderer_vulkan`; `niri --check-vulkan` probes Vulkan 1.3 devices and
   reports whether `VulkanRenderer::new` succeeds per device; `NIRI_RENDERER`
   (`auto`/`gles`/`vulkan`) resolves into a plan and startup logs the
   availability/fallback once. The TTY backend now holds a `TtyGpuManager`
   enum over `GbmGlesBackend` and `VulkanBackend`, and renders through
   `TtyRenderer`/`TtyFrame` enums, so `NIRI_RENDERER=vulkan` drives outputs with
   the Vulkan renderer (windows, layer surfaces, UI textures, screenshots'
   texture path). Borders and shadows are compiled for Vulkan, and the
   per-window background effect (blur plus the background composite, including
   liquid glass) now draws on Vulkan too; textured `ShaderRenderElement`s
   resolve `VulkanTexture` samplers. Global and scoped shader passes, carousel
   panel contents (baked through the generic offscreen), decoration light
   (emission plus the reusable Vulkan Dual-Kawase blur), animated resize
   (crossfade shader with no red fallback), screen transitions and the
   magnified screenshot UI bake/open draw on Vulkan. The screenshot UI,
   `screenshot`/`screenshot-screen`/`screenshot-window` IPC actions and the
   portal's full-desktop/area capture and pick-colour magnifier capture through
   the generic offscreen and read back generically, so a Vulkan-only session
   can open the UI and save PNGs. The visuals whose producers remain GLES-only
   also cover screen capture. Shared xray
   backdrops draw on Vulkan too: `EffectBuffer` bakes a `TtyOffscreen`,
   `XrayAlphaMask` is backend-agnostic, the backdrop blurs with `VulkanBlur`
   and `xray_postprocess_and_clip` composites per window/layer; the per-window
   framebuffer effect remains the fallback. Closing and unmap animations draw
   from pre-baked backend-agnostic tile snapshots; the custom close shader
   itself stays GLES-only and Vulkan uses the texture/scale/alpha fallback. Vulkan transition captures are stored unrotated
   (rotated outputs rely on the output frame). The probe has been validated on real hardware
   (RADV, two usable devices) and the feature matrix builds
   (`--no-default-features`, `--no-default-features --features vulkan`,
   default). Runtime TTY validation of the Vulkan output path still needs a
   manual session start on a spare TTY.
   `kde`, `cursor`, `effects` and `navigation` are done. The vertical main axis is available
   globally, per output and per workspace; axis-aware wheel/gesture input
   (`axis_policy`), spatial-movement and move-grab mapping, touch-overview
   gestures and closing-window view offsets are ported. `effects`
   gates the global/region/output/window shader engine and decoration light
   (render path, reload hooks, continuous-redraw gates, backend pass
   registration, decoration-light damage ordering, and the tty ping-pong
   handoff). `navigation` gates the screen magnifier end to end (actions,
   drag grab, center animation, cursor pre-transform, render pass, capture
   suppression, screencast mapping) and the grid-overview entry points
   (toggle/open/close actions, hot corner, four-finger gesture, hardcoded
   H/J/K/L binds), the grid gesture state, the grid focus-ring/tab-indicator/
   minimized-highlight visual updates, the grid renderer, and the whole
   `grid_overview` model plus the `Workspace.grid_overview` field (methods
   fall back to no-op stubs). `navigation` also gates the consolidated
   carousel: rotation/pullback state and methods, panel content sources and
   offscreen buffers, per-host panel element caches and click hit geometry,
   panel rendering, the panel prepasses and the panel render-element
   variants, with stubs for the helpers the remaining render path queries.
   The true minimal build is verified per package with
   `cargo check -p niri --no-default-features`, since workspace builds unify
   defaults from other members.
8. **Multi-action binds — done.** Binds hold ordered action lists per phase
   (`press`/`release` sections included), the parser reports and skips a bad
   action while keeping the rest, `allow-when-locked` requires every action to
   be `spawn`/`spawn-sh`, and the hotkey overlay, MRU and screenshot UI consume
   all actions. `niri msg actions` and `Request::Actions` run a batch validated
   up front and executed in one idle callback. DWT was already integrated
   separately.
