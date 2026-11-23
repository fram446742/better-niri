# Modules, core boundary and delivery tiers

This document decides **where each feature lives** so better-niri can stay fast
and minimal while still offering the full feature set. It is the reference for
every future integration.

## 1. The problem with "one module per feature"

A compositor is not a plugin host. Features differ enormously in what they need:

- HDR needs the renderer, DRM atomic state and scan-out — privileged and
  per-frame.
- Grid overview needs layout internals but only while it is open.
- Shaders need the renderer but must do nothing when disabled.
- Sticky windows need a native primitive; staging and rules are policy.
- Screenshot annotation is UI and policy; it does not belong in the render loop.
- Automation needs events and IPC; it must be able to crash without taking the
  compositor down.

Grouping by arbitrary bundles (for example "output" containing shaders, virtual
outputs and isolated outputs) mixes different lifetimes, dependencies and
maintenance costs. A module boundary should follow **responsibility and cost**,
not a product name.

## 2. Delivery tiers

### Tier 1 — Core (always compiled, always available)

Code that must live in the compositor because it needs privileged access,
per-frame state, or must remain correct for every client and every session.

Core includes:

- DRM/KMS, GLES, direct scan-out, VRR, 10-bit, frame scheduling.
- HDR, colour management, tone mapping, SDR/HDR blending, HDR-safe capture.
- Explicit sync, DRM syncobj, tearing, FIFO, async flips, commit timing,
  SurfaceBarrier, pointer warp.
- Multi-GPU and software EGL.
- Input: libinput, keymaps, per-keyboard configuration, Num Lock, repeat,
  disable-while-typing, gestures, pointer constraints, key bindings.
- Layout: scrollable layout, floating, fullscreen, minimisation, hidden
  workspaces, portrait axis, sticky-window primitive, float-above-fullscreen.
- Outputs: output management, virtual-output backend, isolated-output policy.
- Wayland protocols, config (KDL), IPC, session lock, basic accessibility.
- Screenshot/screencopy/ext-image-copy-capture protocol plumbing.

Core features are **not** removed to make a minimal build. They may be disabled
by configuration at runtime where that makes sense, with zero cost when off.

### Tier 2 — Built-in modules (shipped in the binary, compile-time optional)

User-visible features with significant code or dependencies that can be cleanly
separated from the core invariants. They ship with the normal build, are optional
via Cargo features for minimal images, and are inert until configured.

Proposed modules:

| Module | Responsibility | Default | Heavy deps |
| --- | --- | --- | --- |
| `effects` | One shader engine: global, region, window and focus-ring scopes; multipass; feedback buffers; presets; liquid-glass presets; capture opt-in | on | none (GLES); Vulkan path follows the renderer |
| `navigation` | Grid overview, consolidated carousel, dynamic overview zoom, magnifier | on | none |
| `cursor` | SVG cursor themes, shake-to-enlarge, cursor-specific effects | on | `resvg`/`usvg` |
| `agent` | Agent seat, agent IPC socket, AT-SPI automation helpers | **off** | none, but security-sensitive |
| `kde` | `org_kde_kwin_blur`, KDE screenshot/colour-picker D-Bus, KDE output order | on | `zbus` |
| `vulkan` | Vulkan renderer selection (backend) | on for the full build, off for minimal | Vulkan loader, Smithay `renderer_vulkan` |

Nothing here requires downloading code at runtime. A "minimal" build is
`--no-default-features` plus the core, not a plugin marketplace.

### Tier 3 — Companion services (separate repositories, IPC)

Policy, automation, UI and long-running workflows. They must work without the
compositor exposing internals, survive a compositor restart, and fail without
crashing the session.

- **better-nsticky** — sticky/staged windows, persistent state, scratchpads,
  selectors. Owns the sticky/staged *workflow*; the compositor owns the sticky
  *primitive*.
- **better-companion** — automation based on Piri: rules, singleton, swallow,
  run-or-raise, workspace hooks, scratchpad orchestration, event API.
- **better-session** — save/restore work contexts (windows, workspaces, outputs,
  profiles), combining the ideas of nirinit, niri-session-manager and
  niri-companion.
- **better-sidepanels** — side panels, stacking, per-app rules.
- **better-shot** — screenshots, recording, annotations, HDR-aware capture.
- **better-monitor** — output profiles, safe apply/rollback.
- **better-audio** — PipeWire routing by app/window/context.
- **better-wallpaper** — scenes and per-workspace wallpapers (reuse Noctalia).
- **better-settings** — GTK4/libadwaita frontend; each app remains the authority
  for its own config.
- **better-portal** — XDG portal backend and picker daemon; separate from the
  compositor from the start.

Companion services use:
- niri IPC for compositor state/actions;
- versioned JSON over Unix sockets for their own APIs;
- D-Bus only where the desktop expects it;
- PipeWire native APIs for audio;
- Wayland protocols for capture.

### Tier 4 — Shell integration (frozen for now)

Noctalia plugins only. No custom shell. Plugins are presentation/interaction
layers and call native services; no business logic in Luau.

## 3. Naming and grouping

Do **not** name modules after products or arbitrary bundles. Use domains:

- `presentation` (core): DRM, renderers, HDR/colour, sync, tearing, scan-out.
- `input` (core): devices, keymaps, binds, gestures, pointer.
- `layout` (core): scrolling, floating, workspaces, windows, minimise, sticky.
- `outputs` (core): output management, virtual outputs, isolation, per-output
  policy. **Not** shaders.
- `effects` (module): all shader scopes and visual presets. Liquid glass is a
  preset/effect here, not a second engine.
- `navigation` (module): grid, carousel, zoom, magnifier.
- `cursor` (module): SVG themes, shake.
- `agent` (module): automation seat.
- `kde` (module): KDE protocols and D-Bus services.

Why shaders and virtual outputs are not in one module: virtual outputs are an
output backend primitive that must exist even when no effect is enabled; shaders
are a renderer feature that must cost nothing when disabled. Their dependency
graphs, failure modes and tests are unrelated.

## 4. Module contract

A built-in module must:

1. Live in `src/modules/<name>/` (or a dedicated file under the relevant domain
   for small modules), with its own state struct.
2. Add **one** registration and at most a few hooks in core files; all hooks are
   listed in `UPSTREAM.md`.
3. Check `enabled` once per output/frame, not per window/event where avoidable.
4. Do **no** work when disabled: no elements, no shader compile, no redraw, no
   scanout inhibition, no allocation in the steady state.
5. Keep its configuration in `niri-config` and its actions/events in `niri-ipc`.
6. Have unit/headless tests; hardware-only behaviour is documented in
   `TESTING.md`, never claimed from a build.
7. Be removable at compile time without breaking core builds (if it is a Tier 2
   module). Core may not depend on a module.

## 4.1 Action ownership inside the compositor

The same category rule applies to built-in compositor modules. Each module owns
its action namespace and registers it once; core action matching stays small.

| Module | Owned action prefix | Examples |
| --- | --- | --- |
| core (`layout`, `outputs`, `input`, `presentation`) | `compositor.core.*` (native actions) | window/column/workspace moves, float/fullscreen/minimise, sticky primitive, hidden workspaces, virtual/isolated outputs, per-output HDR/VRR/tearing |
| `effects` | `compositor.effects.*` | toggle/cycle global, region, window and output shaders; reload presets; shader status |
| `navigation` | `compositor.navigation.*` | toggle/open/close grid; overview zoom; carousel rotate; magnifier toggle/move/zoom |
| `cursor` | `compositor.cursor.*` | SVG theme reload/selection, shake actions/status |
| `agent` | `compositor.agent.*` | agent seat enable/disable, agent input/actions, capture receipts, take-over |
| `kde` | `compositor.kde.*` | KDE output order, screenshot/colour-picker requests, blur capability |
| `vulkan` | `compositor.vulkan.*` | renderer selection/status/diagnostics (not a user-facing visual feature) |

Rules:

- One owner per action name; duplicates are a build/startup error.
- Modules may invoke core actions (for example `effects` needs a redraw), but do
  not reimplement them.
- Disabled modules do not register actions and are absent from the action list.
- IPC capability discovery reports module/action availability so tools can
  degrade gracefully.
- Namespaces (`compositor`, `compositor.effects`, ...) are grouping nodes; a
  module owns a leaf subtree. Longest-prefix matching resolves dispatch.
- The native niri action names remain unchanged for compatibility; the
  hierarchical names are the module registry/UDS surface.

The plugin runtime architecture (compiled, native ABI, WASM, UDS) is in
[`PLUGINS.md`](PLUGINS.md).

## 5. Performance rules by tier

- **Core**: no regression versus upstream on the default path. Effects and
  optional protocols are gated.
- **Modules**: zero cost when off; region-limited damage; preserve scan-out when
  possible; no per-frame allocations; no blocking event loop.
- **Companions**: no polling; subscribe to events; release resources; never block
  the compositor's event loop; socket permissions restricted.
- **Shell**: never block the UI; no polling; graceful missing-service handling.

## 6. Compatibility and extension interfaces

We do **not** load `.so` plugins. Rust has no stable ABI, and a compositor cannot
serialize `State`, renderer resources and Wayland objects per frame.

We do expose stable **interfaces**:

- niri IPC actions/events (typed, versioned, additive).
- A capability query so tools know whether native sticky, virtual outputs,
  screen capture, agent seat or a portal are available.
- Wayland protocols for capture/outputs/etc.
- Companion-local JSON/UDS APIs, versioned and documented.

This gives users modularity without an unstable plugin ABI and without breaking
upstream merges.

## 7. Feature-to-tier inventory

### Core (Tier 1)

HDR, colour management, tone mapping, scRGB, HDR metadata, custom primaries,
SDR/HDR mixing, HDR capture conversion, GLES, DMABUF, 10-bit, direct scan-out,
VRR, explicit sync, DRM syncobj, tearing, FIFO, async page flips, commit timing,
SurfaceBarrier, pointer warp, multi-GPU, software EGL, DPMS fixes, per-keyboard
configuration, Num Lock, disable-while-typing, pointer/gestures, minimise, hidden
workspaces, portrait axis, sticky primitive, float-above-fullscreen, virtual
outputs, isolated outputs, window groups/tabs, capture protocols, config/IPC.

### Built-in modules (Tier 2)

`effects` (global/region/window/focus-ring shaders, multipass, feedback,
presets, liquid glass), `navigation` (grid, carousel, dynamic zoom, magnifier),
`cursor` (SVG, shake), `agent`, `kde`, `vulkan`.

### Companion services (Tier 3)

Sticky/staged workflows and scratchpads (nsticky), automation/rules/singleton/
swallow/run-or-raise (companion), sessions, sidepanels, screenshots/recording,
monitor profiles, audio routing, wallpapers, settings, portal daemon.

### Shell (Tier 4)

Noctalia widgets/plugins only.

## 8. Sticky windows: the concrete split

Decision: sticky/staged/scratchpad **workflow is absorbed into
better-companion**; the compositor owns only the primitive.

- Compositor core: a `sticky` property on floating windows, actions to toggle it,
  hidden/stage workspaces, and IPC/window-rule support. It follows
  workspaces/outputs correctly and never moves windows around behind the scenes.
- better-companion: staging, scratchpads, selectors, persistence, per-app rules
  and the user-facing workflow, preserving nsticky's code/history/attribution
  (BSD-3-Clause). It uses the native sticky primitive when advertised and keeps
  a move-based fallback for plain Niri.
- No third sticky daemon. `nsticky` remains available during migration as a
  compatibility CLI/config layer, then becomes an alias or is retired.

This avoids two competing sticky engines and keeps the compositor free of
user-specific staging policy.

## 9. Portals: the concrete split

Decision: **better-portal is a separate daemon from the start.**

- better-portal implements the XDG desktop portal backend (ScreenCast,
  Screenshot, and later remote desktop) over ext-image-copy-capture, PipeWire and
  D-Bus.
- The picker is owned by better-portal as a layer-shell client (outputs,
  windows, regions, permissions). A compositor-native picker may be added later
  behind an IPC request if the external one cannot meet the UX or security bar.
- The compositor keeps capture protocol plumbing and its screenshot/IPC actions;
  it does not embed a portal backend.
- Permissions and persisted grants live in better-portal.
- Do not implement capture twice; the compositor remains the source of buffers.

The niri-elite built-in portal code is a reference/prototype, not a module we
keep long term.

## 10. Migration plan for the current elite base

The elite merge currently contains many features already. We do not need to
extract everything at once:

1. Mark core features in docs and keep them.
2. Wrap effect/overview/cursor/agent/KDE registration behind modules and
   `enabled` checks, with Cargo features.
3. Add the capability query and the sticky primitive.
4. Only then start companions.

## 11. Open decisions

See `ECOSYSTEM.md` section "Open questions". The short list:

Resolved:

- default build: full binary with all modules; minimal via features/profile;
- portal: separate `better-portal` daemon from the start;
- agent seat: module off by default, explicit authorization and restricted
  socket;
- sticky/staged/scratchpads: absorbed into better-companion, native primitive in
  the compositor;
- minimise: core; grid/carousel/magnifier: `navigation` module;
- companion repositories: after Phase 1;
- Piri: fork preserving history and attribution.

Remaining: the exact way better-companion absorbs nsticky (module layout,
compatibility CLI) and whether the portal picker starts external or gets a
compositor IPC fallback.

## Cargo features implemented so far

- `kde` (default on): `org_kde_kwin_blur`, `kde_output_order_v1`, the KDE colour
  picker and `org.kde.kwin.Screenshot`.
- `cursor` (default on): SVG/scalable cursor rendering (`resvg`/`usvg` are
  optional dependencies) and shake-to-enlarge. Off builds fall back to XCursor
  bitmaps and compile the shake path out.
- `effects` (default on): the post-processing shader engine — global output
  passes, region/per-output passes, window-scoped passes, and decoration
  light. Off builds compile those render paths, their config-reload and
  continuous-redraw hooks, and the decoration-light element out; core
  rendering (shadows, borders, blur, HDR, open/close/resize animations) is
  unaffected.
- `agent` (off by default): the automation seat.
