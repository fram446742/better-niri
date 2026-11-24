# Architecture

This document describes how better-niri will carry many features without turning
into an unmergeable fork. It is deliberately conservative: niri is a single
tightly-coupled compositor, and the cheapest maintenance strategy is usually a
small patch in the right existing seam, not a new abstraction.

## 1. Constraints from the codebase

Measured at niri `02ee0585`:

- `src/niri.rs` (7,634 lines) owns `State`, `Niri`, the Wayland globals and the
  render entry point. It changes in 132 of the last 800 upstream commits.
- `src/input/mod.rs` (6,584 lines) owns input dispatch and grabs; 108 recent
  changes. Layout modules (`layout/mod.rs`, `scrolling.rs`, `monitor.rs`,
  `workspace.rs`, `tile.rs`, `floating.rs`) change constantly.
- Rendering is generic over Smithay renderer traits and has three targets:
  `RenderTarget::{Output, Screencast, ScreenCapture}`.
- Smithay types cross nearly every boundary: `Output`, `WlSurface`, renderer
  traits, DRM surfaces, calloop handles.
- Niri already has natural domain boundaries: `handlers/`, `protocols/`,
  `render_helpers/`, `layout/`, `input/`, `ui/`, `dbus/`, plus the
  `niri-config` and `niri-ipc` workspace crates.

## 2. Dynamic plugins: rejected

A `.so` plugin system is not a good fit here:

- **No stable Rust ABI.** Plugins would be tied to the exact rustc version and
  crate hashes of `smithay`, `wayland-server`, `calloop`, etc.
- **Shared mutable state.** Features need `&mut State`, layout internals,
  renderer handles and calloop loops. An RPC boundary would copy or serialize
  per-frame data and destroy latency.
- **Type identity.** Wayland objects, `Output`, buffers and renderer resources
  cannot cross an ABI boundary without unsafe transmutes.
- **Distribution.** Users would need matching plugin binaries for every
  compositor build, recreating the problem the project exists to avoid.
- **Debugging.** Crashes inside plugins are hard to attribute, and version skew
  produces bugs niri upstream cannot reproduce.

If a future feature is genuinely reusable outside the compositor (for example a
shader compiler or preset library), it can become a normal crate with a Rust
API. That is not a plugin system.

## 3. Module organisation

One binary. New behaviour goes into dedicated modules in the domain it belongs
to, with a short registration in the existing core.

```
src/
  handlers/            Wayland protocol implementations (one module per protocol)
  protocols/           protocol globals and helpers
  render_helpers/      renderer-side elements and effects
  layout/              window/workspace geometry and state
  input/               input state, grabs, actions
  ui/                  overlays (overview, dialogs, pickers, strip)
  dbus/                D-Bus services (portals, KDE, GNOME, accessibility)
  backend/             tty, winit, headless, virtual output
  features/            (new, optional) cross-cutting feature glue only
niri-config/src/       KDL parsing, one module per config domain
niri-ipc/src/          typed IPC actions/events
```

Guidelines:

1. **Prefer a new file over editing an upstream file.** A feature that needs a
   new protocol gets `src/handlers/<name>.rs` and `src/protocols/<name>.rs`, then
   one `delegate_dispatch!`/`delegate_global_dispatch!` line.
2. **Extend types with `impl` blocks in the feature module.** Rust allows
   `impl Niri { ... }` in any module of the same crate. This keeps helper methods
   out of `niri.rs`; fields that need access become `pub(crate)`.
3. **Keep core hunks small and stable.** A render hook should be one `if` and one
   function call, not moved code. Document each such hunk in `UPSTREAM.md`.
4. **Do not create a generic "effects pipeline" abstraction before there are two
   concrete users.** `RenderTarget`, render elements and `RenderCtx` already are
   the abstraction.
5. **Separate feature state.** Put persistent per-output/per-window state in
   `OutputState`/window rules via small structs defined in the feature module,
   rather than adding many unrelated fields to core structs.
6. **No reformatting.** Never run `cargo fmt` over an upstream file as part of a
   feature commit. Format only new files or the exact edited range.
7. **One feature, one commit series.** A protocol, its handler, its config, its
   IPC action, tests and docs stay together; unrelated cleanup goes elsewhere.

### Cargo features

Only heavy or platform-specific dependencies are optional:

| Feature | Default | Purpose |
| --- | --- | --- |
| `vulkan` | on | Compiles better-smithay's `renderer_vulkan`, probes physical devices at startup, applies `NIRI_RENDERER` (`auto`/`gles`/`vulkan`) and falls back to EGL/GLES with a logged reason. The TTY backend renders through `TtyRenderer`/`TtyFrame` enums over the GLES and Vulkan multi-GPU renderers, so `vulkan` drives outputs with `VulkanRenderer`; borders, shadows, textured shader elements (panel/decoration-light programs), the per-window blur/background composite and liquid glass recompile their GLSL to SPIR-V and draw. `OffscreenBuffer` is backend-agnostic, so panel-source bakes and the decoration-light emission prepass run on the active renderer; the decoration light blurs its emission with the Vulkan dual-Kawase ladder (`blur_vulkan.rs`), the same ladder as the per-window effect. Producers that remain GLES-only (xray backdrops, resize/screen transitions) stay on GLES and are skipped; xray falls back to the per-window background effect on Vulkan. `niri --check-vulkan` prints the plan as JSON. |
| `agent` | on | Agent seat, agent IPC socket, automation helpers. No heavy dependency; default-on so the compositor is complete. |
| `portal` | on | Built-in XDG portal backend (`zbus` already optional upstream). |
| `svg-cursors` | on | SVG cursor themes (`resvg`/`usvg`). |
| `shaders` | on | User GLSL pipeline; no extra crate, but allows minimal builds. |

Do **not** gate layout/window-management features behind features: distribution
builds must not silently lose hidden workspaces, sticky windows or per-keyboard
configuration.

### Crates

- Keep `niri-config` and `niri-ipc` as the reusable boundaries. They are already
  consumed by external tools.
- Do not add a crate for each feature. A shader crate is justified only if a
  second consumer (e.g. a preset validator CLI) needs the same code.
- `better-smithay` is the only forked dependency. Everything else stays on
  crates.io/GitHub upstream.

### Delivery tiers

Features are classified before they are integrated:

- **Tier 1 core** — must be in the compositor (presentation, HDR/colour, input,
  layout/window primitives, outputs, protocols, config/IPC).
- **Tier 2 built-in modules** — shipped in the binary, compile-time optional,
  inert until configured: `effects`, `navigation`, `cursor`, `agent`, `kde`,
  `vulkan`. The portal backend is the separate `better-portal` daemon.
- **Tier 3 companions** — separate repositories over IPC: better-companion
  (which absorbs nsticky's workflow), sessions, panels, shot, monitor, audio,
  wallpaper, settings, portal.
- **Tier 4 shell** — Noctalia plugins only (frozen).

The complete boundary, naming and zero-cost rules are in
[`MODULES.md`](MODULES.md); the ecosystem responsibilities and roadmap are in
[`ECOSYSTEM.md`](ECOSYSTEM.md). A feature that can be implemented as a companion
must not be pulled into the render loop merely to simplify packaging.

## 4. Where features plug into niri

| Concern | Seam | Pattern |
| --- | --- | --- |
| Wayland protocols | `src/handlers/`, `src/protocols/`, `delegate_dispatch!` | One module per protocol; state stored in `Niri` or window data. |
| Renderer effects | `Niri::render_inner`, `render_helpers`, render elements | Push an element only when active; report damage; never force full redraw when inactive. |
| Outputs/DRM | `src/backend/tty.rs`, `src/backend/mod.rs` | Keep backend-specific code in backend modules; a `RenderTarget`-aware helper avoids duplicating logic. |
| Layout behaviour | `layout/{layout,monitor,workspace,tile,floating}.rs` | Highest risk. Implement state transitions in feature modules and call them from a small number of layout methods. |
| Input | `src/input/mod.rs`, `niri-config/src/binds.rs` | Add actions to the existing action enum; extend grabs in new files. |
| UI overlays | `src/ui/` | New overlay struct + one render/update hook. |
| Config | `niri-config/src/<domain>.rs` | New struct + `MergeWith` part; re-export from `lib.rs`. |
| IPC | `niri-ipc/src/lib.rs`, `src/ipc/` | New action/event variants; server dispatch in one match. |
| D-Bus/portals | `src/dbus/` | New service module; register in `dbus/mod.rs`. |
| Captures | `src/screencasting/`, image-copy handlers | Prefer the existing `RenderTarget` paths over a second renderer. |

### Recommended seam helpers

These are small internal APIs worth adding once, used by several features:

- `OutputEffects` (or equivalent): a per-output struct holding shader chains,
  liquid-glass/HDR state, and whether the output is scanout-eligible. Avoids
  scattering `Option<...>` fields across `OutputState`.
- `RenderTarget`-aware effect gate: one helper that answers "should this effect
  render for this target?" replacing per-feature checks (global shader, window
  shader, liquid glass, blur).
- `LayoutHooks`: a tiny struct of callbacks for grid/carousel/minimize that the
  layout calls at defined points (workspace switch, focus change, tile
  visibility). Only introduce it if direct calls become tangled; a plain
  function call is cheaper to maintain.

Do not introduce traits for these unless a second implementation exists.

## 5. Smithay integration

better-smithay is rebuilt from upstream `master` with thematically grouped
commits:

1. Colour management / HDR primitives (protocol, image descriptions, connector
   colour state, DRM colour pipelines, GLES colour transforms).
2. Async page flips + tearing control (`wp_tearing_control_v1`).
3. SurfaceBarrier + commit-timing (delayed blockers/atomic application).
4. Vulkan renderer (behind Smithay's `renderer_vulkan` feature).
5. Per-client seat global filtering (agent seat).

Rules:

- One fork, one pin. `Cargo.toml` references a single commit for both `smithay`
  and `smithay-drm-extras`.
- Each group stays as separate commits with original authorship; upstream
  merges are merge commits, not squashes, until a group is dropped.
- When upstream Smithay lands an equivalent, delete our commit and rebase the
  remainder. Record the upstream PR/commit in the commit message.
- Do not mix branches from different Smithay forks. Port the commit, not the
  branch.
- `smithay-drm-extras` must be built from the same revision even when unchanged.
- Vulkan is optional at compile time but must share the same DRM/GBM allocation
  and sync code as GLES.

See `COMPATIBILITY.md` for the currently pinned combination.

## 6. Rendering architecture rules

These rules apply to every visual feature:

1. **Inactive effects cost nothing.** No element, no shader compile, no redraw
   scheduling and no scanout inhibition when disabled.
2. **Scanout is a first-class output.** A full-output effect (global shader)
   disables scanout by definition; region/cursor-limited effects must shrink
   their element geometry so the rest stays scanout-eligible.
3. **Damage is proportional.** Effects report the region they change. Static
   shaders must not force continuous redraw; time-animated shaders use the
   configured fps cap and schedule redraws only while visible.
4. **Captures are explicit.** Shaders/effects do not leak into screencasts,
   screenshots or ext-image-copy-capture unless `shaders-in-capture` (or the
   equivalent per-effect option) is enabled. Blocked content stays blocked.
5. **One colour contract.** Define a single working space and encode stage for
   HDR/SDR before adding shaders or Vulkan. Shader code must not guess whether it
   receives sRGB-encoded or linear data.
6. **GLES is the reference.** Every effect works on GLES first. The TTY output
   path is renderer-generic and can run on `VulkanRenderer`; GLSL-only effects
   still feature-detect through `AsGlesRenderer`/`AsGlesFrame` and skip
   themselves on Vulkan with a warning instead of misrendering. New visuals must
   either work on both renderers or follow that same explicit skip.
7. **No per-frame allocations** in steady state: reuse offscreen textures and
   element vectors; avoid `Vec` growth in `render_inner`.
8. **Outputs are independent.** Effects and feedback buffers are per output;
   captures never write live feedback textures.

## 7. Configuration and IPC

- KDL only; no external scripts for native behaviour.
- New options go into the narrowest domain module (`output.rs`, `window_rule.rs`,
  `layout.rs`, `input.rs`, `appearance.rs`, or a new `shader.rs`, `hdr.rs`).
- Every option has a documented default and a parse test (insta snapshots where
  upstream uses them).
- Runtime toggles are IPC actions when the feature is visual/interactive
  (shaders, liquid glass, magnifier, grid, isolated outputs).
- IPC additions are typed in `niri-ipc`; events are additive and backward
  compatible. Do not rename existing fields.
- Duplicated config is forbidden: if upstream already has `blur`, extend it
  rather than adding `better-blur`.

## 8. Test architecture

- Unit tests live next to the module (`#[cfg(test)]`), plus `src/tests/` for
  compositor-level flows.
- Headless visual tests use `niri-visual-tests` and the headless backend; they
  are the main CI signal for layout/overview/grid/shader geometry.
- Protocol tests should assert globals and basic client flows where possible
  without a GPU.
- Hardware-only checks are documented in `TESTING.md` with required GPU/driver;
  a compile is never reported as a functional pass.

## 9. Consequences

- More files, fewer edits to volatile upstream files.
- Some duplicated registration lines (module declarations, dispatch macros) are
  accepted as the price of small conflicts.
- Layout features still require deep edits; they are scheduled last and tested
  hardest.
- No plugin ecosystem. This is intentional: better-niri is a compositor, not a
  plugin host.
