# Packaging and compatibility

How Loom Niri is packaged, which system versions it needs, and what is known
to work with it. The packaging tree is deliberately additive: it does not
change niri's source, workspace manifests or `Cargo.lock`.

- `packaging/arch/` — Arch/CachyOS split source package base (`loom-niri`)
  plus one package per companion and the `loom-desktop` meta package.
- `packaging/aur/` — AUR split recipes: `loom-niri` (pinned source) and
  `loom-niri-git` (tracks `main`); both produce the same
  `loom-niri`, `loom-*` and `loom-desktop` package names. The existing
  root `packaging/aur/PKGBUILD` is the older `niri-elite-git` recipe targeting
  the `main` branch; it is left untouched.
- `packaging/profiles/` — minimal, desktop and full installation profiles.
- `packaging/scripts/` — profile builds, manifest report and the packaging
  verifier.

## Compatibility matrix

| Distribution / channel | Status | Artifacts | Notes |
| --- | --- | --- | --- |
| Arch Linux | source package provided | `packaging/arch/PKGBUILD` | Split build: `loom-niri`, one package per companion and `loom-desktop`. Not in an official repository. |
| CachyOS | source package provided | same as Arch | CachyOS is Arch-compatible; `makepkg` works as-is. Use the tuned toolchain your system provides. |
| AUR | recipes provided, not published | `packaging/aur/loom-niri`, `packaging/aur/loom-niri-git` | Both are split recipes. Publish by copying the `PKGBUILD`, `.SRCINFO` and local recipe files into the AUR repository. |
| Fedora | unmaintained in this tree | `niri.spec.rpkg` | The upstream spec exists but is not validated against this branch's extra crates and portal files. |
| NixOS | compositor only | `flake.nix` | The flake builds niri, not the excluded companion workspaces. |
| Debian/Ubuntu | metadata only | `Cargo.toml` `[package.metadata.deb]` | No Debian source package is maintained here. |
| Other distributions | build from source | `packaging/profiles/` | Use the profile scripts or reproduce their `cargo` commands. |

Conflicts: the `loom-niri` package from every recipe installs
`/usr/bin/niri`, `/usr/bin/niri-session` and the same session files. It
conflicts with `niri`, `niri-git`, `niri-elite`, `niri-elite-git` and replaces
`niri`. Companion packages keep the same names in the pinned and `-git` recipes
(`loom-portal`, `loom-shot`, ...), so switching between them upgrades the
package instead of installing a duplicate.

### Required build versions

Values below come from the current manifests. Re-run
`packaging/scripts/manifest-report.sh` after dependency bumps.

| Requirement | Current value | Notes |
| --- | --- | --- |
| Rust | MSRV `1.95` | The checked-out toolchain in this environment is `1.102.0-nightly`; stable should work at the declared MSRV. |
| Cargo edition | 2024 | Requires a recent toolchain. |
| libclang | any recent LLVM/Clang | Needed by bindgen for PipeWire/SPA. Set `LIBCLANG_PATH=/usr/lib` if detection fails. |
| PipeWire | crate `0.10.1`, feature `v0_3_33` | Runtime server 0.3.33 or newer; Arch has 1.6.x. |
| GTK4 | crate `0.11` | Runtime 4.24.1 was used for the profile checks; `loom-settings` needs the GTK4 development files to build. |
| Wayland / libinput / libdrm / libgbm / libEGL | current Arch packages | See `packaging/arch/PKGBUILD` `depends` for the packaged set. |
| Vulkan loader + ICD | `vulkan-icd-loader` and a GPU driver | The pinned package builds the Vulkan renderer. Runtime use is opt-in. |
| D-Bus | session bus | Required by `niri`'s `dbus`/`systemd` features and `loom-portal`. |
| systemd | user manager | Needed for `niri.service`, `niri-session`, the optional `loom-portal.service` and `loom-companion.service` units and companion idle integration. |
| dinit | optional | Only the `full` profile enables niri's `dinit` feature. `resources/dinit/` is not installed by the packages yet. |

## Installation profiles

Profiles are TOML files describing the exact Cargo features for niri
(`--no-default-features` plus the listed set) and the companion components to
build. They do not depend on niri's `default` feature list, so a future default
change cannot silently alter a profile.

```sh
packaging/scripts/build-profile.sh --list
packaging/scripts/build-profile.sh --check minimal
packaging/scripts/build-profile.sh --build desktop
packaging/scripts/build-profile.sh --check --with-apps full
```

| Profile | Intended use | niri features | Components |
| --- | --- | --- | --- |
| `minimal` | Smallest compositor; no optional modules or companion apps. | none | niri |
| `desktop` | Daily-driver GLES build; user-facing modules and the core apps. | `agent`, `cursor`, `dbus`, `effects`, `kde`, `navigation`, `systemd`, `xdp-gnome-screencast` | niri, portal, shot, settings, monitor, session, wallpaper, audio, sidepanels |
| `full` | Everything, including experimental and optional integration. | `agent`, `cursor`, `dbus`, `dinit`, `effects`, `kde`, `navigation`, `systemd`, `vulkan`, `xdp-gnome-screencast` | all of `desktop` plus companion |

`build-profile.sh` validates every feature against `cargo metadata` before
running cargo and fails with the available feature list when a name is unknown.
`--check` checks niri only by default; add `--with-apps` to check the companion
workspaces too. `--build` always builds the listed companion apps.

### Source build without the scripts

```sh
export LIBCLANG_PATH=/usr/lib
cargo build --release -p niri                      # default features
cargo build --release -p niri --no-default-features # minimal
cargo build --release -p niri --no-default-features \
  --features agent,cursor,dbus,effects,kde,navigation,systemd,xdp-gnome-screencast

for manifest in loom-portal loom-companion loom-monitor loom-session \
                loom-shot loom-wallpaper loom-settings loom-audio \
                loom-sidepanels; do
  cargo build --release --manifest-path "$manifest/Cargo.toml"
done
```

The companion directories are separate Cargo workspaces. `loom-portal`,
`loom-companion` and the other apps each keep their own `Cargo.lock`, so a
root `cargo build` does not build them.

## Split packages

Every recipe is a **split package base**. One source checkout and a single
`build()` produce all packages below; `package_<name>()` only stages files, so
installing a subset never rebuilds niri or another Rust crate.

| Package | Binaries | Contents | depends / optdepends |
| --- | --- | --- | --- |
| `loom-niri` | `niri`, `niri-session` | compositor, session/desktop files, `niri.portal`, `niri-portals.conf`, systemd units, completions, config, docs | niri's GL/Wayland/PipeWire/Vulkan libraries; optional portals, terminal/launcher, GPU drivers |
| `loom-portal` | `loom-portal`, `loom-portal-picker` | `loom.portal`, systemd user unit, D-Bus activation file, example `portals.conf` | `dbus`, `systemd`; optional `xdg-desktop-portal{,-gtk}` |
| `loom-companion` | `loom-companion`, `loomctl` | automation/parking daemon, control CLI and optional systemd user unit | `gcc-libs`, `glibc`; optional `systemd`, `libpulse`, `libnotify` |
| `loom-monitor` | `loom-monitor` | output profile save/apply/rollback | `gcc-libs`, `glibc` |
| `loom-session` | `loom-session` | named workspace/window/output contexts | `gcc-libs`, `glibc` |
| `loom-shot` | `loom-shot` | screenshot capture, built-in annotations | `gcc-libs`, `glibc`; optional `satty`, `wl-clipboard` |
| `loom-wallpaper` | `loom-wallpaper` | per-workspace wallpaper scenes | `gcc-libs`, `glibc`, `wayland`; optional `swaybg`, `swww` |
| `loom-settings` | `loom-settings`, `loom-settingsctl` | GTK4 settings UI, CLI and desktop entry | `gtk4`/`libadwaita`, `gcc-libs`, `glibc` |
| `loom-audio` | `loom-audio` | rule-based PipeWire routing and default sink | `libpipewire`, `gcc-libs`, `glibc`; optional `pipewire`, `wireplumber`, `libpulse`, `pulseaudio` |
| `loom-sidepanels` | `loom-sidepanels` | layer-shell panels with exclusive zones | `cairo`, `glib2`, `wayland`, `gcc-libs`, `glibc`; optional `playerctl`, `libpulse`, `wireplumber`, `libnotify` |
| `loom-noctalia-plugins` | — | Noctalia plugin source (Luau bar widgets) installed under `/usr/share/loom/noctalia-plugins` | optional `noctalia`, `loom-companion`, `loom-monitor`, `loom-session`, `loom-shot`, `loom-audio`, `loom-wallpaper`, `loom-sidepanels` |
| `loom-desktop` | `loom` | meta package and unified tool selector, post-install notes | depends on every package above |

`loom-monitor`, `loom-session` and `loom-sidepanels` need no niri package
beyond the IPC socket; `loom-companion`, `loom-shot`, `loom-settings` and
`loom-audio` are equally independent. Only `loom-niri` provides/conflicts
with `niri`, `niri-git`, `niri-elite` and `niri-elite-git`.

### Install examples

Build the whole split once, then install only what you need:

```sh
cd packaging/arch
makepkg -s                                  # build every split in one pass

sudo pacman -U niri-loom-*.pkg.tar.zst    # compositor only
sudo pacman -U niri-loom-*.pkg.tar.zst \
  loom-portal-*.pkg.tar.zst \
  loom-settings-*.pkg.tar.zst             # compositor + portal + settings
makepkg -si                                 # install the whole split, meta included
```

Or let `makepkg` install selected packages directly:

```sh
makepkg -si --pkg loom-niri
makepkg -si --pkg loom-portal loom-settings
```

`makepkg -si --pkg` still builds all splits, but only the named packages are
installed; install sibling packages from the same build too when they have
local dependencies. `loom-desktop` depends on every split, so install the
whole set (plain `makepkg -si`) rather than the meta package alone. When
`loom-niri` replaces `niri`/`niri-git`, remove the other package first or let
your helper resolve the conflict. Companion packages have no such conflict.

Each package owns its own `/usr/share/doc/<package>/` README, license and
component files. `loom-portal` owns `loom.portal`, its systemd/D-Bus files
and `/usr/share/doc/loom-portal/portals.conf.loom.example`;
`loom-niri` owns `niri.portal` and `niri-portals.conf`. The `loom-desktop`
package installs the `loom` selector (`loom shot`, `loom monitor`, …) and
its `.install` script only prints instructions; it never starts or enables a
service.

### Interaction with the profiles

The installation profiles describe *developer source builds*, not the Arch
subpackages. Both use the same boundaries:

| Profile | Roughly equivalent Arch package set |
| --- | --- |
| `minimal` | build niri with no optional features; install `loom-niri` for the default-feature compositor binary |
| `desktop` | `loom-niri` + `loom-portal`, `loom-shot`, `loom-settings`, `loom-monitor`, `loom-session`, `loom-wallpaper`, `loom-audio`, `loom-sidepanels`, `loom-noctalia-plugins` |
| `full` | `loom-desktop` (every split, including `loom-companion`) |

The packages always build the compositor with its default Cargo features; use
`packaging/scripts/build-profile.sh` when you need a different feature set.

There are no man pages in the tree yet; `loom-niri` installs them
automatically if `resources/man/*.1` appears later.

## Shell completions

Every packaged binary ships bash, fish and zsh completions under the standard
locations (`/usr/share/bash-completion/completions/<bin>`,
`/usr/share/fish/vendor_completions.d/<bin>.fish`,
`/usr/share/zsh/site-functions/_<bin>`). The companion CLIs parse arguments by
hand, so the lists live in `packaging/completions/` and are generated by:

```sh
python3 packaging/scripts/generate-completions.py          # regenerate
python3 packaging/scripts/generate-completions.py --check  # CI/verify
```

Add or change a command/option in the table at the top of that script, rerun it,
and `verify.sh` checks the committed files are in sync and that every split
package installs the completions for its own binaries. `niri` keeps its
upstream-generated completions.

## Portal setup

The fork can use two Screenshot/ScreenCast backends:

1. **niri's built-in backend** — `/usr/share/xdg-desktop-portal/portals/niri.portal`
   and `niri-portals.conf`, which selects `niri` for Screenshot and ScreenCast.
   This is the default for a `niri` desktop session.
2. **loom-portal** — a standalone daemon with its own bus name
   `org.freedesktop.impl.portal.desktop.loom`, descriptor
   `loom.portal`, D-Bus activation file and optional systemd user unit. It
   delegates capture to niri's IPC and adds the external layer-shell picker.

`loom-niri` and `loom-portal` install their descriptors side by side. The
broker decides through `portals.conf`; installing a global file that forces
`loom` for every desktop would be wrong, so `loom-portal` ships its
preference file as
`/usr/share/doc/loom-portal/portals.conf.loom.example`. Install
`loom-portal` to use the standalone backend; the built-in backend alone only
needs `loom-niri`.

To select the standalone backend for the current user:

```ini
# ~/.config/xdg-desktop-portal/portals.conf
[preferred]
org.freedesktop.impl.portal.Screenshot=loom;
org.freedesktop.impl.portal.ScreenCast=loom;
# Keep or add fallbacks for the other interfaces, for example:
org.freedesktop.impl.portal.Access=gtk;
org.freedesktop.impl.portal.Notification=gtk;
org.freedesktop.impl.portal.Secret=oo7-portal;gnome-keyring;
```

Then:

```sh
systemctl --user daemon-reload
systemctl --user enable --now loom-portal.service
systemctl --user restart xdg-desktop-portal.service
```

`xdg-desktop-portal` is the broker and must be installed.
`xdg-desktop-portal-gtk` supplies the file chooser, Access, Notification and
other interfaces that neither niri nor loom-portal implements. Without a
Secret provider, applications that expect the Secret portal will not find one;
`oo7-portal` or `gnome-keyring` are the common choices.

If the service is not enabled, the D-Bus activation file can start
`loom-portal` on demand. Run `journalctl --user -u loom-portal` or check
`busctl --user status org.freedesktop.impl.portal.desktop.loom` when capture
fails.

`niri-session` restarts `xdg-desktop-portal.service` after niri imports the
session environment, because the broker may have been D-Bus activated with the
wrong environment. This is part of `resources/niri.service`.

## Vulkan and GPU notes

- The `vulkan` feature is in niri's default feature set, so `loom-niri` is
  built with loom-smithay's Vulkan renderer. It does not mean Vulkan is used
  at runtime.
- Runtime selection is `NIRI_RENDERER=auto|gles|vulkan` (default `auto`).
  `auto` probes physical devices and falls back to EGL/GLES with a logged
  reason when Vulkan is unavailable.
- `niri --check-vulkan` prints the resolved plan as JSON without starting a
  session. Use it after driver updates and on hybrid-GPU laptops.
- The GLES path remains the compatibility baseline. Vulkan SDR is
  experimental; Vulkan + HDR is only validated for fullscreen passthrough.
  GLSL-only visual effects are skipped on the Vulkan path.
- Install `vulkan-icd-loader` plus the ICD for the GPU: `vulkan-radeon` (AMD),
  `vulkan-intel` (Intel) or `nvidia-utils` (NVIDIA). `mesa` is already a hard
  dependency for EGL/GBM/GLES.
- On hybrid systems, leave `NIRI_RENDERER=auto` and check the probe output
  before forcing Vulkan. Software EGL (llvmpipe) is not a supported compositor
  path here.

## Known-good tools

These are the tools the default config, companion apps and portal integration
are designed around. "Used by" is the actual integration point, not a claim
that the combination was hardware-tested in this tree.

| Tool | Package (Arch) | Used by | Notes |
| --- | --- | --- | --- |
| alacritty | `alacritty` | niri default config | `Mod+T` terminal. |
| fuzzel | `fuzzel` | niri default config | `Mod+D` launcher. |
| mako / dunst | `mako`, `dunst` | desktop session | Notification daemon; either works. |
| waybar | `waybar` | niri default config | Optional bar. |
| loom-sidepanels | `loom-sidepanels` | companion apps | Layer-shell bar/panels with exclusive zones. |
| swaybg | `swaybg` | loom-wallpaper | Default `LOOM_WALLPAPER_COMMAND`. |
| swww | `swww` | loom-wallpaper | Supported through a custom command template. |
| swaylock | `swaylock` | niri default config | `Super+Alt+L`. |
| satty | `satty` | loom-shot | Interactive `--annotate` workflow. |
| wl-clipboard | `wl-clipboard` | loom-shot | `--copy` writes PNG via `wl-copy`. |
| pactl | `libpulse` | loom-audio, loom-sidepanels | PulseAudio-compatible PipeWire backend and volume module. |
| wpctl | `wireplumber` | loom-audio, loom-sidepanels | Native PipeWire CLI backend/fallback. |
| playerctl | `playerctl` | loom-sidepanels | Media module; missing tool renders blank. |
| grim / slurp | — | not used | niri and loom-shot capture through niri IPC. |
| xdg-desktop-portal | `xdg-desktop-portal` | portals | Broker for niri.portal and loom.portal. |
| xdg-desktop-portal-gtk | `xdg-desktop-portal-gtk` | portals | Fallback interfaces. |
| xdg-desktop-portal-gnome | `xdg-desktop-portal-gnome` | niri README | Fallback ScreenCast backend; the fork's built-in backend does not need it. |
| xwayland-satellite | `xwayland-satellite` | niri | X11 application support. |
| polkit-gnome | `polkit-gnome` | desktop session | Authentication agent. |
| dbus | `dbus` | niri-session | `dbus-update-activation-environment`; falls back to `systemctl --user import-environment`. |
| systemd | `systemd` | niri, companion idle | `systemctl`, `loginctl`, `busctl`. |
| pipewire + wireplumber | `pipewire`, `wireplumber` | niri screencast, loom-audio | PipeWire server and session manager. |

## AUR

Two split recipes live under `packaging/aur/`. Both produce the same package
names: `loom-niri`, `loom-portal`, `loom-companion`, `loom-monitor`,
`loom-session`, `loom-shot`, `loom-wallpaper`, `loom-settings`,
`loom-audio`, `loom-sidepanels`, `loom-noctalia-plugins` and the
`loom-desktop` meta package.

| Package base | Source | Version |
| --- | --- | --- |
| `loom-niri` | pinned commit `9ca9e425` | fixed `26.4.0+loom.1-r1` |
| `loom-niri-git` | `main` | `pkgver()` from git |

Only one package base can be published: AUR rejects a second repository that
would overwrite the same split package names. `loom-niri` is the published
base; `loom-niri-git` stays in the tree for local builds and would need
`-git`-suffixed package names before it could be uploaded alongside it.

Build locally before publishing:

```sh
cd packaging/aur/loom-niri-git
makepkg -s
```

`prepare()` fetches the root lockfile and every companion workspace lockfile
into `$srcdir/cargo-home`; `build()` uses `--frozen`, so a missing dependency
fails instead of silently updating a lockfile. `LIBCLANG_PATH=/usr/lib` is set
for bindgen. The source commit is exported as `NIRI_BUILD_COMMIT` so
`niri --version` identifies the packaged revision. All split packages share that
one build.

Each recipe directory also carries the local recipe files that are not in the
source checkout: `loom-settings.desktop`, `loom-desktop.README.md` and
`loom-desktop.install`. Keep them next to `PKGBUILD` when publishing.
`loom-noctalia-plugins` installs the same directory shipped in the repository;
add `/usr/share/loom/noctalia-plugins` as a Noctalia plugin source.

Regenerate `.SRCINFO` after changing any recipe:

```sh
packaging/scripts/update-srcinfo.sh
```

Publishing is a manual release step: copy `PKGBUILD`, `.SRCINFO` and the three
local recipe files into the AUR repository and push from there. Never force-push
an AUR history. The current `loom-niri` base is published at
`ssh://aur@aur.archlinux.org/loom-niri.git`.

## Verification

```sh
packaging/scripts/verify.sh                 # syntax, profiles, staging, metadata, .SRCINFO, cargo checks
packaging/scripts/verify.sh --skip-cargo    # fast structural check
bash -n packaging/scripts/*.sh packaging/arch/PKGBUILD \
  packaging/aur/loom-niri/PKGBUILD packaging/aur/loom-niri-git/PKGBUILD
```

The verifier lints with `shellcheck` when it is installed (otherwise `bash -n`
only), validates profiles against `cargo metadata`, asserts that every recipe
declares the same 11 split packages, and runs each `package_<name>()` against a
fake build tree to compare its staged files with an exact manifest. It then
checks binary ownership (each Cargo binary and `niri-session` is claimed by
exactly one package, with no duplicates or orphans), expands every recipe with
`makepkg --printsrcinfo` to validate `depends`/`optdepends` names and
cross-recipe agreement, and verifies `loom-desktop` depends on every split.
`.SRCINFO` files must be current (regenerate with `update-srcinfo.sh`). Finally
it runs `cargo check` for all three profiles; `--skip-cargo` is useful in a
minimal environment.

## Generated from the manifests

The tables below are generated with
`packaging/scripts/manifest-report.sh`; update this section after a manifest or
dependency change.

### Manifest pins

| Item | Value |
| --- | --- |
| niri package version | `26.4.0-elite` (Arch `pkgver` uses `.elite` because Arch versions cannot contain `-`) |
| Rust MSRV | `1.95` |
| loom-smithay revision | `c76148907cf6dae4441aa7224ac518bc120e356a` |
| Default Cargo features | `agent`, `dbus`, `systemd`, `xdp-gnome-screencast`, `kde`, `cursor`, `effects`, `navigation`, `vulkan` |

### Components and binaries

| Component | Manifest | Binaries | Package |
| --- | --- | --- | --- |
| `niri` | `Cargo.toml` | niri | `loom-niri` |
| `portal` | `loom-portal/Cargo.toml` | loom-portal, loom-portal-picker | `loom-portal` |
| `companion` | `loom-companion/Cargo.toml` | loom-companion, loomctl | `loom-companion` |
| `monitor` | `loom-monitor/Cargo.toml` | loom-monitor | `loom-monitor` |
| `session` | `loom-session/Cargo.toml` | loom-session | `loom-session` |
| `shot` | `loom-shot/Cargo.toml` | loom-shot | `loom-shot` |
| `wallpaper` | `loom-wallpaper/Cargo.toml` | loom-wallpaper | `loom-wallpaper` |
| `settings` | `loom-settings/Cargo.toml` | loom-settings, loom-settingsctl | `loom-settings` |
| `audio` | `loom-audio/Cargo.toml` | loom-audio | `loom-audio` |
| `sidepanels` | `loom-sidepanels/Cargo.toml` | loom-sidepanels | `loom-sidepanels` |

`niri-session` is a shell script shipped by `loom-niri`; it is not a Cargo
target. `loom-desktop` ships no binaries.

### niri Cargo features

| Feature | In default | Purpose |
| --- | --- | --- |
| `agent` | yes | Automation seat over IPC. |
| `cursor` | yes | SVG/scalable cursors and shake-to-enlarge. |
| `dbus` | yes | freedesktop/GNOME D-Bus interfaces and accessibility. |
| `dinit` | no | dinit session environment integration. |
| `effects` | yes | Post-processing shaders, region/window passes and decoration light. |
| `kde` | yes | KDE blur, output order and colour picker interop. |
| `navigation` | yes | Screen magnifier and navigation module. |
| `pipewire` | no | Internal dependency feature, enabled through `xdp-gnome-screencast`. |
| `profile-with-tracy*` | no | Development-only Tracy profiling; never enable in a distro package. |
| `systemd` | yes | Global environment and transient scopes; implies `dbus`. |
| `vulkan` | yes | loom-smithay Vulkan renderer compiled in; runtime opt-in. |
| `xdp-gnome-screencast` | yes | PipeWire screencasting support; implies `dbus` and `pipewire`. |

## Known packaging blockers

- There are no man pages in the tree, so the packages install none.
  `loom-niri` picks up `resources/man/*.1` automatically when they appear.
- `loom-settings` has a desktop entry installed by its package
  (`/usr/share/applications/loom-settings.desktop`); it opens the GTK4 UI.
- `loom-companion` ships an optional user unit
  (`/usr/lib/systemd/user/loom-companion.service`); enable it after niri has
  imported `NIRI_SOCKET` (`systemctl --user enable --now loom-companion`).
  `loom-wallpaper watch`, `loom-audio watch` and `loom-sidepanels` stay
  process-per-output/GUI apps and have no upstream units; the meta package
  documentation shows `spawn-at-startup` lines and leaves enablement to the
  user. `loom-niri` and `loom-portal` install their units as before.
- Debian/Fedora/Nix packaging of the companion workspaces is not provided.
- The packages are not published: AUR publishing, repository signing and
  hardware validation remain release tasks.
