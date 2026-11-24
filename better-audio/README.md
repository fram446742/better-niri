# better-audio

Rule-based audio routing for PipeWire. Part of the Better Desktop ecosystem;
lives inside this repository so no new repository is created (excluded from
niri's Cargo workspace).

## How it works

- Rules match a **playback stream** by app name (case-insensitive substring, or
  a glob with `*`/`?`) and name a **sink** (exact name, or a unique substring).
  Higher `priority` wins; ties go to the rule declared first.
- A rule may also set a **profile**: a linear volume (`1.0` == `100%`, stored in
  PipeWire's `Props`) and a mute state. The profile is applied only when the
  stream's current value differs, so `watch` does not rewrite it every tick.
- `apply` queries sinks, streams and the default sink, then moves every
  matching stream whose current sink differs and applies the profile. Streams
  without a rule are left alone.
- `default <sink>` additionally sets the default sink when it differs.
- `watch` repeats `apply` on an interval, so streams are routed as they appear.
- `restore` moves streams back to the sink they had before the first rule moved
  them, then forgets those entries.

## Backends

`--backend` selects how `better-audio` talks to the server. All backends use the
same rule/plan model, so matching and dry-run behaviour is identical.

| Backend | Implementation | Requirements |
| --- | --- | --- |
| `native` | Direct `libpipewire-0.3` connection through the `pipewire` crate: registry enumeration, link creation/destruction for moves, `default` metadata for the default sink, node `Props` for volume/mute. | Build-time `libpipewire-0.3` headers and library; enabled by the default `native` feature. |
| `pactl` | `pactl -f json list …`, `move-sink-input`, `set-default-sink`, `set-sink-input-volume/-mute`. | `pactl` (PipeWire's PulseAudio compatibility layer). |
| `wpctl` | `pw-dump` JSON snapshots, `pw-cli create-link`/`destroy`/`set-param` for moves and profile, `wpctl set-default` for the default sink. | `pw-dump`, `pw-cli` and `wpctl`; no PulseAudio needed. |

`auto` (the default) probes **native → pactl → wpctl** and uses the first backend
that can reach a server. An explicit `--backend` that is unavailable also falls
back, but the fallback is marked as degraded. `status` reports exactly what
happened:

```json
{
  "requested_backend": "auto",
  "active_backend": "native",
  "active_backend_description": "native libpipewire-0.3",
  "degraded": false,
  "reason": "using native",
  "attempts": [{ "backend": "native", "error": null }],
  "current_default": "alsa_output.usb-headset",
  "configured_default": "headset",
  "rules": [],
  "restore_entries": []
}
```

When a fallback occurs, `reason` carries the probe error, for example
`"native unavailable: cannot connect to PipeWire; using pactl"`. The executables
can be overridden with `--pactl`, `--wpctl`, `--pw-dump`, `--pw-cli` or the
`BETTER_AUDIO_PACTL`, `BETTER_AUDIO_WPCTL`, `BETTER_AUDIO_PW_DUMP`,
`BETTER_AUDIO_PW_CLI` environment variables.

To build without the native crate (for example on a system without PipeWire
headers), use `--no-default-features`; the CLI backends remain available.
Native builds need `libpipewire-0.3` development files and `libclang` for
`libspa-sys` (bindgen).

## Restore and loop avoidance

- The first time a rule moves a stream, `better-audio` records the stream's
  previous sink in `restore`, keyed by `application.name` + `media.name` (node
  ids are not stable across runs).
- Later `apply`/`watch` passes never overwrite that record. Re-running `apply`
  is idempotent: a stream already at its target produces no move and no profile
  write.
- `restore` is a one-shot command. It moves remembered streams home, then
  removes their entries. Entries whose stream is gone are reported under
  `unresolved` and dropped; entries whose sink is temporarily missing are kept.
- Because rules still take precedence, a subsequent `apply` will route the
  stream again. Use `restore` without a running `watch` when you want the move
  to stick.

## Usage

```sh
better-audio sinks
better-audio streams
better-audio rules add spotify headset
better-audio rules add 'fire*' hdmi --priority 5
better-audio rules add mpv 'usb-headset' --volume 0.6 --mute off
better-audio default headset
better-audio apply --dry-run
better-audio apply
better-audio restore --dry-run
better-audio restore
better-audio watch --interval-ms 2000
better-audio status
better-audio --backend=pactl apply
better-audio --backend=wpctl status
better-audio rules remove 0
```

Rules live in `$XDG_CONFIG_HOME/better-audio/rules.json` (override with
`BETTER_AUDIO_CONFIG` or `--config`). Volumes in the rule file are linear
fractions:

```json
{
  "default_sink": "alsa_output.usb-headset",
  "rules": [
    { "app": "spotify", "sink": "headset", "priority": 10, "volume": 0.5 },
    { "app": "mpv", "sink": "hdmi", "mute": true }
  ],
  "restore": [
    { "app": "Spotify", "media_name": "Song", "sink": "alsa_output.hdmi" }
  ]
}
```

Run `watch` from niri's `spawn-at-startup` to route new streams automatically.

## Safety

- `--dry-run` queries the server, prints the planned moves/profiles/default
  change, and never mutates anything (including restore state).
- A rule whose sink does not resolve is reported under `unresolved` instead of
  guessing; an ambiguous sink substring is treated as unresolved.
- Backend failures surface the underlying stderr or PipeWire error. A
  just-written helper script that hits `ETXTBSY` is retried briefly.
- Native moves create the new links **before** removing the old ones, so a
  failed link creation leaves audio on the previous sink.

## Limitations

- `native` and `wpctl` route by relinking node ports. A stream that is not
  directly linked to a sink (for example through a custom filter graph) may show
  no current sink; moving it detaches it from that graph. Virtual sinks such as
  Easy Effects appear as ordinary sinks.
- Native and CLI volume/mute write linear `SPA_PROP_channelVolumes`; the channel
  count is inferred from the stream's output ports. `pactl` and `wpctl` express
  volume as a cubic percentage, so `better-audio` converts between the two
  (a rule value of `0.4` is stored as linear `0.4`, while `wpctl get-volume`
  displays `0.74` and `pactl` displays `74%`).
- `wpctl` (the CLI backend) needs permission to create/destroy links. Links are
  created with `object.linger` so they survive the short-lived `pw-cli` process.
- `pactl` reports PulseAudio sink-input ids; `native`/`wpctl` report PipeWire
  global node ids. Ids are only meaningful for the current run.
- On systems where `default.audio.sink` cannot be written, native default-sink
  changes fail with the permission error; use `--backend=pactl` or `wpctl`
  instead.

## Test

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

Tests use captured `pactl` JSON and `pw-dump` fixtures plus fake executables, so
they need neither PipeWire nor a running audio server. The fixtures live in
`tests/fixtures/`.
