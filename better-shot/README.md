# better-shot

Screenshot capture for niri with optional clipboard and annotation hooks.
Part of the Better Desktop ecosystem; lives inside this repository so no new
repository is created (excluded from niri's Cargo workspace).

## Modes

- `ui` — open niri's interactive screenshot UI (path is used by the UI).
- `screen` — capture the focused output.
- `window` — capture a window (`--id`, default focused).

Screenshots go to `$XDG_PICTURES_DIR/Screenshots` (or
`$HOME/Pictures/Screenshots`) as `better-shot-<epoch>.png`, or `--path`.

## Post-processing

- `--copy` pipes the saved PNG to `wl-copy --type image/png`.
- `--annotate` opens the saved PNG in `satty -f`.
- Override the programs with `BETTER_SHOT_CLIPBOARD` and
  `BETTER_SHOT_ANNOTATE`.

`--copy`/`--annotate` are rejected with `ui` because that mode is asynchronous.

## Usage

```sh
export NIRI_SOCKET=...        # normally set inside niri

better-shot ui
better-shot screen --copy
better-shot window --id 42 --no-pointer --annotate
better-shot screen --path /tmp/shot.png --dry-run
```

`--socket PATH` overrides `$NIRI_SOCKET`. `--dry-run` prints the planned niri
action without capturing.

## Test

```sh
cargo test
cargo clippy --all-targets
```
