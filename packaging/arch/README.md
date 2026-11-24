# Arch / CachyOS packaging

`PKGBUILD` is a split package base (`niri-better`) that produces one package per
component. A single source checkout and a single `build()` serve all of them:

| Package | Contents |
| --- | --- |
| `niri-better` | compositor, `niri-session`, session files, `niri.portal`, completions, docs |
| `better-portal` | `better-portal`, picker, `better.portal`, systemd/D-Bus units, example portals.conf |
| `better-companion` | `better-companion`, `companionctl`, optional systemd user unit |
| `better-monitor` | `better-monitor` |
| `better-session` | `better-session` |
| `better-shot` | `better-shot` |
| `better-wallpaper` | `better-wallpaper` |
| `better-settings` | `better-settings`, `better-settingsctl`, desktop file |
| `better-audio` | `better-audio` |
| `better-sidepanels` | `better-sidepanels` |
| `better-desktop` | meta package depending on every package above |

Build the whole set:

```sh
cd packaging/arch
makepkg -s
```

Install only what you need (the whole set is built either way, but installing a
subset packages only those files):

```sh
makepkg -si --pkg niri-better
makepkg -si --pkg better-portal better-settings
```

`better-settings.desktop`, `better-desktop.README.md` and
`better-desktop.install` are local recipe files; keep them next to `PKGBUILD`
when copying the recipe elsewhere. Regenerate `.SRCINFO` with
`packaging/scripts/update-srcinfo.sh` after any metadata change.
