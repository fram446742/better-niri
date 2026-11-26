# Arch / CachyOS packaging

`PKGBUILD` is a split package base (`loom-niri`) that produces one package per
component. A single source checkout and a single `build()` serve all of them:

| Package | Contents |
| --- | --- |
| `loom-niri` | coexisting compositor, `loom-niri-session`, own XDG paths, `loom-niri.portal`, completions, docs |
| `loom-niri-replace` | compositor installed as `niri`; conflicts with `niri`/`niri-git` |
| `loom-portal` | `loom-portal`, picker, `loom.portal`, systemd/D-Bus units, example portals.conf |
| `loom-companion` | `loom-companion`, `loomctl`, optional systemd user unit |
| `loom-monitor` | output profiles and mirroring |
| `loom-session` | named work contexts |
| `loom-shot` | screenshots, annotations, pins |
| `loom-wallpaper` | scenes and HDR video wallpapers |
| `loom-settings` | `loom-settings`, `loom-settingsctl`, desktop file |
| `loom-audio` | PipeWire routing |
| `loom-sidepanels` | panels, side stacks and minimap |
| `loom-tui` | terminal dashboard |
| `loom-desktop` | meta package depending on the coexisting set; installs `loom` |

Build the whole set:

```sh
cd packaging/arch
makepkg -s
```

Install only what you need (the whole set is built either way):

```sh
makepkg -si --pkg loom-niri
makepkg -si --pkg loom-niri-replace
makepkg -si --pkg loom-portal loom-settings
```

`loom-settings.desktop`, `loom-desktop.README.md` and `loom-desktop.install`
are local recipe files; keep them next to `PKGBUILD` when copying the recipe.
Regenerate `.SRCINFO` with `packaging/scripts/update-srcinfo.sh`.
