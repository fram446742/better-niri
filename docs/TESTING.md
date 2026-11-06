# Testing

Compilation is not validation. This document separates what runs in CI from what
requires real hardware, and records actual results. A feature is only "tested"
when the row says so.

## 1. Current results (2026-11-23)

| Check | Command | Result |
| --- | --- | --- |
| Workspace check (safe `main`) | `cargo check --workspace` with Smithay `a38bb709` | **Pass** |
| Workspace check (elite branch) | `cargo check --workspace` with Smithay `2e8e9edb` | **Pass** |
| Full tests (elite branch) | `cargo test --workspace` | **Pass**, 667 tests, 0 failed |
| Clippy (elite branch) | `cargo clippy --workspace --all-targets` | **Pass**, no errors; warnings remain in elite-derived code |
| Format (elite branch) | `cargo fmt --check` | **Pass** |
| Smithay consolidated, GLES | `cargo check -p smithay` with niri's feature set | **Pass** |
| Smithay consolidated, Vulkan | `cargo check -p smithay --all-features` | **Pass** (warnings only) |
| Release build | `cargo build --release` | Not re-run after the elite merge |
| Virtual/isolated outputs | `cargo test --workspace` (fixtures) + visual tests | Logic tested; real TTY/hotplug/DPMS not yet hardware-tested |
| Hardware features (HDR/VRR/tearing/explicit sync/Vulkan) | — | **Not tested** |

Note: `cargo check --workspace --all-features` in better-smithay fails inside
the `profiling` crate because puffin and tracy backends are mutually exclusive;
that is feature unification, not project code.

Do not mark hardware features as tested based on a build.

## 2. CI checks (no GPU required)

```sh
LIBCLANG_PATH=/usr/lib cargo check --workspace
LIBCLANG_PATH=/usr/lib cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Additional CI gates:

- `cargo test -p niri-config` — includes the default-config parse test that
  catches duplicate binds.
- `cargo test -p niri-ipc` — IPC serialization stability.
- `contrib/check-upstream-drift.sh upstream/main` — config/IPC drift report.
- Headless visual tests (`niri-visual-tests`): the main geometry/behaviour gate
  for overview, grid, minimize placeholders, carousel and shader element bounds.
- Build matrix: default features, `--no-default-features` (with the minimal
  feature set), and `--features vulkan` once it exists.

Debug and release must both build; debug for tests, release for the performance
smoke.

## 3. Feature test plan

### Rendering and colour

| Test | Method | Hardware |
| --- | --- | --- |
| HDR output engages | `niri msg outputs` + `drm_info`/kernel logs show BT.2020 and HDR metadata; monitor reports HDR | HDR-capable GPU + monitor |
| SDR/HDR mixing | SDR window on HDR desktop is not washed out; compare reference-luminance settings | same |
| HDR capture | Screenshot and PipeWire cast are tone-mapped, not blown out | same |
| Tone mapping | mpv `--vo=gpu-next` HDR clip; check peak luminance behaviour | same |
| 10-bit scanout | `drm_info` plane format; no per-frame failures | panel supporting 10-bit |
| Direct scanout | Fullscreen video stays on a plane; `niri msg` debug/logs | any |
| Vulkan vs GLES | Same scene, compare colours/captures; force each path | Vulkan-capable GPU |
| Vulkan fallback | Request Vulkan with unsupported GPU; expect GLES + warning | VM or `VK_ICD` trick |
| Format import | Wine/Proton game, NV12 video, DMA-BUF clients | Nvidia + AMD |
| Shaders × HDR | Global/window shader on an HDR output; verify working space and capture gate | HDR GPU |

### Presentation and DRM

| Test | Method | Hardware |
| --- | --- | --- |
| Tearing | `allow-tearing` game/fullscreen video; observe immediate presentation | AMD/Intel/Nvidia |
| FIFO | Client that queues frames is throttled, no runaway buffers | any |
| Explicit sync | `linux-drm-syncobj` client; no flicker/crash | any |
| Async flips | Enable debug option; check logs for async vs fallback per driver | AMD (primary), Nvidia |
| Pointer warp | Pointer-lock game warps without hide/show | any |
| VRR | On-demand/fullscreen rule; verify refresh changes | VRR display |
| Multi-monitor mixed refresh | 60 Hz + 144 Hz, check frame clocks and latency | two outputs |
| Resolution change / hotplug | Plug/unplug, change mode, verify no stale buffers | any |
| DPMS cycle + memory | 100 × power off/on; sample VRAM/GTT before/after | AMD (`mem_info_vram_used`, `mem_info_gtt_used`) |
| Suspend/resume | `systemctl suspend`, resume, verify outputs and clients | laptop |

### Layout and UX

| Test | Method |
| --- | --- |
| Hidden workspaces | Declare/toggle; verify strip, overview, IPC and move/focus edge cases |
| Virtual outputs | Create/remove at runtime; headless and TTY; capture target |
| Isolated outputs | Overview/switcher/notifications stay off; windows render |
| Sticky windows | Follow workspace/output; toggling; carousel panels |
| Float above fullscreen | Floating rule stays visible over fullscreen |
| Per-keyboard | Two keyboards with different layouts; Num Lock, locale1, IPC events |
| Vertical axis | Portrait output, gestures, directional focus/move, config reload |
| Grid overview | Open/close, focus/move, tab cells, minimized cells, per monitor |
| Minimize | Actions, open-minimized rule, restore, IPC, animation |
| Magnifier | Movement binds, pick-colour overlay |
| Carousel | Multi-monitor rotate, lens interaction, hotplug |
| Shaders | Global/region/window/output, multipass, feedback, hot reload, fps cap, capture gate |
| Liquid glass | Toggle, performance, transparency, HDR interaction |
| SVG cursors / shake | Theme load, shake thresholds, hold/grow |
| KDE blur / screenshot | Konsole blur, Spectacle capture, colour picker, output order |

### Input

Press/release binds, bare modifiers, raw/ASCII keysym fallbacks, multi-action
binds, `toggle-dwt`, pointer warp, confine-to-fullscreen, agent seat isolation.

### Portals and capture

- `xdg-desktop-portal` ScreenCast with the native backend: output, window and
  region selection, permission persistence, cursor inclusion.
- SHM fallback in a VM without GBM.
- `ext-image-copy-capture` clients directly.
- HDR→SDR cast/screenshot.
- KDE screenshot D-Bus and Spectacle.
- `block-out-from` and `block-out-blur` privacy: blocked content never appears in
  any capture path, including shaders-in-capture.

### Stability and performance

- Idle CPU when effects are off; no redraw loop with static shaders.
- Frame time on 4K/144 Hz with blur/shader enabled and disabled.
- Scanout preserved when effects are inactive.
- No allocation growth over a 10-minute animated shader run.
- VRAM/GTT after DPMS cycles and after closing fullscreen games.
- Input latency (high-speed camera or `libinput debug-events` + frame timing)
  before/after effects.

## 4. Hardware matrix

Record the actual machine for each result; do not generalize from one GPU.

| Class | Example | Required for |
| --- | --- | --- |
| AMD iGPU/dGPU (Mesa RADV/radeonsi) | CachyOS desktop | HDR, VRR, tearing, async flips, DPMS memory, Vulkan |
| Intel (Mesa ANV/i915/Xe) | laptop | HDR, VRR, multi-GPU, Vulkan |
| Nvidia proprietary | hybrid laptop/desktop | fences, explicit sync, Vulkan, tearing fallbacks |
| VM (llvmpipe/virgl) | QEMU | software EGL, SHM casts, Vulkan fallback |
| Multi-GPU hybrid | dGPU render + iGPU scanout | render-node selection, format copy |
| HDR monitor | DisplayHDR panel | HDR metadata, tone mapping, captures |
| VRR monitor | FreeSync/GSync | VRR |

Tools: `drm_info`, `niri msg outputs --json`, `niri msg casts`, kernel logs,
`/sys/kernel/debug/dri/*`, `radeontop`/`nvtop`, `pw-top`, `wl-screenrec`,
`gamescope`, `mpv --vo=gpu-next`, `vkcube`, OBS.

## 5. Reporting rules

- State the commit, kernel, Mesa/Nvidia driver and monitor for every hardware
  result.
- Separate "builds", "unit tests pass", "headless visual test passes" and
  "verified on hardware".
- File known failures with logs and reproduction steps; do not hide them behind
  a green CI badge.
- Re-run the relevant rows after every upstream merge that touches rendering,
  DRM or input.
