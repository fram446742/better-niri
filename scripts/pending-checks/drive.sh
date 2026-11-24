#!/bin/sh
# Drive a running niri session (run inside the session, e.g. from foot on tty3)
# and exercise the Vulkan visuals that still have pending parity work.
# Usage: ./scripts/pending-checks/drive.sh [/path/to/niri]
set -u
NIRI=${1:-niri}
say() { printf '\n== %s ==\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

say "session"
$NIRI msg version || exit 1
$NIRI msg outputs | head -20
$NIRI msg workspaces | head -20

say "windows (open/close/move/resize)"
TERM_APP=foot; have alacritty && TERM_APP=alacritty
for _ in 1 2 3; do $NIRI msg action spawn -- sh -c "$TERM_APP -e sh -c 'sleep 120'" >/dev/null; sleep 1; done
$NIRI msg windows | head -30
$NIRI msg action focus-column-right
$NIRI msg action move-column-left
$NIRI msg action toggle-window-floating
$NIRI msg action toggle-window-floating
$NIRI msg action close-window

say "workspaces and screen transitions"
for ws in 2 1 3 1; do $NIRI msg action focus-workspace $ws >/dev/null 2>&1 || true; sleep 1; done

say "overview / carousel"
$NIRI msg action toggle-overview >/dev/null 2>&1 || true
sleep 2
$NIRI msg action toggle-overview >/dev/null 2>&1 || true

say "screenshots"
$NIRI msg action screenshot-screen 2>/dev/null || true
$NIRI msg action screenshot-window 2>/dev/null || true

cat <<'CHECK'

== visual checklist ==
- Windows: rounded corners clipped, borders and shadows visible.
- Move/resize a window: no red rectangle, window stays visible.
- Workspace switches: transition animation (configured) plays.
- Overview/carousel: opens and closes without artifacts.
- If configured: blur/xray behind windows, liquid-glass focus ring.
- Screenshot UI: open it manually with your screenshot bind, check the
  magnifier if enabled, then press Escape (it needs a real key press).
Reported expected-degraded on Vulkan until wired: window/global/scoped
shader effects, blur/xray, liquid glass, carousel panel contents, magnified
screenshot capture, screen transitions.
CHECK
