#!/usr/bin/env bash
# HDR test helper. Needs: an HDR-capable output (EDID PQ/BT2020), mpv with
# HDR passthrough, and ffmpeg with libx265 + zscale to generate the clip.
# Usage: ./scripts/pending-checks/hdr.sh [extra mpv args]
set -u
LOG=${PENDING_LOG:-/tmp/pending-hdr.log}
exec > >(tee -a "$LOG") 2>&1
echo "logging to $LOG"
CLIP=${HDR_CLIP:-/home/franc/Downloads/hdr10+test_lake_2021_02_01.mp4}
TEST_DIR=${NIRI_TEST_DIR:-/tmp/niri-test-session}
NIRI_LOG_FILE=${NIRI_LOG:-/tmp/niri-test-session.log}
MPV_LOG=/tmp/pending-hdr-mpv.log
SUMMARY=/tmp/pending-hdr-summary.txt
: > "$SUMMARY"
DURATION=${HDR_SECONDS:-20}
command -v mpv >/dev/null || { echo "mpv missing"; exit 1; }
if [ ! -s "$CLIP" ]; then
  cat >&2 <<'MSG'
No HDR clip found (and this ffmpeg build cannot synthesize one:
zscale has "no path between colorspaces" for smpte2084, and libx265/x264/
SVT-AV1 reject 10-bit here; only VP9 10-bit encodes, but not PQ-converted).

Provide a real HDR10 sample, then rerun:
  HDR_CLIP=/path/to/hdr10-sample.mp4 ./scripts/pending-checks/hdr.sh
Any public HDR10 demo clip works (mp4/mkv, color_transfer=smpte2084).
You can download one manually, e.g. from a trusted HDR demo/test-pattern site.
MSG
  command -v ffprobe >/dev/null && { echo "ffprobe not run: no clip"; }
  exit 1
fi
if command -v ffprobe >/dev/null; then
  INFO=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height,pix_fmt,color_transfer,color_primaries -of default=nw=1:nk=1 "$CLIP" 2>/dev/null | tr '\n' ' ')
  echo "clip: $INFO (HDR10 needs smpte2084 + bt2020)"
  printf 'clip: %s\n' "$INFO" > /tmp/pending-hdr-clip.txt
fi
CONFIG=${NIRI_TEST_CONFIG:-$TEST_DIR/config.kdl}
if ! grep -qE 'hdr([[:space:]]|$)' "$CONFIG" 2>/dev/null; then
  cat <<'MSG'
WARNING: no `hdr` entry in the active config, so niri does not
advertise colour management and mpv will stay SDR. Regenerate the test copy with:
  HDR_OUTPUT=DP-1 ./scripts/pending-checks/isolated-session.sh
then restart niri. The script writes `hdr mode="auto"` by default; set
HDR_MODE=on to force the whole desktop into HDR (colours will look different).
MSG
fi
echo "playing $CLIP; expect the HDR badge in mpv and a visibly different brightness range"
cat <<'MSG'
Verification while mpv is playing:
- press `i` twice: look for HDR/PQ/BT.2020 in the mpv stats. If it says SDR /
  tone-mapped, the compositor did not take HDR and mpv converted for you.
- in the niri log (RUST_LOG=niri=debug), look for connector colour/HDR lines
  and "hdr" state changes when the fullscreen window starts.
- with `mode="auto"` the desktop returns to SDR when the HDR window closes; with
  `mode="on"` SDR desktop content stays PQ-encoded and looks different on most
  monitors by design.
- the `hdr` node persists in the test copy: re-run isolated-session.sh without
  HDR_OUTPUT to get an SDR session again.
MSG
mpv --fs --vo=gpu-next --target-colorspace-hint=yes --hdr-compute-peak=no --no-config \
  --log-file="$MPV_LOG" --msg-level=all=info,vo/gpu-next=debug "$CLIP" "$@"
RC=$?
{
  echo "mpv exit: $RC"
  echo "--- mpv HDR summary ---"
  grep -iE 'Dynamic tone map target|HDR metadata|display .*nits|peak' "$MPV_LOG" 2>/dev/null | tail -10
  echo "--- niri HDR/colour lines ($NIRI_LOG_FILE) ---"
  sed -E 's/\x1b\[[0-9;]*[A-Za-z]//g' "$NIRI_LOG_FILE" 2>/dev/null \
    | grep -iE 'hdr=true|hdr=false|updated HDR signalling|connector color capabilities' | tail -10
  echo "--- niri HDR state ---"
  if sed -E 's/\x1b\[[0-9;]*[A-Za-z]//g' "$NIRI_LOG_FILE" 2>/dev/null | grep -q 'hdr=false'; then
    echo "WARNING: niri created the DRM compositor with hdr=false (no hdr output node applied)"
  fi
  sed -E 's/\x1b\[[0-9;]*[A-Za-z]//g' "$NIRI_LOG_FILE" 2>/dev/null | grep -q 'hdr=true' \
    && echo "niri: HDR signalling enabled (hdr=true)"
  echo "--- niri border/shadow/clipping shader init ---"
  grep -E 'compiled border|Vulkan renderer|renderer plan' "$NIRI_LOG_FILE" 2>/dev/null | tail -5
} > "$SUMMARY"
echo "summary: $SUMMARY (mpv log: $MPV_LOG)"
exit $RC
