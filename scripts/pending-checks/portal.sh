#!/usr/bin/env bash
# Exercise better-portal from a user session bus. Build it first:
#   cargo build --manifest-path better-portal/Cargo.toml
set -u
TEST_DIR=${NIRI_TEST_DIR:-/tmp/niri-test-session}
LOG=${PENDING_LOG:-/tmp/pending-portal.log}
exec > >(tee -a "$LOG") 2>&1
echo "logging to $LOG"
BIN=${BETTER_PORTAL_BIN:-./better-portal/target/debug/better-portal}
BUS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=/run/user/$(id -u)/bus}
export DBUS_SESSION_BUS_ADDRESS=$BUS
[ -x "$BIN" ] || { echo "missing $BIN (cargo build --manifest-path better-portal/Cargo.toml)"; exit 1; }
say() { printf '\n== %s ==\n' "$*"; }
OUT=/tmp/better-portal-shot.png
rm -f "$OUT"

say "start portal (background)"
"$BIN" >/tmp/better-portal.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null' EXIT
sleep 1

say "interface introspection"
gdbus introspect --session --dest org.freedesktop.impl.portal.desktop.better \
  --object-path /org/freedesktop/portal/desktop 2>&1 | \
  grep -E 'Screenshot|PickColor|CreateSession|SelectSources|Start' | head -20

# The impl methods take an exported Request object path as the first argument
# and deliver results through the Request.Response signal. Driving that from a
# shell needs a client that exports the Request; use a real portal client or
# the portal's own integration tests instead of hand-written gdbus calls.
say "portal integration tests"
if [ -f better-portal/Cargo.toml ]; then
  cargo test --manifest-path better-portal/Cargo.toml --quiet 2>&1 | tail -5
fi

echo "Screenshot/PickColor/ScreenCast need a portal client that exports a Request object;"
echo "use a real app (screenshot keybind in a portal-aware app, OBS for ScreenCast) or the tests above."
echo "The full ScreenCast stream needs PipeWire and niri running in this session; the portal proxies niri."
