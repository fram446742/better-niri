# better-shot

Screenshot capture and screen recording for niri with optional clipboard,
non-interactive annotations, HDR handling, multiple output formats and JSON
metadata. Part of the Better Desktop ecosystem; lives inside this repository so
no new repository is created (excluded from niri's Cargo workspace).

## Modes

- `ui` — open niri's interactive screenshot UI (the path is used by the UI).
- `screen` — capture the focused output.
- `window` — capture a window (`--id`, default focused).
- `record` — record an output, region or (via a generic backend) a window.
  See [Recording](#recording).

Screenshots go to `$XDG_PICTURES_DIR/Screenshots` (or
`$HOME/Pictures/Screenshots`) as `better-shot-<epoch>.<ext>`, or `--path`.
`--copy`, annotations, `--metadata`, `--format` and `--hdr` are rejected with
`ui` because that mode saves asynchronously and cannot be post-processed.

## Recording

`better-shot record` delegates recording to an existing tool instead of linking
a capture or PipeWire stack. Backend selection is automatic unless overridden
with `--backend`:

| Backend | Capture source | Output | Region | Window | Audio | HDR tone-map | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `wf-recorder` | wlr-screencopy | `-o NAME` | `-g x,y WxH` | ✗ | `-a` / `--audio=DEV` | ✗ | Preferred when installed. |
| `command` | user template | placeholders | placeholders | placeholders | placeholder | placeholder | Escape hatch for portal recorders. |
| `portal` | xdg-desktop-portal ScreenCast | — | — | — | — | — | Probed via `gdbus`; **not implemented** (needs PipeWire fd negotiation). |

`auto` picks `wf-recorder` when it is in `PATH` (or `BETTER_SHOT_WF_RECORDER`
points at it), otherwise a configured command template. If neither is usable,
the error lists every backend and why it is unavailable. `--backend=portal`
always explains that better-shot does not hand-roll a PipeWire client.

### Targets

| Flag | Behaviour |
| --- | --- |
| *(none)* | Record niri's focused output (queried through `FocusedOutput` IPC). |
| `--output=DP-1` | Record a named output. |
| `--region=x,y,w,h` | Record a rectangle in compositor/physical pixels. Passed to wf-recorder as `x,y WxH`. |
| `--window=ID` | Record one window. `wf-recorder` has no per-window wlr-screencopy target, so this requires `--backend=command`. `--window=focused` (or a bare `--window`) asks niri for the focused window. niri's own screenshot/screencast UI window is refused. |

Target flags are mutually exclusive. `--region` needs a positive width and
height. The default target requires `NIRI_SOCKET` (or `--socket`); pass
`--output` to record without niri IPC.

### Codecs, containers and audio

| `--codec` | ffmpeg encoder | mp4 | mkv | webm |
| --- | --- | --- | --- | --- |
| `h264` | `libx264` | ✓ | ✓ | ✗ |
| `hevc` | `libx265` | ✓ | ✓ | ✗ |
| `av1` | `libaom-av1` | ✓ | ✓ | ✓ |
| `auto` | `libx264`, or `libaom-av1` for webm | ✓ | ✓ | ✓ |

`--format=mp4|mkv|webm` selects the container; without it the format comes from
`--path` (`.mp4`, `.mkv`, `.webm`) and defaults to MP4. A path without an
extension gets the format's extension appended. Paths and formats that
disagree are rejected, and WebM with h264/hevc is rejected before starting a
recorder.

`--audio`:

- `none` (default) — no audio.
- `default` — the recorder's default input (`wf-recorder -a`).
- `DEVICE` — a specific source name, e.g. `alsa_input.usb-mic`.
- `sink:SINK` — record a sink monitor; `.monitor` is appended when absent.
- `source:SOURCE` — explicit source; the prefix is stripped.

### HDR

Recording HDR is not the screenshot tone mapper. `--hdr=keep` (default) leaves
capture to the backend. `wf-recorder`/wlr-screencopy has no HDR or tone-map
switch, so `--hdr=tonemap` is rejected for it with a pointer to the command
backend. On HDR-enabled outputs, `keep` records whatever the compositor hands
to wlr-screencopy (often SDR-clipped); use a command backend with an ffmpeg
`zscale`/`tonemap` filter if a real HDR-to-SDR conversion is required.

Command templates receive the requested mode as `{hdr}` and
`BETTER_SHOT_RECORD_HDR` (`keep` or `tonemap`).

### Generic command backend

`--command` (or `BETTER_SHOT_RECORD_COMMAND`) runs a shell command with `sh -c`.
It must contain `{path}`; known placeholders are replaced with shell-quoted
values, and unknown braces (such as `${HOME}`) are left untouched:

```
{path} {output} {region} {geometry} {window} {fps} {codec} {encoder}
{format} {audio} {hdr} {backend}
```

The same values are exported as `BETTER_SHOT_RECORD_PATH`, `_OUTPUT`,
`_REGION`, `_GEOMETRY`, `_WINDOW`, `_FPS`, `_CODEC`, `_ENCODER`, `_FORMAT`,
`_AUDIO`, `_HDR` and `_BACKEND`. This backend is the only one that can record a
window or perform HDR conversion, because the external recorder decides what it
supports.

Example with a portal-capable recorder:

```sh
better-shot record --backend=command --window=42 \
  --command='my-portal-recorder --window {window} --output {path}'
```

### Lifecycle, state files and stopping

Recordings default to
`$XDG_VIDEOS_DIR/Recordings/better-shot-<epoch>.<ext>` (then
`$HOME/Videos/Recordings`, then the temp dir). Override with `--path`.

While a recording runs, better-shot writes
`$XDG_STATE_HOME/better-shot/recordings/record-<pid>.json` (falling back to
`~/.local/state/better-shot/recordings/`). The file records the PID, output
path, target, backend, codec/format, start time, command line and log path. It
is the PID file used by `--stop`:

```sh
better-shot record --output=DP-1 --duration=30 --json
better-shot record --stop --json
```

- `--duration=SECONDS` stops the recorder gracefully: SIGINT first, then
  SIGTERM and SIGKILL if it does not exit. `--fps=auto` (or `0`) leaves the
  frame rate to the backend (wf-recorder then uses damage-driven frames).
- SIGINT/SIGTERM sent to `better-shot` are forwarded to the recorder, and the
  child gets `PR_SET_PDEATHSIG` so a killed parent does not normally leave an
  orphan.
- `--stop` finds the newest live state file, signals that PID, waits, and
  removes the state and log. `--stop --dry-run` reports what it would stop
  without signalling. Stale state files (dead PIDs) are pruned automatically.
- A non-zero recorder exit is reported with a mapped reason (compositor
  connection failure, denied/portal permission, missing dependency) plus the
  last stderr lines and the preserved log path. A recorder that exits before
  `--duration`, or exits successfully without writing a non-empty file, is
  also an error.
- `--json` prints the final path, backend, target, codec/encoder, format, fps,
  audio, HDR mode, PID, wall-clock duration, how it stopped (`duration`,
  `signal` or `backend`), the command and `dry_run`.

`--dry-run` prints the resolved recorder command and output path without
starting a process or touching the state directory.

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

better-shot record --output=DP-1 --duration=10
better-shot record --region=100,200,1280,720 --fps=30 --codec=hevc --json
better-shot record --output=DP-1 --format=mkv --audio=sink:speakers
better-shot record --output=DP-1 --dry-run
better-shot record --stop
better-shot record --stop --json --dry-run
```

`--socket PATH` overrides `$NIRI_SOCKET`. `--dry-run` prints the planned niri
action without contacting niri, capturing, or writing metadata; HDR detection is
therefore empty in dry runs. For `record`, `--dry-run` prints the resolved
recorder command instead of starting it.

## Limitations

- The xdg-desktop-portal `ScreenCast` backend is detected and reported but not
  implemented. Consuming its PipeWire stream needs file-descriptor negotiation;
  use `wf-recorder` or a `--command` template for portal-capable recorders.
- `wf-recorder` cannot record a single window and has no HDR tone-map option.
  better-shot rejects those combinations rather than silently changing them.
- HDR recording is compositor-defined. There is no recording-side tone map and
  no HDR metadata validation; on HDR outputs `keep` usually captures clipped
  SDR pixels.
- Container validation only enforces WebM's AV1 requirement. Encoder names
  (`libx264`, `libx265`, `libaom-av1`) must exist in the local ffmpeg/wf-recorder
  build; otherwise the recorder's error is surfaced.
- `--stop` stops the newest live recording. Several concurrent recordings are
  possible, but older ones need to be stopped with a signal to their PID from
  their state file. PID liveness checks cannot detect all PID reuse.
- `--duration` is wall-clock time from spawn to exit. Shutdown latency and the
  20 ms supervision poll make it approximate; encoded frame counts can differ.
- The generic command backend runs through `sh -c`. Placeholders are safely
  quoted, but the rest of the template is the user's responsibility and must
  write to `{path}` itself.
- niri currently renders its screenshot UI as an internal overlay rather than
  an IPC-visible window. The refusal of `--window` targets that look like
  niri's screenshot/screencast UI is therefore defence in depth.
- HDR state is configured-mode detection plus `max_bpc`; niri IPC does not
  expose whether HDR is currently engaged.
- niri always captures SDR, so `--hdr=tonemap` rejects current screenshots. The
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

Tests are display-free, GPU-free and compositor-free. Screenshot tests use
synthetic images for HDR tone mapping, annotation geometry/pixels, format
encoding, path generation, metadata and CLI parsing. Recording tests put fake
`wf-recorder`/recorder scripts on a temporary `PATH`, and drive state-file
start/stop, duration stops, early exits, JSON reports, backend selection and
argument mapping without touching the real session.
