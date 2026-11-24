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
fi
CONFIG=${NIRI_TEST_CONFIG:-$TEST_DIR/config.kdl}
if ! grep -qE 'hdr([[:space:]]|$)' "$CONFIG" 2>/dev/null; then
  cat <<'MSG'
WARNING: no `hdr` entry in the active config, so niri does not
advertise colour management and mpv will stay SDR. Add it to the test copy, then restart niri:
  output "DP-1" {
      hdr mode="on" {
          reference-luminance 203
      }
  }
MSG
fi
echo "playing $CLIP; expect the HDR badge in mpv and a visibly different brightness range"
cat <<'MSG'
Verification while mpv is playing:
- press `i` twice: look for HDR/PQ/BT.2020 in the mpv stats. If it says SDR /
  tone-mapped, the compositor did not take HDR and mpv converted for you.
- in the niri log (RUST_LOG=niri=debug), look for connector colour/HDR lines
  and "hdr" state changes when the fullscreen window starts.
- washed-out in another session is expected on winit (nested) or without an
  `hdr` output node; that is not the Vulkan output path.
MSG
exec mpv --fs --vo=gpu-next --target-colorspace-hint=yes --hdr-compute-peak=no --no-config "$CLIP" "$@"
