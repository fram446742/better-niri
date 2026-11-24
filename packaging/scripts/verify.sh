#!/usr/bin/env bash
# Verify the packaging tree.
#
# Checks:
#   - bash syntax for every script, PKGBUILD and .install file;
#   - shellcheck (when installed);
#   - profiles against cargo metadata features, component manifests and binaries;
#   - every binary installed by the PKGBUILDs exists as a Cargo bin target;
#   - portal/session artifacts referenced by the PKGBUILDs exist;
#   - .SRCINFO presence and makepkg parsing;
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
            sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            die "unknown option: $1 (try --help)"
            ;;
    esac
done

printf '==> syntax\n'
shell_files=(
    packaging/scripts/build-profile.sh
    packaging/scripts/lib.sh
    packaging/scripts/manifest-report.sh
    packaging/scripts/update-srcinfo.sh
    packaging/scripts/verify.sh
    packaging/arch/PKGBUILD
    packaging/arch/better-desktop/PKGBUILD
    packaging/arch/better-desktop/better-desktop.install
    packaging/aur/niri-better/PKGBUILD
    packaging/aur/niri-better-git/PKGBUILD
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

printf '==> binary inventory\n'
extract_pkgbuild_array() {
    local file=$1 var=$2
    bash -c 'source "$1" >/dev/null 2>&1 || exit 1; declare -p "$2"' _ "$file" "$var" |
        grep -o '"[^"]*"' | tr -d '"'
}

declare -A metadata_bins=()
for component in $(all_components); do
    manifest=$(component_manifest_path "$root" "$component") ||
        die "component $component has no manifest"
    if [ ! -f "$manifest" ]; then
        fail "missing manifest $manifest"
        continue
    fi
    while IFS= read -r bin; do
        [ -n "$bin" ] && metadata_bins["$bin"]=1
    done < <(manifest_binary_names "$manifest")
done

declare -A pkgbuild_bins=()
for file in packaging/arch/PKGBUILD packaging/aur/niri-better/PKGBUILD packaging/aur/niri-better-git/PKGBUILD; do
    if [ ! -f "$file" ]; then
        fail "missing $file"
        continue
    fi
    while IFS= read -r bin; do
        [ -n "$bin" ] && pkgbuild_bins["$bin"]=1
    done < <(extract_pkgbuild_array "$file" _bins)
done

if [ "${#metadata_bins[@]}" -eq 0 ]; then
    fail "cargo metadata returned no binary targets"
else
    pass "cargo metadata declares ${#metadata_bins[@]} binary targets"
fi
if [ "${#pkgbuild_bins[@]}" -eq 0 ]; then
    fail "PKGBUILDs declare no binaries"
else
    pass "PKGBUILDs install ${#pkgbuild_bins[@]} binaries"
fi
for bin in "${!pkgbuild_bins[@]}"; do
    if [ -z "${metadata_bins[$bin]:-}" ]; then
        fail "PKGBUILD installs '$bin' but no Cargo bin target produces it"
    fi
done
for component in $(all_components); do
    while IFS= read -r bin; do
        if [ -z "${pkgbuild_bins[$bin]:-}" ]; then
            fail "component $component produces '$bin' but the PKGBUILD does not install it"
        fi
    done < <(component_binaries "$component")
done

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

if grep -q 'depends=(niri-better)' packaging/arch/better-desktop/PKGBUILD; then
    pass "better-desktop depends on niri-better"
else
    fail "better-desktop must depend on niri-better"
fi

printf '==> scripts\n'
for file in packaging/scripts/*.sh; do
    if [ -x "$file" ]; then
        pass "executable $file"
    else
        fail "not executable: $file"
    fi
done

printf '==> docs\n'
for section in 'Compatibility matrix' 'Installation profiles' 'Portal setup' 'Vulkan' 'AUR'; do
    if grep -qF "$section" docs/PACKAGING.md; then
        pass "docs/PACKAGING.md has '$section'"
    else
        fail "docs/PACKAGING.md is missing '$section'"
    fi
done

if [ "$skip_srcinfo" = 0 ] && command -v makepkg >/dev/null 2>&1; then
    printf '==> .SRCINFO\n'
    for dir in packaging/arch packaging/aur/niri-better packaging/aur/niri-better-git packaging/arch/better-desktop; do
        if [ ! -f "$dir/.SRCINFO" ]; then
            fail "missing $dir/.SRCINFO"
            continue
        fi
        if (cd "$dir" && makepkg --printsrcinfo >/dev/null 2>&1); then
            pass "makepkg parses $dir/PKGBUILD"
        else
            fail "makepkg cannot parse $dir/PKGBUILD"
        fi
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
