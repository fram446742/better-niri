#!/usr/bin/env bash
# Clone the user's niri config into an isolated directory for testing on tty3.
# niri accepts a directory for -c: it loads <dir>/config.kdl and resolves
# `include` relative to it.
set -euo pipefail
LOG=${PENDING_LOG:-/tmp/pending-isolated.log}
exec > >(tee -a "$LOG") 2>&1
DIR=${1:-/tmp/niri-test-session}
NIRI_BIN=${NIRI_BIN:-/home/franc/Projects/better-niri/better-niri/target/debug/niri}
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
  python3 "$(dirname "$0")/hdr-config.py" "$HDR_OUTPUT" "$DIR"
fi

if grep -RqE 'hdr([[:space:]]|$)' "$DIR" --include='*.kdl'; then
  cat <<'MSG'
NOTE: this test config enables HDR. The Vulkan renderer does not yet composite
SDR content into the HDR blend space, so text/desktop may look rough or
differently antialiased compared with the SDR session. Re-run
isolated-session.sh WITHOUT HDR_OUTPUT (fresh copy) for visual/text tests.
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
  instance or a fixed D-Bus name (vicinae, a second better-portal/xdg portal,
  screen recorders). Two better-portal instances cannot own
  org.freedesktop.impl.portal.desktop.better at once; for ScreenCast tests
  stop the main one or run a private session bus.
- Desktop portal/screen recording from the main session captures the main
  output, not this test session; run the portal inside the test session or
  use a private bus (dbus-run-session).
MSG
