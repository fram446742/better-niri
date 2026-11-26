#!/usr/bin/env bash
# Clone the user's niri config into an isolated directory for testing on tty3.
# niri accepts a directory for -c: it loads <dir>/config.kdl and resolves
# `include` relative to it.
set -euo pipefail
LOG=${PENDING_LOG:-/tmp/pending-isolated.log}
exec > >(tee -a "$LOG") 2>&1
DIR=${1:-/tmp/niri-test-session}
NIRI_BIN=${NIRI_BIN:-/home/franc/Projects/loom-niri/loom-niri/target/debug/niri}
[ -x "$NIRI_BIN" ] || NIRI_BIN=$(command -v niri || true)
case "$DIR" in /|"$HOME"|"") echo "refusing to use $DIR"; exit 1;; esac
SRC="$HOME/.config/niri"
[ -d "$SRC" ] || { echo "no $SRC"; exit 1; }
rm -rf -- "$DIR"
mkdir -p "$DIR"
cp -a "$SRC/." "$DIR/"
echo "cloned $SRC -> $DIR"

# Optional HDR override: HDR_OUTPUT=DP-1 inserts an hdr node into the existing
# output block (niri uses the first matching block, so appending a second one
# has no effect).
if [ -n "${HDR_OUTPUT:-}" ]; then
  HDR_MODE=${HDR_MODE:-auto} python3 "$(dirname "$0")/hdr-config.py" "$HDR_OUTPUT" "$DIR"
  echo "HDR prepared for $HDR_OUTPUT with mode=${HDR_MODE:-auto}"
fi

if grep -RqE 'hdr([[:space:]]|$)' "$DIR" --include='*.kdl'; then
  cat <<'MSG'
NOTE: this test config enables HDR. With `hdr mode="on"` the whole desktop is
composited into PQ/BT.2020, so SDR content looks different (dimmer or
washed out on many monitors) by design; `mode="auto"` (the default used by
this script) only switches to HDR while fullscreen HDR content is shown and
keeps the desktop SDR. Re-run isolated-session.sh WITHOUT HDR_OUTPUT for a
fresh SDR copy.
MSG
fi

if [ -n "$NIRI_BIN" ] && [ -x "$NIRI_BIN" ]; then
  "$NIRI_BIN" validate -c "$DIR" && echo "config validates with $NIRI_BIN"
else
  echo "no niri binary found; build the local one with: cargo build -p niri"
fi

cat <<MSG

Run on tty3 (Ctrl+Alt+F3) WITHOUT --session, so the main session's
systemd/D-Bus environment is untouched:
    NIRI_RENDERER=vulkan "$NIRI_BIN" -c "$DIR"

Service isolation notes:
- Do not use --session: it imports the environment globally and can clobber
  the main session's systemd/D-Bus state.
- Disable or point away anything in spawn-at-startup that binds a single
  instance or a fixed D-Bus name (vicinae, a second loom-portal/xdg portal,
  screen recorders). Two loom-portal instances cannot own
  org.freedesktop.impl.portal.desktop.loom at once; for ScreenCast tests
  stop the main one or run a private session bus.
- Desktop portal/screen recording from the main session captures the main
  output, not this test session; run the portal inside the test session or
  use a private bus (dbus-run-session).
MSG
