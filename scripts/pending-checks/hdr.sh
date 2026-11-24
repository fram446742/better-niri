#!/usr/bin/env bash
# HDR test helper. Needs: an HDR-capable output (EDID PQ/BT2020), mpv with
# HDR passthrough, and ffmpeg with libx265 + zscale to generate the clip.
# Usage: ./scripts/pending-checks/hdr.sh [extra mpv args]
set -u
LOG=${PENDING_LOG:-/tmp/pending-hdr.log}
exec > >(tee -a "$LOG") 2>&1
echo "logging to $LOG"
CLIP=${HDR_CLIP:-/tmp/hdr10-test.mp4}
command -v mpv >/dev/null || { echo "mpv missing"; exit 1; }
if [ ! -f "$CLIP" ]; then
  command -v ffmpeg >/dev/null || { echo "ffmpeg missing and $CLIP not present"; exit 1; }
  FILTER="zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:primaries=bt2020:transfer=smpte2084:matrix=bt2020nc,format=yuv420p10le"
  COMMON=(-hide_banner -loglevel error -y -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=20" -vf "$FILTER")
  TAGS=(-color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc)
  echo "generating HDR10 test clip at $CLIP"
  # x265 10-bit is the ideal HDR10 carrier; fall back to VP9 10-bit with HDR
  # colour tags (this ffmpeg build rejects 10-bit x264/x265/SVT-AV1).
  if ffmpeg "${COMMON[@]}" -c:v libx265 -pix_fmt yuv420p10le "${TAGS[@]}" \
      -x265-params "hdr-opt=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc" "$CLIP"; then
    echo "encoded with libx265 (HDR10)"
  elif ffmpeg "${COMMON[@]}" -c:v libvpx-vp9 -pix_fmt yuv420p10le "${TAGS[@]}" -b:v 0 -crf 24 "$CLIP"; then
    echo "encoded with libvpx-vp9 (10-bit + HDR colour tags)"
  else
    echo "no working 10-bit encoder in this ffmpeg build (tried libx265, libvpx-vp9)" >&2
    exit 1
  fi
fi
fi
echo "playing $CLIP; expect the HDR badge in mpv and a visibly different brightness range"
exec mpv --fs --target-colorspace-hint=yes --hdr-compute-peak=no --no-config "$CLIP" "$@"
