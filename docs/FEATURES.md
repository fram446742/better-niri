# Feature inventory and status

Status legend:

- **Upstream** — present in niri `main` at `02ee0585` (audited 2026-11-23).
- **Partial** — the mechanism exists upstream but is incomplete for the feature as
  described (e.g. 10-bit formats exist, general HDR output does not).
- **Absent** — not in upstream at this commit.
- **Planned** — accepted for better-niri; not integrated yet.
- **Deferred** — evaluated, not scheduled (reason given).

The status column was refreshed on 2026-11-25 against `integration/elite-base`:
everything marked **Integrated** is implemented and covered by the workspace test
suite (894 tests on the default build, 425 with `--no-default-features`).
All planned core protocol ports are integrated;
hardware behaviour (VRR, tearing, explicit sync, multi-GPU, HDR mixed) is
implemented but still needs validation on real panels. The rows below keep the
original integration contract in their notes; treat this column as the current
state.

## Rendering and colour

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| HDR output signalling (`HDR_OUTPUT_METADATA`, BT.2020, 10-bit) | Absent | niri-spicy | Integrate niri-spicy | Integrated | Default auto on HDR-capable outputs; `off`/`on` and runtime `niri msg output <out> hdr …` overrides; hardware pass entered/left HDR on DP-1. |
| `wp_color_management_v1` | Absent | niri-spicy | niri-spicy | Integrated | Windows scRGB, BT.2100, extended target volume, custom primaries. |
| Tone mapping | Absent | niri-spicy | niri-spicy | Integrated | `hdr { peak-luminance N }` overrides EDID peak in metadata/tone map/advertised luminance; cursor-plane/LUT acceleration on the HDR path. |
| scRGB / Windows HDR | Absent | niri-spicy | niri-spicy | Integrated | Wine/Proton detection fixes included. |
| HDR metadata and transitions | Absent | niri-spicy | niri-spicy | Integrated | Connector colour state is staged and switched with the frame; HDR mode changes reconnect the output to re-probe 10-bit formats. |
| Custom primaries | Absent | niri-spicy | niri-spicy | Integrated | Smithay side required. |
| SDR/HDR mixing | Absent | niri-spicy | niri-spicy | Integrated | SDR composited at reference luminance; niri-elite has an equivalent always-HDR blend fix. |
| HDR → SDR capture | Absent | niri-spicy, niri-elite | niri-spicy | Integrated | Must cover screenshot, ext-image-copy-capture, PipeWire. |
| Vulkan renderer | Absent | niri-spicy + losnoco/smithay | niri-spicy, optional Cargo feature | Integrated | TTY backend renders through `TtyRenderer`/`TtyFrame` over GLES and Vulkan; `NIRI_RENDERER=auto|gles|vulkan` selects it with a logged fallback. Borders, shadows, clipping, decorations, global/scoped/textured shaders, per-window blur and liquid glass, carousel panels, decoration light, snapshots/resize/transitions, screenshot UI and magnifier, xray backdrops, HDR mixed compositing, closing animations and the capture protocols all draw on Vulkan (SHM via offscreen readback, dmabuf via `Bind<Dmabuf>`). |
| GLES renderer | Upstream | upstream | upstream | Upstream | HDR/Vulkan work must not regress it. |
| DMA-BUF import/export | Upstream | upstream | upstream | Upstream | Extend format probes for HDR. |
| GPU format probing | Partial | niri-spicy, niri-elite | niri-spicy | Integrated | niri already has a 10-bit scanout format list; HDR needs per-plane colour probing. |
| 10-bit output | Partial | niri-spicy | niri-spicy | Integrated | Upstream lists `Abgr2101010`; no HDR colour pipeline. |
| Direct scanout | Upstream | upstream | upstream | Upstream | Must survive effects-disabled paths. |
| Shader pipeline (GLSL) | Absent | biri | biri | Integrated | Global, `region-shader`, per-window, per-output and focus-ring chains; multipass, feedback, presets, watched-file reload. |
| Multipass + frame history | Absent | biri | biri | Integrated | `niri_prev`, `niri_buffer`, `niri_screen`, `niri_source`, per-pass chains, black-seeded feedback. |
| Blur | Upstream | upstream | upstream | Upstream | `passes`, `offset`, `noise`, `saturation`, `xray`; `ext-background-effect-v1`. |
| KDE blur protocol | Absent | kde-niri | kde-niri | Integrated | `org_kde_kwin_blur` is different from upstream's `ext-background-effect-v1`. |
| Liquid glass / refraction / lens / edge light | Absent | zaroutt/Niri-glass via niri-elite | niri-elite port | Integrated | Implement as a background-effect shader, not a second engine. |
| VRR | Upstream | upstream | upstream | Upstream | On-demand and per-window rules. |
| Tearing control | Absent | urayde | urayde | Integrated | Upstream Smithay has no `wp_tearing_control_v1`; better-smithay must add it. Per-output gate: `allow-tearing always|never|fullscreen` (focused fullscreen by default). |

## Presentation and DRM

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Explicit sync (`linux-drm-syncobj-v1`) | Absent | urayde | urayde | Integrated | Upstream Smithay already has `drm_syncobj`. |
| Tearing | Absent | urayde | urayde | Integrated | Window rule + content type + async flip path. |
| FIFO (`wp-fifo-v1`) | Absent | urayde | urayde | Integrated | Upstream Smithay already has `fifo`. |
| Mailbox / immediate presentation | Absent | urayde, niri-spicy | urayde | Integrated | Expressed through tearing/content-type and async flips. |
| Async page flips | Absent | urayde (niri) + losnoco (Smithay) | urayde + losnoco | Integrated | Driver fallback to blocking commit required. |
| SurfaceBarrier / atomic layout transactions | Absent | niri-spicy + losnoco | niri-spicy | Integrated | Committing surfaces register into the transaction's `SurfaceBarrier`; buffer readiness (dmabuf/syncobj) still comes from the default per-surface pre-commit hook, and the deadline timer force-releases a stalled barrier. Ready barriers are released in `refresh()` right after `notify_blocker_cleared()`, before `layout.refresh()`. |
| Commit timing (`wp-commit-timing-v1`) | Absent | niri-spicy + losnoco | niri-spicy | Integrated | Protocol-only (no config, matching spicy). Due timers are signalled per output at its next target presentation time before `refresh()`, unmapped surfaces included; the earliest deadline arms a refresh-aligned wakeup timer. |
| Pointer warp (`wp_pointer_warp_v1`) | Absent | urayde | urayde | Integrated | PR #4001 closed unmerged; upstream Smithay has `pointer_warp`. |
| Multi-GPU | Partial | niri-elite (SHORiN) | niri-elite | Integrated (hardware validation pending) | Better render-node selection and display-only GPU handling. |
| Software EGL | Absent | niri-elite | niri-elite | Integrated | Upstream explicitly skips llvmpipe/softpipe. |
| DPMS frame callbacks | Upstream | upstream | do not port | Upstream | Fixed by `78b7adfe`, `bb38b7e1`. |
| DPMS VRAM/GTT cleanup | Partial | demoji PR #4750 (open) | backport RiversJin #4550 | Integrated | Dead buffer imports are reclaimed by the 1 s fallback timer even with no rendered frame (both renderers, every GPU); two EGL regression tests cover no outputs and monitors off. Hardware GTT profiling still needs a DRM seat. |
| DPMS isolated-output policy | Absent | biri | biri | Integrated (core) | `skip-isolated` keeps isolated outputs on while the rest sleep. |
| Suspend/resume, hotplug resource cleanup | Partial | upstream + niri-spicy | integration work | Partial | Covered by hardware tests. |

## Outputs and workspaces

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Virtual outputs | Absent | biri (niri PR #3800, open) | biri | Integrated (core) | TTY + headless; IPC/CLI create/remove; config `create-virtual`; virtual-output frame-callback path. |
| Isolated outputs | Absent | biri | biri | Integrated (core) | Overview/Alt-Tab/hotkey overlay/config notification hidden; `power-off-monitors skip-isolated=true` keeps them lit. |
| Hidden workspaces | Absent | biri (niri PR #2997, open) | biri | Integrated | `hidden` workspace config, `toggle-workspace-visibility`, hidden-aware IPC/focus/unhide rules; kept out of overview/render regions. |
| Portrait / vertical main axis | Absent | biri | biri | Integrated | Per-output/workspace; directional input stays physical. |
| Per-output behaviour | Upstream | upstream | upstream | Upstream | Output config and layout rules. |
| Multi-monitor improvements | Partial | niri-elite, biri | combine | Integrated | Carousel, isolated outputs, per-output gestures. |
| Different refresh rates | Upstream | upstream | upstream | Upstream | Per-output frame clocks. |
| Sticky windows | Absent | biri | biri native | Integrated | Native monitor-level sticky space, `open-sticky`, `toggle-window-sticky`, `float-above-fullscreen`; sticky windows render above workspaces. |
| Sticky floating windows | Absent | biri (niri PR #3302, closed) | biri | Integrated | `open-sticky` implies floating; sticky tiles live on the monitor, not a workspace. |
| Float above fullscreen | Absent | biri (niri PR #4062, open) | biri | Integrated | Opt-in window rule. |
| Stage / scratchpad workspaces | External | better-nsticky | better-nsticky | Integrated | Keep as external daemon; add compatibility mode. |

## Windows and desktop UX

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Grid overview | Absent | niri-elite (SHORiN) | niri-elite | Integrated | Per-monitor, tab cells, gestures, IPC focus report. |
| Window minimisation | Absent | niri-spicy, niri-elite | niri-spicy base + elite UX | Integrated | Real state, actions, rule, grid placeholder, strip. |
| Magnifier | Absent | niri-elite (SHORiN) | niri-elite | Integrated | Movement actions/binds; pick-colour overlay. |
| Window groups | Upstream tabs / partial | upstream + kde-niri | upstream | Partial | Upstream tabbed-column display only; Naxdy’s tile groups (auto-group, expel, tab titles) remain deferred — see `UNIMPLEMENTED.md`. |
| Hot corners | Upstream (overview booleans) | niri-elite | niri-elite | Integrated | Extend to arbitrary actions (`grid-overview`, etc.). |
| Agent seat | Absent | niri-elite (SHORiN) | niri-elite | Integrated | Second seat over IPC; needs Smithay per-client seat filter. |
| Focus-ring shaders | Absent | biri | biri | Integrated | Watched-file per-window focus-ring shaders, animated rainbow/wax ring, hot reload, and decoration light spill. |
| Custom borders | Upstream (colours/width) | upstream + biri shaders | biri shader layer | Integrated | Border/focus-ring shader programs with watched files and light spill; upstream border geometry retained. |
| SVG cursors | Absent | niri-elite (SHORiN) | niri-elite | Integrated | `resvg`/`usvg` optional dependency. |
| Shake-to-enlarge cursor | Absent | niri-elite (SHORiN) | niri-elite | Integrated | Cursor config section. |
| Carousel overview | Absent | biri | biri | Integrated | Consolidated multi-monitor cover-flow. |
| Dynamic overview zoom | Absent | biri | biri | Integrated | Zoom presets and actions. |
| Drag physics | Absent | biri | biri | Integrated | Elastic drag animation. |
| Multi-action binds | Absent | biri | biri | Integrated | Run several actions from one bind. |

## Input

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| Per-keyboard configuration | Absent | biri (niri PR #4459, open) | biri | Integrated (core) | Named `keyboard` blocks inherit unset fields; keymap/repeat/numlock/locale1/IPC follow the last physical keyboard. |
| Num Lock state | Upstream config | biri | biri | Integrated | biri fixes Num Lock/locale1/IPC across keyboard switches. |
| Keyboard layouts | Upstream | upstream | upstream | Upstream | Per-keyboard support extends this. |
| Disable-while-typing controls | Upstream config | biri | biri | Integrated (core input) | Runtime `toggle-dwt`, composes with config; resets on restart; applies to hot-plugged touchpads. |
| Pointer behaviour | Upstream | upstream + urayde | urayde | Integrated | Pointer warp; niri-elite's confine-to-fullscreen. |
| Gestures | Upstream | upstream + niri-elite/biri | combine | Integrated | Grid and carousel gestures reuse input plumbing. |
| Accessibility | Upstream | upstream + niri-elite | hybrid | Integrated | AccessKit tree covers workspaces, windows, overview, MRU, screenshot UI and the exit dialog; no separate AT-SPI helper is bundled. |
| Press/release binds | Absent | niri-elite | niri-elite | Integrated | Guarantees release after press. |
| Bare modifier triggers | Absent | niri-elite | niri-elite | Integrated | `Mod` as a bind trigger. |
| Raw/ASCII keysym fallback | Absent | niri-elite | niri-elite | Integrated | Non-ASCII layouts keep working. |

## Portals, capture and applications

| Feature | Upstream | Best source | Decision | better-niri | Notes |
| --- | --- | --- | --- | --- | --- |
| XDG desktop portal backend | Absent | niri-elite (SHORiN) | niri-elite | Integrated | `org.freedesktop.impl.portal.desktop.niri`. |
| ScreenCast | Upstream via GNOME portal | niri-elite native | niri-elite | Integrated | Native picker and permissions. |
| Screenshot | Upstream CLI/UI | niri-elite + kde-niri | niri-elite | Integrated | Path reply over IPC, picker UI. |
| Region capture | Upstream (image copy) | upstream | upstream | Upstream | Verify HDR→SDR conversion. |
| Window capture | Upstream (image copy) | upstream + urayde | upstream | Upstream | urayde has cursor-session rework; compare before porting. |
| Cursor capture | Upstream | urayde | urayde | Integrated | ext-image-copy cursor session refinements. |
| KDE integration | Partial (D-Bus) | kde-niri | kde-niri | Integrated | `org_kde_kwin_blur`, KDE output order, colorpicker and the `org.kde.kwin.Screenshot` interface (fullscreen/area/window/interactive, saved-path replies) integrated. |
| Spectacle support | Absent | kde-niri | kde-niri | Integrated | Through KDE screenshot D-Bus. |
| PipeWire | Upstream | upstream | upstream | Upstream |  |
| DMA-BUF → SHM fallback | Upstream | upstream | upstream | Upstream | niri-elite's fallback was merged upstream. |
| HDR capture conversion | Absent | niri-spicy | niri-spicy | Integrated | Must not blow out SDR output. |
| Privacy in captures | Partial (`block-out-from`) | demoji `block-out-blur` | demoji | Integrated | Blocked-out windows can be blurred instead of black; popups still hidden. |

## Reference-fork refinements

| Feature | Source | Status |
| --- | --- | --- |
| `hdr peak-luminance` | niri-spicy | Integrated |
| Per-output `allow-tearing` | niri-spicy | Integrated |
| Initial `xdg_toplevel` Suspended while locked/off | niri-spicy | Integrated |
| Async NVIDIA frame-fence wait before KMS queue | niri-spicy | Integrated (hardware pending) |
| Active decorations only on focused workspace | demoji | Integrated |
| `overview { focus-ring; shadow }` overrides | demoji | Integrated |
| Sticky focus switching/centering | biri | Integrated |
| Pointer warp on carousel confirm | biri | Integrated |
| Hidden-workspace child placement | biri | Integrated |
| Spawn floating relative to cursor | Naxdy | Integrated |
| Blur brightness/contrast | Naxdy | Integrated |
| Layer fade in/out | Naxdy | Deferred — see `UNIMPLEMENTED.md` |
| HDR scanout colour pipelines, post-blend encode, HDR cursor plane | niri-spicy | Deferred — see `UNIMPLEMENTED.md` |
| Screenshot2 `CaptureWorkspace` | Naxdy | Deferred — old interface captures all outputs; see `UNIMPLEMENTED.md` |

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
