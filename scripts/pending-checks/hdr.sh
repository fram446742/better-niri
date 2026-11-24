#!/bin/sh
# HDR test helper. Needs: an HDR-capable output (EDID PQ/BT2020), mpv with
# HDR passthrough, and ffmpeg with libx265 + zscale to generate the clip.
# Usage: ./scripts/pending-checks/hdr.sh [extra mpv args]
set -u
CLIP=${HDR_CLIP:-/tmp/hdr10-test.mp4}
command -v mpv >/dev/null || { echo "mpv missing"; exit 1; }
if [ ! -f "$CLIP" ]; then
  command -v ffmpeg >/dev/null || { echo "ffmpeg missing and $CLIP not present"; exit 1; }
  echo "generating HDR10 test clip at $CLIP"
  ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=20" \
    -vf "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:primaries=bt2020:transfer=smpte2084:matrix=bt2020nc,tonemap=clip:desat=0,zscale=t=bt2020:primaries=bt2020:transfer=smpte2084:matrix=bt2020nc,format=yuv420p10le" \
    -c:v libx265 -x265-params "hdr-opt=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400" \
    -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc "$CLIP" || exit 1
fi
echo "playing $CLIP; expect the HDR badge in mpv and a visibly different brightness range"
exec mpv --fs --target-colorspace-hint=yes --hdr-compute-peak=no --no-config "$CLIP" "$@"
