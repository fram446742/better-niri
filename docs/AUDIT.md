# loom-niri — Phase A audit (2026-11-23)

This is the initial repository and feature audit requested before integration work.
It records what was verified in the local clones, not what READMEs claim. Every
status below was checked with `git log`, `git diff`, source reads, or the GitHub
API on 2026-11-23.

Companion documents:

- [`FEATURES.md`](FEATURES.md) — full feature matrix and status.
- [`INTEGRATIONS.md`](INTEGRATIONS.md) — provenance, branches, commits, chosen implementation.
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — proposed module layout and integration seams.
- [`UPSTREAM.md`](UPSTREAM.md) — upstream sync procedure and conflict map.
- [`COMPATIBILITY.md`](COMPATIBILITY.md) — pinned Niri/Smithay/DRM/renderer combinations.
- [`TESTING.md`](TESTING.md) — CI vs hardware test plan and current results.
- [`ATTRIBUTION.md`](ATTRIBUTION.md) — authors, projects, licences.

## 1. Repository state

| Repository | Local path | Branch / HEAD | Relation to upstream | Working tree |
| --- | --- | --- | --- | --- |
| loom-niri | `loom-niri/` | `main` @ `02ee0585` | identical to `niri-wm/niri` `main` @ `02ee0585` (3,097 commits, 0 ahead) | clean |
| loom-smithay | `loom-smithay/` | `master` @ `a38bb709` | identical to `Smithay/smithay` `master` @ `a38bb709` (the revision niri pins) | clean |
| niri (reference) | `niri/` | `main` @ `02ee0585` | upstream | clean |
| biri | `biri/` | `main` @ `5b465e4c` | base `2fbc6f3b` (2026-11-21), 379 feature commits ahead, 2 behind current niri | clean |
| niri-elite | `niri-elite/` | `main` @ `31458c94` | base `fcbc971c` (2026-10-27), 317 commits ahead, 92 behind | clean |
| niri-spicy | `niri-spicy/` | `spicy-main` @ `1bfaa833` | base `5f4469b6` (2026-09-22), 67 commits ahead, 171 behind | clean |
| kde-niri (Naxdy) | `kde-niri/` | `main` @ `b09a30b7` | base `54c7fdcd` (2025-11-25), 148 ahead, 540 behind | clean |
| smithay (losnoco) | `smithay/` | `spicy-master` @ `dba9452b` | base Smithay `79bbed5e` (2026-09-22), 50 ahead, 84 behind | clean |
| loom-nsticky | `../loom-nsticky/source` | `main` @ `f4f5bfd` | independent daemon, JSON over niri IPC | clean |

Actions taken during the audit (all reversible):

- Added `upstream` remote to loom-niri (`niri-wm/niri`) and fetched it; `origin/main == upstream/main`.
- Added `upstream`, `mekcck`, `losnoco` remotes to loom-smithay for comparison.
- Fetched `MEKCCK/smithay` `66b0670` and `losnoco/smithay` `spicy-master` into loom-smithay object storage and pinned them under `refs/audit/*`; no branches were merged.
- Repointed loom-niri's `smithay` and `smithay-drm-extras` dependencies at `fram446742/loom-smithay`, same revision `a38bb709` (byte-identical content, no functional change), establishing the single-Smithay relationship.
- `LIBCLANG_PATH=/usr/lib cargo check --workspace` passes.

No new forks or repositories were created. No history was rewritten and nothing was pushed.

## 2. What upstream already ships

At `02ee0585`, niri mainline already has:

- Background blur and the `ext-background-effect-v1` path (`handlers/background_effect.rs`,
  `render_helpers/background_effect.rs`, `niri-config::Blur`: `passes`, `offset`,
  `noise`, `saturation`, `xray`). This is *not* the same protocol as
  `org_kde_kwin_blur`.
- `ext-image-copy-capture-v1` for outputs, windows and cursors, plus screencopy
  (PR #4554 merged 2026-09-12), and the GNOME/mutter screencast D-Bus paths.
- PipeWire SHM fallback when no GBM device is available (merged from SHORiN work).
- Tabbed columns (`ToggleColumnTabbedDisplay`, `SetColumnDisplay`).
- VRR (`variable-refresh-rate`, per-window rules, on-demand), direct scanout,
  10-bit scanout format constants, `block-out-from` (including `kind="hide"`),
  hot corners (booleans that open the overview), `toggle-touchpad` (PR #3316).
- DPMS frame-callback guards: `send_frame_callbacks()` and the fallback timer
  already return early when `monitors_active` is false (commits `78b7adfe`,
  `bb38b7e1`). The demoji frame-callback patch is therefore **already upstream**;
  do not port it again.
- `move-window-to-new-workspace-below/above` and `move-column-to-new-workspace-*`.

Not in upstream at this commit: HDR / colour management, Vulkan renderer,
tearing / FIFO / DRM syncobj / commit-timing / content-type / pointer-warp,
window minimization, grid overview, magnifier, agent seat, virtual outputs,
isolated outputs, hidden workspaces, native sticky windows, per-keyboard
configuration, `float-above-fullscreen`, user shaders, SVG cursors,
shake-to-enlarge, built-in XDG portal backend, KDE D-Bus screenshot/colour
picker, liquid glass, consolidated carousel overview, vertical main axis.

## 3. Representative sources and divergence

| Project | Divergence | Why it matters |
| --- | --- | --- |
| niri-elite | 317 commits over 27 days, 166 files, +45,944/−2,119 | Broadest UX feature set: grid, minimize, magnifier, agent seat, portal backend, SVG cursors, liquid glass. Merge-built from upstream + SHORiN + urayde + zaroutt, so history is not directly cherry-pickable everywhere. |
| biri | 379 commits, base only 2 commits behind current niri | Freshest large feature fork; clean per-feature commits for layout/input/shader work. Lowest porting risk today. |
| niri-spicy (losnoco) | 67 commits, 171 behind | Only public fork with a working Vulkan renderer plus HDR/colour-management, tone mapping, async flips, tearing, syncobj, FIFO, commit-timing, SurfaceBarrier, minimize. Uses losnoco/smithay. |
| kde-niri (Naxdy) | 148 commits, 540 behind (synced from a 2025-11 base) | KDE integration and blur work; much of its blur lineage is now upstream. Only cherry-pick specific commits. |
| urayde/niri | current main @ `80e0f89` (2026-11-18) | Canonical, fresh implementation of tearing, FIFO, syncobj, pointer warp, async flips, built on upstream Smithay. |
| SHORiN-KiWATA/niri | main @ `4109fc7` (2026-11-09) | Origin of grid overview, magnifier, agent seat, portal backend, SVG cursors, shake-to-enlarge. |
| demoji/niri | main @ `3d4ba3e` | DPMS frame-callback fix (now upstream), `block-out-blur` privacy rule, overview focus-ring/shadow overrides, PR #4750 memory work (still open). |
| zaroutt/Niri-glass | main @ `72ee163` | Liquid glass in 3 files; already hand-ported by niri-elite. |
| losnoco/smithay | `spicy-master` @ `dba9452b`, 50 commits | Origin of the colour-management lineage, DRM colour pipelines, SurfaceBarrier, commit-timing, async flips, Vulkan renderer. |
| MEKCCK/smithay | `66b0670`, 16 commits | Rebase of the losnoco colour lineage plus urayde async flips, tearing, and per-client seat filtering. No Vulkan renderer. |
| loom-nsticky | independent daemon | Sticky/staged/scratchpad window management over niri IPC; BSD-3-Clause. |

## 4. Duplicate implementations and chosen base

| Feature | Candidates | Choice | Reason |
| --- | --- | --- | --- |
| Smithay feature set | upstream, MEKCCK, losnoco, urayde, dividebysandwich, reverb256 | **losnoco lineage as base**, plus MEKCCK seat filtering | Only lineage with Vulkan renderer + colour management + DRM colour pipelines + SurfaceBarrier/commit-timing. MEKCCK is a rebase of part of it on a newer base; useful as an adaptation reference, not as a second dependency. |
| HDR / colour management | niri-spicy, niri-elite (same lineage) | **niri-spicy** | Origin of the lineage, includes Vulkan HDR, tone mapping, PQ offload, output recreation on HDR toggle, metadata handling. niri-elite adds rebase fixes but is a subset without Vulkan. |
| Tearing / FIFO / syncobj / pointer warp | urayde, niri-spicy, niri-elite | **urayde** | Freshest (2026-11-18), separate small commits, built on upstream Smithay protocol modules, no dependency on SurfaceBarrier. |
| Async page flips | urayde (niri side), losnoco/MEKCCK (Smithay) | **urayde niri side + losnoco Smithay** | Same lineage in Smithay; urayde's niri integration is current. |
| Commit-timing / SurfaceBarrier | niri-spicy only | **niri-spicy, later** | Only implementation. Valuable but reworks transaction handling; defer until HDR/presentation baseline is stable. |
| Grid overview, minimize, magnifier, agent seat, portal backend, SVG cursors, shake-to-enlarge | niri-elite (SHORiN) | **niri-elite** | The only complete, rebased implementations. |
| Liquid glass | zaroutt/Niri-glass, niri-elite port | **niri-elite port** | Already rebased onto a recent niri; it is a background-effect shader, not a second renderer. |
| User shader pipeline | biri | **biri** | Most complete: global/region/window/output scopes, multipass, feedback, redraw scheduling, capture opt-in, presets, hot reload. |
| Hidden workspaces, virtual outputs, isolated outputs, sticky floating windows, float-above-fullscreen, per-keyboard, vertical main axis, carousel overview, multi-action binds, toggle-dwt | biri | **biri** | Freshest base (2 commits behind), clean thematic commits. |
| KDE blur protocol | kde-niri, niri-elite | **kde-niri** (review niri-elite's `src/protocols/kde_blur.rs`) | Upstream has `ext-background-effect-v1`, not `org_kde_kwin_blur`. |
| KDE screenshot / colour picker / output order | kde-niri | **kde-niri** | Only implementation; verify against current niri D-Bus APIs before porting. |
| Window groups | upstream tabbed columns, kde-niri | **upstream** + optional Naxdy group refinements | Upstream already has tabbed columns; do not reimplement groups. |
| Blur | upstream, kde-niri extra modes | **upstream** + evaluate Naxdy extras | Upstream already has passes/noise/saturation/xray. |
| DPMS frame callbacks | demoji, upstream | **upstream** | Already fixed upstream; porting would duplicate. |
| DPMS memory cleanup | RiversJin/niri PR #4550 (`4da66421`), demoji PR #4750 | **backport #4550** | #3295/#4750 remain open; #4550 is diagnosis-complete and user-confirmed. Reclaim dead imports on the 1 s timer; tests in `src/tests/renderer_cleanup.rs`. |
| Native sticky vs loom-nsticky | biri, loom-nsticky | **biri native** + nsticky compatibility mode | Native sticky is compositor-level and follows workspaces correctly; nsticky keeps staging/scratchpads, its unique value. |

## 5. Smithay decision, in detail

Current loom-smithay is exactly upstream Smithay at niri's pinned revision.
That is the correct starting point, but it cannot support HDR, Vulkan or tearing.

Candidate comparison, measured locally:

| Tree | Base | Commits ahead | Files / lines | Unique features |
| --- | --- | --- | --- | --- |
| MEKCCK `66b0670` | upstream `8a270232` (2026-10-14) | 16 | 30 files, +4,457/−109 | async flips, tearing, seat filter, colour management (rebased), connector colour state, GLES colour override |
| losnoco `dba9452b` | upstream `79bbed5e` (2026-09-22) | 50 | 57 files, +14,194/−303 | everything above except seat filter, plus Vulkan renderer, DRM colourop, SurfaceBarrier, commit-timing, tone mapping |
| merge prototype | upstream `a38bb709` (2026-11-19) | — | 3 conflicts | `CHANGELOG.md`, `anvil/src/udev.rs`, `src/wayland/shell/xdg/decoration.rs` |

`smithay-drm-extras` is untouched by both forks, so a single Smithay revision can
pair with upstream `smithay-drm-extras`.

Decision: loom-smithay will be rebuilt as upstream `master` + reviewed,
thematically grouped commits from the losnoco lineage, then MEKCCK's
`seat: allow filtering which clients see a seat global` (needed by the agent
seat), then any losnoco-to-newer-upstream adaptation that MEKCCK already solved
(e.g. surface-cache API changes). Each group is a separate commit so it can be
dropped when upstream lands an equivalent.

Rejected alternatives:

- Keep two Smithay dependencies (one for HDR, one for Vulkan): violates the
  single-graphics-stack rule, causes duplicate protocol globals and type
  incompatibility.
- Take MEKCCK wholesale and port Vulkan separately: Vulkan is ~14k lines and its
  colour pipeline is coupled to losnoco's DRM colourop code.
- Merge losnoco wholesale and stop: leaves 84 upstream commits of drift and
  loses MEKCCK's seat filter.

## 6. Proposed modular architecture (summary)

Full detail in [`ARCHITECTURE.md`](ARCHITECTURE.md).

- **No dynamic `.so` plugins.** Rust has no stable ABI; `State`, renderer
  traits, calloop and Wayland types are deeply intertwined. A plugin boundary
  would require either `unsafe` transmutes or a serialized RPC layer in the
  render loop. Both are worse than small core patches.
- **One binary, feature modules.** New behaviour lives in dedicated modules in
  the existing domain directories (`src/handlers/`, `src/protocols/`,
  `src/render_helpers/`, `src/layout/`, `src/input/`, `src/ui/`, plus
  `niri-config/src/` and `niri-ipc/src/`). Core files get short call sites, not
  moved code.
- **Cargo features only for heavy optional backends**: `vulkan` (Smithay Vulkan
  renderer), `agent`, portal backend, SVG cursor decoding. Never gate normal
  window-management features behind features that distributions may forget.
- **No new crates initially.** `niri-config` and `niri-ipc` already provide the
  reusable boundaries. Extract a shader crate only if a second consumer appears.
- **Internal seams**: `RenderTarget` already distinguishes Output / Screencast /
  ScreenCapture; `handlers/*` and `protocols/*` are natural protocol seams;
  `niri-config` uses per-domain modules; IPC actions are an enum. Use these
  instead of inventing a plugin API.
- **Upstream sync**: merge `upstream/main` into `main`; never rebase the main
  branch; keep feature commits topical; run `contrib/check-upstream-drift.sh`
  before and after each merge.

## 7. Niri files that will need modification

Grouped by risk. "New module" means the feature's logic should live in a new
file; the listed core files only get registration/hook edits.

| Area | Core files touched | Risk |
| --- | --- | --- |
| Smithay pin / renderer selection | `Cargo.toml`, `Cargo.lock`, `src/backend/tty.rs`, `src/backend/mod.rs`, `src/render_helpers/renderer.rs` | high |
| HDR / colour management | `src/backend/tty.rs`, `src/niri.rs`, `src/render_helpers/*`, `src/handlers/*`, `niri-config/src/output.rs`, new `render_helpers/colorimetry.rs` | high |
| Vulkan renderer | `src/backend/tty.rs`, `src/backend/winit.rs`, `src/backend/headless.rs`, `src/render_helpers/*`, `src/screencasting/*` | high |
| Tearing / FIFO / syncobj / pointer warp | `src/niri.rs`, `src/backend/tty.rs`, `src/handlers/*`, `src/protocols/mod.rs`, `niri-config/src/*` | medium |
| Commit-timing / SurfaceBarrier | `src/utils/transaction.rs`, `src/handlers/*`, `src/niri.rs` | high |
| Minimize | `src/layout/{tile,workspace,monitor,scrolling}.rs`, `src/window/*`, `src/input/mod.rs`, `src/ui/*`, `niri-ipc` | high |
| Grid overview | `src/layout/grid_overview*`, `src/layout/*`, `src/niri.rs`, `src/input/mod.rs` | high |
| Hidden workspaces | `src/layout/{workspace,monitor,scrolling}.rs`, `src/layout/mod.rs` | high |
| Virtual / isolated outputs | `src/backend/{tty,headless,virtual_output}.rs`, `src/layout/monitor.rs`, `src/niri.rs` | high |
| Sticky / float-above-fullscreen | `src/layout/{floating,workspace,monitor}.rs`, `src/window/*` | medium |
| Per-keyboard config | `src/input/mod.rs`, `niri-config/src/input.rs`, `src/niri.rs`, `niri-ipc` | medium |
| Vertical main axis | `src/layout/{axis,scrolling,monitor}.rs`, `src/input/mod.rs` | high |
| Shaders | `src/render_helpers/*`, `src/niri.rs`, `src/backend/tty.rs`, `niri-config/src/*` | medium |
| Liquid glass | `src/render_helpers/{background_effect,framebuffer_effect,xray}.rs`, shaders | low |
| Portals / KDE | `src/dbus/*`, `niri-ipc`, new portal modules | medium |
| Input binds (press/release, raw keysym) | `src/input/mod.rs`, `niri-config/src/binds.rs` | medium |

## 8. Upstream conflict estimate

Measured churn over the last 800 upstream commits:

| File | Commits | Consequence |
| --- | --- | --- |
| `src/niri.rs` | 132 | Every feature touches it; keep edits to short call sites. |
| `src/input/mod.rs` | 108 | Input features (per-keyboard, binds) will conflict; port last and re-express on upstream structure. |
| `niri-config/src/lib.rs` | 77 | Prefer new per-domain config modules re-exported from `lib.rs` over large inline edits. |
| `src/backend/tty.rs` | 61 | HDR/Vulkan/tearing all converge here; expect recurring conflicts and keep renderer backend code isolated. |
| `src/layout/mod.rs`, `scrolling.rs`, `monitor.rs`, `workspace.rs`, `tile.rs`, `floating.rs` | 58/50/33/28/27/16 | Layout features are the highest long-term conflict risk. Port biri's freshest commits and keep hooks minimal. |
| `niri-ipc/src/lib.rs` | 34 | Add actions in one place; expect trivial conflicts. |

Qualitative estimate after integration:

- Low: protocols, portal/D-Bus, UI overlays, shader helpers, SVG cursors.
- Medium: input/binds, screencasting/capture, config registration, IPC.
- High: HDR/Vulkan in `tty.rs`, minimize/grid/hidden-workspaces/vertical-axis in layout.

Mitigation: upstream-first merge policy, feature commits that touch disjoint
files where possible, tests around integration seams, and a documented list of
"expected conflict" hunks (see `UPSTREAM.md`).

## 9. Known technical problems to fix (not just features)

1. **HDR transparency and blur** — upstream blur assumes an sRGB-ish blend space;
   HDR needs a defined working space and consistent alpha handling. Must be
   designed with the colour pipeline, not patched after.
2. **HDR capture** — screenshots/screencasts must tone-map to SDR; niri-spicy and
   niri-elite both have pieces. Verify against ext-image-copy-capture and PipeWire.
3. **Vulkan format coverage** — DMABUF import, NV12, unsupported-format fallback,
   sync/fence handling, Wine/Proton paths. Must fail back to GLES per output.
4. **Direct scanout vs effects** — global shaders, liquid glass and blur can
   disable scanout. Keep effects gated; report damage per region; preserve
   scanout when inactive.
5. **DPMS/VRAM growth** — frame-callback leak is fixed upstream. The renderer
   cache cleanup from PR #4550 is backported and unit-tested; GTT profiling on
   a real DRM seat is still pending (see `TESTING.md`).
6. **Shaders × HDR × Vulkan** — three interacting spaces. Define one shader
   contract (working space, encode stage, feedback buffer format) and implement
   GLES first; Vulkan must match or be disabled with a clear warning.
7. **Multi-GPU / software EGL** — upstream rejects software EGL and has limited
   render-node selection; niri-elite has fixes. Test on hybrid laptops and VMs.
8. **Tearing + HDR** — async flips and colour pipelines interact in DRM atomic
   commits; per-plane format probing must include the colour path.
9. **Nvidia fence behaviour** — niri-spicy has explicit "wait for frame fences
   asynchronously" fixes; validate on Nvidia before claiming support.

## 10. Integration roadmap

Ordered by dependency and risk. Each step is its own branch/commit series with
tests and documentation.

0. **Repository hygiene** (done): upstream remotes, Smithay pin, drift script,
   audit docs, build verification.
1. **Smithay consolidation**: upstream + losnoco colour/Vulkan/async/tearing/
   barrier/commit-timing + MEKCCK seat filter; compile both GLES and Vulkan;
   document per-feature commits. This blocks everything graphics-related.
2. **HDR / colour management** (niri-spicy): protocol, connector state, 10-bit
   scanout, SDR-into-HDR blending, tone mapping, HDR-safe capture; GLES first.
3. **Presentation baseline** (urayde): tearing, FIFO, syncobj, pointer warp,
   async flips; reconcile with HDR scanout.
4. **Vulkan renderer** (niri-spicy): optional, per-output fallback to GLES,
   covers niri's elements, shaders, capture.
5. **Capture / screencasting**: ext-image-copy-capture status is upstream;
   add HDR→SDR, window capture verification, built-in portal backend + picker
   (niri-elite), SHM fallback verification.
6. **Layout and window-management** (biri): hidden workspaces, sticky,
   virtual outputs, isolated outputs, float-above-fullscreen, per-keyboard,
   vertical axis, multi-action binds, carousel.
7. **Shaders** (biri): global/region/window/output/focus-ring, multipass,
   feedback, redraw scheduling, capture gate; integrate with HDR working space.
8. **Desktop UX** (niri-elite/SHORiN): grid, minimize, magnifier, agent seat,
   hot-corner actions, SVG cursors, shake-to-enlarge, liquid glass.
9. **KDE** (Naxdy): `org_kde_kwin_blur`, KDE screenshot/colour picker, output
   order; group refinements only if still missing upstream.
10. **Privacy / stability**: `block-out-blur`, texture-cache/DPMS investigation,
    Nvidia fences, multi-GPU, suspend/resume.
11. **nsticky integration**: native sticky plus a compatibility mode in
    loom-nsticky (delegate sticky to the compositor, keep staging/scratchpads).
12. **Validation**: CI, headless visual tests, hardware matrix, docs updates.

## 11. Deliverable checklist

1. Feature table by project — `FEATURES.md`.
2. Upstream vs fork status — `FEATURES.md` status column.
3. Chosen implementation for duplicates — section 4 above and `INTEGRATIONS.md`.
4. URLs, branches, commits — `INTEGRATIONS.md`.
5. Smithay/Niri dependencies — `COMPATIBILITY.md`.
6. Modular architecture proposal — `ARCHITECTURE.md`.
7. Niri files needing modification — section 7 above.
8. Conflict estimate — section 8 and `UPSTREAM.md`.
9. Technical problems — section 9.
10. Dependency-ordered roadmap — section 10.
