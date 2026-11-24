# better-shot

Screenshot capture for niri with optional clipboard, non-interactive
annotations, HDR handling, multiple output formats and JSON metadata. Part of
the Better Desktop ecosystem; lives inside this repository so no new repository
is created (excluded from niri's Cargo workspace).

## Modes

- `ui` — open niri's interactive screenshot UI (the path is used by the UI).
- `screen` — capture the focused output.
- `window` — capture a window (`--id`, default focused).

Screenshots go to `$XDG_PICTURES_DIR/Screenshots` (or
`$HOME/Pictures/Screenshots`) as `better-shot-<epoch>.<ext>`, or `--path`.
`--copy`, annotations, `--metadata`, `--format` and `--hdr` are rejected with
`ui` because that mode saves asynchronously and cannot be post-processed.

## Output formats

`--format=png|jpeg|webp` selects the file format. Without it, the format is
inferred from `--path` (`.png`, `.jpg`/`.jpeg`, `.webp`), defaulting to PNG.
Generated paths use the matching extension.

- PNG: lossless RGBA, the native format niri writes.
- JPEG: lossy RGB, quality set with `--quality 1-100` (default 90).
- WebP: lossless RGBA.

`--copy` always puts a **PNG** on the clipboard, even when the saved file is
JPEG or WebP. `--annotate` (satty) receives the saved file as-is.

## HDR

### What is detected

better-shot asks niri for the focused output (`FocusedOutput`, which exposes the
output name and `max_bpc`), then reads that output's `hdr` block from niri's
config:

```
output "eDP-1" {
    max-bpc 10
    hdr mode="on" {
        reference-luminance 203
    }
}
```

Config lookup follows niri's precedence: `$NIRI_CONFIG` (a file or a directory
containing `config.kdl`), `$XDG_CONFIG_HOME/niri/config.kdl`, then
`~/.config/niri/config.kdl`; if no user config exists, `/etc/niri/config.kdl`
is tried. `include`d files are **not** followed.

Two limitations are inherent to niri's current IPC and are not worked around:

- IPC has no live "HDR is active" flag. `hdr mode="on"` means the output is
  always signalled as HDR; `hdr mode="auto"` means HDR engages only while an HDR
  fullscreen surface is shown. better-shot reports the configured mode, not a
  guaranteed live state.
- `window` mode detects the **focused output**, not necessarily the output the
  requested window is on (niri's window IPC data has no output field).

### What niri captures

niri renders screenshots through its `ScreenCapture` target and explicitly
converts the HDR blend space back to SDR while capturing
(`set_sdr_capture_blend`). Captures written by niri are therefore already
tone-mapped, 8-bit RGB(A) PNGs. better-shot detects that from the decoded image
and refuses to apply a second tone-map.

### `--hdr` modes

| Mode | Behaviour |
| --- | --- |
| `keep` (default) | Leave niri's output untouched. On current niri this is already SDR. A high-bit-depth capture is preserved if no conversion is needed; converting it to JPEG/WebP clips values to 8 bits. |
| `sdr` | Require SDR output. Accepts niri's 8-bit captures; rejects high-bit-depth captures with a message pointing at `--hdr=tonemap`. |
| `tonemap` | Apply a PQ → sRGB tone map, but only to a genuinely high-bit-depth capture. On current niri's 8-bit captures this fails with an explanation instead of double-mapping. |

The tone mapper decodes the PQ (SMPTE ST 2084) transfer function, scales
absolute light (10 000 cd/m²) to the output's configured SDR reference white
(BT.2408 203 cd/m² by default), converts BT.2020 to BT.709 primaries, applies a
simple Reinhard highlight roll-off per channel, and encodes sRGB. It is a
display-referred still-image conversion, not a colour-managed HDR pipeline.
Only 16-bit integer PQ images are supported; 32-bit float inputs are rejected.

Detection results and the niri-tone-maps fact are included in JSON output and
in `--metadata` sidecars.

## Annotations

`--annotate` without a value keeps the interactive satty workflow
(`satty -f <file>`, override with `BETTER_SHOT_ANNOTATE`).

`--annotate=SPEC` draws directly on the captured image with no GUI, display, or
font dependency. It is repeatable:

```
--annotate=rect:x,y,w,h
--annotate=text:x,y,STRING
--annotate=arrow:x1,y1,x2,y2
```

- Coordinates are physical pixels in the captured image (after scale/transform).
- Rectangles are 3 px red outlines. Arrows have a 3 px shaft and a spread head.
- Text uses a built-in 5×7 bitmap font scaled 3×, red with a one-pixel drop
  shadow. Lowercase is rendered uppercase; unsupported characters render as
  `?`. Everything after the second comma is the text, so commas and spaces are
  allowed (quote the whole argument in a shell).
- Geometry that does not fit in the image is rejected with an error rather than
  silently clipped. Text may extend past the right edge and is clipped.

Example:

```sh
better-shot screen --annotate=rect:120,80,640,360 \
  --annotate=arrow:700,500,420,240 --annotate=text:130,60,Step 1
```

The built-in pipeline lets niri write a PNG, decodes it, draws, then rewrites the
requested format in place. `satty` can be combined with built-in annotations: the
annotations are baked in first, then satty opens the result.

## Metadata

`--metadata` writes a JSON sidecar next to the image, appending `.json`
(`shot.png` → `shot.png.json`). It records:

- capture mode, window id, output name and `max_bpc`
- timestamp (Unix seconds), tool version and the full command line
- detected HDR config, `niri_tone_maps_capture`, requested `--hdr` mode and the
  reference luminance used
- output path, format, dimensions and source colour type
- applied annotation specs and whether the image was copied

## Usage

```sh
export NIRI_SOCKET=...        # normally set inside niri

better-shot ui
better-shot screen --copy
better-shot window --id 42 --no-pointer --annotate
better-shot screen --format webp --copy --metadata
better-shot screen --path /tmp/shot.jpg --quality 92 --hdr=sdr
better-shot screen --path /tmp/shot.png --dry-run
```

`--socket PATH` overrides `$NIRI_SOCKET`. `--dry-run` prints the planned niri
action without contacting niri, capturing, or writing metadata; HDR detection is
therefore empty in dry runs.

## Limitations

- HDR state is configured-mode detection plus `max_bpc`; niri IPC does not
  expose whether HDR is currently engaged.
- niri always captures SDR, so `--hdr=tonemap` rejects current captures. The
  tone mapper is ready for high-bit-depth captures (and is unit-tested), but
  producing such a capture currently requires a niri change outside this app.
- niri config `include` files are not scanned for `output`/`hdr` blocks.
- Coordinates are absolute pixels; there is no scale-relative or percentage
  syntax.
- JPEG has no alpha channel; transparency is dropped. WebP is lossless and may
  produce larger files than PNG for some content.
- `satty` support for JPEG/WebP depends on the satty build's decoders.

## Test

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

Tests are display-free and use synthetic images for HDR tone mapping, annotation
geometry/pixels, format encoding, path generation, metadata and CLI parsing.
