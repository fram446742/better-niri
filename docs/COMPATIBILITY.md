# Dependency and compatibility matrix

## 1. Current pin (baseline, verified)

| Component | Source | Revision | Notes |
| --- | --- | --- | --- |
| better-niri | `fram446742/better-niri` | `02ee0585b9da359cb30aa2a13a6023a6e471cc23` | Identical to niri upstream `main` at audit time. |
| better-smithay | `fram446742/better-smithay` | `a38bb709a0400759b6a4fab6129e0f1dd72808c3` | Identical to Smithay `master`; the revision niri upstream pins. |
| smithay-drm-extras | `fram446742/better-smithay` | same `a38bb709…` | Must always match `smithay`. |
| niri-config / niri-ipc | workspace | `26.4.0` | Version follows the workspace. |
| Rust | toolchain | MSRV 1.88 | Smithay raised its MSRV to 1.88; niri matches. |

`LIBCLANG_PATH=/usr/lib cargo check --workspace` passes at this pin (2026-11-23,
24 s incremental, Rust 1.102.0-nightly).

## 2. Planned pin after Smithay consolidation

The exact commit will be filled in when the consolidated branch is reviewed and
pushed. The rules are more important than the hash:

| Component | Required state |
| --- | --- |
| `smithay` | better-smithay `master`, one commit |
| `smithay-drm-extras` | the **same** better-smithay commit |
| Smithay features | upstream set niri already enables, plus `renderer_vulkan`/`backend_vulkan` when niri's `vulkan` feature is on |
| Niri `vulkan` feature | off by default until parity tests pass; GLES remains the reference renderer |
| Protocol XMLs | `wp-color-management-v1`, `wp-tearing-control-v1`, `ext-background-effect-v1` (upstream), KDE blur XML only if the KDE protocol is enabled |

## 3. Source-fork combinations (do not mix blindly)

| Fork | Niri base | Smithay dependency | Compatibility note |
| --- | --- | --- | --- |
| niri-elite | `fcbc971c` | MEKCCK/smithay `66b0670` | Works for its HDR/protocol set; lacks Vulkan. |
| niri-spicy | `5f4469b6` | `Smithay/smithay` `79bbed5e` **patched by path** to losnoco `spicy-master` | A fresh clone without `../smithay` builds against upstream Smithay and fails: the `rev` and the local patch disagree. Do not reproduce this layout in better-niri. |
| biri | `2fbc6f3b` | upstream Smithay `a38bb709` | No graphics fork; shaders are GLES on upstream Smithay. |
| kde-niri | `54c7fdcd` (2025-11) | path `../smithay` | One year of upstream drift; cherry-pick only. |
| losnoco/smithay | Smithay `79bbed5e` | — | 84 commits behind current Smithay. |
| MEKCCK/smithay | Smithay `8a270232` | — | 50 behind; colour code partially adapted to newer surface cache. |

## 4. Single-Smithay policy

- better-niri declares exactly one `smithay` and one `smithay-drm-extras` source.
- No `[patch]` table with a machine-specific path is committed. Use the command
  below for local development.
- When Vulkan is disabled, the same Smithay commit must still build and run with
  GLES only.
- Updating Smithay is a reviewed commit that updates both `rev` lines,
  `Cargo.lock` and this document.
- Dropping a private Smithay commit is required when upstream lands an
  equivalent; record the upstream commit/PR.

### Local development against the sibling checkout

From `better-niri/`, with `../better-smithay` checked out:

```sh
LIBCLANG_PATH=/usr/lib cargo \
  --config 'patch."https://github.com/fram446742/better-smithay.git".smithay.path="../better-smithay"' \
  --config 'patch."https://github.com/fram446742/better-smithay.git".smithay-drm-extras.path="../better-smithay/smithay-drm-extras"' \
  check --workspace
```

Do not commit a `[patch]` table pointing at `../better-smithay`; it breaks
clean checkouts and CI.

## 5. Renderer and colour combinations

| Combination | Status | Required validation |
| --- | --- | --- |
| GLES + SDR | Supported upstream | CI build + hardware smoke. |
| GLES + HDR | Planned | GPU/driver matrix; colour and transparency tests. |
| Vulkan + SDR | Planned, optional | Format import, sync, scanout, capture. |
| Vulkan + HDR | Planned, optional | Tone mapping, scanout formats, Nvidia fences. |
| Vulkan requested but unsupported | Must fall back to GLES with a clear log/config error | Test with a Vulkan-less build/GPU. |
| Software EGL (llvmpipe) | Upstream rejects; planned support | VM test. |
| Multi-GPU (render on dGPU, scan out on iGPU) | Partial upstream | Hybrid laptop test; format list `Abgr2101010` already accounts for Nvidia copy quirks. |

## 6. System dependencies (Arch/CachyOS reference)

Build-time:

- Rust >= 1.88 (nightly is used in this environment but stable should work).
- `clang` / `libclang` (`LIBCLANG_PATH=/usr/lib` if bindgen cannot find it).
- `libpipewire-0.3` headers (screencasting).
- `libseat`, `libinput`, `libdrm`, `libgbm`, `libEGL`, `libGLESv2`.
- `pango`, `cairo`, `gtk4` (accessibility/portal UI), `libdisplay-info`.
- Vulkan loader/headers when the `vulkan` feature is enabled.

Runtime (hardware validation):

- DRM/KMS with atomic modesetting; Intel/AMD/Nvidia.
- Wayland clients: Firefox, mpv (`--vo=gpu-next`), gamescope, Wine/Proton.
- PipeWire + WirePlumber for casts; xdg-desktop-portal for portal tests.

## 7. Protocol/API compatibility notes

- `ext-image-copy-capture-v1` is upstream; do not implement a second capture
  protocol. HDR conversion hooks into the existing paths.
- `ext-background-effect-v1` is upstream; `org_kde_kwin_blur` is separate and
  additive.
- Upstream Smithay already provides `fifo`, `drm_syncobj`, `commit_timing`,
  `content_type`, `pointer_warp`, `image_copy_capture`, `background_effect`.
  Only `tearing_control`, colour management, DRM colour pipelines, async flips,
  SurfaceBarrier and the Vulkan renderer need better-smithay commits.
- `niri-config` and `niri-ipc` are public APIs for external tools. Add fields and
  actions; do not rename or remove without a deprecation plan.
- The default config must parse and must not contain duplicate binds; CI runs
  `cargo test -p niri-config`.

## 8. Known incompatibilities to track

1. niri-spicy's `Cargo.toml` revision and path patch disagree; our layout must
   avoid that trap.
2. losnoco's colour code predates the newer Smithay surface-cache API; MEKCCK's
   `fb3300c2` is the reference adaptation.
3. Vulkan renderer and GLES-only shader code are not interchangeable until the
   shader contract is implemented on both; gate with a runtime warning.
4. Tearing + HDR + async flips share DRM atomic state; validate with the colour
   pipeline active before enabling by default.
5. KDE D-Bus interfaces from Naxdy target a 2025 niri; re-check signatures
   against current `src/dbus`.
