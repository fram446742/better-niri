# HDR notes

This is the working reference for the HDR path. It records what is implemented,
what was compared against COSMIC's experimental HDR fork, and what still needs
hardware.

## Implemented

- Per-output `hdr mode="auto|on|off"` with EDID capability checks, reference
  white, peak-luminance override and connector metadata.
- `wp_color_manager_v1` exposure gated on an HDR-configured output.
- Per-surface content classification from `wp_color_management_surface_v1`:
  sRGB, wide-gamut SDR, PQ, HLG, extended-linear/scRGB and Windows-BT.2100.
- sRGB→PQ blending with BT.709→BT.2020 gamut conversion, reference-white
  scaling and ICtCp tone mapping where content exceeds the sink.
- Per-element `ScanoutColorTransform`, post-blend PQ encode through the CRTC
  gamma LUT, and an HDR cursor-plane transform.
- SDR capture/readback paths and HDR-aware screenshot metadata in loom-shot.
- 10-bit `wl_shm` formats (`Xrgb2101010`, `Xbgr2101010`) are advertised so a
  color-managed client can provide a 10-bit surface.

## Compared with COSMIC's HDR fork

The experimental COSMIC fork (`Liquidmasl/cosmic-comp`, branch `hdr`) was read
as a second implementation. The useful parity points:

| Area | COSMIC | loom-niri |
| --- | --- | --- |
| Output policy | HDR enabled by config/env; desktop stays HDR | `auto` engages only for fullscreen HDR content; `on` keeps the desktop HDR |
| Surface descriptions | PQ + scRGB | broader: wide-gamut SDR, HLG, ext-linear, BT.2100 |
| Composition | SDR intermediate + full-frame HDR postprocess | per-draw blend-space composition, preserving scanout classification |
| Cursor | composited through the HDR postprocess | CPU PQ transform and post-blend cursor transform |
| KMS | PQ/BT.2020 metadata, max-bpc rule | PQ/BT.2020 metadata, 10-bit gate, EDID peak handling |
| Tests | shader/CPU parity vectors | blend transform, scanout gating, protocol and Vulkan render tests |

Two parity fixes came out of the comparison: the output image description now
carries the sink's mastering luminance and BT.2020 mastering primaries, and an
output whose `max bpc` range cannot reach 10 is kept in SDR instead of forcing a
PQ signal through an 8-bit link.

## Layer surfaces and wallpapers

A layer surface is an ordinary `wl_surface` in the render path: if it attaches
a `wp_color_management_surface_v1` description, the compositor classifies it
like any other surface. Without a description it is SDR, and on an HDR output
the compositor encodes it into the PQ/BT.2020 blend space. This is correct SDR
handling, not HDR scanout of the wallpaper's source pixels.

A true HDR wallpaper therefore needs two things on the client side:

1. a 10-bit buffer (`Xrgb2101010` over `wl_shm`, or a DMA-BUF), and
2. a PQ/BT.2020 image description attached to the layer surface.

The compositor now advertises the 10-bit shm formats and the color-management
global. The native wallpaper backend stays SDR for images and tone-maps HDR
video to SDR; enabling 10-bit PQ output is a client feature, not a compositor
flag. COSMIC's own `cosmic-bg` still has `hdr_layer = false` in its draw path,
so it is not a completed reference for this either.

## Remaining hardware checks

- NVIDIA KMS: post-blend encode, HDR cursor plane, frame fences, suspend/resume.
- A real HDR sink: mixed SDR/HDR windows, rotated outputs, direct scanout
  transitions, screen capture with a colour-managed client.
- A live Plasma/Spectacle call for the KDE ScreenShot2 interface.

Until those runs are recorded under `docs/validation/`, HDR support is tested
by unit, protocol and RADV rendering tests only.
