# Arch / CachyOS packaging

`PKGBUILD` is a split package base (`niri-better`) that produces one package per
component. A single source checkout and a single `build()` serve all of them:

| Package | Contents |
| --- | --- |
| `niri-better` | compositor, `niri-session`, session files, `niri.portal`, completions, docs |
| `loom-portal` | `loom-portal`, picker, `loom.portal`, systemd/D-Bus units, example portals.conf |
| `loom-companion` | `loom-companion`, `loomctl`, optional systemd user unit |
| `loom-monitor` | `loom-monitor` |
| `loom-session` | `loom-session` |
| `loom-shot` | `loom-shot` |
| `loom-wallpaper` | `loom-wallpaper` |
| `loom-settings` | `loom-settings`, `loom-settingsctl`, desktop file |
| `loom-audio` | `loom-audio` |
| `loom-sidepanels` | `loom-sidepanels` |
| `loom-desktop` | meta package depending on every package above; installs `loom` |

Build the whole set:

```sh
cd packaging/arch
makepkg -s
```

Install only what you need (the whole set is built either way, but installing a
subset packages only those files):

```sh
makepkg -si --pkg niri-better
makepkg -si --pkg loom-portal loom-settings
```

`loom-settings.desktop`, `loom-desktop.README.md` and
`loom-desktop.install` are local recipe files; keep them next to `PKGBUILD`
when copying the recipe elsewhere. Regenerate `.SRCINFO` with
`packaging/scripts/update-srcinfo.sh` after any metadata change.
