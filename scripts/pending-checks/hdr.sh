#!/usr/bin/env bash
# HDR test helper. Needs: an HDR-capable output (EDID PQ/BT2020), mpv with
# HDR passthrough, and ffmpeg with libx265 + zscale to generate the clip.
# Usage: ./scripts/pending-checks/hdr.sh [extra mpv args]
set -u
LOG=${PENDING_LOG:-/tmp/pending-hdr.log}
exec > >(tee -a "$LOG") 2>&1
echo "logging to $LOG"
CLIP=${HDR_CLIP:-/tmp/hdr10-test.mp4}
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
if ! grep -qE 'hdr([[:space:]]|$)' "$HOME/.config/niri/config.kdl" 2>/dev/null; then
  cat <<'MSG'
WARNING: no `hdr` entry in ~/.config/niri/config.kdl, so niri does not
advertise colour management and mpv will stay SDR. Add, then restart niri:
  output "DP-1" {
      hdr mode="on" {
          reference-luminance 203
      }
  }
MSG
fi
echo "playing $CLIP; expect the HDR badge in mpv and a visibly different brightness range"
exec mpv --fs --vo=gpu-next --target-colorspace-hint=yes --hdr-compute-peak=no --no-config "$CLIP" "$@"
