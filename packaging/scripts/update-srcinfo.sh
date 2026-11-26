#!/usr/bin/env bash
# Regenerate .SRCINFO for every packaging recipe in this tree.
#
# This only writes local files; it never publishes to the AUR. For the -git
# recipe it substitutes a real pkgver derived from main when
# the branch is available locally, because makepkg --printsrcinfo does not run
# pkgver() without a source checkout.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$script_dir/lib.sh"

root=$(repo_root)

recipes=(
    packaging/arch
    packaging/aur/loom-niri-git
)

for recipe in "${recipes[@]}"; do
    dir=$root/$recipe
    [ -f "$dir/PKGBUILD" ] || die "missing $dir/PKGBUILD"
    # Local recipe files (desktop entry, meta README/install) must travel with
    # the PKGBUILD. Fail early instead of emitting a .SRCINFO whose sources are
    # missing.
    [ -f "$dir/loom-settings.desktop" ] || die "missing $dir/loom-settings.desktop"
    if [ "$recipe" = "packaging/arch" ]; then
        [ -f "$dir/loom-desktop.install" ] || die "missing $dir/loom-desktop.install"
        [ -f "$dir/loom-desktop.README.md" ] || die "missing $dir/loom-desktop.README.md"
    else
        [ -f "$dir/loom-desktop-git.install" ] || die "missing $dir/loom-desktop-git.install"
        [ -f "$dir/loom-desktop-git.README.md" ] || die "missing $dir/loom-desktop-git.README.md"
    fi
    printf '==> %s/.SRCINFO\n' "$recipe"
    (
        cd "$dir"
        makepkg --printsrcinfo >.SRCINFO.tmp
        mv .SRCINFO.tmp .SRCINFO
    )
done

git_recipe=$root/packaging/aur/loom-niri-git/.SRCINFO
if git -C "$root" rev-parse --verify --quiet main >/dev/null; then
    branch_version="r$(git -C "$root" rev-list --count main).g$(git -C "$root" rev-parse --short=7 main)"
    sed -i "s/^\tpkgver = .*/\tpkgver = ${branch_version}/" "$git_recipe"
    printf '==> loom-niri-git pkgver set to %s\n' "$branch_version"
else
    printf 'warning: main not found; leaving loom-niri-git pkgver as generated\n' >&2
fi
