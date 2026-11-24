#!/usr/bin/env bash
# Regenerate .SRCINFO for every packaging recipe in this tree.
#
# This only writes local files; it never publishes to the AUR. For the -git
# recipe it substitutes a real pkgver derived from integration/elite-base when
# the branch is available locally, because makepkg --printsrcinfo does not run
# pkgver() without a source checkout.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$script_dir/lib.sh"

root=$(repo_root)

recipes=(
    packaging/arch
    packaging/aur/niri-better
    packaging/aur/niri-better-git
)

for recipe in "${recipes[@]}"; do
    dir=$root/$recipe
    [ -f "$dir/PKGBUILD" ] || die "missing $dir/PKGBUILD"
    # Local recipe files (desktop entry, meta README/install) must travel with
    # the PKGBUILD. Fail early instead of emitting a .SRCINFO whose sources are
    # missing.
    for local_file in better-settings.desktop better-desktop.README.md better-desktop.install; do
        [ -f "$dir/$local_file" ] || die "missing $dir/$local_file"
    done
    printf '==> %s/.SRCINFO\n' "$recipe"
    (
        cd "$dir"
        makepkg --printsrcinfo >.SRCINFO.tmp
        mv .SRCINFO.tmp .SRCINFO
    )
done

git_recipe=$root/packaging/aur/niri-better-git/.SRCINFO
if git -C "$root" rev-parse --verify --quiet integration/elite-base >/dev/null; then
    branch_version="r$(git -C "$root" rev-list --count integration/elite-base).g$(git -C "$root" rev-parse --short=7 integration/elite-base)"
    sed -i "s/^\tpkgver = .*/\tpkgver = ${branch_version}/" "$git_recipe"
    printf '==> niri-better-git pkgver set to %s\n' "$branch_version"
else
    printf 'warning: integration/elite-base not found; leaving niri-better-git pkgver as generated\n' >&2
fi
