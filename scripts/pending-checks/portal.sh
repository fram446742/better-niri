#!/usr/bin/env bash
# Exercise loom-portal from a user session bus. Build it first:
#   cargo build --manifest-path loom-portal/Cargo.toml
set -u
TEST_DIR=${NIRI_TEST_DIR:-/tmp/niri-test-session}
LOG=${PENDING_LOG:-/tmp/pending-portal.log}
exec > >(tee -a "$LOG") 2>&1
echo "logging to $LOG"
BIN=${LOOM_PORTAL_BIN:-./loom-portal/target/debug/loom-portal}
BUS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=/run/user/$(id -u)/bus}
export DBUS_SESSION_BUS_ADDRESS=$BUS
[ -x "$BIN" ] || { echo "missing $BIN (cargo build --manifest-path loom-portal/Cargo.toml)"; exit 1; }
say() { printf '\n== %s ==\n' "$*"; }
OUT=/tmp/loom-portal-shot.png
rm -f "$OUT"

say "start portal (background)"
"$BIN" >/tmp/loom-portal.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null' EXIT
sleep 1

say "interface introspection"
gdbus introspect --session --dest org.freedesktop.impl.portal.desktop.loom \
  --object-path /org/freedesktop/portal/desktop 2>&1 | \
  grep -E 'Screenshot|PickColor|CreateSession|SelectSources|Start' | head -20

# The impl methods take an exported Request object path as the first argument
# and deliver results through the Request.Response signal. Driving that from a
# shell needs a client that exports the Request; use a real portal client or
# the portal's own integration tests instead of hand-written gdbus calls.
say "screenshot through xdg-desktop-portal"
if gdbus introspect --session --dest org.freedesktop.portal.Desktop \
    --object-path /org/freedesktop/portal/desktop >/dev/null 2>&1; then
  monitor=/tmp/pending-portal-response.log
  : >"$monitor"
  timeout 25 dbus-monitor --session \
    "type='signal',interface='org.freedesktop.portal.Request',member='Response'" \
    >"$monitor" 2>&1 &
  mon_pid=$!
  sleep 1
  gdbus call --session --dest org.freedesktop.portal.Desktop \
    --object-path /org/freedesktop/portal/desktop \
    --method org.freedesktop.portal.Screenshot.Screenshot \
    "" "{'interactive': <false>}" 2>&1 || true
  sleep 6
  kill "$mon_pid" 2>/dev/null
  grep -A2 "member=Response" "$monitor" | head -8
  if grep -q "uint32 0" "$monitor"; then
    echo "Screenshot request succeeded (response 0)"
    grep -o "'uri': <'[^']*'>" "$monitor" | head -1
  else
    echo "No successful Response seen; check the frontend's portal selection and /tmp/loom-portal.log"
  fi
else
  echo "xdg-desktop-portal frontend is not running on this bus; backend-only checks above."
  echo "Start a session with xdg-desktop-portal and configure better via portals.conf."
fi

say "portal integration tests"
if [ -f loom-portal/Cargo.toml ]; then
  cargo test --manifest-path loom-portal/Cargo.toml 2>&1 | grep -E '^test result|Running|FAILED' | tail -12
fi

echo "Direct impl calls need a client that exports a Request object; the frontend screenshot"
echo "above covers that path. The full ScreenCast stream needs PipeWire and niri in this"
echo "session (OBS, or a portal test app); the portal proxies niri."
