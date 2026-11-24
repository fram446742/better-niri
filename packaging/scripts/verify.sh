#!/usr/bin/env bash
# Verify the packaging tree.
#
# Checks:
#   - bash syntax for every script, PKGBUILD and .install file;
#   - shellcheck (when installed);
#   - profiles against cargo metadata features, component manifests and binaries;
#   - every recipe declares the same split packages and package functions;
#   - a staged package() run for every split, with an exact file manifest;
#   - every binary is owned by exactly one package (no duplicates, no orphans);
#   - depends/optdepends reference known packages and match across recipes;
#   - portal/session artifacts referenced by the PKGBUILDs exist;
#   - .SRCINFO parses, matches the checked-in file and lists every split;
#   - the better-desktop meta package depends on every other split;
#   - minimal/desktop/full `cargo check` runs for niri.
#
# Usage: packaging/scripts/verify.sh [--skip-cargo] [--skip-srcinfo]
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$script_dir/lib.sh"

root=$(repo_root)
cd "$root"

skip_cargo=0
skip_srcinfo=0
failures=0

tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/bn-verify.XXXXXX")
trap 'rm -rf "$tmpdir"' EXIT

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

pass() {
    printf 'ok: %s\n' "$*"
}

while [ "$#" -gt 0 ]; do
    case $1 in
        --skip-cargo)
            skip_cargo=1
            shift
            ;;
        --skip-srcinfo)
            skip_srcinfo=1
            shift
            ;;
        -h|--help)
            sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            die "unknown option: $1 (try --help)"
            ;;
    esac
done

recipes=(
    packaging/arch
    packaging/aur/niri-better
    packaging/aur/niri-better-git
)

split_packages=(
    niri-better
    better-portal
    better-companion
    better-monitor
    better-session
    better-shot
    better-wallpaper
    better-settings
    better-audio
    better-sidepanels
    better-noctalia-plugins
    better-desktop
)

companion_packages=(
    better-portal
    better-companion
    better-monitor
    better-session
    better-shot
    better-wallpaper
    better-settings
    better-audio
    better-sidepanels
)

# component -> package that owns its binaries
package_of_component() {
    case $1 in
        niri) printf '%s\n' niri-better ;;
        portal) printf '%s\n' better-portal ;;
        companion) printf '%s\n' better-companion ;;
        monitor) printf '%s\n' better-monitor ;;
        session) printf '%s\n' better-session ;;
        shot) printf '%s\n' better-shot ;;
        wallpaper) printf '%s\n' better-wallpaper ;;
        settings) printf '%s\n' better-settings ;;
        audio) printf '%s\n' better-audio ;;
        sidepanels) printf '%s\n' better-sidepanels ;;
        *) return 1 ;;
    esac
}

# Official Arch packages (or virtual names) that may appear in depends and
# optdepends. Keep this list in sync with the PKGBUILDs; a typo here fails the
# check instead of reaching an AUR user.
known_official_packages=(
    alacritty
    cairo
    dbus
    fuzzel
    gcc-libs
    glib2
    glibc
    gtk4
    libdisplay-info
    libdrm
    libglvnd
    libinput
    libnotify
    libpipewire
    libpulse
    libxkbcommon
    mako
    mesa
    nvidia-utils
    noctalia
    pango
    pixman
    playerctl
    polkit-gnome
    pulseaudio
    satty
    seatd
    swaybg
    swaylock
    swww
    systemd
    systemd-libs
    vulkan-icd-loader
    vulkan-intel
    vulkan-radeon
    waybar
    wayland
    wireplumber
    wl-clipboard
    pipewire
    xdg-desktop-portal
    xdg-desktop-portal-gnome
    xdg-desktop-portal-gtk
    xwayland-satellite
)

expects_binaries() {
    local component
    case $1 in
        niri-better) printf '%s\n' niri niri-session ;;
        better-noctalia-plugins) return 0 ;;
        *)
            component=$(package_component "$1") || return 1
            component_binaries "$component"
            ;;
    esac
}

package_component() {
    case $1 in
        niri-better) printf '%s\n' niri ;;
        better-portal) printf '%s\n' portal ;;
        better-companion) printf '%s\n' companion ;;
        better-monitor) printf '%s\n' monitor ;;
        better-session) printf '%s\n' session ;;
        better-shot) printf '%s\n' shot ;;
        better-wallpaper) printf '%s\n' wallpaper ;;
        better-settings) printf '%s\n' settings ;;
        better-audio) printf '%s\n' audio ;;
        better-sidepanels) printf '%s\n' sidepanels ;;
        *) return 1 ;;
    esac
}

expected_files() {
    local pkg=$1
    case $pkg in
        niri-better)
            cat <<'EOF'
usr/bin/niri
usr/bin/niri-session
usr/share/wayland-sessions/niri.desktop
usr/share/xdg-desktop-portal/portals/niri.portal
usr/share/xdg-desktop-portal/niri-portals.conf
usr/lib/systemd/user/niri.service
usr/lib/systemd/user/niri-shutdown.target
usr/share/doc/niri-better/README.md
usr/share/doc/niri-better/PACKAGING.md
usr/share/doc/niri-better/default-config.kdl
usr/share/licenses/niri-better/LICENSE
usr/share/bash-completion/completions/niri
usr/share/fish/vendor_completions.d/niri.fish
usr/share/zsh/site-functions/_niri
usr/share/nushell/completions/niri.nu
EOF
            ;;
        better-portal)
            cat <<'EOF'
usr/bin/better-portal
usr/bin/better-portal-picker
usr/share/xdg-desktop-portal/portals/better.portal
usr/lib/systemd/user/better-portal.service
usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.better.service
usr/share/doc/better-portal/README.md
usr/share/doc/better-portal/portals.conf.better.example
usr/share/licenses/better-portal/LICENSE
EOF
            ;;
        better-settings)
            cat <<'EOF'
usr/bin/better-settings
usr/bin/better-settingsctl
usr/share/applications/better-settings.desktop
usr/share/doc/better-settings/README.md
usr/share/licenses/better-settings/LICENSE
EOF
            ;;
        better-noctalia-plugins)
            cat <<'EOF'
usr/share/better-desktop/noctalia-plugins/better-audio/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-audio/widget.luau
usr/share/better-desktop/noctalia-plugins/better-companion/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-companion/widget.luau
usr/share/better-desktop/noctalia-plugins/better-monitor/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-monitor/widget.luau
usr/share/better-desktop/noctalia-plugins/better-session/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-session/widget.luau
usr/share/better-desktop/noctalia-plugins/better-shot/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-shot/widget.luau
usr/share/better-desktop/noctalia-plugins/better-sidepanels/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-sidepanels/widget.luau
usr/share/better-desktop/noctalia-plugins/better-sticky/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-sticky/widget.luau
usr/share/better-desktop/noctalia-plugins/better-sticky/translations/en.json
usr/share/better-desktop/noctalia-plugins/better-wallpaper/plugin.toml
usr/share/better-desktop/noctalia-plugins/better-wallpaper/widget.luau
usr/share/better-desktop/noctalia-plugins/README.md
usr/share/better-desktop/noctalia-plugins/verify.sh
usr/share/doc/better-noctalia-plugins/README.md
usr/share/licenses/better-noctalia-plugins/LICENSE
EOF
            ;;
        better-desktop)
            cat <<'EOF'
usr/share/doc/better-desktop/README.md
usr/share/licenses/better-desktop/LICENSE
EOF
            ;;
        better-companion)
            cat <<'EOF'
usr/bin/better-companion
usr/bin/companionctl
usr/share/doc/better-companion/README.md
usr/share/licenses/better-companion/LICENSE
EOF
            ;;
        better-monitor|better-session|better-shot|better-wallpaper|better-audio|better-sidepanels)
            cat <<EOF
usr/bin/${pkg}
usr/share/doc/${pkg}/README.md
usr/share/licenses/${pkg}/LICENSE
EOF
            ;;
        *)
            return 1
            ;;
    esac
}

# Run package_<name>() against a fake build tree so every recipe's staging can
# be inspected without a Rust build. Fake executables satisfy the only
# post-install command a package function runs (`niri completions`).
stage_package() {
    local recipe=$1 pkg=$2
    local stage="$tmpdir/stage-${recipe//\//_}-$pkg"
    rm -rf "$stage"
    mkdir -p "$stage/src/target/release" "$stage/pkg"

    local bin
    while IFS= read -r bin; do
        printf '#!/bin/sh\n[ "$1" = completions ] && printf "stub\\n"\nexit 0\n' \
            >"$stage/src/target/release/$bin"
        chmod 755 "$stage/src/target/release/$bin"
    done < <(all_bins)

    cp "$root/$recipe/better-settings.desktop" "$stage/src/better-settings.desktop"
    cp "$root/$recipe/better-desktop.README.md" "$stage/src/better-desktop.README.md"
    ln -s "$root" "$stage/src/better-niri"

    if ! (
        cd "$stage/src"
        # shellcheck disable=SC1090
        source "$root/$recipe/PKGBUILD"
        srcdir=$stage/src
        pkgdir=$stage/pkg
        pkgname=$pkg
        "package_$pkg"
    ) >"$stage/log" 2>&1; then
        printf 'staging %s from %s failed:\n' "$pkg" "$recipe" >&2
        sed 's/^/    /' "$stage/log" >&2
        return 1
    fi
    printf '%s\n' "$stage/pkg"
}

# Print every planned binary exactly once. Used to seed the fake target dir.
all_bins() {
    local component
    for component in $(all_components); do
        component_binaries "$component"
    done
    printf '%s\n' niri-session
}

printf '==> syntax\n'
shell_files=(
    packaging/scripts/build-profile.sh
    packaging/scripts/lib.sh
    packaging/scripts/manifest-report.sh
    packaging/scripts/update-srcinfo.sh
    packaging/scripts/verify.sh
    packaging/arch/PKGBUILD
    packaging/arch/better-desktop.install
    packaging/aur/niri-better/PKGBUILD
    packaging/aur/niri-better/better-desktop.install
    packaging/aur/niri-better-git/PKGBUILD
    packaging/aur/niri-better-git/better-desktop.install
)
for file in "${shell_files[@]}"; do
    if [ ! -f "$file" ]; then
        fail "missing $file"
    elif bash -n "$file"; then
        pass "bash -n $file"
    else
        fail "bash -n $file"
    fi
done

if command -v shellcheck >/dev/null 2>&1; then
    printf '==> shellcheck\n'
    # PKGBUILDs are Arch metadata scripts; shellcheck cannot model their
    # externally-consumed variables, so they get `bash -n` above.
    if shellcheck --severity=warning -x packaging/scripts/*.sh; then
        pass "shellcheck scripts"
    else
        fail "shellcheck reported problems"
    fi
else
    printf '==> shellcheck not installed; bash -n only\n'
fi

printf '==> recipes\n'
for recipe in "${recipes[@]}"; do
    if [ ! -f "$recipe/PKGBUILD" ]; then
        fail "missing $recipe/PKGBUILD"
        continue
    fi
    for local_file in better-settings.desktop better-desktop.README.md better-desktop.install; do
        if [ -f "$recipe/$local_file" ]; then
            pass "recipe file $recipe/$local_file"
        else
            fail "missing $recipe/$local_file"
        fi
    done
    if grep -qx '\[Desktop Entry\]' "$recipe/better-settings.desktop" &&
        grep -qx 'Type=Application' "$recipe/better-settings.desktop" &&
        grep -qx 'Exec=better-settings' "$recipe/better-settings.desktop"; then
        pass "$recipe/better-settings.desktop is a valid application entry"
    else
        fail "$recipe/better-settings.desktop lacks Desktop Entry/Type/Exec"
    fi

    if ! mapfile -t recipe_packages < <(
        bash -c 'source "$1" >/dev/null 2>&1; printf "%s\n" "${pkgname[@]}"' _ "$recipe/PKGBUILD"
    ); then
        fail "cannot read pkgname from $recipe/PKGBUILD"
        continue
    fi
    if [ "${recipe_packages[*]}" = "${split_packages[*]}" ]; then
        pass "$recipe declares all ${#split_packages[@]} split packages"
    else
        fail "$recipe pkgname does not match the expected split list"
    fi
    for pkg in "${split_packages[@]}"; do
        if bash -c 'source "$1" >/dev/null 2>&1; declare -F "package_$2" >/dev/null' \
            _ "$recipe/PKGBUILD" "$pkg"; then
            :
        else
            fail "$recipe is missing package_$pkg()"
        fi
    done
    pass "$recipe defines package() for every split"
done

# Local recipe files must be identical across recipes, otherwise the -git and
# pinned packages would install different desktop entries or meta docs.
for local_file in better-settings.desktop better-desktop.README.md better-desktop.install; do
    reference="packaging/arch/$local_file"
    if [ ! -f "$reference" ]; then
        fail "missing canonical $reference"
        continue
    fi
    for recipe in "${recipes[@]}"; do
        if cmp -s "$reference" "$recipe/$local_file"; then
            pass "$recipe/$local_file matches packaging/arch"
        else
            fail "$recipe/$local_file differs from packaging/arch"
        fi
    done
done

printf '==> profiles\n'
profiles=(packaging/profiles/minimal.toml packaging/profiles/desktop.toml packaging/profiles/full.toml)
for profile in "${profiles[@]}"; do
    if [ ! -f "$profile" ]; then
        fail "missing profile $profile"
        continue
    fi
    if ! name=$(profile_name "$profile"); then
        fail "cannot read name from $profile"
        continue
    fi
    mapfile -t features < <(profile_features "$profile")
    mapfile -t apps < <(profile_apps "$profile")
    [ "${#features[@]}" -gt 0 ] || features=()
    [ "${#apps[@]}" -gt 0 ] || apps=()

    if ( validate_features "$root" "$name" "${features[@]}" ) &&
       ( validate_apps "$root" "$name" "${apps[@]}" ); then
        pass "profile $name: ${#features[@]} features, ${#apps[@]} components"
    else
        fail "profile $name is invalid"
    fi
done

printf '==> split package staging\n'
declare -A metadata_bins=()
for component in $(all_components); do
    manifest=$(component_manifest_path "$root" "$component") ||
        die "component $component has no manifest"
    if [ ! -f "$manifest" ]; then
        fail "missing manifest $manifest"
        continue
    fi
    manifest_bins=$(manifest_binary_names "$manifest") ||
        die "cannot read binary targets from $manifest"
    while IFS= read -r bin; do
        [ -n "$bin" ] || continue
        metadata_bins["$bin"]=1
    done < <(component_binaries "$component")
    while IFS= read -r bin; do
        [ -n "$bin" ] || continue
        if ! printf '%s\n' "$manifest_bins" | grep -qxF -- "$bin"; then
            fail "component $component expects '$bin' but $manifest does not declare it"
        fi
    done < <(component_binaries "$component")
done
if [ "${#metadata_bins[@]}" -eq 0 ]; then
    fail "no component binaries were declared"
else
    pass "components declare ${#metadata_bins[@]} packaged binary targets"
fi

for recipe in "${recipes[@]}"; do
    [ -f "$recipe/PKGBUILD" ] || continue
    declare -A owner=()
    for pkg in "${split_packages[@]}"; do
        if ! staged=$(stage_package "$recipe" "$pkg"); then
            fail "staging $pkg from $recipe"
            continue
        fi

        mapfile -t actual < <(cd "$staged" && find . \( -type f -o -type l \) | sed 's|^\./||' | LC_ALL=C sort)
        mapfile -t expected < <(expected_files "$pkg" | LC_ALL=C sort)
        if [ "${actual[*]}" = "${expected[*]}" ]; then
            pass "$recipe: $pkg stages ${#actual[@]} files"
        else
            fail "$recipe: $pkg staging does not match its manifest"
            printf '  unexpected/missing differences:\n' >&2
            diff -u <(printf '%s\n' "${expected[@]}") <(printf '%s\n' "${actual[@]}") |
                sed 's/^/    /' >&2 || true
        fi

        mapfile -t staged_bins < <(cd "$staged/usr/bin" 2>/dev/null && find . -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort)
        mapfile -t expected_bins < <(expects_binaries "$pkg" | LC_ALL=C sort)
        if [ "${staged_bins[*]}" = "${expected_bins[*]}" ]; then
            pass "$recipe: $pkg owns ${#staged_bins[@]} binaries"
        else
            fail "$recipe: $pkg binary set differs (expected: ${expected_bins[*]}; staged: ${staged_bins[*]})"
        fi
        for bin in "${staged_bins[@]}"; do
            if [ -n "${owner[$bin]:-}" ]; then
                fail "$recipe: '$bin' is installed by both ${owner[$bin]} and $pkg"
            else
                owner["$bin"]=$pkg
            fi
        done
    done

    for bin in "${!metadata_bins[@]}"; do
        if [ -z "${owner[$bin]:-}" ]; then
            fail "$recipe: Cargo binary '$bin' is not owned by any split package"
        fi
    done
    pass "$recipe: every Cargo binary has exactly one owner"
done

printf '==> package metadata\n'
# Generate the effective .SRCINFO and validate dependency names plus
# cross-recipe consistency. This uses makepkg's own split-package expansion, so
# per-package variables are exactly what pacman will see.
if command -v makepkg >/dev/null 2>&1; then
    srcinfo_files=()
    for recipe in "${recipes[@]}"; do
        [ -f "$recipe/PKGBUILD" ] || continue
        out="$tmpdir/$(printf '%s' "$recipe" | tr '/' '_').SRCINFO"
        if (cd "$recipe" && makepkg --printsrcinfo >"$out"); then
            srcinfo_files+=("$recipe:$out")
        else
            fail "makepkg --printsrcinfo fails for $recipe"
        fi
    done

    known=$(printf '%s\n' "${split_packages[@]}" "${known_official_packages[@]}" | paste -sd, -)
    expected_meta=$(printf '%s\n' "${split_packages[@]}" | grep -v '^better-desktop$' | paste -sd, -)
    if KNOWN="$known" EXPECTED_META="$expected_meta" python3 - ${srcinfo_files[@]} <<'PY'
import os
import re
import sys

known = set(os.environ["KNOWN"].split(","))
expected_meta = set(os.environ["EXPECTED_META"].split(","))
problems = []
parsed = {}


def dep_name(value):
    return re.split(r"[<>=]", value, maxsplit=1)[0].strip()


def opt_name(value):
    return dep_name(value.split(":", 1)[0])


for entry in sys.argv[1:]:
    recipe, path = entry.split(":", 1)
    packages = {}
    current = None
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            m = re.match(r"pkgname = (.+)$", line)
            if m:
                current = m.group(1)
                packages[current] = {"depends": [], "optdepends": [], "provides": [],
                                     "conflicts": [], "install": None}
                continue
            if current is None:
                continue
            m = re.match(r"\t(depends|optdepends|provides|conflicts) = (.+)$", line)
            if m:
                attr, value = m.groups()
                if attr in ("depends", "optdepends"):
                    name = dep_name(value) if attr == "depends" else opt_name(value)
                    if name not in known:
                        problems.append(
                            f"{recipe}: {current} {attr} references unknown package '{name}'"
                        )
                packages[current][attr].append(value)
                continue
            m = re.match(r"\tinstall = (.+)$", line)
            if m:
                packages[current]["install"] = m.group(1)
    parsed[recipe] = packages
    if not packages:
        problems.append(f"{recipe}: .SRCINFO declares no packages")
    meta = packages.get("better-desktop")
    if meta is None:
        problems.append(f"{recipe}: better-desktop package missing")
    else:
        deps = {dep_name(v) for v in meta["depends"]}
        if deps != expected_meta:
            problems.append(
                f"{recipe}: better-desktop depends {sorted(deps)} but split packages are "
                f"{sorted(expected_meta)}"
            )
    for pkg, attrs in packages.items():
        if pkg not in expected_meta and pkg != "better-desktop":
            problems.append(f"{recipe}: unexpected package '{pkg}' in .SRCINFO")

# Metadata must be identical across the pinned arch, pinned AUR and -git
# recipes. pkgver/pkgdesc on the pkgbase may differ, package stanzas may not.
attributes = ("depends", "optdepends", "provides", "conflicts", "install")
if len(parsed) > 1:
    names = sorted(parsed)
    reference_recipe = names[0]
    reference = parsed[reference_recipe]
    for other_recipe in names[1:]:
        other = parsed[other_recipe]
        for pkg in sorted(set(reference) | set(other)):
            left = reference.get(pkg)
            right = other.get(pkg)
            if left is None or right is None:
                problems.append(
                    f"{other_recipe}: package '{pkg}' missing compared with {reference_recipe}"
                )
                continue
            for attr in attributes:
                left_value = left[attr]
                right_value = right[attr]
                if attr == "provides":
                    # Versioned provides differ between the pinned and -git
                    # recipes; compare the provided names only.
                    left_value = sorted(dep_name(v) for v in left_value)
                    right_value = sorted(dep_name(v) for v in right_value)
                if left_value != right_value:
                    problems.append(
                        f"{other_recipe}: {pkg} {attr} differs from {reference_recipe}"
                    )

for problem in problems:
    print(f"FAIL: {problem}", file=sys.stderr)
sys.exit(1 if problems else 0)
PY
    then
        pass "split metadata: known deps, meta coverage, cross-recipe agreement"
    else
        fail "split package metadata check"
    fi
else
    printf '==> makepkg not installed; metadata check skipped\n'
fi

printf '==> package artifacts\n'
artifacts=(
    resources/niri.desktop
    resources/niri.portal
    resources/niri-portals.conf
    resources/niri.service
    resources/niri-shutdown.target
    resources/niri-session
    resources/default-config.kdl
    better-portal/better.portal
    better-portal/packaging/better-portal.service
    better-portal/packaging/org.freedesktop.impl.portal.desktop.better.service
    better-portal/packaging/portals.conf
    docs/PACKAGING.md
)
for artifact in "${artifacts[@]}"; do
    if [ -f "$artifact" ]; then
        pass "artifact $artifact"
    else
        fail "missing artifact $artifact"
    fi
done

printf '==> scripts\n'
for file in packaging/scripts/*.sh; do
    if [ -x "$file" ]; then
        pass "executable $file"
    else
        fail "not executable: $file"
    fi
done

printf '==> docs\n'
for section in 'Compatibility matrix' 'Installation profiles' 'Split packages' 'Portal setup' 'Vulkan' 'AUR'; do
    if grep -qF "$section" docs/PACKAGING.md; then
        pass "docs/PACKAGING.md has '$section'"
    else
        fail "docs/PACKAGING.md is missing '$section'"
    fi
done

if [ "$skip_srcinfo" = 0 ] && command -v makepkg >/dev/null 2>&1; then
    printf '==> .SRCINFO\n'
    for recipe in "${recipes[@]}"; do
        if [ ! -f "$recipe/.SRCINFO" ]; then
            fail "missing $recipe/.SRCINFO"
            continue
        fi
        generated="$tmpdir/srcinfo-$(printf '%s' "$recipe" | tr '/' '_')"
        if ! (cd "$recipe" && makepkg --printsrcinfo >"$generated"); then
            fail "makepkg cannot parse $recipe/PKGBUILD"
            continue
        fi
        pass "makepkg parses $recipe/PKGBUILD"

        left="$generated"
        right="$recipe/.SRCINFO"
        if [ "$recipe" = packaging/aur/niri-better-git ]; then
            # makepkg does not run pkgver() on a bare recipe; update-srcinfo.sh
            # substitutes the branch revision. Ignore that one line.
            left="$tmpdir/srcinfo-git-normalised"
            right="$tmpdir/srcinfo-git-checked-normalised"
            sed 's/^\tpkgver = .*/\tpkgver = IGNORED/' "$generated" >"$left"
            sed 's/^\tpkgver = .*/\tpkgver = IGNORED/' "$recipe/.SRCINFO" >"$right"
        fi
        if diff -q "$left" "$right" >/dev/null; then
            pass "$recipe/.SRCINFO is current"
        else
            fail "$recipe/.SRCINFO is stale; run packaging/scripts/update-srcinfo.sh"
            diff -u "$right" "$left" | sed 's/^/    /' >&2 || true
        fi

        for pkg in "${split_packages[@]}"; do
            if grep -qx "pkgname = $pkg" "$recipe/.SRCINFO"; then
                :
            else
                fail "$recipe/.SRCINFO is missing split package $pkg"
            fi
        done
    done
else
    printf '==> .SRCINFO check skipped\n'
fi

if [ "$skip_cargo" = 0 ]; then
    printf '==> cargo checks\n'
    export LIBCLANG_PATH=${LIBCLANG_PATH:-/usr/lib}
    for profile in minimal desktop full; do
        if "$script_dir/build-profile.sh" --check "$profile"; then
            pass "cargo check profile $profile"
        else
            fail "cargo check profile $profile"
        fi
    done
else
    printf '==> cargo checks skipped\n'
fi

if [ "$failures" -ne 0 ]; then
    printf '\n%d packaging check(s) failed\n' "$failures" >&2
    exit 1
fi
printf '\nall packaging checks passed\n'
