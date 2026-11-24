#!/usr/bin/env bash
# Build or check one installation profile.
#
# Usage:
#   packaging/scripts/build-profile.sh [--check|--build] [--with-apps|--no-apps] PROFILE
#   packaging/scripts/build-profile.sh --list
#
# PROFILE is a file stem in packaging/profiles (minimal, desktop, full) or a
# path to a profile .toml file.
#
# niri is always built with --no-default-features plus exactly the features
# listed in the profile, so changing Cargo.toml's `default` cannot silently
# change a profile. Companion crates are built from their own workspaces.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
source "$script_dir/lib.sh"

root=$(repo_root)
profile_arg=
mode=build
with_apps=
with_apps_set=0

usage() {
    sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

list_profiles() {
    local file
    for file in "$root"/packaging/profiles/*.toml; do
        [ -f "$file" ] || continue
        printf '%-10s %s\n' "$(profile_name "$file")" "$(profile_description "$file")"
    done
}

while [ "$#" -gt 0 ]; do
    case $1 in
        --check)
            mode=check
            shift
            ;;
        --build)
            mode=build
            shift
            ;;
        --with-apps)
            with_apps=1
            with_apps_set=1
            shift
            ;;
        --no-apps)
            with_apps=0
            with_apps_set=1
            shift
            ;;
        --list)
            list_profiles
            exit 0
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        -*)
            die "unknown option: $1 (try --help)"
            ;;
        *)
            [ -z "$profile_arg" ] || die "only one profile may be given"
            profile_arg=$1
            shift
            ;;
    esac
done

[ -n "$profile_arg" ] || {
    usage >&2
    die "a profile is required"
}

case $profile_arg in
    */*) profile=$profile_arg ;;
    *) profile=$root/packaging/profiles/$profile_arg.toml ;;
esac
[ -f "$profile" ] || die "no such profile: $profile_arg (try --list)"

name=$(profile_name "$profile")
mapfile -t features < <(profile_features "$profile")
mapfile -t apps < <(profile_apps "$profile")

[ "${#features[@]}" -gt 0 ] || features=()
[ "${#apps[@]}" -gt 0 ] || apps=()

validate_features "$root" "$name" "${features[@]}"
validate_apps "$root" "$name" "${apps[@]}"

if [ "$with_apps_set" = 0 ]; then
    if [ "$mode" = build ]; then
        with_apps=1
    else
        with_apps=0
    fi
fi

export LIBCLANG_PATH=${LIBCLANG_PATH:-/usr/lib}
if [ -z "${NIRI_BUILD_COMMIT:-}" ] && git -C "$root" rev-parse --short HEAD >/dev/null 2>&1; then
    export NIRI_BUILD_COMMIT
    NIRI_BUILD_COMMIT=$(git -C "$root" rev-parse --short HEAD)
fi

feature_arg=
if [ "${#features[@]}" -gt 0 ]; then
    feature_arg=$(IFS=,; printf '%s' "${features[*]}")
fi

printf '==> profile %s (%s): %s\n' "$name" "$mode" "$(profile_description "$profile")"
printf '    features: %s\n' "${feature_arg:-<none>}"
printf '    apps:     %s\n' "$(IFS=' '; printf '%s' "${apps[*]}")"

niri_args=(-p niri --no-default-features)
[ -n "$feature_arg" ] && niri_args+=(--features "$feature_arg")
if [ "$mode" = check ]; then
    niri_args=(check "${niri_args[@]}")
else
    niri_args=(build --release "${niri_args[@]}")
fi

(
    cd "$root"
    cargo "${niri_args[@]}"
)

if [ "$with_apps" = 0 ]; then
    printf '==> ok: niri %s for profile %s\n' "$mode" "$name"
    exit 0
fi

action=build
cargo_mode=(--release)
if [ "$mode" = check ]; then
    action=check
    cargo_mode=()
fi

for component in "${apps[@]}"; do
    [ "$component" = niri ] && continue
    manifest=$(component_manifest_path "$root" "$component")
    printf '==> %s %s\n' "$component" "$action"
    cargo "$action" "${cargo_mode[@]}" --manifest-path "$manifest"
done

printf '==> ok: profile %s %s complete\n' "$name" "$mode"
