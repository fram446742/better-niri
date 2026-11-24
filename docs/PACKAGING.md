# Packaging and compatibility

How Better Niri is packaged, which system versions it needs, and what is known
to work with it. The packaging tree is deliberately additive: it does not
change niri's source, workspace manifests or `Cargo.lock`.

- `packaging/arch/` — Arch/CachyOS source packages (`niri-better` and the
  `better-desktop` meta package).
- `packaging/aur/` — AUR recipes: `niri-better` (pinned source) and
  `niri-better-git` (tracks `integration/elite-base`). The existing root
  `packaging/aur/PKGBUILD` is the older `niri-elite-git` recipe targeting the
  `main` branch; it is left untouched.
- `packaging/profiles/` — minimal, desktop and full installation profiles.
- `packaging/scripts/` — profile builds, manifest report and the packaging
  verifier.

## Compatibility matrix

| Distribution / channel | Status | Artifacts | Notes |
| --- | --- | --- | --- |
| Arch Linux | source package provided | `packaging/arch/PKGBUILD` | Builds the compositor with default features and all companion apps. Not in an official repository. |
| CachyOS | source package provided | same as Arch | CachyOS is Arch-compatible; `makepkg` works as-is. Use the tuned toolchain your system provides. |
| AUR | recipes provided, not published | `packaging/aur/niri-better`, `packaging/aur/niri-better-git` | Publish only by copying `PKGBUILD` + `.SRCINFO` into the AUR repository. |
| Fedora | unmaintained in this tree | `niri.spec.rpkg` | The upstream spec exists but is not validated against this branch's extra crates and portal files. |
| NixOS | compositor only | `flake.nix` | The flake builds niri, not the excluded companion workspaces. |
| Debian/Ubuntu | metadata only | `Cargo.toml` `[package.metadata.deb]` | No Debian source package is maintained here. |
| Other distributions | build from source | `packaging/profiles/` | Use the profile scripts or reproduce their `cargo` commands. |

Conflicts: `niri-better`, `niri-better-git` and `niri-elite-git` all install
`/usr/bin/niri`, `/usr/bin/niri-session` and the same session files. They
conflict with `niri`, `niri-git`, `niri-elite` and each other.

### Required build versions

Values below come from the current manifests. Re-run
`packaging/scripts/manifest-report.sh` after dependency bumps.

| Requirement | Current value | Notes |
| --- | --- | --- |
| Rust | MSRV `1.95` | The checked-out toolchain in this environment is `1.102.0-nightly`; stable should work at the declared MSRV. |
| Cargo edition | 2024 | Requires a recent toolchain. |
| libclang | any recent LLVM/Clang | Needed by bindgen for PipeWire/SPA. Set `LIBCLANG_PATH=/usr/lib` if detection fails. |
| PipeWire | crate `0.10.1`, feature `v0_3_33` | Runtime server 0.3.33 or newer; Arch has 1.6.x. |
| GTK4 | crate `0.11` | Runtime 4.24.1 was used for the profile checks; `better-settings` needs the GTK4 development files to build. |
| Wayland / libinput / libdrm / libgbm / libEGL | current Arch packages | See `packaging/arch/PKGBUILD` `depends` for the packaged set. |
| Vulkan loader + ICD | `vulkan-icd-loader` and a GPU driver | The pinned package builds the Vulkan renderer. Runtime use is opt-in. |
| D-Bus | session bus | Required by `niri`'s `dbus`/`systemd` features and `better-portal`. |
| systemd | user manager | Needed for `niri.service`, `niri-session`, `better-portal.service` and companion idle integration. |
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

for manifest in better-portal better-companion better-monitor better-session \
                better-shot better-wallpaper better-settings better-audio \
                better-sidepanels; do
  cargo build --release --manifest-path "$manifest/Cargo.toml"
done
```

The companion directories are separate Cargo workspaces. `better-portal`,
`better-companion` and the other apps each keep their own `Cargo.lock`, so a
root `cargo build` does not build them.

## Arch and CachyOS

Build and install the pinned package:

```sh
cd packaging/arch
makepkg -si
```

The package name is `niri-better`; it provides `niri`, `niri-elite`,
`wayland-compositor` and `xdg-desktop-portal-impl`. It installs:

- `/usr/bin/niri`, `/usr/bin/niri-session`
- `/usr/share/wayland-sessions/niri.desktop`
- `/usr/share/xdg-desktop-portal/portals/{niri,better}.portal`
- `/usr/share/xdg-desktop-portal/niri-portals.conf`
- `/usr/lib/systemd/user/{niri.service,niri-shutdown.target,better-portal.service}`
- `/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.better.service`
- `better-portal`, `better-portal-picker`, `better-companion`, `companionctl`,
  `better-shot`, `better-monitor`, `better-session`, `better-wallpaper`,
  `better-settings`, `better-settingsctl`, `better-audio` and
  `better-sidepanels` in `/usr/bin`
- shell completions, the default config, licenses and per-app READMEs under
  `/usr/share/doc/niri-better`

There are no man pages in the tree yet; the package installs them automatically
if `resources/man/*.1` appears later.

The `better-desktop` meta package depends on `niri-better` and documents the
post-install session and portal setup:

```sh
cd packaging/arch/better-desktop
makepkg -si
```

Its `.install` script only prints instructions; it never starts or enables a
service.

## Portal setup

The fork can use two Screenshot/ScreenCast backends:

1. **niri's built-in backend** — `/usr/share/xdg-desktop-portal/portals/niri.portal`
   and `niri-portals.conf`, which selects `niri` for Screenshot and ScreenCast.
   This is the default for a `niri` desktop session.
2. **better-portal** — a standalone daemon with its own bus name
   `org.freedesktop.impl.portal.desktop.better`, descriptor
   `better.portal`, D-Bus activation file and optional systemd user unit. It
   delegates capture to niri's IPC and adds the external layer-shell picker.

Both descriptors are installed together. The broker decides through
`portals.conf`; installing a global file that forces `better` for every desktop
would be wrong, so the package ships better-portal's preference file as
`/usr/share/doc/niri-better/portals.conf.better.example`.

To select the standalone backend for the current user:

```ini
# ~/.config/xdg-desktop-portal/portals.conf
[preferred]
org.freedesktop.impl.portal.Screenshot=better;
org.freedesktop.impl.portal.ScreenCast=better;
# Keep or add fallbacks for the other interfaces, for example:
org.freedesktop.impl.portal.Access=gtk;
org.freedesktop.impl.portal.Notification=gtk;
org.freedesktop.impl.portal.Secret=oo7-portal;gnome-keyring;
```

Then:

```sh
systemctl --user daemon-reload
systemctl --user enable --now better-portal.service
systemctl --user restart xdg-desktop-portal.service
```

`xdg-desktop-portal` is the broker and must be installed.
`xdg-desktop-portal-gtk` supplies the file chooser, Access, Notification and
other interfaces that neither niri nor better-portal implements. Without a
Secret provider, applications that expect the Secret portal will not find one;
`oo7-portal` or `gnome-keyring` are the common choices.

If the service is not enabled, the D-Bus activation file can start
`better-portal` on demand. Run `journalctl --user -u better-portal` or check
`busctl --user status org.freedesktop.impl.portal.desktop.better` when capture
fails.

`niri-session` restarts `xdg-desktop-portal.service` after niri imports the
session environment, because the broker may have been D-Bus activated with the
wrong environment. This is part of `resources/niri.service`.

## Vulkan and GPU notes

- The `vulkan` feature is in niri's default feature set, so `niri-better` is
  built with better-smithay's Vulkan renderer. It does not mean Vulkan is used
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
| better-sidepanels | bundled | companion apps | Layer-shell bar/panels with exclusive zones. |
| swaybg | `swaybg` | better-wallpaper | Default `BETTER_WALLPAPER_COMMAND`. |
| swww | `swww` | better-wallpaper | Supported through a custom command template. |
| swaylock | `swaylock` | niri default config | `Super+Alt+L`. |
| satty | `satty` | better-shot | Interactive `--annotate` workflow. |
| wl-clipboard | `wl-clipboard` | better-shot | `--copy` writes PNG via `wl-copy`. |
| pactl | `libpulse` | better-audio, better-sidepanels | PulseAudio-compatible PipeWire backend and volume module. |
| wpctl | `wireplumber` | better-audio, better-sidepanels | Native PipeWire CLI backend/fallback. |
| playerctl | `playerctl` | better-sidepanels | Media module; missing tool renders blank. |
| grim / slurp | — | not used | niri and better-shot capture through niri IPC. |
| xdg-desktop-portal | `xdg-desktop-portal` | portals | Broker for niri.portal and better.portal. |
| xdg-desktop-portal-gtk | `xdg-desktop-portal-gtk` | portals | Fallback interfaces. |
| xdg-desktop-portal-gnome | `xdg-desktop-portal-gnome` | niri README | Fallback ScreenCast backend; the fork's built-in backend does not need it. |
| xwayland-satellite | `xwayland-satellite` | niri | X11 application support. |
| polkit-gnome | `polkit-gnome` | desktop session | Authentication agent. |
| dbus | `dbus` | niri-session | `dbus-update-activation-environment`; falls back to `systemctl --user import-environment`. |
| systemd | `systemd` | niri, companion idle | `systemctl`, `loginctl`, `busctl`. |
| pipewire + wireplumber | `pipewire`, `wireplumber` | niri screencast, better-audio | PipeWire server and session manager. |

## AUR

Two recipes live under `packaging/aur/`:

| Package | Source | Version |
| --- | --- | --- |
| `niri-better` | pinned commit `35c0e54b` | fixed `26.4.0.elite` |
| `niri-better-git` | `integration/elite-base` | `pkgver()` from git |

Build locally before publishing:

```sh
cd packaging/aur/niri-better-git
makepkg -s
```

`prepare()` fetches the root lockfile and every companion workspace lockfile
into `$srcdir/cargo-home`; `build()` uses `--frozen`, so a missing dependency
fails instead of silently updating a lockfile. `LIBCLANG_PATH=/usr/lib` is set
for bindgen. The source commit is exported as `NIRI_BUILD_COMMIT` so
`niri --version` identifies the packaged revision.

Regenerate `.SRCINFO` after changing either recipe:

```sh
packaging/scripts/update-srcinfo.sh
```

Publishing is manual and out of scope for this tree: copy `PKGBUILD` and
`.SRCINFO` into the AUR repository and push from there. Never force-push an AUR
history.

## Verification

```sh
packaging/scripts/verify.sh                 # syntax, profiles, metadata, .SRCINFO, cargo checks
packaging/scripts/verify.sh --skip-cargo    # fast structural check
bash -n packaging/scripts/*.sh packaging/arch/PKGBUILD \
  packaging/arch/better-desktop/PKGBUILD packaging/aur/niri-better*/PKGBUILD
```

The verifier lints with `shellcheck` when it is installed (otherwise `bash -n`
only), validates profiles against `cargo metadata`, checks that every binary a
PKGBUILD installs exists as a Cargo `[[bin]]` target, checks the portal/session
artifacts, and runs `cargo check` for all three profiles. `--skip-cargo` is
useful in a minimal environment.

## Generated from the manifests

The tables below are generated with
`packaging/scripts/manifest-report.sh`; update this section after a manifest or
dependency change.

### Manifest pins

| Item | Value |
| --- | --- |
| niri package version | `26.4.0-elite` (Arch `pkgver` uses `.elite` because Arch versions cannot contain `-`) |
| Rust MSRV | `1.95` |
| better-smithay revision | `c76148907cf6dae4441aa7224ac518bc120e356a` |
| Default Cargo features | `agent`, `dbus`, `systemd`, `xdp-gnome-screencast`, `kde`, `cursor`, `effects`, `navigation`, `vulkan` |

### Components and binaries

| Component | Manifest | Binaries |
| --- | --- | --- |
| `niri` | `Cargo.toml` | niri |
| `portal` | `better-portal/Cargo.toml` | better-portal, better-portal-picker |
| `companion` | `better-companion/Cargo.toml` | better-companion, companionctl |
| `monitor` | `better-monitor/Cargo.toml` | better-monitor |
| `session` | `better-session/Cargo.toml` | better-session |
| `shot` | `better-shot/Cargo.toml` | better-shot |
| `wallpaper` | `better-wallpaper/Cargo.toml` | better-wallpaper |
| `settings` | `better-settings/Cargo.toml` | better-settings, better-settingsctl |
| `audio` | `better-audio/Cargo.toml` | better-audio |
| `sidepanels` | `better-sidepanels/Cargo.toml` | better-sidepanels |

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
| `vulkan` | yes | better-smithay Vulkan renderer compiled in; runtime opt-in. |
| `xdp-gnome-screencast` | yes | PipeWire screencasting support; implies `dbus` and `pipewire`. |

## Known packaging blockers

- There are no man pages in the tree, so the packages install none. The
  PKGBUILDs pick up `resources/man/*.1` automatically when they appear.
- The companion apps are bundled in `niri-better` rather than split into
  per-app packages. A future split would keep the same binaries and only
  change the package boundaries.
- `better-settings` has no `.desktop` file, so it is launched from the CLI or a
  user-created launcher.
- `better-companion`, `better-wallpaper watch`, `better-audio watch` and
  `better-sidepanels` have no upstream systemd units; the meta package
  documentation shows `spawn-at-startup` lines and leaves enablement to the
  user.
- Debian/Fedora/Nix packaging of the companion workspaces is not provided.
- The packages are not published: AUR publishing, repository signing and
  hardware validation remain release tasks.
