# better-monitor

Output profiles, rules and watch mode for niri, with validation, safe apply and
rollback. Part of the Better Desktop ecosystem; it lives inside this repository
so no new repository is created (excluded from niri's Cargo workspace).

`better-monitor` talks to niri through the IPC socket only. Fields that niri
does not expose over IPC (HDR, startup focus, isolation, mirroring) are stored,
validated and reported as **config-only**; see
[IPC-visible vs config-only fields](#ipc-visible-vs-config-only-fields).

## Features

- Snapshot the current output settings into a named profile: on/off, mode,
  scale, transform, position, VRR/adaptive sync and max BPC.
- Schema v2 adds per-output `max_bpc`, `hdr`, `focus_at_startup`, `isolated` and
  `mirror`, plus a profile-level `match` so one profile can follow a monitor by
  make/model/serial/name.
- Apply through niri's IPC (`Request::Output`) as an ordered action list:
  on → mode → scale → transform → position → VRR → max BPC → off.
- Validate the resulting layout first: refuse a layout with no enabled output,
  overlapping positions, a mirror of a missing/disabled output, mirror + isolated,
  duplicate matches, or invalid scale/mode.
- `--dry-run` prints the complete ordered action list and every validation
  error without touching the compositor.
- The pre-apply layout is stored as `__previous`; `restore` re-applies it and
  does not overwrite `__previous`. A failure in the middle of an apply sends
  compensating actions to restore `__previous` and reports exactly what
  happened.
- Rules (match + profile + priority) with deterministic explain output.
- Watch mode with debounce, an advisory process lock, a post-apply cooldown and
  a post-apply fingerprint so its own changes cannot loop. `--once` evaluates
  one cycle for tests/adapters.
- `--json` on every command emits one machine-readable envelope.
- Profiles written before schema v2 still load; `migrate` rewrites them.

## Usage

```sh
export NIRI_SOCKET=...            # normally set inside niri

better-monitor save desk          # all outputs
better-monitor save desk eDP-1    # just one output
better-monitor apply desk --dry-run
better-monitor apply desk
better-monitor apply desk --confirm-timeout 20   # roll back unless `confirm`
better-monitor confirm            # keep the profile applied
better-monitor cancel             # restore __previous now
better-monitor restore            # undo the last apply (keeps __previous)
better-monitor list
better-monitor show desk
better-monitor delete desk
better-monitor migrate            # rewrite old profiles as schema v2
```

Rules:

```sh
better-monitor rules add dock --profile desk --priority 10 \
  --make "Dell Inc." --model U2720Q --serial ABC123
better-monitor rules add fallback --profile laptop --priority 0 --match-name eDP-1
better-monitor rules list
better-monitor rules explain --output DP-1
better-monitor rules apply --dry-run
better-monitor rules apply
```

Watch:

```sh
better-monitor watch --profile desk          # apply this profile on changes
better-monitor watch                         # evaluate rules on changes
better-monitor watch --once --json           # one evaluation, for adapters/tests
better-monitor watch --debounce-ms 500 --poll-ms 1000 --cooldown-ms 3000
```

Global options: `--socket PATH`, `--state PATH`, `--rules-file PATH`, `--json`,
`--dry-run`, `--help`. `--socket` overrides `$NIRI_SOCKET`; `--state` overrides
`$BETTER_MONITOR_STATE`; `--rules-file` overrides `$BETTER_MONITOR_RULES`.
Profiles and rules default to `$XDG_STATE_HOME/better-monitor/` (see
[State files](#state-files)).

## Profile schema (v2)

```json
{
  "version": 2,
  "match": { "make": "Dell Inc.", "model": "U2720Q", "serial": "ABC123" },
  "outputs": {
    "monitor": {
      "off": false,
      "scale": 1.5,
      "mode": [3840, 2160, 60000],
      "transform": "Normal",
      "position": [0, 0],
      "vrr": true,
      "vrr_on_demand": false,
      "max_bpc": "10",
      "hdr": { "mode": "auto", "reference_luminance": 203 },
      "focus_at_startup": true,
      "isolated": false,
      "mirror": null
    }
  }
}
```

- `version` is `2` for newly saved profiles. A missing version means v1; v1
  JSON is accepted unchanged.
- `outputs` keys are connector names for name-keyed profiles. With `match`, the
  profile must contain exactly one output entry and that entry is applied to
  the matching connected output.
- `mode` is `[width, height, refresh-millihertz]` (60000 = 60 Hz).
- `vrr` is variable refresh rate / adaptive sync. `adaptive_sync` and
  `variable_refresh_rate` are accepted as input aliases. `vrr_on_demand`
  selects niri's on-demand VRR mode.
- `max_bpc` is one of 6, 8, 10, 12, 14, 16. niri serialises it as a string
  (`"10"`); a plain number (`10`) is also accepted when reading.
- `transform` is one of `Normal`, `90`, `180`, `270`, `Flipped`, `Flipped90`,
  `Flipped180`, `Flipped270`.
- `hdr.mode` is `auto` or `on`; `reference_luminance` is in cd/m².
- `mirror` names another output (a profile key or connector). It is validated
  as a relationship but not applied over IPC (see limitations).

### Matching

`match` is a case-insensitive, `*`-wildcard predicate. Present fields must
match; absent fields are wildcards. An empty `match` matches every output, so
it works as a fallback rule predicate but is ambiguous for a profile unless
combined with a name.

```json
"match": { "model": "U2720*", "serial": "ABC*" }
```

`name` matches the connector (`eDP-1`). If more than one connected output
matches a profile's `match`, the first in sorted name order is used and the
apply report contains a warning; add `name` or `serial` to disambiguate.

### IPC-visible vs config-only fields

| Field | Applied over IPC? | Notes |
| --- | --- | --- |
| `off` | yes | `OutputAction::Off` / `On` |
| `mode` | yes | `OutputAction::Mode` |
| `scale` | yes | `OutputAction::Scale` |
| `transform` | yes | `OutputAction::Transform` |
| `position` | yes | `OutputAction::Position` |
| `vrr`, `vrr_on_demand` | yes | `OutputAction::Vrr` |
| `max_bpc` | yes | `OutputAction::MaxBpc` |
| `hdr` | **no** | config file only |
| `focus_at_startup` | **no** | startup behaviour, config only |
| `isolated` | **no** | config file only |
| `mirror` | **no** | this niri tree does not implement output mirroring |

IPC output changes are temporary: niri does not write them into the config.
`save` can only capture IPC-visible fields, so HDR, startup focus, isolation
and mirror have to be added to a profile by hand. Apply sends the IPC-visible
subset; every config-only field produces a `config_only` note in the report
(and a `config-only:` line on stderr in the default output). To make those
fields persistent, add or edit the niri config:

```kdl
output "DP-1" {
    hdr mode="on" {
        reference-luminance 203
    }
    focus-at-startup
    isolated
}
```

`mirror` cannot be expressed in this tree's niri config either. A profile that
mirrors an isolated output is refused; a valid mirror is recorded and reported
as config-only rather than silently pretended to be applied.

## Safe apply with a confirmation window

A wrong mode or transform can leave the screen unreadable, so `apply` can run
with a confirmation window:

```sh
better-monitor apply laptop --confirm-timeout 30
# the screen changes; if it is still usable:
better-monitor confirm
# if you cannot see or the layout is wrong (run over SSH or a TTY):
better-monitor cancel
```

`--confirm-timeout` stores `__previous` as usual, writes
`$XDG_STATE_HOME/better-monitor/pending.json` and starts a detached
`better-monitor __confirm-watch` process. The watcher restores `__previous`
when the deadline passes unless `confirm` removed the record. `cancel` restores
immediately. Each record carries a unique id, so a newer apply supersedes an
older watcher instead of racing it; a failed restore is retried by the next
manual `restore`. The JSON apply report includes a `confirmation` object with
the id, pending path, deadline and timeout.

The watcher is a normal child process, so it does not survive a reboot; the
pending file is left in place and `better-monitor restore` still works.

## Safety

Before sending anything, `better-monitor` overlays the profile on the current
layout and validates:

1. **At least one output is enabled.** Disabling every output is refused.
2. **No overlapping positions.** Enabled, non-mirrored outputs with a known
   mode and position are compared as logical rectangles (mode ÷ scale, with
   90°/270° transforms swapping width/height). Touching edges are allowed.
3. **Mirrors are consistent.** A mirror target must exist, must not be the
   output itself and must not be disabled.
4. **Mirror + isolated is refused.** If either side of a mirror relationship is
   `isolated`, the profile is refused.
5. **Matches resolve to one output.** A profile matcher with no match, or two
   entries resolving to the same connector, is refused.
6. **Scale and mode are sane.** Non-finite/non-positive scale and zero-sized
   modes are refused.

`--dry-run` still runs all validation. It prints the ordered action list and
reports validation errors with a failing exit status, so CI and adapters can
check a profile without a display.

On a real apply the previous state is saved to `__previous` first, then actions
are sent in order. Actions that disable outputs are last, so the new output is
enabled and positioned before an old one is turned off. If an action fails:

- the failure, the output and the action are reported;
- compensating actions from the saved snapshot are sent immediately;
- the status is `rolled_back` when recovery succeeded, `rollback_failed` when it
  did not, with `rollback_errors` describing the second failure;
- `__previous` remains on disk for a manual `restore`.

This is compensating rollback, not an atomic transaction: niri applies each
output action separately, and there is a short window where the layout is
between states.

`restore` re-applies `__previous` directly (no conflict validation, because it
is a recovery path) and does not overwrite `__previous`. It also attempts a
compensating rollback to the pre-restore layout if it fails halfway.

## Rules

Rules are stored in `rules.json` under the state directory. A rule has a name,
a profile, a priority and a match. For each connected output the matching rule
with the highest priority wins; equal priorities are broken by rule name, which
is reported as a tie in `rules explain`.

```json
{
  "version": 2,
  "rules": [
    {
      "name": "dock",
      "profile": "desk",
      "priority": 10,
      "match": { "make": "Dell Inc.", "serial": "ABC123" }
    },
    {
      "name": "fallback",
      "profile": "laptop",
      "priority": 0,
      "match": {}
    }
  ]
}
```

`rules explain` prints, per output, the winning rule/profile, the full list of
considered rules (matched or not) and a reason. It also plans the merged
profile so `--dry-run` shows the resulting action list. `rules apply` merges all
winning rules into one profile and applies it once, which gives one
`__previous` snapshot and one rollback path for the whole change.

A rule's profile is bound to the matched output when the profile has one
output entry and no profile-level `match`. This lets a single profile (saved
from one dock) follow an identical monitor that reconnects under a different
connector name.

## Watch

`watch` observes output topology and applies either `--profile NAME` or the
winning rules. It uses:

- **polling** of `Request::Outputs` (default every 1000 ms), plus an event-stream
  connection used only as a wake-up hint. niri's `Event` enum in this tree has
  no output-added/removed/mode-change event, so watch does not pretend to
  receive one; it detects changes from the output snapshot. This is documented
  upstream limitation, not a hidden fallback.
- **debounce** (default 500 ms): a fingerprint must stay unchanged for the
  debounce interval before applying, so a dock that enumerates several outputs
  in quick succession triggers one apply.
- **post-apply baseline and cooldown** (default 3000 ms): after applying, watch
  re-reads outputs, stores the *post-apply* fingerprint and ignores it. A
  topology change that is still settling waits for the cooldown. This is the
  loop lock: watch never re-applies a profile because of its own actions.
- **process lock**: `$XDG_STATE_HOME/better-monitor/watch.lock` contains the
  PID. A second `watch` refuses to start; a lock whose PID no longer exists is
  removed automatically.
- **`--once`**: evaluate the current outputs immediately (no debounce wait, no
  event thread, no process lock) and exit. This is the supported integration
  point for tests and adapters.

If applying is refused, watch records the post-apply fingerprint anyway and
waits for the next topology change instead of spinning on a bad profile.

## JSON output

Every command accepts `--json` and prints one envelope:

```json
{
  "command": "apply",
  "ok": true,
  "data": {
    "profile": "desk",
    "status": "applied",
    "dry_run": false,
    "resolved": { "desk": "DP-1" },
    "actions": [ { "output": "DP-1", "phase": "on", "action": "On" } ],
    "applied": [ "..." ],
    "errors": [],
    "warnings": [],
    "config_only": [],
    "previous_saved": true,
    "failed_action": null,
    "rollback_actions": [],
    "rollback_errors": []
  }
}
```

- `ok` is false for refused/rolled-back applies, parse errors, IPC errors and
  `rollback_failed`; the process exits non-zero as well.
- Statuses are `planned`, `applied`, `refused`, `rolled_back`,
  `rollback_failed`, `restored`.
- `watch` without `--once` writes one envelope per evaluated cycle (NDJSON).
- Without `--json` the legacy shapes are kept: `apply`/`restore` print the
  planned `[[output, action], ...]` array, `show`/`save` print the profile and
  `list` prints the profile names. Diagnostics go to stderr.

## Interop

### better-session

`better-session` contexts store `outputs` using the same core shape as a
`better-monitor` profile entry (`off`, `mode`, `scale`, `transform`, `position`,
`vrr`). You can paste a context's output entries into `profile.outputs`, or
paste a profile's `outputs` into a context; v2's new fields are optional and do
not break the shared shape. `better-session` restores outputs directly, so it
does not need the `better-monitor` binary.

### better-companion

The existing `monitor.*` adapter preset already calls:

```sh
better-monitor apply "${BETTER_MONITOR_PROFILE:-default}"
better-monitor restore
```

Those commands and their default output are unchanged. For machine-readable
routing, have the adapter call:

```sh
better-monitor apply "$BETTER_MONITOR_PROFILE" --json
better-monitor restore --json
better-monitor rules apply --json
better-monitor watch --once --json
```

`watch --once` is preferable when the adapter already receives output
added/removed events: it evaluates the current snapshot once, reports the
winner and planned/applied actions, and exits. A long-running
`better-monitor watch` is preferable when niri is the only event source, since
it debounces hotplug churn and holds the loop lock.

## State files

| Path | Contents |
| --- | --- |
| `$XDG_STATE_HOME/better-monitor/profiles.json` | profile store (`version`, `profiles`) |
| `$XDG_STATE_HOME/better-monitor/rules.json` | rules (`version`, `rules`) |
| `$XDG_STATE_HOME/better-monitor/watch.lock` | PID lock for continuous watch |

`$XDG_STATE_HOME` defaults to `~/.local/state`. `BETTER_MONITOR_STATE` can point
`profiles.json` elsewhere; rules then default to `rules.json` beside it.
`BETTER_MONITOR_RULES` overrides the rules path. Writes use a temporary file and
rename.

## Limitations

- **Output topology events do not exist in niri's IPC event stream here.** Watch
  polls `Request::Outputs`; the event stream only wakes it earlier. On a niri
  version with output events, the poll can be increased or removed.
- **Config-only fields are not applied.** HDR, `focus_at_startup`, `isolated`
  and `mirror` require config edits or are unsupported in this tree. They are
  stored, validated where possible and reported, never faked.
- **Mirroring is not implemented by niri here.** `mirror` exists in the schema
  for relationships and future use; applying it produces a config-only note.
- **Overlap detection needs known geometry.** An output that is newly connected
  and has no mode/position yet cannot be checked; niri will place it. Check with
  `--dry-run` after the output appears.
- **HDR reference luminance is not readable over IPC.** It can be written to a
  profile and migrated, but applying only reports it as config-only.
- **Custom modes are restored as standard modes.** The snapshot keeps the
  width, height and refresh rate (and `is_custom_mode` is not part of the
  profile), so a custom modeline is re-applied as a regular mode entry. Use the
  config file for modelines.
- **Apply is not atomic.** Rollback is compensating output actions, not a
  transaction; a disconnected socket can prevent recovery.
- **No `--force`.** Invalid layouts are refused; fix the profile instead.
- **IPC changes are temporary.** A niri config reload or reconnect can replace
  them with config values; use `watch`/rules or a config file for persistence.

## Test

No display or real niri is needed: the integration tests run a fake niri IPC
socket.

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```
