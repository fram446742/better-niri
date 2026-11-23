# Project status

Last updated: 2026-11-23.

## Safe baseline (`main`)

`main` is niri upstream `02ee0585` plus documentation, the better-smithay pin,
and the upstream drift script. It builds against better-smithay `a38bb709`
(Smithay upstream, the revision niri pins) and has no behavioural changes.

Run:

```sh
LIBCLANG_PATH=/usr/lib cargo check --workspace
cargo test --workspace
./contrib/check-upstream-drift.sh
```

## Integration branches (local, not merged to main)

### better-smithay `integration/hdr-color` @ `344be37b`

Upstream Smithay `a38bb709` + losnoco `spicy-master` (colour management, DRM
colour pipelines, async flips, `wp_tearing_control_v1`, SurfaceBarrier, tone
mapping, Vulkan renderer) + MEKCCK per-client seat filter.

Verified:

- `cargo check -p smithay` with niri's GLES feature set: pass.
- `cargo check -p smithay --all-features` (Vulkan): pass.

### better-niri `integration/smithay-hdr`

Consumes the pin above and adds:

- the three Smithay API adaptations (frame submission tuple, `PresentationMode`,
  async DMABUF feedback);
- `linux-drm-syncobj-v1` from urayde (`4a009bda`);
- `wp_pointer_warp_v1` from urayde (`d441c77e`).

Verified on this branch:

| Check | Result |
| --- | --- |
| `cargo check --workspace` | pass |
| `cargo test --workspace` | pass, 9 suites, 331 tests |
| `cargo clippy --workspace --all-targets` | pass, one upstream `niri-ipc` warning |
| `cargo fmt --check` | pass |
| `cargo build --release` | pass, 27 MB binary |

**Not verified:** any hardware behaviour (HDR, VRR, tearing, explicit sync,
pointer warp, direct scanout, DPMS). Do not merge to `main` before the hardware
matrix in `TESTING.md` has real results.

## Next steps

1. Runtime-test the integration branch on AMD/Intel/Nvidia and multi-monitor.
2. Port HDR/colour management from niri-spicy onto this branch (GLES first),
   then the Vulkan renderer.
3. Add tearing/FIFO/async flips; FIFO needs urayde's transaction blocker context.
4. Continue with biri's layout features on a separate branch (they are only two
   upstream commits behind and are independent of the graphics stack).
