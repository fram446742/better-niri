#!/usr/bin/env bash
# Clone the user's niri config into an isolated directory for testing on tty3.
# niri accepts a directory for -c: it loads <dir>/config.kdl and resolves
# `include` relative to it.
set -euo pipefail
LOG=${PENDING_LOG:-/tmp/pending-isolated.log}
exec > >(tee -a "$LOG") 2>&1
DIR=${1:-/tmp/niri-test-session}
case "$DIR" in /|"$HOME"|"") echo "refusing to use $DIR"; exit 1;; esac
SRC="$HOME/.config/niri"
[ -d "$SRC" ] || { echo "no $SRC"; exit 1; }
rm -rf -- "$DIR"
mkdir -p "$DIR"
cp -a "$SRC/." "$DIR/"
echo "cloned $SRC -> $DIR"

# Optional HDR override: HDR_OUTPUT=DP-1 adds an hdr node if none exists.
if [ -n "${HDR_OUTPUT:-}" ] && ! grep -qE 'hdr([[:space:]]|$)' "$DIR/config.kdl"; then
  printf '\noutput "%s" {\n    hdr mode="on" {\n        reference-luminance 203\n    }\n}\n' \
    "$HDR_OUTPUT" >> "$DIR/config.kdl"
  echo "added hdr mode=on for $HDR_OUTPUT"
fi

if command -v niri >/dev/null; then
  niri validate -c "$DIR" && echo "config validates"
fi

cat <<MSG

Run on tty3 (Ctrl+Alt+F3) WITHOUT --session, so the main session's
systemd/D-Bus environment is untouched:
    NIRI_RENDERER=vulkan niri -c "$DIR"

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
