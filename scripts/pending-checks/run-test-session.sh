#!/usr/bin/env bash
# Start the isolated niri test session with full debug logging.
# Run this ON tty3; niri takes over the VT. The log survives after exit.
set -u
DIR=${1:-${NIRI_TEST_DIR:-/tmp/niri-test-session}}
NIRI_BIN=${NIRI_BIN:-/home/franc/Projects/loom-niri/loom-niri/target/debug/niri}
[ -x "$NIRI_BIN" ] || NIRI_BIN=$(command -v niri || true)
[ -n "$NIRI_BIN" ] || { echo "no niri binary; cargo build -p niri"; exit 1; }
LOG=${NIRI_LOG:-/tmp/niri-test-session.log}
: > "$LOG"
echo "config: $DIR/config.kdl"
echo "binary: $NIRI_BIN"
echo "log:    $LOG"
exec env NIRI_RENDERER="${NIRI_RENDERER:-vulkan}" RUST_LOG="${RUST_LOG:-niri=debug}" \
  "$NIRI_BIN" -c "$DIR" >>"$LOG" 2>&1
