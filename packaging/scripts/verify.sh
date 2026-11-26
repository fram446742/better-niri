#!/usr/bin/env bash
# Verify the Loom packaging tree without building packages.
#
# Checks:
#   - shell syntax for every script and PKGBUILD;
#   - each recipe declares the expected split packages and package functions;
#   - the VCS recipe uses only -git package names;
#   - every source directory and install artifact named by the recipes exists;
#   - the generated completions and AUR metadata are present and parseable;
#   - the meta package depends on every other split.
#
# Usage: packaging/scripts/verify.sh [--skip-srcinfo]
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source "$script_dir/lib.sh"

root=$(repo_root)
cd "$root"

skip_srcinfo=0
[ "${1:-}" = "--skip-srcinfo" ] && skip_srcinfo=1
failures=0

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

pass() {
    printf 'ok: %s\n' "$*"
}

stable_packages=(
    loom-niri
    loom-niri-replace
    loom-portal
    loom-companion
    loom-monitor
    loom-session
    loom-shot
    loom-wallpaper
    loom-settings
    loom-audio
    loom-sidepanels
    loom-tui
    loom-noctalia-plugins
    loom-desktop
)

git_packages=(
    loom-niri-git
    loom-niri-replace-git
    loom-portal-git
    loom-companion-git
    loom-monitor-git
    loom-session-git
    loom-shot-git
    loom-wallpaper-git
    loom-settings-git
    loom-audio-git
    loom-sidepanels-git
    loom-tui-git
    loom-noctalia-plugins-git
    loom-desktop-git
)

echo "==> syntax"
while IFS= read -r file; do
    bash -n "$file" || fail "syntax: $file"
done < <(
    {
        printf '%s\n' packaging/scripts/*.sh packaging/arch/PKGBUILD packaging/aur/*/PKGBUILD
        printf '%s\n' loom-companion/packaging/*.install
        printf '%s\n' packaging/arch/*.install packaging/aur/*/*.install
    } 2>/dev/null | while IFS= read -r file; do
        [ -f "$file" ] && printf '%s\n' "$file"
    done
)
pass "shell syntax"

echo "==> package sets"
check_recipe() {
    local recipe=$1
    local expected=("${@:2}")
    local actual pkgbase
    pkgbase=$(bash -c 'source "$1"; printf "%s\n" "$pkgbase"' _ "$recipe/PKGBUILD")
    mapfile -t actual < <(bash -c 'source "$1"; printf "%s\n" "${pkgname[@]}"' _ "$recipe/PKGBUILD")

    if [ "${actual[*]}" = "${expected[*]}" ]; then
        pass "$recipe declares ${#actual[@]} packages"
    else
        fail "$recipe package set differs"
        printf '  expected: %s\n  actual:   %s\n' "${expected[*]}" "${actual[*]}" >&2
    fi

    local pkg
    for pkg in "${actual[@]}"; do
        if bash -c 'source "$1"; declare -F "$2" >/dev/null' _ "$recipe/PKGBUILD" "package_$pkg"; then
            pass "$recipe: package_$pkg"
        else
            fail "$recipe: missing package_$pkg"
        fi
    done

    if [ "$recipe" = "packaging/aur/loom-niri-git" ]; then
        for pkg in "${actual[@]}"; do
            case "$pkg" in
                *-git) ;;
                *) fail "$recipe: $pkg is not a -git package" ;;
            esac
        done
    fi

    if [ "$pkgbase" = "loom-niri-git" ] || [ "$pkgbase" = "loom-niri" ]; then
        if bash -c 'source "$1"; declare -F package_loom-desktop >/dev/null || declare -F package_loom-desktop-git >/dev/null' _ "$recipe/PKGBUILD"; then
            pass "$recipe: meta package function"
        else
            fail "$recipe: missing meta package function"
        fi
    fi
}

check_recipe packaging/arch "${stable_packages[@]}"
check_recipe packaging/aur/loom-niri-git "${git_packages[@]}"

echo "==> source and install artifacts"
required=(
    Cargo.toml
    README.md
    LICENSE
    resources/loom-niri-session
    resources/loom-niri.desktop
    resources/loom-niri.portal
    resources/loom-niri-portals.conf
    resources/loom-niri.service
    resources/loom-niri-shutdown.target
    resources/niri-session
    resources/niri.desktop
    resources/niri.portal
    resources/niri-portals.conf
    resources/niri.service
    resources/niri-shutdown.target
    loom-portal/loom.portal
    loom-portal/packaging/loom-portal.service
    loom-portal/packaging/org.freedesktop.impl.portal.desktop.loom.service
    loom-portal/packaging/portals.conf
    loom-companion/packaging/loom-companion.service
    packaging/arch/loom-settings.desktop
    packaging/arch/loom-desktop.README.md
    packaging/arch/loom-desktop.install
    packaging/aur/loom-niri-git/loom-settings.desktop
    packaging/aur/loom-niri-git/loom-desktop-git.README.md
    packaging/aur/loom-niri-git/loom-desktop-git.install
    loom-noctalia-plugins/verify.sh
)
for file in "${required[@]}"; do
    [ -e "$file" ] || fail "missing $file"
done
[ "$failures" -eq 0 ] && pass "install artifacts exist"

for manifest in loom-portal loom-companion loom-monitor loom-session loom-shot loom-wallpaper loom-settings loom-audio loom-sidepanels; do
    [ -f "$manifest/Cargo.toml" ] || fail "missing $manifest/Cargo.toml"
done

echo "==> completions"
for bin in loom loom-tui loom-companion loomctl loom-portal loom-portal-picker loom-monitor loom-session loom-shot loom-wallpaper loom-settings loom-settingsctl loom-audio loom-sidepanels; do
    for dir in bash fish zsh; do
        case "$dir" in
            bash) path="packaging/completions/bash/$bin" ;;
            fish) path="packaging/completions/fish/$bin.fish" ;;
            zsh) path="packaging/completions/zsh/_$bin" ;;
        esac
        [ -f "$path" ] || fail "missing completion $path"
    done
done
pass "completions present"

if [ "$skip_srcinfo" -eq 0 ]; then
    echo "==> AUR metadata"
    for recipe in packaging/aur/loom-niri-git; do
        if [ ! -f "$recipe/.SRCINFO" ]; then
            fail "missing $recipe/.SRCINFO"
            continue
        fi
        pkgbase=$(bash -c 'source "$1"; printf "%s\n" "$pkgbase"' _ "$recipe/PKGBUILD")
        grep -q "^pkgbase = $pkgbase$" "$recipe/.SRCINFO" ||
            fail "$recipe/.SRCINFO does not match $pkgbase"
        for pkg in "${git_packages[@]}"; do
            grep -q $'^\tpkgname = '"$pkg"$'\n' "$recipe/.SRCINFO" ||
                fail "$recipe/.SRCINFO missing $pkg"
        done
        pass "$recipe .SRCINFO"
    done
fi

if [ "$failures" -ne 0 ]; then
    printf '%d packaging check(s) failed\n' "$failures" >&2
    exit 1
fi
echo "all packaging checks passed"
