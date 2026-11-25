#!/usr/bin/env bash
# Automated hardware pass for an isolated niri session on a free VT.
#
# Run from a text console (for example tty3 after Ctrl+Alt+F3 + login), not from
# inside the desktop session: this script starts `$NIRI_BIN` directly on the
# current DRM device, drives it over IPC and stops it again.
#
#   ./scripts/pending-checks/hardware-validation.sh
#   NIRI_RENDERER=vulkan HDR_OUTPUT=DP-1 ./scripts/pending-checks/hardware-validation.sh
#
# HDR_OUTPUT additionally prepares the test config for HDR and runs the HDR
# clip check. Tearing, explicit-sync client behaviour, commit-timing ordering
# and multi-GPU cross-device rendering still need the visual checklist printed
# at the end; this script records the capabilities, renderer, scanout formats,
# VRR toggle, a screenshot and the renderer log evidence for them.
#
# `--check` validates the prerequisites without starting a compositor.
set -u

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
NIRI_BIN=${NIRI_BIN:-"$ROOT/target/debug/niri"}
TEST_DIR=${NIRI_TEST_DIR:-/tmp/niri-test-session}
OUTDIR=${HW_OUTDIR:-/tmp/hardware-validation}
LOG="$OUTDIR/niri.log"
SUMMARY="$OUTDIR/summary.txt"
RENDERER=${NIRI_RENDERER:-vulkan}
HDR_OUTPUT=${HDR_OUTPUT:-}
CHECK_ONLY=${1:-}

pass=0
fail=0
skip=0
declare -a results=()

record() {
    local status=$1 message=$2
    results+=("$status $message")
    printf '%-5s %s\n' "$status" "$message"
    case $status in
        PASS) pass=$((pass + 1)) ;;
        FAIL) fail=$((fail + 1)) ;;
        SKIP) skip=$((skip + 1)) ;;
    esac
}

have() { command -v "$1" >/dev/null 2>&1; }

prereqs() {
    [ -x "$NIRI_BIN" ] || { echo "missing $NIRI_BIN (build with: cargo build -p niri)" >&2; return 1; }
    local cards
    cards=$(find /dev/dri -maxdepth 1 -name 'card[0-9]*' 2>/dev/null | wc -l)
    [ "$cards" -gt 0 ] || { echo "no /dev/dri/card* devices; is this a DRM seat?" >&2; return 1; }
    if [ -n "${WAYLAND_DISPLAY:-}" ] && [ -n "${NIRI_SOCKET:-}" ]; then
        echo "warning: NIRI_SOCKET/WAYLAND_DISPLAY are set; run this from a free VT, not inside the desktop session" >&2
    fi
    echo "niri:      $NIRI_BIN ($("$NIRI_BIN" --version 2>/dev/null | head -1))"
    echo "renderer:  $RENDERER"
    echo "config:    $TEST_DIR"
    echo "output:    ${HW_OUTPUT:-first connected; set HW_OUTPUT=DP-1 to pin}"
    echo "hdr:       ${HDR_OUTPUT:-off (set HDR_OUTPUT=DP-1 to test HDR)}"
    return 0
}

if [ "$CHECK_ONLY" = "--check" ]; then
    mkdir -p "$OUTDIR"
    prereqs
    exit $?
fi

set -e
mkdir -p "$OUTDIR"
: > "$SUMMARY"

echo "==> isolated config"
HDR_OUTPUT="$HDR_OUTPUT" "$ROOT/scripts/pending-checks/isolated-session.sh" "$TEST_DIR" >"$OUTDIR/isolated.log" 2>&1 || {
    echo "isolated-session.sh failed; see $OUTDIR/isolated.log" >&2
    exit 1
}

echo "==> starting niri on this VT (renderer=$RENDERER)"
export NIRI_BIN NIRI_RENDERER="$RENDERER" RUST_LOG=${RUST_LOG:-niri=debug}
"$NIRI_BIN" -c "$TEST_DIR" >"$LOG" 2>&1 &
NIRI_PID=$!
cleanup() {
    if kill -0 "$NIRI_PID" 2>/dev/null; then
        kill -INT "$NIRI_PID" 2>/dev/null || true
        for _ in $(seq 1 40); do kill -0 "$NIRI_PID" 2>/dev/null || break; sleep 0.1; done
        kill -TERM "$NIRI_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

NIRI_SOCKET=""
for _ in $(seq 1 150); do
    NIRI_SOCKET=$(sed -n 's/.*IPC listening on: //p' "$LOG" 2>/dev/null | tail -1)
    [ -n "$NIRI_SOCKET" ] && [ -S "$NIRI_SOCKET" ] && break
    if ! kill -0 "$NIRI_PID" 2>/dev/null; then break; fi
    sleep 0.1
done
if [ -z "$NIRI_SOCKET" ] || [ ! -S "$NIRI_SOCKET" ]; then
    record FAIL "compositor did not start (see $LOG)"
    printf '%s\n' "${results[@]}" > "$SUMMARY"
    exit 1
fi
export NIRI_SOCKET

echo "==> checking the renderer and capabilities"
if grep -q 'using the Vulkan renderer' "$LOG"; then
    record PASS "Vulkan renderer selected"
elif [ "$RENDERER" = gles ] && grep -q 'using the GLES renderer\|using the EGL' "$LOG"; then
    record PASS "GLES renderer selected"
else
    record FAIL "renderer selection not found in the log"
fi
if grep -q 'renderer plan:' "$LOG"; then
    record PASS "renderer plan logged: $(grep -o 'renderer plan:.*' "$LOG" | tail -1)"
else
    record SKIP "renderer plan line not present"
fi

"$NIRI_BIN" msg --json version >"$OUTDIR/version.json" 2>/dev/null || true
"$NIRI_BIN" msg --json capabilities >"$OUTDIR/capabilities.json" 2>/dev/null || true
"$NIRI_BIN" msg --json outputs >"$OUTDIR/outputs.json" 2>/dev/null || true
if [ -s "$OUTDIR/capabilities.json" ]; then
    if python3 - "$OUTDIR/capabilities.json" "$RENDERER" <<'PY'
import json, sys
caps = json.load(open(sys.argv[1]))
required = ["sticky", "hdr", "tearing", "fifo", "explicit_sync", "pointer_warp",
            "commit_timing", "surface_barrier", "screencopy", "ext_image_copy_capture",
            "screencast", "screenshot", "pick_color", "multi_action_binds"]
missing = [name for name in required if not caps.get(name)]
if sys.argv[2] == "vulkan" and not caps.get("vulkan"):
    missing.append("vulkan")
if missing:
    print("missing capabilities: " + ", ".join(missing))
    sys.exit(1)
print("capabilities ok: " + ", ".join(required) + (" + vulkan" if sys.argv[2] == "vulkan" else ""))
PY
    then
        record PASS "capability report includes every planned feature"
    else
        record FAIL "capability report is incomplete"
    fi
else
    record FAIL "niri msg capabilities produced no JSON"
fi

echo "==> outputs, VRR and scanout"
if python3 - "$OUTDIR/outputs.json" "${HW_OUTPUT:-}" <<'PY'
import json, sys
outputs = json.load(open(sys.argv[1]))
if not isinstance(outputs, dict) or not outputs:
    print("no outputs")
    sys.exit(1)
want = sys.argv[2]
name = want if want in outputs else next(iter(outputs))
out = outputs[name]
print(f"{name}: mode={out.get('current_mode')} vrr_supported={out.get('vrr_supported')} "
      f"vrr_enabled={out.get('vrr_enabled')} max_bpc={out.get('max_bpc')} "
      f"logical={out.get('logical')}")
open(sys.argv[1] + ".name", "w").write(name)
PY
then
    OUTPUT=$(cat "$OUTDIR/outputs.json.name" 2>/dev/null || true)
    if [ -n "$OUTPUT" ]; then
        record PASS "output $OUTPUT enumerated"
        if python3 - "$OUTDIR/outputs.json" "$OUTPUT" <<'PY'
import json, sys
out = json.load(open(sys.argv[1]))[sys.argv[2]]
sys.exit(0 if out.get("vrr_supported") else 1)
PY
        then
            before=$("$NIRI_BIN" msg --json outputs 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]]["vrr_enabled"])' "$OUTPUT" 2>/dev/null || echo false)
            "$NIRI_BIN" msg output "$OUTPUT" vrr on >/dev/null 2>&1 || true
            sleep 0.5
            after=$("$NIRI_BIN" msg --json outputs 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]]["vrr_enabled"])' "$OUTPUT" 2>/dev/null || echo false)
            if [ "$after" = "True" ] || [ "$after" = "true" ]; then
                record PASS "VRR toggled on and reported enabled"
            else
                record FAIL "VRR on was requested but vrr_enabled stayed $after"
            fi
            "$NIRI_BIN" msg output "$OUTPUT" vrr off >/dev/null 2>&1 || true
            if [ "$before" = "True" ] || [ "$before" = "true" ]; then
                "$NIRI_BIN" msg output "$OUTPUT" vrr on >/dev/null 2>&1 || true
            fi
        else
            record SKIP "output does not advertise VRR"
        fi
    else
        record FAIL "could not determine an output name"
    fi
else
    record FAIL "no connected outputs reported"
fi

if grep -q 'keeping renderable 10-bit format' "$LOG"; then
    record PASS "10-bit scanout formats kept: $(grep -o 'keeping renderable 10-bit format.*' "$LOG" | tail -1)"
else
    record SKIP "no 10-bit scanout format kept (only expected for HDR outputs)"
fi
if grep -q 'explicit sync enabled' "$LOG"; then
    record PASS "explicit sync enabled for the primary GPU"
else
    record FAIL "explicit sync was not enabled"
fi
if grep -q 'updated HDR signalling' "$LOG"; then
    record PASS "HDR signalling updated: $(grep -o 'updated HDR signalling.*' "$LOG" | tail -1)"
else
    if [ -n "$HDR_OUTPUT" ]; then
        record FAIL "HDR was requested but no signalling update was logged"
    else
        record SKIP "HDR not requested (set HDR_OUTPUT=DP-1)"
    fi
fi
devices=$(grep -c 'adding device:' "$LOG" 2>/dev/null || true)
if [ "${devices:-0}" -ge 2 ]; then
    record PASS "multi-device setup seen: $devices DRM devices"
else
    record SKIP "single DRM device in this machine/session"
fi

echo "==> screenshot capture"
SHOT="$OUTDIR/screenshot.png"
if "$NIRI_BIN" msg action screenshot-screen --stdout --write-to-disk=false >"$SHOT" 2>"$OUTDIR/screenshot.err"; then
    if [ -s "$SHOT" ] && [ "$(head -c 8 "$SHOT" | od -An -tx1 | tr -d ' \n')" = "89504e470d0a1a0a" ]; then
        record PASS "screenshot captured through IPC ($(stat -c %s "$SHOT") bytes)"
    else
        record FAIL "screenshot output is not a PNG"
    fi
else
    record FAIL "screenshot IPC failed: $(cat "$OUTDIR/screenshot.err" 2>/dev/null | head -1)"
fi

if [ -n "$HDR_OUTPUT" ] && [ -s "${HDR_CLIP:-/home/franc/Downloads/hdr10+test_lake_2021_02_01.mp4}" ]; then
    echo "==> HDR clip"
    if HDR_SECONDS=${HDR_SECONDS:-10} NIRI_LOG="$LOG" "$ROOT/scripts/pending-checks/hdr.sh" >"$OUTDIR/hdr.log" 2>&1; then
        if grep -q 'niri: HDR signalling enabled' "$OUTDIR/hdr.log"; then
            record PASS "HDR clip played with signalling enabled"
        else
            record FAIL "HDR clip ran but the summary did not confirm signalling"
        fi
    else
        record FAIL "hdr.sh failed; see $OUTDIR/hdr.log"
    fi
else
    record SKIP "HDR clip not run (set HDR_OUTPUT and provide HDR_CLIP)"
fi

cat <<'CHECKLIST'

==> manual checklist (hardware-only behaviour)
- Tearing: open a fullscreen game/video with `allow-tearing true`; confirm the
  tearing-control client flips asynchronously without a RED/BLUE frame.
- Commit timing: run a client using wp_commit_timing_v1 (niri-visual-tests) and
  check the frame lands on its target presentation time without stalls.
- Multi-GPU: move an output to the secondary GPU/render node and play a video;
  check for cross-device copies and no corruption.
- Rotated outputs: set 90/270 and switch workspaces; the transition must keep
  its orientation.
- HDR mixed: with hdr mode="on", the SDR desktop must keep readable contrast;
  with mode="auto" it returns to SDR after the clip.
- Closing/opening animations, popups and xray/blur in a live Vulkan session.
- Portal screenshot/screencast through a real client.
CHECKLIST

printf '%s\n' "${results[@]}" > "$SUMMARY"
printf '\nPASS=%d FAIL=%d SKIP=%d\n' "$pass" "$fail" "$skip" | tee -a "$SUMMARY"
echo "log: $LOG"
echo "summary: $SUMMARY"
[ "$fail" -eq 0 ]
