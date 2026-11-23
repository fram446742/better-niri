#!/usr/bin/env bash
# Check better-niri against upstream niri for config/IPC drift and Smithay pin
# consistency. Run before and after every `git merge upstream/main`.
#
# Usage:
#   contrib/check-upstream-drift.sh                 # compare with upstream/main
#   contrib/check-upstream-drift.sh <ref>           # compare with another ref
#
# Exit code: 0 when nothing upstream has is missing here and the Smithay pin is
# consistent; 1 on drift; 2 on usage/setup errors.
#
# This is a heuristic text comparison, not a KDL or Rust parser. It is a
# reminder, not a hard correctness gate.

set -euo pipefail

ref="${1:-upstream/main}"
repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if ! git rev-parse --verify --quiet "$ref" >/dev/null; then
    echo "error: ref '$ref' not found; add the upstream remote and fetch it first:" >&2
    echo "  git remote add upstream https://github.com/niri-wm/niri.git && git fetch upstream main" >&2
    exit 2
fi

status=0

# --- config options ---------------------------------------------------------------------------

extract_options() {
    sed -e 's|//.*$||' -e 's/[{}]//g' -e 's/[;"]//g' \
        | tr ' \t' '\n\n' \
        | grep -E '^[a-z][a-z0-9-]*$' \
        | grep -vxE 'include|optional|true|false|match|proportion|fixed|off|on|left|right|top|bottom|center|auto|none' \
        | sort -u
}

upstream_cfg="$(git show "$ref:resources/default-config.kdl" 2>/dev/null || true)"
if [ -z "$upstream_cfg" ]; then
    echo "error: cannot read resources/default-config.kdl from '$ref'" >&2
    exit 2
fi

printf '%s\n' "$upstream_cfg" | extract_options > /tmp/better-niri-upstream-options.txt
extract_options < resources/default-config.kdl > /tmp/better-niri-options.txt

missing_cfg="$(comm -23 /tmp/better-niri-upstream-options.txt /tmp/better-niri-options.txt)"
extra_cfg="$(comm -13 /tmp/better-niri-upstream-options.txt /tmp/better-niri-options.txt)"

if [ -n "$missing_cfg" ]; then
    status=1
    echo "== config options upstream has, this fork's default config does not =="
    printf '  %s\n' $missing_cfg
    echo "   (our default config may simply not use them; check docs/wiki)"
    echo
fi

if [ -n "$extra_cfg" ]; then
    echo "== config options only in this fork's default config =="
    printf '  %s\n' $extra_cfg
    echo
fi

# --- IPC struct fields ------------------------------------------------------------------------

extract_ipc_fields() {
    awk '
        /^pub struct (Window|WindowLayout|Workspace|Output|KeyboardLayouts|Version) \{/ { struct=$3; in_struct=1; next }
        in_struct && /^\}/ { in_struct=0; struct=""; next }
        in_struct && /^    pub [a-z_]+:/ { name=$2; sub(":", "", name); print struct "." name }
    '
}

upstream_ipc="$(git show "$ref:niri-ipc/src/lib.rs" 2>/dev/null || true)"
if [ -z "$upstream_ipc" ]; then
    echo "error: cannot read niri-ipc/src/lib.rs from '$ref'" >&2
    exit 2
fi

printf '%s\n' "$upstream_ipc" | extract_ipc_fields | sort -u > /tmp/better-niri-upstream-ipc.txt
extract_ipc_fields < niri-ipc/src/lib.rs | sort -u > /tmp/better-niri-ipc.txt

missing_ipc="$(comm -23 /tmp/better-niri-upstream-ipc.txt /tmp/better-niri-ipc.txt)"
extra_ipc="$(comm -13 /tmp/better-niri-upstream-ipc.txt /tmp/better-niri-ipc.txt)"

if [ -n "$missing_ipc" ]; then
    status=1
    echo "== IPC fields upstream has, this fork does not =="
    printf '  %s\n' $missing_ipc
    echo "   (clients such as waybar read these)"
    echo
fi

if [ -n "$extra_ipc" ]; then
    echo "== IPC fields only in this fork =="
    printf '  %s\n' $extra_ipc
    echo
fi

# --- Smithay pin consistency ------------------------------------------------------------------

smithay_block="$(awk '
    /^\[workspace\.dependencies\.smithay\]/ { in_block=1; next }
    /^\[/ { in_block=0 }
    in_block { print }
' Cargo.toml)"
extras_block="$(awk '
    /^\[workspace\.dependencies\.smithay-drm-extras\]/ { in_block=1; next }
    /^\[/ { in_block=0 }
    in_block { print }
' Cargo.toml)"

extract_field() { printf '%s\n' "$1" | sed -n "s/^$2 = \"\(.*\)\"/\1/p" | head -1; }

smithay_git="$(extract_field "$smithay_block" git)"
smithay_rev="$(extract_field "$smithay_block" rev)"
extras_git="$(extract_field "$extras_block" git)"
extras_rev="$(extract_field "$extras_block" rev)"

if [ -z "$smithay_rev" ] || [ -z "$extras_rev" ]; then
    status=1
    echo "== Smithay pin =="
    echo "  could not read both smithay and smithay-drm-extras revisions from Cargo.toml"
    echo
elif [ "$smithay_rev" != "$extras_rev" ]; then
    status=1
    echo "== Smithay pin =="
    echo "  smithay=$smithay_rev"
    echo "  smithay-drm-extras=$extras_rev"
    echo "  both must be the same better-smithay commit"
    echo
fi

if [ "$smithay_git" != "$extras_git" ]; then
    status=1
    echo "== Smithay source =="
    echo "  smithay=$smithay_git"
    echo "  smithay-drm-extras=$extras_git"
    echo "  both must use the same better-smithay repository"
    echo
fi

if [ -n "$smithay_git" ] && [ "$smithay_git" != "https://github.com/fram446742/better-smithay.git" ]; then
    status=1
    echo "== Smithay source =="
    echo "  expected https://github.com/fram446742/better-smithay.git, found $smithay_git"
    echo
fi

# A pinned path override committed to Cargo.toml breaks clean checkouts.
if grep -q '^path = "\.\./better-smithay' Cargo.toml; then
    status=1
    echo "== Smithay pin =="
    echo "  committed path override to ../better-smithay found; use the cargo --config"
    echo "  patch command documented in docs/COMPATIBILITY.md instead"
    echo
fi

if [ "$status" -eq 0 ]; then
    echo "no drift: nothing upstream has is missing here; Smithay pin is consistent"
fi

exit "$status"
