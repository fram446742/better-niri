# better-audio

Rule-based audio routing for PipeWire, on top of `pactl`. Part of the Better
Desktop ecosystem; lives inside this repository so no new repository is created
(excluded from niri's Cargo workspace).

## How it works

- Rules match a **playback stream** by app name (case-insensitive substring, or
  a glob with `*`/`?`) and name a **sink** (exact name, or a unique substring).
  Higher `priority` wins; ties go to the rule declared first.
- `apply` queries sinks, streams and the default sink, then moves every
  matching stream whose current sink differs. Streams without a rule are left
  alone.
- `default <sink>` additionally sets the default sink when it differs.
- `watch` repeats `apply` on an interval, so streams are routed as they appear.
- The backend is a single `pactl` executable: `$BETTER_AUDIO_PACTL`, or
  `--pactl PATH`. This keeps the tool independent of any compositor and makes it
  testable with a fake program.

Rules live in `$XDG_CONFIG_HOME/better-audio/rules.json` (override with
`BETTER_AUDIO_CONFIG` or `--config`).

## Usage

```sh
better-audio sinks
better-audio streams
better-audio rules add spotify headset
better-audio rules add 'fire*' hdmi --priority 5
better-audio default headset
better-audio apply --dry-run
better-audio apply
better-audio watch --interval-ms 2000
better-audio status
better-audio rules remove 0
```

Run `watch` from niri's `spawn-at-startup` to route new streams automatically.

## Safety

- `--dry-run` prints the planned moves and never mutates anything.
- A rule whose sink does not resolve is reported under `unresolved` instead of
  guessing; an ambiguous sink substring is treated as unresolved.
- `pactl` failures surface its stderr; a just-written `pactl` wrapper that hits
  `ETXTBSY` is retried briefly.

## Test

```sh
cargo test
cargo clippy --all-targets
```

The tests use a fake `pactl` script (JSON queries plus recorded mutations), so
they need neither PipeWire nor a running audio server.
