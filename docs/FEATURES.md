# Feature inventory and status

Status legend:

- **Upstream** — present in niri `main` at `02ee0585` (audited 2026-11-23).
- **Partial** — the mechanism exists upstream but is incomplete for the feature as
  described (e.g. 10-bit formats exist, general HDR output does not).
- **Absent** — not in upstream at this commit.
- **Planned** — accepted for better-niri; not integrated yet.
- **Deferred** — evaluated, not scheduled (reason given).

`better-niri` deliberately starts empty: no feature below is integrated yet.
This table is the integration contract, not a claim of completion.

## Rendering and colour

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| HDR output signalling (`HDR_OUTPUT_METADATA`, BT.2020, 10-bit) | Absent | niri-spicy | Integrate niri-spicy | Planned | Needs Smithay connector colour state. |
| `wp_color_management_v1` | Absent | niri-spicy | niri-spicy | Planned | Windows scRGB, BT.2100, extended target volume, custom primaries. |
| Tone mapping | Absent | niri-spicy | niri-spicy | Planned | Peak-luminance override, cursor-plane/LUT acceleration. |
| scRGB / Windows HDR | Absent | niri-spicy | niri-spicy | Planned | Wine/Proton detection fixes included. |
| HDR metadata and transitions | Absent | niri-spicy | niri-spicy | Planned | Output recreation on HDR toggle. |
| Custom primaries | Absent | niri-spicy | niri-spicy | Planned | Smithay side required. |
| SDR/HDR mixing | Absent | niri-spicy | niri-spicy | Planned | SDR composited at reference luminance; niri-elite has an equivalent always-HDR blend fix. |
| HDR → SDR capture | Absent | niri-spicy, niri-elite | niri-spicy | Planned | Must cover screenshot, ext-image-copy-capture, PipeWire. |
| Vulkan renderer | Absent | niri-spicy + losnoco/smithay | niri-spicy, optional Cargo feature | Planned | Per-output fallback to GLES. |
| GLES renderer | Upstream | upstream | upstream | Upstream | HDR/Vulkan work must not regress it. |
| DMA-BUF import/export | Upstream | upstream | upstream | Upstream | Extend format probes for HDR. |
| GPU format probing | Partial | niri-spicy, niri-elite | niri-spicy | Planned | niri already has a 10-bit scanout format list; HDR needs per-plane colour probing. |
| 10-bit output | Partial | niri-spicy | niri-spicy | Planned | Upstream lists `Abgr2101010`; no HDR colour pipeline. |
| Direct scanout | Upstream | upstream | upstream | Upstream | Must survive effects-disabled paths. |
| Shader pipeline (GLSL) | Absent | biri | biri | Partial | Global + `region-shader` + per-window-rule scoped chains, multipass, feedback; per-output presets and focus-ring shaders pending. |
| Multipass + frame history | Absent | biri | biri | Integrated | `niri_prev`, `niri_buffer`, `niri_screen`, `niri_source`, per-pass chains, black-seeded feedback. |
| Blur | Upstream | upstream | upstream | Upstream | `passes`, `offset`, `noise`, `saturation`, `xray`; `ext-background-effect-v1`. |
| KDE blur protocol | Absent | kde-niri | kde-niri | Planned | `org_kde_kwin_blur` is different from upstream's `ext-background-effect-v1`. |
| Liquid glass / refraction / lens / edge light | Absent | zaroutt/Niri-glass via niri-elite | niri-elite port | Planned | Implement as a background-effect shader, not a second engine. |
| VRR | Upstream | upstream | upstream | Upstream | On-demand and per-window rules. |
| Tearing control | Absent | urayde | urayde | Planned | Upstream Smithay has no `wp_tearing_control_v1`; better-smithay must add it. |

## Presentation and DRM

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Explicit sync (`linux-drm-syncobj-v1`) | Absent | urayde | urayde | Planned | Upstream Smithay already has `drm_syncobj`. |
| Tearing | Absent | urayde | urayde | Planned | Window rule + content type + async flip path. |
| FIFO (`wp-fifo-v1`) | Absent | urayde | urayde | Planned | Upstream Smithay already has `fifo`. |
| Mailbox / immediate presentation | Absent | urayde, niri-spicy | urayde | Planned | Expressed through tearing/content-type and async flips. |
| Async page flips | Absent | urayde (niri) + losnoco (Smithay) | urayde + losnoco | Planned | Driver fallback to blocking commit required. |
| SurfaceBarrier / atomic layout transactions | Absent | niri-spicy + losnoco | niri-spicy | Planned (later) | Reworks transaction handling; adopt after presentation baseline. |
| Commit timing (`wp-commit-timing-v1`) | Absent | niri-spicy + losnoco | niri-spicy | Planned (later) | Smithay module exists upstream but niri does not wire it; spicy's version is coupled to barrier work. |
| Pointer warp (`wp_pointer_warp_v1`) | Absent | urayde | urayde | Planned | PR #4001 closed unmerged; upstream Smithay has `pointer_warp`. |
| Multi-GPU | Partial | niri-elite (SHORiN) | niri-elite | Planned | Better render-node selection and display-only GPU handling. |
| Software EGL | Absent | niri-elite | niri-elite | Planned | Upstream explicitly skips llvmpipe/softpipe. |
| DPMS frame callbacks | Upstream | upstream | do not port | Upstream | Fixed by `78b7adfe`, `bb38b7e1`. |
| DPMS VRAM/GTT cleanup | Partial | demoji PR #4750 (open) | investigate | Deferred | Reproduce and profile first; no timer mitigation without evidence. |
| DPMS isolated-output policy | Absent | biri | biri | Integrated (core) | `skip-isolated` keeps isolated outputs on while the rest sleep. |
| Suspend/resume, hotplug resource cleanup | Partial | upstream + niri-spicy | integration work | Planned | Covered by hardware tests. |

## Outputs and workspaces

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Virtual outputs | Absent | biri (niri PR #3800, open) | biri | Integrated (core) | TTY + headless; IPC/CLI create/remove; config `create-virtual`; virtual-output frame-callback path. |
| Isolated outputs | Absent | biri | biri | Integrated (core) | Overview/Alt-Tab/hotkey overlay/config notification hidden; `power-off-monitors skip-isolated=true` keeps them lit. |
| Hidden workspaces | Absent | biri (niri PR #2997, open) | biri | Planned | Config + IPC actions; keep out of overview/carousel. |
| Portrait / vertical main axis | Absent | biri | biri | Planned | Per-output/workspace; directional input stays physical. |
| Per-output behaviour | Upstream | upstream | upstream | Upstream | Output config and layout rules. |
| Multi-monitor improvements | Partial | niri-elite, biri | combine | Planned | Carousel, isolated outputs, per-output gestures. |
| Different refresh rates | Upstream | upstream | upstream | Upstream | Per-output frame clocks. |
| Sticky windows | Absent | biri | biri native | Planned | Native compositor implementation wins over IPC moving. |
| Sticky floating windows | Absent | biri (niri PR #3302, closed) | biri | Planned | Floating windows follow workspaces of their output. |
| Float above fullscreen | Absent | biri (niri PR #4062, open) | biri | Planned | Opt-in window rule. |
| Stage / scratchpad workspaces | External | better-nsticky | better-nsticky | Planned | Keep as external daemon; add compatibility mode. |

## Windows and desktop UX

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Grid overview | Absent | niri-elite (SHORiN) | niri-elite | Planned | Per-monitor, tab cells, gestures, IPC focus report. |
| Window minimisation | Absent | niri-spicy, niri-elite | niri-spicy base + elite UX | Planned | Real state, actions, rule, grid placeholder, strip. |
| Magnifier | Absent | niri-elite (SHORiN) | niri-elite | Planned | Movement actions/binds; pick-colour overlay. |
| Window groups | Upstream tabs / partial | upstream + kde-niri | upstream | Upstream | Do not reimplement tabbed columns; evaluate Naxdy's auto-group refinements later. |
| Hot corners | Upstream (overview booleans) | niri-elite | niri-elite | Planned | Extend to arbitrary actions (`grid-overview`, etc.). |
| Agent seat | Absent | niri-elite (SHORiN) | niri-elite | Planned | Second seat over IPC; needs Smithay per-client seat filter. |
| Focus-ring shaders | Absent | biri | biri | Planned | Per-window shader files, hot reload, light spill. |
| Custom borders | Upstream (colours/width) | upstream + biri shaders | biri shader layer | Planned | Keep upstream border geometry. |
| SVG cursors | Absent | niri-elite (SHORiN) | niri-elite | Planned | `resvg`/`usvg` optional dependency. |
| Shake-to-enlarge cursor | Absent | niri-elite (SHORiN) | niri-elite | Planned | Cursor config section. |
| Carousel overview | Absent | biri | biri | Planned | Consolidated multi-monitor cover-flow. |
| Dynamic overview zoom | Absent | biri | biri | Planned | Zoom presets and actions. |
| Drag physics | Absent | biri | biri | Planned | Elastic drag animation. |
| Multi-action binds | Absent | biri | biri | Planned | Run several actions from one bind. |

## Input

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Per-keyboard configuration | Absent | biri (niri PR #4459, open) | biri | Integrated (core) | Named `keyboard` blocks inherit unset fields; keymap/repeat/numlock/locale1/IPC follow the last physical keyboard. |
| Num Lock state | Upstream config | biri | biri | Planned | biri fixes Num Lock/locale1/IPC across keyboard switches. |
| Keyboard layouts | Upstream | upstream | upstream | Upstream | Per-keyboard support extends this. |
| Disable-while-typing controls | Upstream config | biri | biri | Integrated (core input) | Runtime `toggle-dwt`, composes with config; resets on restart; applies to hot-plugged touchpads. |
| Pointer behaviour | Upstream | upstream + urayde | urayde | Planned | Pointer warp; niri-elite's confine-to-fullscreen. |
| Gestures | Upstream | upstream + niri-elite/biri | combine | Planned | Grid and carousel gestures reuse input plumbing. |
| Accessibility | Upstream | upstream + niri-elite | hybrid | Planned | Existing a11y plus agent/AT-SPI tooling. |
| Press/release binds | Absent | niri-elite | niri-elite | Planned | Guarantees release after press. |
| Bare modifier triggers | Absent | niri-elite | niri-elite | Planned | `Mod` as a bind trigger. |
| Raw/ASCII keysym fallback | Absent | niri-elite | niri-elite | Planned | Non-ASCII layouts keep working. |

## Portals, capture and applications

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| XDG desktop portal backend | Absent | niri-elite (SHORiN) | niri-elite | Planned | `org.freedesktop.impl.portal.desktop.niri`. |
| ScreenCast | Upstream via GNOME portal | niri-elite native | niri-elite | Planned | Native picker and permissions. |
| Screenshot | Upstream CLI/UI | niri-elite + kde-niri | niri-elite | Planned | Path reply over IPC, picker UI. |
| Region capture | Upstream (image copy) | upstream | upstream | Upstream | Verify HDR→SDR conversion. |
| Window capture | Upstream (image copy) | upstream + urayde | upstream | Upstream | urayde has cursor-session rework; compare before porting. |
| Cursor capture | Upstream | urayde | urayde | Planned | ext-image-copy cursor session refinements. |
| KDE integration | Partial (D-Bus) | kde-niri | kde-niri | Partial | `org_kde_kwin_blur` + KDE output order integrated; screenshot2/colour picker pending. |
| Spectacle support | Absent | kde-niri | kde-niri | Planned | Through KDE screenshot D-Bus. |
| PipeWire | Upstream | upstream | upstream | Upstream | |
| DMA-BUF → SHM fallback | Upstream | upstream | upstream | Upstream | niri-elite's fallback was merged upstream. |
| HDR capture conversion | Absent | niri-spicy | niri-spicy | Planned | Must not blow out SDR output. |
| Privacy in captures | Partial (`block-out-from`) | demoji `block-out-blur` | demoji | Integrated | Blocked-out windows can be blurred instead of black; popups still hidden. |

## Architecture, configuration, tooling

| Item | Decision |
| --- | --- |
| Dynamic `.so` plugins | Rejected; ABI and render-loop cost outweigh flexibility. |
| Cargo workspace crates | Only existing `niri-config` / `niri-ipc`; no new crate until a second consumer exists. |
| Cargo features | Only heavy backends: `vulkan`, `agent`, portal, SVG. |
| Configuration | KDL, per-domain modules, defaults documented, runtime toggles where meaningful. |
| IPC | Typed actions/events in `niri-ipc`, no scripts for native behaviour. |
| Upstream sync | Merge-based, upstream first, drift script, no rebase of `main`. |
| Smithay | Single fork `fram446742/better-smithay`, pinned by commit; never two Smithays. |
| Documentation | This directory; update with every integration. |
