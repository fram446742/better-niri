# better-audio

> Part of [Better Desktop](https://github.com/fram446742/better-desktop).

Rule-based audio routing for PipeWire. It is its own Cargo workspace; the
[Better Desktop](https://github.com/fram446742/better-desktop) repository
documents the whole set and the `better` selector.

## How it works

- Rules match a **playback stream** by app name (case-insensitive substring, or
  a glob with `*`/`?`) and name a **sink** (exact name, or a unique substring).
  Higher `priority` wins; ties go to the rule declared first.
- A rule may also set a **profile**: a linear volume (`1.0` == `100%`, stored in
  PipeWire's `Props`) and a mute state. The profile is applied only when the
  stream's current value differs, so `watch` does not rewrite it every tick.
- Optional **context fields** on a rule (`binary`, `title`, `workspace`) match
  the focused window, read from `--focus-command` (default
  `$BETTER_AUDIO_FOCUS_COMMAND`). Matching is per stream: an app with several
  streams gets each one routed, and context never assumes one stream per app.
- **Sources** (capture devices) can be listed and made the default, and named
  **profiles** set a default sink, default source and a global volume/mute set
  with `profile apply` / `profile restore`.
- **Device mixer**: `volume` and `mute` set a sink or source directly. Device
  listings and `status` report the current linear volume and mute flag, and an
  already-matching value is a no-op (idempotent preview/apply).
- `apply` queries sinks, streams and the default sink, then moves every
  matching stream whose current sink differs and applies the profile. Streams
  without a rule are left alone.
- `default <sink>` additionally sets the default sink when it differs.
- `watch` repeats `apply` on an interval, re-evaluates rules when the device set
  changes, and skips identical plans (`ChangeGate`) so it does not switch
  streams unnecessarily.
- `restore` moves streams back to the sink they had before the first rule moved
  them, then forgets those entries.
- `diagnose` prints the backend attempts, sinks/sources/defaults, links, focus
  context, every stream's matched rule indices, the winning rule and a reason.

## Backends

`--backend` selects how `better-audio` talks to the server. All backends use the
same rule/plan model, so matching and dry-run behaviour is identical.

| Backend | Implementation | Requirements |
| --- | --- | --- |
| `native` | Direct `libpipewire-0.3` connection through the `pipewire` crate: registry enumeration (sinks, sources, streams, links), link creation/destruction for moves, `default` metadata for default sink/source, node `Props` for volume/mute. | Build-time `libpipewire-0.3` headers and library; enabled by the default `native` feature. |
| `pactl` | `pactl -f json list …` for sinks/sources/streams, `move-sink-input`, `set-default-sink`/`set-default-source`, `set-sink-input-volume/-mute`, `set-sink-volume/-mute` and `set-source-volume/-mute`. | `pactl` (PipeWire's PulseAudio compatibility layer). |
| `wpctl` | `pw-dump` JSON snapshots, `pw-cli create-link`/`destroy`/`set-param` for moves, stream profiles and sink/source volume+mute, `wpctl set-default` for defaults. | `pw-dump`, `pw-cli` and `wpctl`; no PulseAudio needed. |

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

## Sources, profiles and hotplug

```sh
better-audio sources
better-audio links
better-audio default-source alsa_input.usb-mic
better-audio profile apply headset
better-audio profile restore --dry-run
better-audio diagnose
```

A profile is a named set of defaults and a global stream volume/mute:

```json
{
  "profiles": {
    "headset": {
      "sink": "alsa_output.usb-headset",
      "source": "alsa_input.usb-mic",
      "volume": 0.5,
      "mute": false
    }
  }
}
```

`profile apply` remembers the previous defaults in `profile_restore` and
`profile restore` returns to them. Only values that differ are written, so
applying a profile twice is a no-op. Source support by backend: `native`,
`pactl` and `wpctl` enumerate sources and set the default source; builds
without the native feature still get it from the CLI backends.

`watch` polls the device set (`DeviceSnapshot`) and reports changes (sink or
source plugged/removed, default changed). Rules are re-evaluated on that change
and every interval; a `ChangeGate` suppresses an identical plan, so repeated
ticks do not rewrite volume or relink a stream. This is polling, not a
PipeWire event subscription.

## Device mixer

```sh
better-audio volume sink <device> <0..1>
better-audio mute sink <device> on|off
better-audio volume source <device> <0..1>
better-audio mute source <device> on|off
```

`<device>` is an exact name or a unique substring, resolved separately in the
sink or source list. Volumes are linear (`0.5` is 50%); `pactl` converts to its
cubic percentage and `wpctl`/`native` write PipeWire `Props`, so the same value
means the same loudness on every backend. Invalid values (`<0`, `>1`, NaN) are
rejected before a backend is contacted. A value that already matches is a no-op
(`changed: false`) and is not written. `--dry-run` resolves the device and
reports `changed` without writing. `sinks`, `sources` and `status` include each
device's current `volume` and `mute`; `--json` wraps a command result as
`{ "command": "volume", "ok": true, "data": { ... } }`.

## Context-aware rules

Rules can additionally match the focused window/workspace:

```json
{
  "rules": [
    {
      "app": "firefox",
      "binary": "firefox",
      "title": "Meeting*",
      "workspace": "work",
      "sink": "headset",
      "volume": 0.8
    }
  ]
}
```

The focus JSON is read from `--focus-command` (or
`$BETTER_AUDIO_FOCUS_COMMAND`), for example:

```sh
better-audio --focus-command 'niri msg --json focused-window' apply
```

Accepted keys are `app`/`app_id`/`application`, `title` and
`workspace`/`workspace_name`. `app`/`binary` also match
`application.name`/`application.process.binary` stream properties; `title` and
`workspace` come from the focused context. A matching rule is resolved per
stream, so several streams from one app are each routed according to their own
properties.

## Diagnostics and mixer

`diagnose` prints machine-readable JSON with backend attempts and degradation,
sinks, sources, defaults, links, the focus context, all rules, and for each
stream the matched rule indices, the winner and a human-readable reason. It is
read-only.

There is **no graphical mixer**. A GTK/egui mixer would add a GUI toolkit,
D-Bus/portal or event-loop stack to a deliberately small CLI daemon; the JSON
commands (`sinks`, `sources`, `streams`, `links`, `diagnose`, `status`) are the
supported interface. Script a mixer against those commands if needed.

## Usage

```sh
better-audio sinks
better-audio sources
better-audio links
better-audio streams
better-audio rules add spotify headset
better-audio rules add 'fire*' hdmi --priority 5
better-audio rules add mpv 'usb-headset' --volume 0.6 --mute off
better-audio default headset
better-audio default-source alsa_input.usb-mic
better-audio volume sink alsa_output.usb-headset 0.5
better-audio mute source alsa_input.usb-mic on
better-audio volume sink headset 0.75 --dry-run --json
better-audio profile apply headset
better-audio profile restore
better-audio apply --dry-run
better-audio apply
better-audio restore --dry-run
better-audio restore
better-audio watch --interval-ms 2000
better-audio diagnose
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
- Hotplug detection is polling (`watch --interval-ms`), not a PipeWire event
  subscription; a very short-lived device may be missed between polls.
- Context rules correlate the focused window with stream properties by
  app/binary/title/workspace. They do not inspect per-window PipeWire client
  credentials beyond those properties.
- No graphical mixer or tray applet (see above).

## Test

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

Tests use captured `pactl` JSON and `pw-dump` fixtures plus fake executables, so
they need neither PipeWire nor a running audio server. The fixtures live in
`tests/fixtures/`. The suite also covers input sources, context-aware rules,
named profiles and restore, hotplug snapshots, the change gate and rule
diagnostics.
