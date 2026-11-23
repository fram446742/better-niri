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

Reconciled with upstream `02ee0585`:

- focus flags on move actions, `pass_through` binds, DPMS frame-callback guards;
- ext-image-copy capture and the newer damage-tracker capture paths;
- stationary view position for tile IPC, station/gesture restriction;
- HDR screencopy uses `CaptureBuffer` and the native portal `node_sink` path.

Verified on this branch:

| Check | Result |
| --- | --- |
| `cargo check --workspace` | pass |
| `cargo test --workspace` | pass, 652 tests |
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

## Next steps

1. Runtime-test the elite branch on AMD/Intel/Nvidia and multi-monitor. Keep the
   currently running compositor untouched.
2. Use `better-nsticky`'s fake-niri/e2e harness for IPC-level tests of new
   actions, and headless `niri-visual-tests` for grid/minimise/shader geometry.
3. Add the remaining biri features on top: hidden workspaces, virtual/isolated
   outputs, sticky floating windows, per-keyboard config, vertical axis,
   shader pipeline and carousel.
4. Add Naxdy's KDE screenshot/colour-picker/output-order and `org_kde_kwin_blur`.
5. Add demoji's `block-out-blur`; investigate PR #4750 texture-cache cleanup.
6. Integrate better-nsticky as a compatibility client (delegate sticky to the
   compositor, keep staging/scratchpads).
7. Only then consider moving the branch to `main`.
