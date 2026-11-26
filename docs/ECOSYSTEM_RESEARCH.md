# Better Desktop ecosystem research

This is the working map of the reference projects and what happened to their
ideas. Status values: `integrated` (code or behaviour lives in this tree),
`candidate` (worth adding), `rejected` (deliberately not used; reason given),
`reference` (used for audit only).

## Compositor and graphics

| Project | Status | Where / reason |
| --- | --- | --- |
| niri-wm/niri | integrated | Base upstream. |
| MEKCCK/niri-elite | integrated | Merged feature base (grid, magnifier, portal, HDR, cursors, glass). |
| barrulus/biri | integrated | Shaders, outputs, hidden/sticky, vertical axis, per-keyboard, carousel. |
| SHORiN-KiWATA/niri | integrated | Through niri-elite. |
| urayde/niri | integrated | Tearing, FIFO, syncobj, pointer warp, async flips. |
| dividebysandwich/niri, losnoco/niri, kakxem/niri | integrated | HDR/colour lineage in the elite base and niri-spicy ports. |
| Naxdy/niri | integrated / candidate | KDE interop and blur integrated; window groups and layer fades remain candidates. |
| demoj1/niri | integrated | Block-out blur, DPMS fixes, overview overrides, cache cleanup. |
| zaroutt/Niri-glass | integrated | Liquid glass through niri-elite. |
| Smithay/smithay | integrated | Upstream infrastructure. |
| MEKCCK/smithay | integrated | First parent of better-smithay. |
| losnoco/smithay | integrated | Second parent (Vulkan, colour pipelines, barriers). |
| urayde/smithay, dividebysandwich/smithay, reverb256/smithay | reference | Audited; the useful parts are already in the two parents. |

## Automation and window management

| Project | Status | Where / reason |
| --- | --- | --- |
| Asthestarsfalll/piri | integrated (behaviour) | Compiled-plugin ideas, rules, run-or-raise, swallow, marks are in better-companion; no Piri code is vendored. |
| nsticky | integrated | Workflow absorbed; native sticky in the compositor and compatibility aliases in companion. |
| niri-float-sticky, niri-pip | integrated (behaviour) | Native sticky windows cover the use case; no second daemon. |
| niri-scratchpad, niri-scratchpad-rs, ndrop | integrated | `scratchpad.*` in better-companion. |
| niri-ror | integrated | `windows.run-or-raise`. |
| niri-sidebar | rejected | Superseded by the full niri-sidepanels port. |
| zgavin/niri-sidepanels | integrated | `better-sidepanels-stack`, full port. |
| niri-companion (Python) | reference | Overlaps companion profiles/sessions; not adopted to avoid two automation daemons. |
| nirimap | integrated (behaviour) | The sidepanel `taskbar` module lists windows and focuses them; a graphical minimap remains out of scope. |
| miri | rejected | Alternative layout engines conflict with niri’s scrollable model. |
| nfsm | rejected | Fullscreen behaviour is covered by window rules. |
| oniri | rejected | Single-window maximize is expressible with existing rules. |
| wl-freeze | rejected | External process freezer; unrelated to compositor state. |
| niri-empty, niri-scripts, niri_tweaks, Nirius | reference | Ideas covered by rules/automation; no code imported. |

## Sessions

| Project | Status | Where / reason |
| --- | --- | --- |
| amaanq/nirinit | integrated (behaviour) | Workspace/window snapshots and launch mapping in better-session. |
| MTeaHead/niri-session-manager | integrated | Periodic/auto snapshots, pruning and restore warnings. |
| swaytreesave | reference | Layout-tree idea only; niri has no sway tree. |

## Monitors and settings

| Project | Status | Where / reason |
| --- | --- | --- |
| srinivasr/nirimod | integrated (idea) | KDL-preserving editor in better-settings. |
| niri_display_manager | integrated | Canvas/drag editor with profiles and rollback. |
| nirimon | reference | TUI idea; CLI/JSON covers it. |
| nirilayout | integrated | Profiles. |
| kanshi | integrated (behaviour) | Watch/rules apply profiles on hotplug. |

## Audio, sensors and system utilities

| Project | Status | Where / reason |
| --- | --- | --- |
| ledati16/pwsw | integrated | PipeWire routing rules in better-audio. |
| mikkurogue/kunai | reference | Per-keyboard layouts already handled by the compositor. |
| Zhaith-Izaliel/iio-niri | integrated | `rotate.*` in companion. |
| saltnpepper97/stasis | integrated | `idle.*` inhibitors in companion. |
| jfreed-dev/niri-screensaver | rejected | Shell/Noctalia responsibility. |
| system76-scheduler-niri | rejected | External scheduler policy; not desktop state. |
| nouritsu/niri-cwd | integrated | `windows.cwd` in better-companion. |
| probeldev/niri-screen-time | integrated | `screentime.*` in better-companion with a Noctalia widget. |
| niriSKL.nvim, vim-niri-nav, arbtt-capture-wl | rejected | Editor/activity tools outside the desktop tooling scope. |

## Capture

| Project | Status | Where / reason |
| --- | --- | --- |
| niri-shot, Nirisnap | integrated | Capture, annotations, OCR, redaction, pins, history merged in better-shot. |
| utsushot | rejected | Requires relaunching the client on a temporary output; a capture tool must not control application rendering. |
| niri-screenshot-tools | rejected | External annotation pipeline; better-shot has its own. |

## Wallpapers and shaders

| Project | Status | Where / reason |
| --- | --- | --- |
| Noctalia wallpaper | reference | Rendering can stay in the shell; better-wallpaper owns scenes. |
| hyprlax | rejected | niri IPC exposes no scroll position to drive real parallax; a fake offset would be misleading. |
| Pandora | reference | Dynamic wallpaper daemon idea. |
| YangYuS8/lwe | reference | Video/scene catalogue; native video is in better-wallpaper. |
| nirimation, liixini/shaders | reference | Shader collections for the compositor shader engine. |

## Shells, bars and bindings

Noctalia is the selected shell. Other shells and bars (DMS, Exo, iNiR, Glimpse,
delta-shell, dux-shell, gpui-shell, desktop-shell, IgnisNiriShell, ignis,
ironbar, ashell, bar-rs, vibepanel, Waybar, i3bar-river, i3status-rust) are
`reference` only: we do not ship a second shell or bar. Niri Waybar modules and
Eww/QML/GTK bindings are `rejected` as separate maintenance surfaces; the
Noctalia plugin source is our integration layer.

## Noctalia

| Project | Status |
| --- | --- |
| noctalia-dev/noctalia | integrated (plugin target) |
| official-plugins, community-plugins | reference |
| better-noctalia-plugins | integrated (in-repo plugin source) |
