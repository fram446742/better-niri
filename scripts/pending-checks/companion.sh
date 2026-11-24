#!/usr/bin/env bash
# Exercise better-companion rules/park/swallow plumbing against the current
# niri session. Build: cargo build --manifest-path better-companion/Cargo.toml
set -u
TEST_DIR=${NIRI_TEST_DIR:-/tmp/niri-test-session}
LOG=${PENDING_LOG:-/tmp/pending-companion.log}
exec > >(tee -a "$LOG") 2>&1
echo "logging to $LOG"
BASE=${BETTER_COMPANION:-./better-companion/target/debug}
SOCK=/tmp/better-companion-test.sock
export BETTER_COMPANION_SOCKET=$SOCK
say() { printf '\n== %s ==\n' "$*"; }
[ -x "$BASE/better-companion" ] || { echo "build better-companion first"; exit 1; }
rm -f "$SOCK"
"$BASE/better-companion" --socket "$SOCK" >/tmp/better-companion.log 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null; rm -f "$SOCK"' EXIT
sleep 1

ctl() { "$BASE/companionctl" --socket "$SOCK" "$@"; }
say "status"; ctl status || true
say "namespaces"; ctl list || true
say "compositor queries"
ctl call compositor version || true
ctl call compositor outputs || true
ctl call compositor workspaces || true
say "rule lifecycle"
ctl call rules set '{"name":"test-rule","match":{"app_id":"foot"},"steps":[{"namespace":"compositor","action":"focused-window","params":{}}]}' || true
ctl call rules list || true
ctl call rules explain '{"window":1}' || true
ctl call rules remove '{"name":"test-rule"}' || true
say "parking"
ctl call parker park || true
ctl call parker list || true
say "window find / run-or-raise"
ctl call windows find '{"app_id":"foot"}' || true
ctl call windows run-or-raise '{"app_id":"foot","command":"foot"}' || true
say "swallow"
ctl call swallow status || true
echo "Swallow needs a child launched from a terminal after: ctl call swallow enable '{\"app_id\":\"foot\"}'"
