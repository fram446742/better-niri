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

## Rejected and deferred ideas, in detail

A one-line status is not enough to know whether a decision should be revisited.
Each entry below explains what the project does, why it is not integrated, what
would make us reconsider, and what we use instead.

### niri-sidebar (Vigintillionn)

A single floating window stack at the screen edge. `niri-sidepanels` is its
direct successor with two independent panels, per-panel widths, peek behaviour
and managed struts, and we ported that successor in full as
`better-sidepanels-stack`. Keeping both would duplicate the same window-stack
state and fight over the same windows.

*Reconsider if* niri-sidepanels stops being maintained and a sidebar-specific
feature has no equivalent there. *Use instead:* better-sidepanels-stack.

### miri (MintyDoggo)

Alternative layouts such as master/stack. They replace the scrolling tape's
placement model. Integrating them would either fork the layout engine or add a
second placement mode with different IPC/config semantics, and neither niri's
actions nor our grid/carousel assume a master area.

*Reconsider if* upstream niri adopts pluggable layouts. *Use instead:* the
existing scrolling layout plus named workspaces; no second engine.

### nfsm and oniri

`nfsm` applies external rules to fullscreen windows; `oniri` maximises a window
when it is alone. Both are policies expressible with niri window rules and
companion rules (`window-rule`, `open-maximized`, workspace/window rule steps).
Shipping daemons for them would create a second configuration source.

*Reconsider if* the rule language cannot express a concrete case; it would then
be added as a companion rule action, not a separate daemon.

### wl-freeze (Zerodya)

Suspends processes with SIGSTOP/SIGCONT. It is a process-management tool, not a
desktop-state feature. Running it from companion would require freeze policy,
scope selection and crash recovery that belong to a dedicated tool; the user can
run `wl-freeze` next to the session. *Reconsider if* we add a general process
policy module; even then it should be an explicit opt-in adapter, not core.

### system76-scheduler-niri (Kirottu)

Tunes process priorities from the focused application through System76
Scheduler. That is host policy, depends on a system daemon and affects every
process, not only Better Desktop windows. Integrating it would make companion
depend on a distro-specific service and could surprise users.

*Reconsider if* a generic priority-policy interface exists in the session;
until then it stays an external integration.

### Editor and activity tools

`niriSKL.nvim`, `vim-niri-nav` and `arbtt-capture-wl` target Neovim navigation
and activity logging. They do not own compositor state, and activity capture is
privacy-sensitive. A screen-time tracker is useful, and we implemented one in
companion, but editor navigation belongs to the editor plugins.

*Reconsider if* we add a documented activity API; the Neovim side would remain
an external plugin.

### niri-screenshot-tools

Sends captures to external annotators or servers. better-shot already owns the
capture, annotation and export pipeline; adding another router would split the
history and annotation model. The `--command`/hook escape hatch covers custom
targets. *Reconsider if* a service protocol needs first-class support; it would
be a better-shot output backend.

### utsushot

Captures at higher resolution by relaunching the target application on a
temporary high-resolution virtual output. This requires controlling the client's
lifecycle and mapping, which a screenshot tool must not do silently; it also
restarts the user's application. The compositor can create virtual outputs, and
a user can do this deliberately, but making it a transparent capture mode risks
data loss and unexpected window movement.

*Reconsider if* niri offers a per-window offscreen render API with a safe
opt-in. *Use instead:* normal capture at native resolution.

### hyprlax

Multi-layer parallax wallpapers driven by compositor scroll/pointer motion.
Our native wallpaper backend only receives workspace focus, and niri IPC exposes
no scroll offset; faking parallax from focus changes would look wrong and would
add per-frame compositing cost. Noctalia or an external renderer can own this
effect.

*Reconsider if* a stable motion signal is exposed to layer-shell clients.
*Use instead:* static scenes, cross-fades and native video.

### Shells and bars

DankMaterialShell, Exo, iNiR, Glimpse, delta-shell, dux-shell, gpui-shell,
desktop-shell, IgnisNiriShell, ignis, ironbar, ashell, bar-rs, vibepanel,
Waybar, i3bar-river, i3status-rust.

Noctalia is the chosen shell. Shipping or maintaining another shell would
duplicate launcher, lock, notification, wallpaper and bar code with a different
plugin API, and the user could not run two shells at once. Our integration
surface is the Noctalia plugin source plus a small independent sidepanel app for
users who do not run Noctalia. DMS remains unverified because its canonical URL
was not confirmed; IgnisNiriShell is abandoned and would only be historical
reference. *Reconsider if* Noctalia's API cannot express a required interaction;
the fix then belongs in a plugin, not a second shell.

### Bindings and bar modules

`niri-gtk`, `qml-niri`, Eww/waybar taskbar modules and window-button modules are
UI bindings for other shells. We use JSON IPC in our own tools and Noctalia's
API in the plugins; adding language bindings would create parallel maintenance
without a consumer. *Reconsider if* an external tool asks for a stable binding.

### DMS, kode54/niri

DMS was mentioned as an alternative shell but its canonical URL could not be
confirmed during the audit, so it is not a source. `kode54/niri` did not resolve
to a repository during the audit; the colour work attributed to that author is
in the losnoco/niri-smithay lineage, which we already use.
