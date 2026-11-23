# Integration provenance

This document records where every feature comes from, which implementation was
chosen, and how it is ported. It is the attribution and audit trail required for
a GPL-3.0-or-later combined work.

All hashes were verified in the local clones on 2026-11-23. Short hashes resolve
on GitHub; full history remains in the source repositories.

## 1. Source repositories

| Role | Repository | Branch | Audited HEAD | License |
| --- | --- | --- | --- | --- |
| Base | https://github.com/niri-wm/niri | `main` | `02ee0585` | GPL-3.0-or-later |
| Our compositor | https://github.com/fram446742/better-niri | `main` | `02ee0585` (start) | GPL-3.0-or-later |
| Our Smithay | https://github.com/fram446742/better-smithay | `master` | `a38bb709` (start) | MIT |
| UX/grid/agent/portal | https://github.com/MEKCCK/niri-elite | `main` | `31458c94` | GPL-3.0-or-later |
| Layout/shaders/wayland | https://github.com/barrulus/biri | `main` | `5b465e4c` | GPL-3.0-or-later |
| HDR/Vulkan/presentation | https://github.com/losnoco/niri | `spicy-main` | `1bfaa833` | GPL-3.0-or-later |
| HDR/Vulkan Smithay | https://github.com/losnoco/smithay | `spicy-master` | `dba9452b` | MIT |
| Colour rebase + seat filter | https://github.com/MEKCCK/smithay | `elite-color-management` @ `66b06706` | `66b06706` | MIT |
| Tearing/FIFO/syncobj | https://github.com/urayde/niri | `main` | `80e0f89` | GPL-3.0-or-later |
| Grid/magnifier/agent origin | https://github.com/SHORiN-KiWATA/niri | `main` | `4109fc7` | GPL-3.0-or-later |
| Blur/KDE/groups | https://github.com/Naxdy/niri | `main` | `b09a30b7` | GPL-3.0-or-later |
| DPMS/privacy | https://github.com/demoj1/niri | `main` | `3d4ba3e` | GPL-3.0-or-later |
| Liquid glass | https://github.com/zaroutt/Niri-glass | `main` | `72ee163` | GPL-3.0-or-later |
| Sticky/staged daemon | https://github.com/fram446742/better-nsticky | `main` | `f4f5bfd` | BSD-3-Clause |

Divergence measured against the newest common niri commit:

| Fork | Common base | Ahead | Behind current niri |
| --- | --- | --- | --- |
| niri-elite | `fcbc971c` (2026-10-27) | 317 | 92 |
| biri | `2fbc6f3b` (2026-11-21) | 379 | 2 |
| niri-spicy | `5f4469b6` (2026-09-22) | 67 | 171 |
| kde-niri | `54c7fdcd` (2025-11-25) | 148 | 540 |
| losnoco/smithay | `79bbed5e` (2026-09-22) | 50 | 84 |
| MEKCCK/smithay | `8a270232` (2026-10-14) | 16 | 50 |

## 2. Porting rules

- Use `git cherry-pick -x <commit>` where possible so the original hash is in the
  message. Resolve conflicts in a separate commit, never by reformatting.
- Preserve the original author. If we rewrite more than a trivial amount, add a
  `Co-authored-by`/adaptation note.
- Port the commit, not the fork. Do not merge an entire reference branch into
  `main`.
- Drop commits already upstream. This audit found several; see section 4.
- Keep protocol, config, IPC, tests and docs of a feature in the same series.
- If two sources implement the same feature, port one and record why; salvage
  individual fixes as separate commits referencing the alternative.

## 2b. Base decision update: niri-elite + MEKCCK Smithay

Per project direction, the functional base is now
[`MEKCCK/niri-elite`](https://github.com/MEKCCK/niri-elite) and
[`MEKCCK/smithay`](https://github.com/MEKCCK/smithay), not the niri-spicy line.
The other projects are added on top of that base.

- better-smithay: `integration/elite-base` @
  `2e8e9edbe8a89af91d6bf3369c379cff21af76ff`.
  First parent MEKCCK/smithay `66b0670` (stabilised colour management, connector
  colour state, async flips, tearing, seat filtering); second parent the
  losnoco lineage `dba9452b` for the Vulkan renderer, DRM colour pipelines,
  SurfaceBarrier and tone-mapping helpers. The two colour modules were
  otherwise identical apart from `ImageDescription::max_luminance`.
- better-niri: `integration/elite-base`, merge commit `ea02411a` of
  niri-elite `31458c94` (base `fcbc971c`) with current upstream `02ee0585`.
  Reconciliation commit `bbd46e08` restored elite layout semantics and adapted
  to upstream APIs.

The merge kept 22 conflicted files. Material resolutions:

| Area | Resolution |
| --- | --- |
| Move actions | upstream focus flag + elite grid `MoveTargets` |
| Binds | upstream `pass_through` + elite actions |
| Capture | upstream `CaptureBuffer`/damage tracker + elite HDR reference luminance |
| Native portal | `node_sink.deliver()` instead of the Mutter signal emitter |
| DPMS / frame callbacks | upstream guards kept; elite preview/minimise throttling added |
| Window states | upstream fullscreen/maximise + elite minimise actions |
| Tests | elite tests' auto-width harness restored; a separate default-width op serves upstream's preset-width test |

Verified on the branch: `cargo check`, 652 tests, clippy without errors, fmt.
No hardware validation yet.

This supersedes the earlier "niri-spicy first" plan in section 3 below. The
spicy/losnoco work remains valuable for Vulkan and the niri-side HDR/Vulkan
port; it is now layered on the elite base rather than being the base.

## 3. Feature provenance and chosen ports


### HDR, colour management, Vulkan, presentation — niri-spicy + losnoco/smithay

Chosen because it is the origin of the colour-management lineage and the only
public Vulkan renderer.

| Feature | Source commits (niri-spicy unless noted) |
| --- | --- |
| Initial HDR + colour-management protocol | `38fcc689`, `bb9821e2`, `69ea9493`, `35a6364c`, `ed43ea7e`, `f9399ed8` |
| Windows scRGB / BT.2100 / custom primaries / extended target | `f9eedb8a`, `ecf1569f`, `45da70e7` |
| HDR→SDR capture | `3c690d5f` |
| Tone mapping, peak luminance, metadata handling | `8398f816`, `34722b2a`, `598c5908`, `d4e02772` |
| PQ encode/LUT, cursor plane and scanout on HDR | `31b38918`, `96c7e4a7`, `fdf7d06b`, `d9131c33` |
| HDR-safe captures | `3c690d5f` |
| Vulkan renderer (niri side) | `bb0a1171`, `8ab5f02c`, `54c01f39`, `f013c629`, `413e1e67`, `232a09aa`, `144d110a`, `a75e6d72`, `b767fa98`, `d2cbca6e`, `8665ae26` |
| Nvidia fence handling | `1bfaa833` |
| Minimise (base) | `6784ec55`, `ef6921f9`, `6527e69f`, `3c465025` |
| Smithay colour/Vulkan/DRM/SurfaceBarrier | losnoco/smithay `c3b85c9e`, `29db5bce`, `db1c0719`, `fb41e9a4`, `9e4ef8ad`…`0147580a`, `f67c4539`…`556930ca`, `560cb473`…`8665ae26`, `a3e90fa5`, `650a3457`, `a6a4be6d`, `064e3066` |
| Colour rebase onto newer Smithay + seat filter | MEKCCK/smithay `66b06706`, `fb3300c2`, `1397aa4e`, `32e62b9d` |

Extra HDR refinements present only in niri-elite (salvage if still applicable
after the rebase):

- `9ad02a53` drop leftovers of the single 10-bit probe.
- `22202df2` resolve subsurfaces to their root for preferred descriptions.
- `1d8a6532` composite SDR into the blend space of always-HDR outputs.

Do **not** port niri-elite's HDR block wholesale: it duplicates the spicy lineage
and lacks the Vulkan/tone-mapping work.

### Tearing, FIFO, syncobj, pointer warp, async flips — urayde/niri

Chosen because it is the freshest implementation (2026-11-18), uses upstream
Smithay protocol modules, and is split into small commits.

| Feature | Commit |
| --- | --- |
| `linux-drm-syncobj-v1` | `c528aea` |
| `wp-fifo-v1` | `cf58e85` |
| `wp-tearing-control-v1` | `d7d6541` |
| `allow-tearing` window rule | `872528d` |
| Async flips (niri side) | `ec4e78e` |
| `wp_pointer_warp_v1` | `b429d30` |
| ext-image-copy cursor rework | `90d5c1b`, `2c0b311`, `5935130` |
| Block-out `kind="hide"` | `689c1e9` (already upstream as `689c1e91`) |
| Skip frame callbacks when monitors inactive | `78b7adf` (already upstream as `78b7adfe`) |

niri-spicy's equivalents (`2590c4aa`, `d3ce41b0`, `2abeb32d`, `f974615e`) are a
fallback reference if urayde's patches stop applying. Do not port both.

### SurfaceBarrier and commit-timing — niri-spicy

| Feature | Commit |
| --- | --- |
| `wp-commit-timing-v1` (niri side) | `7b32bd25` |
| SurfaceBarrier atomic transactions | `50b1be19` |
| Smithay barrier/delayed blockers | losnoco `f06a5999`, `0a12ae58`, `4a33ddb7`, `1e5b84d1`, `556930ca` |

Deferred until HDR/presentation is stable: this reworks transaction handling and
is the most invasive change in the spicy series. Upstream Smithay already has a
`commit_timing` module, so only niri-side wiring and SurfaceBarrier need care.

### Grid, minimisation UX, magnifier, agent seat, portal, SVG, shake — niri-elite

Chosen because it is the only rebased, integrated implementation. niri-elite's
history includes merges of SHORiN (`c3e9426d`, SHORiN `b3078ceb`; `1f1cfc29`,
SHORiN `c5c38173`) and upstream.

| Feature | niri-elite commits |
| --- | --- |
| Grid overview + tests | `e63afcdb`…`ddb6df28` (128 commits; core: `3c05f5c4`, `692e2495`, `93386e02`, `4065216b`, `914616a2`) |
| Minimise UX/grid integration | `1b1e5c99`…`b1a208dc`, plus spicy `6784ec55` base |
| Magnifier / pick-colour overlay | `35ca0003`…`c5c36de4` |
| Agent seat | `59c2d659`…`f06507ed` |
| Built-in portal + native cast picker | `8cef18e4`…`4745d5ba`, merge `1f1cfc29` |
| SVG cursors | `e9e03172`…`34b373eb` |
| Shake-to-enlarge | `1ee0b70f`…`2f22ee75` |
| Liquid glass (zaroutt port) | merge `cd2d387a`; cleanup `de179064` |
| Hot-corner actions | `9166dcb4` |
| Tearing/content-type (elite variant) | `9e3f4855`…`225aeb0b` (use urayde instead) |
| Input binds/press-release/raw keysyms | `54035d6c`…`64ef0610` |
| Vertical main axis | `52d84ca5`…`6a9239d9`, then reverted by `3e0a17e8` (use biri) |

### Layout, shaders, virtual/isolated outputs — biri

Chosen because it is only 2 commits behind current niri and split into thematic
commits. This is the lowest-risk large integration today.

| Feature | biri commits |
| --- | --- |
| Hidden workspaces (+ fixes) | `fd841547`, `0dea47aa`…`7fdbd5ec`, `f0bef03f`, `4440b906`, `472ff113` |
| Virtual outputs | `d6fdcb80`…`a150917f` (branch `origin/virtual-outputs`, merged into `main`; upstream PR #3800) |
| Isolated outputs | `6e590e51`…`aee66f3d` |
| Sticky floating windows | `fd841547`, `699e508b` |
| `float-above-fullscreen` | `fd841547` |
| Per-keyboard configuration | `7fdbd5ec`, `526e6d2b`, `d893c811`, `0b5cdcc1`, `e2909102`, `88152e2c` |
| Vertical main axis | `f20e14a7`…`0f3860a8` |
| Global shader | `f948f557`…`2c2591c0` |
| Scoped/region/window shaders | `31cc5af9`…`f0f0ce01` |
| Region/window/focus-ring shaders | `f4b5c4b9`…`0cb779d1` |
| Per-output shader presets | `fde00905`…`3774b4c9` |
| `toggle-dwt` | `54d1770c` |
| Carousel overview | `d6d75c40`…`87b37916` |
| Multi-action binds | `5dd439d5`…`7101252f` |
| Drag physics | `d0e0693c` |

Design documents worth carrying over (adapt names/paths):

- `docs/superpowers/specs/2026-06-18-global-shader-design.md`
- `docs/superpowers/specs/2026-06-22-global-shader-multipass-design.md`
- `docs/superpowers/specs/2026-06-22-global-shader-redraw-intelligence-design.md`
- `docs/superpowers/specs/2026-06-23-scoped-shaders-design.md`
- `docs/superpowers/specs/2026-06-23-shaders-in-capture-design.md`
- `docs/superpowers/specs/2026-07-20-consolidated-carousel-overview-design.md`
- `docs/superpowers/specs/2026-09-07-virtual-outputs-design.md`
- `docs/superpowers/specs/2026-09-17-vertical-layout.md`

### KDE — kde-niri (Naxdy)

Imported so far: `org_kde_kwin_blur` (via niri-elite) and
`kde_output_order_v1` (Naxdy `fffe1e88`, wired to `sorted_outputs`; adapted to
the elite base's existing blur/background-effect state).

Much of Naxdy's blur lineage is already upstream. Port only what is still
missing and current-API compatible.

| Feature | Commits |
| --- | --- |
| `org_kde_kwin_blur` | `69a9029a` and the blur series `99e8ceaf`…`5faa13eb` (port only the protocol handler if upstream blur already covers rendering) |
| KDE screenshot2 / Spectacle | `69a9029a`…`81e2f9e1` (`src/dbus/kwin_screenshot2.rs`) |
| KDE colour picker | `757007fc` (`src/dbus/kwin_colorpicker.rs`) |
| KDE output order | `fffe1e88` (`src/protocols/kde_output_order.rs`) |
| Window group refinements | `1b7b2b55`…`475c787c` (only if still missing after upstream tabs) |
| Blur improvements (true blur, region blur, fps cap) | `758ee16b`, `11b8cacd`, `733131b8`, `7f0b08a8` (evaluate; upstream may already cover) |

### Privacy and DPMS — demoji

| Item | Commit / status |
| --- | --- |
| `block-out-blur` window rule | `3d4ba3e` |
| Overview focus-ring/shadow overrides | `36f6868` |
| Frame callbacks while monitors off | `c5cba9b` — **already upstream**, do not port |
| Texture cache cleanup on timer (PR #4750) | open, not in `main`; investigate first |

### Liquid glass — zaroutt/Niri-glass

Only three files; port through niri-elite's rebased version:

- `src/render_helpers/liquid_glass.rs` (small wrapper)
- `src/render_helpers/shaders/clipped_surface.frag` (~493 lines)
- `src/render_helpers/shaders/mod.rs` (uniforms)
- plus `niri-config/src/appearance.rs` options and `background_effect`/`xray`
  integration in niri-elite merge `cd2d387a`.

Implement as an option of the existing background-effect pipeline. It is a
shader, not a second renderer.

## 4. Already upstream — do not re-port

Verified in niri `02ee0585`:

- `ext-image-copy-capture-v1` for outputs, windows and cursors (PR #4554).
- PipeWire SHM fallback (merged from SHORiN; niri-elite `21be84f6` notes it).
- KDE/`ext-background-effect`-style blur configuration and rendering (upstream
  uses `ext-background-effect-v1`, not `org_kde_kwin_blur`).
- DPMS frame-callback guard (`78b7adfe`, `bb38b7e1`).
- `toggle-touchpad` (PR #3316).
- `block-out-from kind="hide"` (`689c1e91`).
- Tabbed columns (window groups), VRR, hot corners (overview booleans),
  direct scanout, 10-bit scanout format list.
- `move-window-to-new-workspace-{below,above}`.

Several fork commits are therefore obsolete. Porting them would create duplicate
code and conflict with upstream on the next sync.

## 5. better-nsticky

- Repository: `fram446742/better-nsticky`, `main` @ `f4f5bfd`, BSD-3-Clause,
  originally a fork of `lonerOrz/nsticky`.
- Mechanism: a daemon connects to `$NIRI_SOCKET`, watches events, and moves
  pinned windows between workspaces; it exposes its own CLI socket and adds
  staging, scratchpads, rules, persistence and a selector.
- Chosen integration: keep it as an independent daemon for staging/scratchpads,
  and implement native sticky windows in better-niri (biri implementation).
  Then add a compatibility mode in nsticky:
  - If the compositor advertises native sticky support (new IPC capability),
    `nsticky sticky …` delegates to native actions instead of moving windows.
  - Staging/scratchpads remain nsticky-only until/if they become compositor
    features.
- Do not duplicate the move-based sticky mechanism in the compositor, and do not
  implement staging twice.

## 6. Attribution obligations

- Keep original copyright headers and authors in ported files.
- Use `cherry-pick -x`; mention the source repository and commit in the body.
- `docs/ATTRIBUTION.md` lists projects and authors; update it for every series.
- GPL-3.0-or-later is compatible with all compositor forks listed (all GPL-3.0).
  Smithay is MIT; better-smithay remains MIT and must keep its notices. better-nsticky is
  BSD-3-Clause; it is combined at runtime over IPC, not linked, and its notices
  stay in its repository.
