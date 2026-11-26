# Better Desktop — Noctalia plugins

Luau plugins that expose the Better Desktop companions in the Noctalia bar. They
are thin presentation layers: every action runs the companion CLI with an
argument array (no shell parsing), and every read uses its JSON output.

This directory is a Noctalia **plugin source**. It is not a Cargo workspace and
does not add a runtime dependency to the compositor or the companions.

## Plugins

| Plugin | Bar widget | Left click | Right click | Scroll |
| --- | --- | --- | --- | --- |
| `better-companion` | daemon reachable via `companionctl status` | open the action registry in a terminal | open better-settings | — |
| `better-sticky` | staging/scratchpad | `stage toggle` on the focused window | toggle the configured scratchpad slot | — |
| `better-monitor` | number of saved profiles | apply the next profile | apply `default` | cycle profiles |
| `better-session` | selected context | restore the selected context | list contexts in a terminal | change selection |
| `better-shot` | — | interactive screenshot UI | capture the focused output to the clipboard | — |
| `better-audio` | default sink | cycle default sink | re-apply routing rules | cycle default sink |
| `better-wallpaper` | active scene | reapply the active scene | print the scene mapping in a terminal | cycle scenes |
| `better-sidepanels` | — | validate the config | send `SIGHUP` to reload | reload |
| `better-screentime` | total tracked time | open the per-app ranking | companion command reference | — |

Each plugin declares `plugin_api = 24` (argument-array `noctalia.runAsync`), so
it needs a Noctalia release that supports API 24 or newer. The widgets degrade
visibly (`companion?`, `display?`, ...) when the CLI is not installed, and emit
an error notification when a command fails.

## Install

1. In Noctalia: **Settings → Plugins → Add source**, choose **path**, and point
   it at this directory (`better-noctalia-plugins/`).
2. Enable the plugins you want; each one is independent and can be removed
   without affecting the others.
3. Add the widgets to a bar through the normal Noctalia bar configuration.

Packaging installs the same tree under
`/usr/share/better-desktop/noctalia-plugins` so it can be added as a source
without cloning the repository.

## Scripted interaction

Every widget accepts events through Noctalia's plugin IPC, which is useful for
keybinds, tests and automation:

```sh
noctalia msg plugin better-desktop/better-companion:status focused refresh
noctalia msg plugin better-desktop/better-companion:status focused click
noctalia msg plugin better-desktop/better-companion:status focused right-click
```

`refresh` reruns the widget's data read, `click` invokes the same handler as a
left click, and `right-click` the secondary action. Use `all` instead of
`focused` for provider entries; bar widgets are addressed by their output.

## Requirements

- Noctalia with plugin API 24+.
- The matching companion binary on `PATH` (`companionctl`, `better-monitor`,
  `better-session`, `better-shot`, `better-audio`, `better-wallpaper`,
  `better-sidepanels`).
- A running niri/better-niri session for the widgets that use niri IPC.

## Security

- Commands are passed as argument arrays where Noctalia supports it; no user
  input or window title is interpolated into a shell string.
- The plugins only call the documented CLIs. They do not open sockets, read
  files, or run arbitrary commands.
- `better-sticky` uses a declared string setting for the scratchpad slot; it is
  passed as a JSON argument, not shell-quoted text.

## Validation

Run `./verify.sh` to check the manifests and parse every Luau entry with the
local `lua5.4` (syntax only; Luau runtime globals are not available outside
Noctalia). The script also asserts that each entry referenced by a manifest
exists and that each plugin id is unique.

No live Noctalia instance is available in this repository's test environment,
so the widgets are syntax- and manifest-checked but not exercised against the
shell. That remains a manual check together with the hardware validation pass.
