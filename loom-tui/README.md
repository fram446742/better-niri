# loom-tui

> Part of [Loom](https://github.com/fram446742/loom).

A terminal dashboard for the Loom desktop. It reads the shared
[`loom-state`](https://github.com/fram446742/loom-niri/tree/main/loom-state) event cache, so it does not poll niri for window
or workspace state.

```sh
loom tui
# or
loom-tui
```

Tabs: **Overview**, **Windows**, **Outputs**. `Tab`/`Shift+Tab` switch tabs,
`j`/`k` move, `Enter` focuses the selected window, `q` quits. The footer shows
whether the compositor connection is live.

The dashboard is read-only except for the focus action; it does not change
layout, outputs or settings. It is intentionally small: monitor profiles,
sessions and audio remain in their own CLIs.
