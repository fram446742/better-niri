# Attribution

better-niri is a combined work. This file records the projects, authors and
licences behind the code. It is updated whenever a feature series is integrated.

## Upstream foundations

- **niri** — https://github.com/niri-wm/niri — Ivan Molodetskikh (YaLTeR) and
  contributors. GPL-3.0-or-later. The compositor, its architecture, protocols,
  configuration, IPC and the overwhelming majority of the code.
- **Smithay** — https://github.com/Smithay/smithay — Victor Berger, Victoria
  Brekenfeld and contributors. MIT. Wayland server infrastructure, DRM/KMS,
  renderers, input and protocol implementations.
- **PaperWM** — https://github.com/PaperWM/PaperWM — the scrollable-tiling idea
  that inspired niri.

## Feature sources

| Project | Maintainer / authors | Licence | Contributed to better-niri |
| --- | --- | --- | --- |
| https://github.com/MEKCCK/niri-elite | MEKCCK and contributors | GPL-3.0-or-later | Integration strategy reference, grid overview, minimisation UX, magnifier, agent seat, portal backend, SVG cursors, hot-corner actions, input binds, liquid-glass port |
| https://github.com/SHORiN-KiWATA/niri | SHORiN-KiWATA | GPL-3.0-or-later | Origin of grid overview, magnifier, minimisation, agent seat, portal backend, SVG cursors, shake-to-enlarge |
| https://github.com/barrulus/biri | barrulus and contributors | GPL-3.0-or-later | Hidden workspaces, virtual/isolated outputs, sticky floating windows, float-above-fullscreen, per-keyboard configuration, vertical main axis, multi-action binds, shader pipeline, focus-ring shaders, carousel overview, drag physics |
| https://github.com/urayde/niri | urayde and contributors | GPL-3.0-or-later | Tearing control, FIFO, DRM syncobj, pointer warp, async page flips |
| https://github.com/urayde/smithay | urayde and contributors | MIT | DRM async page-flip groundwork |
| https://github.com/losnoco/niri | Christopher Snowhill (losnoco) and contributors | GPL-3.0-or-later | HDR, colour management, tone mapping, Vulkan renderer, minimize base, presentation fixes |
| https://github.com/losnoco/smithay | Christopher Snowhill (losnoco) and contributors | MIT | Colour management, DRM colour pipelines, SurfaceBarrier, async flips, tearing, Vulkan renderer |
| https://github.com/MEKCCK/smithay | MEKCCK and contributors | MIT | Colour-management rebase onto newer Smithay, per-client seat filtering, GLES colour override |
| https://github.com/dividebysandwich/niri | dividebysandwich | GPL-3.0-or-later | HDR/colour-management work used by the losnoco/niri-elite lineage |
| https://github.com/Naxdy/niri | Naxdy and contributors | GPL-3.0-or-later | KDE blur protocol, KDE screenshot/colour-picker D-Bus, output order, window groups, layer fades, `org.kde.KWin.ScreenShot2`, blur improvements |
| niri-spicy (spicy-main) | niri-spicy contributors | GPL-3.0-or-later | HDR peak-luminance override, tearing gate, suspended toplevels, NVIDIA fences, scanout colour transforms, post-blend PQ encode, HDR cursor plane |
| https://github.com/demoj1/niri | demoj1 | GPL-3.0-or-later | `block-out-blur` privacy rule, overview focus-ring/shadow overrides, DPMS/frame-callback investigation |
| https://github.com/zaroutt/Niri-glass | zaroutt | GPL-3.0-or-later | Liquid glass shader and parameters |
| https://github.com/reverb256/smithay | reverb256 | MIT | `hdr-modern` reference for HDR colour handling (audited) |
| https://github.com/AmmoniumX/niri-qol | AmmoniumX | GPL-3.0-or-later | Hidden-workspace and QoL work folded into biri |
| https://github.com/Asthestarsfalll/piri | Asthestarsfalll and contributors | MIT | Automation architecture reference (compiled plugins, rules, run-or-raise, swallow); better-companion is an in-tree implementation and vendors no Piri code |
| https://github.com/lonerOrz/nsticky | lonerOrz | BSD-3-Clause | Sticky-window daemon that better-nsticky extends |
| better-nsticky | fram446742 and contributors | BSD-3-Clause | Staging, scratchpads, rules, persistence, selector, compatibility integration |
| https://github.com/zgavin/niri-sidepanels | zgavin and contributors | MIT | Floating sidepanel stacks (`toggle-window`/`send`/`flip`/`focus`/`reorder`/struts), ported into `better-sidepanels/stack` |

The companion tools are original implementations. Their behaviour was informed
by these projects, without vendoring their code:

- Piri, niri-ror, niri-pip, niri-float-sticky, nsticky, niri-scratchpad,
  niri-scratchpad-rs and ndrop for automation, staging, scratchpads,
  run-or-raise, marks and swallowing.
- niri-sidepanels for the floating side stacks.
- nirinit, niri-session-manager and niri-companion for session capture and
  restore.
- NiriMod, niri_display_manager, nirimon, nirilayout and kanshi for settings,
  monitor profiles and hotplug application.
- PWSW for audio routing; iio-niri, stasis, niri-cwd and niri-screen-time for
  rotation, idle inhibition, working directory and usage tracking.
- Nirisnap, niri-shot and utsushot (considered) for capture, annotations, OCR,
  redaction and pins.
- hyprlax and Pandora (considered), plus Linux Wallpaper Engine for video
  scenes.
- Noctalia and its official/community plugins for the shell integration.

Individual contributors visible in the audited histories include (non-exhaustive):
alter, andriyDev, arthur-ba1, barrulus, bczhc, Carlos Ferrabone, cmeissl,
Christopher Snowhill, dividebysandwich, fætalize, greyxp1, ham, hecate cantus,
HigherOrderLogic, kakxem, kode54, LarsMans, LuckShiba, Makai, MEKCCK,
Michael Yang, Mițca Dumitru, pgaskin, shorinkiwata, Sertonix, trevarj, urayde,
willybarret, Yuzhou Nie, Zakaria Habri.

## Upstream pull requests referenced

| PR | Author | Status at audit | Relevance |
| --- | --- | --- | --- |
| niri #4554 `ext-image-copy-capture` | pgaskin | merged 2026-09-12 | Capture is upstream; do not reimplement |
| niri #3316 `toggle-touchpad` | barrulus | merged 2026-10-11 | Upstream |
| niri #2997 hidden workspaces | argosnothing | open | biri implementation |
| niri #3302 sticky floating windows | AtefR | closed unmerged | biri implementation |
| niri #3800 virtual outputs | willybarret | open | biri implementation |
| niri #4001 pointer warp | — | closed unmerged | urayde implementation |
| niri #4062 `float-above-fullscreen` | bczhc | open | biri implementation |
| niri #4459 per-keyboard config | arthur-ba1 | open | biri implementation |
| niri #4550 renderer cache cleanup | RiversJin | closed (policy), backported | Dead imports reclaimed on the fallback timer; `4da66421` |
| niri #4750 texture cache cleanup | trevarj | open | Same diagnosis; not adopted (superseded by the #4550 backport) |

Protocol and specification authors whose work underpins the colour and
presentation features include the Wayland colour-management, tearing-control,
FIFO, commit-timing and DRM syncobj authors and reviewers. Their protocol XML
and specification text are used under the licences shipped with
`wayland-protocols` and the Linux kernel DRM headers.

## Licence compatibility

| Component | Licence | Combined with GPL-3.0-or-later |
| --- | --- | --- |
| niri and forks | GPL-3.0-or-later | yes |
| Smithay / better-smithay | MIT | yes (MIT is GPL-compatible; keep notices) |
| better-nsticky | BSD-3-Clause | yes for the daemon; combined over IPC, not linked |
| Wayland protocol XML | MIT / HPND-style as shipped | yes, keep notices |
| Linux DRM headers | MIT/GPL-2.0 WITH Linux-syscall-note | used as intended |

Obligations:

- Keep copyright headers and licence files.
- Preserve original authorship in ported commits (`cherry-pick -x` plus a note).
- Do not relicense Smithay-derived files; they remain MIT with their notices.
- Publish better-niri under GPL-3.0-or-later and better-smithay under MIT.
