<h1 align="center"><img alt="niri" src="https://github.com/user-attachments/assets/07d05cd0-d5dc-4a28-9a35-51bae8f119a0"></h1>
<p align="center">A scrollable-tiling Wayland compositor.</p>
<p align="center">
    <a href="https://matrix.to/#/#niri:matrix.org"><img alt="Matrix" src="https://img.shields.io/badge/matrix-%23niri-blue?logo=matrix"></a>
    <a href="https://github.com/niri-wm/niri/blob/main/LICENSE"><img alt="GitHub License" src="https://img.shields.io/github/license/niri-wm/niri"></a>
    <a href="https://github.com/niri-wm/niri/releases"><img alt="GitHub Release" src="https://img.shields.io/github/v/release/niri-wm/niri?logo=github"></a>
</p>

<p align="center">
    <a href="https://niri-wm.github.io/niri/Getting-Started.html">Getting Started</a> | <a href="https://niri-wm.github.io/niri/Configuration%3A-Introduction.html">Configuration</a> | <a href="https://github.com/niri-wm/niri/discussions/325">Setup&nbsp;Showcase</a>
</p>

<img width="1280" height="720" alt="niri with a few windows open" src="https://github.com/user-attachments/assets/dea5909e-1859-4aaa-9d88-d37f9663e00b" />

## About

Windows are arranged in columns on an infinite strip going to the right.
Opening a new window never causes existing windows to resize.

Every monitor has its own separate window strip.
Windows can never "overflow" onto an adjacent monitor.

Workspaces are dynamic and arranged vertically.
Every monitor has an independent set of workspaces, and there's always one empty workspace present all the way down.

The workspace arrangement is preserved across disconnecting and connecting monitors where it makes sense.
When a monitor disconnects, its workspaces will move to another monitor, but upon reconnection they will move back to the original monitor.

## Loom

This tree is the Loom fork of niri. Alongside the compositor it ships
a set of small, independent tools that share niri's IPC and configuration
conventions: companion (automation, scratchpads, sticky workflow), session,
monitor, settings, shot, audio, wallpaper, sidepanels and the Noctalia plugin
source. They are maintained as separate workspaces and mirrored to their own
repositories, so each can be built, tested and released on its own:
[Loom](https://github.com/fram446742/loom),
[loom-companion](https://github.com/fram446742/loom-companion),
[loom-session](https://github.com/fram446742/loom-session),
[loom-monitor](https://github.com/fram446742/loom-monitor),
[loom-settings](https://github.com/fram446742/loom-settings),
[loom-shot](https://github.com/fram446742/loom-shot),
[loom-audio](https://github.com/fram446742/loom-audio),
[loom-wallpaper](https://github.com/fram446742/loom-wallpaper),
[loom-sidepanels](https://github.com/fram446742/loom-sidepanels),
[loom-portal](https://github.com/fram446742/loom-portal) and
[loom-noctalia-plugins](https://github.com/fram446742/loom-noctalia-plugins).

The compositor is built from niri and the work of its community: niri-elite,
biri, SHORiN, urayde, niri-spicy, Naxdy, demoji, Niri-glass and the corresponding
Smithay forks. The companion tools follow ideas from Piri, nsticky,
niri-float-sticky, niri-pip, niri-scratchpad, ndrop, niri-ror, niri-sidepanels,
nirinit, niri-session-manager, niri_display_manager, NiriMod, kanshi, PWSW,
iio-niri, stasis, niri-cwd, niri-screen-time, Nirisnap, niri-shot, hyprlax and
Noctalia. Documentation, provenance and the status of every reference project are in
`docs/ECOSYSTEM_RESEARCH.md` and `docs/ATTRIBUTION.md`. The research file also
holds the permanent record of rejected ideas: nothing is dropped without a
written reason and the conditions that would make us revisit it. Ported and
adapted reference tests are listed in `docs/REFERENCE_TESTS.md`; the HDR
pipeline and its remaining hardware checks are in `docs/HDR.md`.

## Features

- Built from the ground up for scrollable tiling
- [Dynamic workspaces](https://niri-wm.github.io/niri/Workspaces.html) like in GNOME
- An [Overview](https://github.com/user-attachments/assets/379a5d1f-acdb-4c11-b36c-e85fd91f0995) that zooms out workspaces and windows
- Built-in screenshot UI
- Monitor and window screencasting through xdg-desktop-portal-gnome
    - You can [block out](https://niri-wm.github.io/niri/Configuration%3A-Window-Rules.html#block-out-from) sensitive windows from screencasts
    - [Dynamic cast target](https://niri-wm.github.io/niri/Screencasting.html#dynamic-screencast-target) that can change what it shows on the go
- [Touchpad](https://github.com/niri-wm/niri/assets/1794388/946a910e-9bec-4cd1-a923-4a9421707515) and [mouse](https://github.com/niri-wm/niri/assets/1794388/8464e65d-4bf2-44fa-8c8e-5883355bd000) gestures
- Group windows into [tabs](https://niri-wm.github.io/niri/Tabs.html)
- Configurable layout: gaps, borders, struts, window sizes
- [Gradient borders](https://niri-wm.github.io/niri/Configuration%3A-Layout.html#gradients) with Oklab and Oklch support
- [Background blur](https://niri-wm.github.io/niri/Window-Effects.html) for windows and layer-shell surfaces
- [Animations](https://github.com/niri-wm/niri/assets/1794388/ce178da2-af9e-4c51-876f-8709c241d95e) with support for [custom shaders](https://github.com/niri-wm/niri/assets/1794388/27a238d6-0a22-4692-b794-30dc7a626fad)
- Live-reloading config
- Works with [screen readers](https://niri-wm.github.io/niri/Accessibility.html)

## Video Demo

https://github.com/niri-wm/niri/assets/1794388/bce834b0-f205-434e-a027-b373495f9729

Also check out these videos that showcase a lot of the niri functionality:

- [Niri Is My New Favorite Wayland Compositor](https://www.youtube.com/watch?v=DeYx2exm04M) by Brodie Robertson
- [How Is niri This Good? Live Demo + Config](https://www.youtube.com/watch?v=7XmD5UyyhZQ) by Nick Janetakis

## Status

Niri is stable for day-to-day use and does most things expected of a Wayland compositor.
Many people are daily-driving niri, and are happy to help in our [Matrix channel].

Give it a try!
Follow the instructions on the [Getting Started](https://niri-wm.github.io/niri/Getting-Started.html) page.
Grab a desktop shell like [Dux Modular Shell] or [Noctalia] (or build a more traditional setup): niri by itself is not a complete desktop environment.
Also check out [awesome-niri], a list of niri-related links and projects.

Here are some points you may have questions about:

- **Multi-monitor**: yes, a core part of the design from the very start. Mixed DPI works.
- **Fractional scaling**: yes, plus all niri UI stays pixel-perfect.
- **NVIDIA**: seems to work fine.
- **Floating windows**: yes, starting from niri 25.01.
- **Input devices**: niri supports tablets, touchpads, and touchscreens.
You can map the tablet to a specific monitor, or use [OpenTabletDriver].
We have touchpad gestures, but no touchscreen gestures yet.
- **Wlr protocols**: yes, we have most of the important ones like layer-shell, gamma-control, screencopy.
You can check on [wayland.app](https://wayland.app) at the bottom of each protocol's page.
- **Performance**: while I run niri on beefy machines, I try to stay conscious of performance.
I've seen someone use it fine on an Eee PC 900 from 2008, of all things.
- **Xwayland**: [integrated](https://niri-wm.github.io/niri/Xwayland.html#using-xwayland-satellite) via xwayland-satellite starting from niri 25.08.

## Media

[niri: Making a Wayland compositor in Rust](https://youtu.be/Kmz8ODolnDg?list=PLRdS-n5seLRqrmWDQY4KDqtRMfIwU0U3T) · *December 2024*

My talk from the 2024 Moscow RustCon about niri, and how I do randomized property testing and profiling, and measure input latency.
The talk is in Russian, but I prepared full English subtitles that you can find in YouTube's subtitle language selector.

[An interview with Ivan, the developer behind Niri](https://www.trommelspeicher.de/podcast/special_the_developer_behind_niri) · *June 2025*

An interview by a German tech podcast Das Triumvirat (in English).
We talk about niri development and history, and my experience building and maintaining niri.

[A tour of the niri scrolling-tiling Wayland compositor](https://lwn.net/Articles/1025866/) · *July 2025*

An LWN article with a nice overview and introduction to niri.

[How to test a Wayland compositor?](https://youtu.be/w44fYK5z418?list=PLK4NE4dSsLlw) · *October 2026*

My talk about some testing techniques used in niri: property-based tests, snapshot tests, client-server tests.
The first half overlaps with my RustCon talk, the second half is new.
The talk is in Russian, but I prepared full English subtitles that you can find in YouTube's subtitle language selector.

## Loom tools

The companion applications live as independent Cargo workspaces inside this
repository (`loom-companion/`, `loom-portal/`, ...).
Their sources still build and package on their own, but for development you can
run every tool through the `loom` command, which selects the right binary and
falls back to the tool's own workspace:

```sh
cargo run --bin loom -- --list
cargo run --bin loom -- shot screen --copy
cargo run --bin loom -- monitor list
cargo run --bin loom -- companion status
```

Installed packages put the same command in `/usr/bin/loom`; it runs the
individual `loom-*` binaries, so each tool remains usable on its own. The
direct shims are still available:

```sh
cargo run --bin loom-monitor -- --help
cargo run --bin loom-settings -- --version
cargo run --bin loomctl -- status
cargo run --bin loom-shot -- screen --copy
cargo run --bin loom-audio -- sinks
cargo run --bin loom-wallpaper -- status
cargo run --bin loom-session -- list
cargo run --bin loom-sidepanels -- --check
cargo run --bin loom-portal -- --version
```

Available bins: `loom`, `loom-companion`, `loomctl`, `loom-portal`,
`loom-portal-picker`, `loom-monitor`, `loom-session`, `loom-shot`,
`loom-wallpaper`, `loom-settings`, `loom-settingsctl`, `loom-audio`,
`loom-sidepanels`. Each shim forwards its arguments and exit status to the
tool's own `cargo run`, so the separate lockfiles and packaging stay intact.
Use the workspace directly (`cd loom-monitor && cargo run -- …`) when you want
a fully isolated build.

## Contributing

If you'd like to help with niri, there are plenty of both coding- and non-coding-related ways to do so.
See [CONTRIBUTING.md](https://github.com/niri-wm/niri/blob/main/CONTRIBUTING.md) for an overview.

## Inspiration

Niri is heavily inspired by [PaperWM] which implements scrollable tiling on top of GNOME Shell.

One of the reasons that prompted me to try writing my own compositor is being able to properly separate the monitors.
Being a GNOME Shell extension, PaperWM has to work against Shell's global window coordinate space to prevent windows from overflowing.

## Tile Scrollably Elsewhere

Here are some other projects which implement a similar workflow:

- [PaperWM]: scrollable tiling on top of GNOME Shell.
- [karousel]: scrollable tiling on top of KDE.
- [scroll](https://github.com/dawsers/scroll) and [papersway]: scrollable tiling on top of sway/i3.
- Hyprland has a built-in [scrolling layout](https://wiki.hypr.land/Configuring/Layouts/Scrolling-Layout/).
- [Paneru] and [PaperWM.spoon]: scrollable tiling on top of macOS.

## Contact

Our main communication channel is a Matrix chat, feel free to join and ask a question: https://matrix.to/#/#niri:matrix.org

We also have a community Discord server: https://discord.gg/vT8Sfjy7sx

[PaperWM]: https://github.com/paperwm/PaperWM
[waybar]: https://github.com/Alexays/Waybar
[fuzzel]: https://codeberg.org/dnkl/fuzzel
[awesome-niri]: https://github.com/niri-wm/awesome-niri
[karousel]: https://github.com/peterfajdiga/karousel
[papersway]: https://spwhitton.name/tech/code/papersway/
[Paneru]: https://github.com/karinushka/paneru
[PaperWM.spoon]: https://github.com/mogenson/PaperWM.spoon
[Matrix channel]: https://matrix.to/#/#niri:matrix.org
[OpenTabletDriver]: https://opentabletdriver.net/
[Dux Modular Shell]: https://duxlabs.dev/
[Noctalia]: https://noctalia.dev/
