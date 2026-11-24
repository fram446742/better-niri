#!/usr/bin/env bash
# Exercise better-portal from a user session bus. Build it first:
#   cargo build --manifest-path better-portal/Cargo.toml
set -u
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

say "Screenshot"
gdbus call --session --dest org.freedesktop.impl.portal.desktop.better \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Screenshot.Screenshot \
  "" "" "file://$OUT" 2>&1 || echo "Screenshot call failed (check /tmp/better-portal.log)"

say "PickColor"
gdbus call --session --dest org.freedesktop.impl.portal.desktop.better \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Screenshot.PickColor "" "" 2>&1 || true

say "ScreenCast (create -> select sources -> start -> close)"
gdbus call --session --dest org.freedesktop.impl.portal.desktop.better \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.ScreenCast.CreateSession \
  "" "" "" 2>&1 || true

if [ -f "$OUT" ]; then echo "PASS: screenshot at $OUT"; else echo "SKIP/FAIL: no screenshot written"; fi
echo "ScreenCast needs a real PipeWire client (e.g. OBS or xdg-desktop-portal test app) for a full Start; the portal proxies niri."
