# Dependency and compatibility matrix

## 1. Current pins

### Safe baseline (`main`)

| Component | Source | Revision | Notes |
| --- | --- | --- | --- |
| better-niri | `fram446742/better-niri` | `main` (audit baseline `02ee0585` + docs) | No behavioural changes. |
| better-smithay | `fram446742/better-smithay` | `a38bb709a0400759b6a4fab6129e0f1dd72808c3` | Identical to Smithay `master`; the revision niri upstream pins. |
| smithay-drm-extras | same better-smithay commit | `a38bb709…` | Must always match `smithay`. |

### Integration branch (`integration/elite-base`)

| Component | Source | Revision | Notes |
| --- | --- | --- | --- |
| better-niri | `fram446742/better-niri` | branch `integration/elite-base` | niri upstream + niri-elite feature base, reconciled. |
| better-smithay | `fram446742/better-smithay` | `252e346b8244e8e95029526e7d174b4fe7bce0aa` | MEKCCK/smithay `66b0670` + losnoco Vulkan/DRM-colour/SurfaceBarrier. |
| smithay-drm-extras | same better-smithay commit | `252e346b…` | |
| niri-config / niri-ipc | workspace | `26.4.0-elite` | Elitish version string; to be renamed for better-niri. |
| Rust | toolchain | MSRV 1.88 | |

Verified at this pin: `cargo check --workspace`, `cargo test --workspace`
(652 tests), `cargo clippy --workspace --all-targets` (no errors) and
`cargo fmt --check`. Hardware is still untested.

## 2. Integration-branch API adaptation

Porting the niri-elite base onto current upstream required these changes:

| Area | Adaptation |
| --- | --- |
| DRM frame submission | `frame_submitted()` returns the feedback tuple directly; async DMABUF feedback is built from plane `formats_async`. |
| `PresentationMode` | `render_frame()` passes `PresentationMode::VSync` until tearing is wired. |
| Layout move actions | focus flag added to window/column moves; elite's grid `MoveTargets` kept alongside it. |
| Bind table | upstream `pass_through` field added to elite-created binds. |
| HDR screencopy | upstream's `CaptureBuffer`/damage-tracker path retained, with reference luminance passed to `render_to_dmabuf`/`render_to_shm`. |
| Native portal cast | uses `node_sink.deliver()`; the generic Mutter signal emitter path is no longer used on the native portal path. |
| Config | `force-tearing` restored as a KDL child; `nested-show-host-cursor` kept from upstream. |
| Capture paths | ext-image-copy (upstream) kept; elite's window-copy HDR hooks adapted to `capture_bbox`. |

The full integration registry is in `UPSTREAM.md`.

## 3. Source-fork combinations (do not mix blindly)

| Fork | Niri base | Smithay dependency | Compatibility note |
| --- | --- | --- | --- |
| niri-elite | `fcbc971c` | MEKCCK/smithay `66b0670` | The feature base chosen for better-niri; lacks Vulkan, so losnoco is merged on top in better-smithay. |
| niri-spicy | `5f4469b6` | `Smithay/smithay` `79bbed5e` **patched by path** to losnoco `spicy-master` | A fresh clone without `../smithay` builds against upstream Smithay and fails. Not reproduced here. |
| biri | `2fbc6f3b` | upstream Smithay `a38bb709` | Next source: layout, hidden/virtual/isolated outputs, per-keyboard, shaders. |
| kde-niri | `54c7fdcd` (2025-11) | path `../smithay` | One year of upstream drift; cherry-pick only. |
| losnoco/smithay | Smithay `79bbed5e` | — | Merged into better-smithay for Vulkan/DRM colour/SurfaceBarrier. |
| MEKCCK/smithay | Smithay `8a270232` | — | First parent of better-smithay `integration/elite-base`. |

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
