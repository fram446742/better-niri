#!/usr/bin/env bash
# Drive a running niri session (run inside the session, e.g. from foot on tty3)
# and exercise the Vulkan visuals (windows, animations, overview, capture).
# Usage: ./scripts/pending-checks/drive.sh [/path/to/niri]
set -u
TEST_DIR=${NIRI_TEST_DIR:-/tmp/niri-test-session}
LOG=${PENDING_LOG:-/tmp/pending-drive.log}
exec > >(tee -a "$LOG") 2>&1
echo "logging to $LOG"
NIRI=${NIRI_BIN:-/home/franc/Projects/better-niri/better-niri/target/debug/niri}
[ -x "$NIRI" ] || NIRI=niri
say() { printf '\n== %s ==\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }
OUTDIR=${PENDING_OUTDIR:-/tmp/pending-drive}
mkdir -p "$OUTDIR"
snap() {
  "$NIRI" msg --json version >"$OUTDIR/version.json" 2>&1 || true
  "$NIRI" msg --json outputs >"$OUTDIR/outputs.json" 2>&1 || true
  "$NIRI" msg --json workspaces >"$OUTDIR/workspaces.json" 2>&1 || true
  "$NIRI" msg --json windows >"$OUTDIR/windows.json" 2>&1 || true
}

say "session (expect isolated config $TEST_DIR/config.kdl)"
$NIRI msg version || exit 1
if grep -RqsE 'hdr[[:space:]]' "$TEST_DIR" --include='*.kdl'; then
  echo "HDR config: $(grep -RhE 'hdr[[:space:]]' "$TEST_DIR" --include='*.kdl' | head -1 | tr -s ' ')"
  echo "  (mode=on keeps the whole desktop in PQ/BT.2020 and changes how SDR colours look;"
  echo "   mode=auto only switches while fullscreen HDR content plays)"
else
  echo "HDR config: none (SDR session)"
fi
$NIRI msg outputs | head -20
$NIRI msg workspaces | head -20
snap

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

snap
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
- Closing windows: animate with the baked snapshot; opening windows fade/scale.
- Workspace switches on a rotated output: transition orientation is correct.
- Popups (menus, tooltips): blurred backdrop if a background effect is set.
Everything above, including window/global/scoped shaders, blur/xray, liquid
glass, carousel panel contents, magnified screenshot capture and screen
transitions, is expected to work on Vulkan; report anything missing.
CHECK
