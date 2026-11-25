# Testing

Compilation is not validation. This document separates what runs in CI from what
requires real hardware, and records actual results. A feature is only "tested"
when the row says so.

## 1. Current results (2026-11-25)

`scripts/test-all.sh` is the single non-hardware gate. It runs the compositor
suite, the feature matrix, every companion workspace, packaging, completions,
the Noctalia plugins and the external better-nsticky checkout, and writes a TSV
summary under `/tmp/better-test-run-<timestamp>/summary.tsv`.

| Check | Command | Result |
| --- | --- | --- |
| Compositor workspace | `cargo test --workspace` | **Pass**, 890 tests, 0 failed |
| Minimal build | `cargo test -p niri --no-default-features` | **Pass**, 420 tests, 0 failed |
| IPC clap feature | `cargo test -p niri-ipc --features clap` | **Pass** |
| Visual-test build | `cargo build -p niri-visual-tests` | **Pass** |
| Feature matrix | `cargo check -p niri --no-default-features --features <each>` for agent, effects, navigation, cursor, kde, dbus, systemd, xdp-gnome-screencast, vulkan; plus the full explicit set | **Pass** |
| Clippy | `cargo clippy --workspace --all-targets` + all companion workspaces | **Pass** (root keeps pre-existing style warnings; companions clean) |
| Format | `cargo fmt --check` in niri and every workspace | **Pass** |
| Companion workspaces | `cargo test --workspace` in each of the nine apps | **Pass**; companion 225, portal 42, monitor 80, session 54, shot 122, wallpaper 43, settings 98, audio 54, sidepanels 62 |
| Packaging | `packaging/scripts/verify.sh` | **Pass** |
| Completions | `generate-completions.py --check` | **Pass**, 36 files in sync |
| Noctalia plugins | `better-noctalia-plugins/verify.sh` + live shell | **Pass**, 8 loaded, refresh/click exercised |
| better-nsticky (external) | `cargo test --all-targets` in the local checkout | **Pass**, 473 tests |
| Automated hardware pass | `HDR_OUTPUT=DP-1 scripts/pending-checks/hardware-validation.sh` on DP-1 | **Pass**: Vulkan, capabilities, VRR, 10-bit, explicit sync, 2 DRM devices, runtime HDR auto/off/auto, screenshot, HDR clip `hdr=true`→`hdr=false` |
| Subjective hardware | tearing game, commit-timing client, second-GPU output, rotated transitions, HDR mixed contrast, popups/animations, real portal client | **Manual**, see `docs/validation/hardware.md` |

Note: `cargo check --workspace --all-features` in better-smithay fails inside
the `profiling` crate because puffin and tracy backends are mutually exclusive;
that is feature unification, not project code.

Do not mark hardware features as tested based on a build. The automated hardware
pass above is real DP-1 evidence; the subjective checklist still needs eyes.

## 2. CI checks (no GPU required)

```sh
LIBCLANG_PATH=/usr/lib cargo check --workspace
LIBCLANG_PATH=/usr/lib cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
./scripts/test-all.sh
```

Additional CI gates:

- `cargo test -p niri-config` — includes the default-config parse test that
  catches duplicate binds.
- `cargo test -p niri-ipc` — IPC serialization stability.
- `contrib/check-upstream-drift.sh upstream/main` — config/IPC drift report.
- Headless visual tests (`niri-visual-tests`): the main geometry/behaviour gate
  for overview, grid, minimize placeholders, carousel and shader element bounds.
- Build matrix: default features, `--no-default-features` (with the minimal
  feature set), `--no-default-features --features vulkan`, and
  `--features vulkan` (default + Vulkan).
- Vulkan readiness: `niri --check-vulkan` prints the resolved plan as JSON
  (preference, active path, render-tree capability, per-device support and the
  fallback reason). It does not need a running compositor, so it can run in CI
  and on user machines when diagnosing drivers.
- Vulkan TTY output: start niri on a spare TTY with `NIRI_RENDERER=vulkan`.
  Expect core window/layer rendering to work and a startup log naming any
  GLSL-only visual that is disabled. **Passed on 2026-11-24**: "using the Vulkan
  renderer", border/shadow pixel shaders compiled, 4K DP-1 output created. This
  stays a manual check because it owns a DRM device and cannot run in the
  normal test suite.

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
  - Automated: `cargo test -p niri --lib renderer_cleanup` imports two EGL
    textures, drops one (and later the other) with no output rendering, and
    asserts the 1 s fallback timer reclaims it while live textures survive.
    Run once with no outputs and once with `deactivate_monitors`.
  - Manual: driver counters (DRM fdinfo / `amdgpu_top` / `nvtop`) across real
    DPMS cycles and a fullscreen game exit.
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
