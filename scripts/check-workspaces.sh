#!/usr/bin/env bash
# Format, check and test every Better Desktop Cargo workspace.
#
# Each `better-*` directory is its own workspace (it is excluded from niri's
# Cargo workspace). A workspace whose system libraries are absent is skipped
# with a note instead of failing the whole run, so a developer without GTK or
# PipeWire development headers can still test the pure workspaces.
#
# Usage: scripts/check-workspaces.sh [workspace ...]
#   scripts/check-workspaces.sh                  # every better-* workspace
#   scripts/check-workspaces.sh better-monitor   # selected workspaces
#
# Environment:
#   LIBCLANG_PATH  defaults to /usr/lib (bindgen for PipeWire/SPA bindings).
#   STRICT_FMT=1   treat `cargo fmt --check` drift as a failure. By default it
#                  is reported as a warning because some workspaces predate the
#                  repository-wide formatting pass and are owned elsewhere.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/.." && pwd)
cd "$root"

export LIBCLANG_PATH=${LIBCLANG_PATH:-/usr/lib}
export CARGO_TERM_COLOR=${CARGO_TERM_COLOR:-always}

# Keep this list in sync with the top-level `better-*` directories. The order
# is cheapest-first so a dependency problem shows up after the pure workspaces.
workspaces=(
    better-monitor
    better-session
    better-shot
    better-companion
    better-wallpaper
    better-portal
    better-audio
    better-settings
    better-sidepanels
    better-noctalia-plugins
)

# pkg-config modules required by each workspace. Empty means pure Rust.
system_dependencies() {
    case $1 in
        better-settings) printf '%s\n' gtk4 ;;
        better-audio) printf '%s\n' libpipewire-0.3 ;;
        better-wallpaper) printf '%s\n' wayland-client xkbcommon ;;
        better-portal) printf '%s\n' wayland-client xkbcommon ;;
        better-sidepanels) printf '%s\n' wayland-client cairo xkbcommon ;;
        *) printf '%s\n' ;;
    esac
}

missing_modules() {
    local workspace=$1 module missing=()
    while IFS= read -r module; do
        [ -n "$module" ] || continue
        if ! pkg-config --exists "$module" 2>/dev/null; then
            missing+=("$module")
        fi
    done < <(system_dependencies "$workspace")
    if [ "${#missing[@]}" -gt 0 ]; then
        printf '%s\n' "${missing[*]}"
    fi
}

# A cargo check failure caused by a missing system library is a skip, not an
# error. Anything else (type error, bad manifest, missing crate) fails.
is_missing_system_dependency() {
    grep -qiE \
        'pkg-config .* (exited with status|was not found)|was not found in the pkg-config search path|could not find system library|Unable to find libclang|requires .* which was not found'
}

selected=()
if [ "$#" -gt 0 ]; then
    selected=("$@")
else
    selected=("${workspaces[@]}")
fi

if ! command -v cargo >/dev/null 2>&1; then
    printf 'error: cargo is not installed\n' >&2
    exit 1
fi

ran=0
skipped=()
warnings=()
failed=()

run_step() {
    local output status=0
    output=$("$@" 2>&1) || status=$?
    if [ "$status" -eq 0 ]; then
        return 0
    fi
    printf '%s\n' "$output" >&2
    return "$status"
}

for workspace in "${selected[@]}"; do
    manifest="$workspace/Cargo.toml"
    if [ ! -f "$manifest" ]; then
        if [ -d "$workspace" ]; then
            printf 'skip %-24s no Cargo.toml\n' "$workspace"
            skipped+=("$workspace (no Cargo.toml)")
            continue
        fi
        printf 'error: unknown workspace %s\n' "$workspace" >&2
        failed+=("$workspace")
        continue
    fi
    if [ ! -f "$workspace/Cargo.lock" ] && [ ! -f "$workspace/src/lib.rs" ]; then
        printf 'skip %-24s no standalone crate sources\n' "$workspace"
        skipped+=("$workspace (no crate sources)")
        continue
    fi

    missing=$(missing_modules "$workspace")
    if [ -n "$missing" ]; then
        printf 'skip %-24s missing pkg-config: %s\n' "$workspace" "$missing"
        skipped+=("$workspace (missing: $missing)")
        continue
    fi

    printf '==> %s\n' "$workspace"
    if ! fmt_output=$(cargo fmt --manifest-path "$manifest" --all -- --check 2>&1); then
        if [ "${STRICT_FMT:-0}" = 1 ]; then
            printf '%s\n' "$fmt_output" >&2
            failed+=("$workspace (fmt)")
            continue
        fi
        warnings+=("$workspace (fmt drift; run cargo fmt --manifest-path $manifest)")
    fi

    check_log=$(mktemp "${TMPDIR:-/tmp}/check-workspace-cargo.XXXXXX")
    if ! cargo check --manifest-path "$manifest" --all-targets >"$check_log" 2>&1; then
        cat "$check_log" >&2
        if is_missing_system_dependency <"$check_log"; then
            printf 'skip %-24s cargo check needs a system library not found by pkg-config\n' \
                "$workspace"
            skipped+=("$workspace (system dependency)")
            rm -f "$check_log"
            continue
        fi
        rm -f "$check_log"
        failed+=("$workspace (check)")
        continue
    fi
    rm -f "$check_log"

    if ! run_step cargo test --manifest-path "$manifest"; then
        failed+=("$workspace (test)")
        continue
    fi
    ran=$((ran + 1))
done

printf '\n'
for entry in "${warnings[@]}"; do
    printf 'warning: %s\n' "$entry"
done
for entry in "${skipped[@]}"; do
    printf 'skipped: %s\n' "$entry"
done
if [ "${#failed[@]}" -gt 0 ]; then
    for entry in "${failed[@]}"; do
        printf 'FAILED: %s\n' "$entry" >&2
    done
    exit 1
fi
printf 'checked %d workspace(s), skipped %d, %d warning(s)\n' \
    "$ran" "${#skipped[@]}" "${#warnings[@]}"
