#!/usr/bin/env bash
# Smoke-test loom-portal on a private session bus.
#
# Starts a private dbus-daemon, runs loom-portal on it without a compositor,
# lists the backend bus name, and calls Screenshot once. Without niri the call
# is expected to return response 2 (other failure), which proves the method and
# its request lifecycle are reachable.
#
# Use --dry-run to print the plan without starting a bus, which is also useful
# on machines without dbus-daemon/busctl/gdbus.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/.." && pwd)

bus_name=org.freedesktop.impl.portal.desktop.loom
object_path=/org/freedesktop/portal/desktop
request_path=/org/freedesktop/portal/desktop/request/smoke/test
binary=${LOOM_PORTAL_BIN:-}
dry_run=0
keep=0
timeout=10

usage() {
    cat <<'EOF'
Usage: scripts/smoke.sh [OPTIONS]

Options:
  --binary PATH    loom-portal binary. Default: $LOOM_PORTAL_BIN, then
                   ./target/debug/loom-portal, ./target/release/loom-portal,
                   then loom-portal from $PATH.
  --timeout SECS   Wait at most SECS for the bus name (default: 10).
  --keep           Keep the temporary bus/portal logs.
  --dry-run        Print the plan and exit without starting a bus.
  -h, --help       Show this help.

Exit status is 0 for a successful smoke test or a skipped environment.
EOF
}

die() {
    printf 'loom-portal smoke: %s\n' "$*" >&2
    exit 1
}

need_value() {
    [ "$#" -ge 2 ] || die "$1 needs a value"
}

while [ "$#" -gt 0 ]; do
    case $1 in
        --binary)
            need_value "$@"
            binary=$2
            shift 2
            ;;
        --timeout)
            need_value "$@"
            timeout=$2
            shift 2
            ;;
        --keep)
            keep=1
            shift
            ;;
        --dry-run)
            dry_run=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1 (try --help)"
            ;;
    esac
done

if [ -z "$binary" ]; then
    for candidate in \
        "$root/target/debug/loom-portal" \
        "$root/target/release/loom-portal"
    do
        if [ -x "$candidate" ]; then
            binary=$candidate
            break
        fi
    done
fi
if [ -z "$binary" ]; then
    binary=$(command -v loom-portal 2>/dev/null || true)
fi

if [ "$dry_run" = 1 ]; then
    printf 'loom-portal smoke plan\n'
    printf '  binary:      %s\n' "${binary:-<not found; build with cargo build>}"
    printf '  bus name:    %s\n' "$bus_name"
    printf '  object path: %s\n' "$object_path"
    printf '  steps:\n'
    printf '    1. start dbus-daemon --session on a private address\n'
    printf '    2. run the portal with LOOM_PORTAL_PICKER=/nonexistent... and no WAYLAND_DISPLAY\n'
    printf '    3. list %s on the private bus\n' "$bus_name"
    printf '    4. call org.freedesktop.impl.portal.Screenshot.Screenshot\n'
    printf '       and accept response 0 or 2 (2 means niri is absent, as expected)\n'
    printf '  dry run: no bus started, no portal started, nothing written\n'
    exit 0
fi

[ -n "$binary" ] || die "no loom-portal binary; run cargo build or pass --binary"
[ -x "$binary" ] || die "not executable: $binary"
command -v dbus-daemon >/dev/null 2>&1 || {
    printf 'loom-portal smoke: skipped (dbus-daemon not installed; use --dry-run)\n'
    exit 0
}
if ! command -v busctl >/dev/null 2>&1 && ! command -v gdbus >/dev/null 2>&1; then
    printf 'loom-portal smoke: skipped (neither busctl nor gdbus installed; use --dry-run)\n'
    exit 0
fi

tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/loom-portal-smoke.XXXXXX")
bus_pid=
portal_pid=
cleanup() {
    status=$?
    if [ -n "$portal_pid" ]; then
        kill "$portal_pid" 2>/dev/null || true
        wait "$portal_pid" 2>/dev/null || true
    fi
    if [ -n "$bus_pid" ]; then
        kill "$bus_pid" 2>/dev/null || true
        wait "$bus_pid" 2>/dev/null || true
    fi
    if [ "$keep" = 1 ]; then
        printf 'loom-portal smoke: kept %s\n' "$tmp_dir"
    else
        rm -rf -- "$tmp_dir"
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM

dbus-daemon --session --nofork --print-address --nopidfile \
    > "$tmp_dir/address" 2> "$tmp_dir/dbus.log" &
bus_pid=$!

address=
for _ in $(seq 1 100); do
    if [ -s "$tmp_dir/address" ]; then
        address=$(head -n 1 "$tmp_dir/address")
        [ -n "$address" ] && break
    fi
    sleep 0.05
done
[ -n "$address" ] || {
    cat "$tmp_dir/dbus.log" >&2 || true
    die "dbus-daemon did not print an address"
}

export DBUS_SESSION_BUS_ADDRESS=$address
printf 'private bus: %s\n' "$address"

env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
    LOOM_PORTAL_PICKER=/nonexistent/loom-portal-picker \
    "$binary" > "$tmp_dir/portal.log" 2>&1 &
portal_pid=$!

list_name() {
    if command -v busctl >/dev/null 2>&1; then
        busctl --address="$address" list --no-pager 2>/dev/null |
            awk -v name="$bus_name" '$1 == name { found = 1 } END { exit found ? 0 : 1 }'
        return
    fi
    gdbus call --address "$address" --dest org.freedesktop.DBus \
        --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.ListNames 2>/dev/null |
        grep -qF "'$bus_name'"
}

attempts=$((timeout * 20))
[ "$attempts" -gt 0 ] || attempts=1
owned=0
for _ in $(seq 1 "$attempts"); do
    if ! kill -0 "$portal_pid" 2>/dev/null; then
        cat "$tmp_dir/portal.log" >&2 || true
        die "loom-portal exited before owning $bus_name"
    fi
    if list_name; then
        owned=1
        break
    fi
    sleep 0.05
done
[ "$owned" = 1 ] || {
    cat "$tmp_dir/portal.log" >&2 || true
    die "loom-portal did not own $bus_name within ${timeout}s"
}

printf 'name owned: %s\n' "$bus_name"
if command -v busctl >/dev/null 2>&1; then
    busctl --address="$address" list --no-pager 2>/dev/null |
        grep -F "$bus_name" || true
fi

printf 'calling Screenshot (no compositor, response 0 or 2 expected)\n'
if command -v gdbus >/dev/null 2>&1; then
    reply=$(gdbus call --address "$address" --dest "$bus_name" \
        --object-path "$object_path" \
        --method org.freedesktop.impl.portal.Screenshot.Screenshot \
        "$request_path" "" "" "@a{sv} {}")
    printf 'reply: %s\n' "$reply"
    case $reply in
        "(uint32 0,"*|"(uint32 2,"*) ;;
        *) die "unexpected Screenshot reply" ;;
    esac
else
    reply=$(busctl --address="$address" call "$bus_name" "$object_path" \
        org.freedesktop.impl.portal.Screenshot Screenshot \
        ooss "$request_path" "" "" 0)
    printf 'reply: %s\n' "$reply"
    case $reply in
        "u 0 "*|"u 2 "*) ;;
        *) die "unexpected Screenshot reply" ;;
    esac
fi

printf 'loom-portal smoke: PASS\n'
