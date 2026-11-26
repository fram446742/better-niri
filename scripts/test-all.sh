#!/usr/bin/env bash
# Run every non-hardware test that belongs to the Loom tree:
# compositor feature matrix, headless visual-test build, every companion
# workspace, packaging, completions, Noctalia plugins and the external
# loom-nsticky checkout when present.
#
# Usage: ./scripts/test-all.sh [--quick]
#   --quick skips the slower clippy and per-feature cargo checks.
#
# Logs and a TSV summary land in $TEST_LOG_DIR (default
# /tmp/loom-test-run-<timestamp>). Exit status is non-zero if anything failed.
set -u

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
LOGDIR=${TEST_LOG_DIR:-/tmp/loom-test-run-$(date +%Y%m%d-%H%M%S)}
mkdir -p "$LOGDIR"
export LIBCLANG_PATH=${LIBCLANG_PATH:-/usr/lib}
export CARGO_TERM_COLOR=never

QUICK=0
[ "${1:-}" = "--quick" ] && QUICK=1

pass=0
fail=0
skip=0
declare -a rows=()

run() {
    local name=$1
    shift
    local log="$LOGDIR/${name//\//_}.log"
    local start end status
    start=$(date +%s)
    if timeout "${TEST_TIMEOUT:-2400}" "$@" >"$log" 2>&1; then
        status=PASS
        pass=$((pass + 1))
    else
        status=FAIL
        fail=$((fail + 1))
    fi
    end=$(date +%s)
    rows+=("$status\t$name\t$((end - start))s\t$log")
    printf '%-5s %-42s %4ss\n' "$status" "$name" "$((end - start))"
    if [ "$status" = FAIL ]; then
        tail -n 12 "$log" | sed 's/^/      /'
    fi
}

run_in() {
    local dir=$1 name=$2
    shift 2
    case $dir in /*) ;; *) dir="$ROOT/$dir" ;; esac
    run "$name" bash -c 'cd "$1" && shift && exec "$@"' bash "$dir" "$@"
}

run_skip() {
    local name=$1 reason=$2
    rows+=("SKIP\t$name\t0s\t$reason")
    skip=$((skip + 1))
    printf '%-5s %-42s %s\n' SKIP "$name" "$reason"
}

echo "Loom test run"
echo "root:  $ROOT"
echo "logs:  $LOGDIR"
echo

# --- compositor -----------------------------------------------------------

run root-fmt cargo fmt --manifest-path "$ROOT/Cargo.toml" --all --check
run root-tests cargo test --workspace --manifest-path "$ROOT/Cargo.toml"
run minimal-tests cargo test -p niri --no-default-features --manifest-path "$ROOT/Cargo.toml"
run niri-ipc-clap cargo test -p niri-ipc --features clap --manifest-path "$ROOT/Cargo.toml"
run visual-tests-build cargo build -p niri-visual-tests --manifest-path "$ROOT/Cargo.toml"

if [ "$QUICK" = 0 ]; then
    run root-clippy cargo clippy --workspace --all-targets --manifest-path "$ROOT/Cargo.toml"

    features=(agent effects navigation cursor kde dbus systemd xdp-gnome-screencast vulkan)
    for feature in "${features[@]}"; do
        run "check-feature-$feature" cargo check -p niri --no-default-features \
            --features "$feature" --manifest-path "$ROOT/Cargo.toml"
    done
    run check-full-explicit cargo check -p niri --no-default-features \
        --features "agent,dbus,systemd,xdp-gnome-screencast,kde,cursor,effects,navigation,vulkan" \
        --manifest-path "$ROOT/Cargo.toml"
fi

# --- companion applications ----------------------------------------------

apps=(
    loom-companion
    loom-portal
    loom-monitor
    loom-session
    loom-shot
    loom-wallpaper
    loom-settings
    loom-audio
    loom-sidepanels
    loom-tui
)
for app in "${apps[@]}"; do
    run_in "$app" "$app-fmt" cargo fmt --check
    run_in "$app" "$app-tests" cargo test --workspace
    if [ "$QUICK" = 0 ]; then
        run_in "$app" "$app-clippy" cargo clippy --workspace --all-targets
    fi
done

# Stack manager ported from zgavin/niri-sidepanels (own workspace).
run_in "loom-sidepanels/stack" "sidepanels-stack-fmt" cargo fmt --check
run_in "loom-sidepanels/stack" "sidepanels-stack-tests" cargo test
if [ "$QUICK" = 0 ]; then
    run_in "loom-sidepanels/stack" "sidepanels-stack-clippy" cargo clippy --all-targets
fi

# --- packaging, completions, plugins, external nsticky -------------------

run packaging-verify bash "$ROOT/packaging/scripts/verify.sh"
run completions-check python3 "$ROOT/packaging/scripts/generate-completions.py" --check
run noctalia-plugins bash "$ROOT/loom-noctalia-plugins/verify.sh"

NSTICKY=${NSTICKY_SOURCE:-$HOME/Projects/loom-nsticky/source}
[ -f "$NSTICKY/Cargo.toml" ] || NSTICKY=$HOME/Projects/better-nsticky/source
if [ -f "$NSTICKY/Cargo.toml" ]; then
    run_in "$NSTICKY" nsticky-tests cargo test --all-targets
    run_in "$NSTICKY" nsticky-fmt cargo fmt --check
else
    run_skip nsticky-tests "no checkout at $NSTICKY (set NSTICKY_SOURCE)"
fi

# --- summary --------------------------------------------------------------

SUMMARY="$LOGDIR/summary.tsv"
{
    printf 'status\tname\tduration\tlog\n'
    printf '%b\n' "${rows[@]}"
} >"$SUMMARY"

echo
printf 'PASS=%d FAIL=%d SKIP=%d\n' "$pass" "$fail" "$skip"
echo "summary: $SUMMARY"
echo "logs:    $LOGDIR"
[ "$fail" -eq 0 ]
