# loom-companion

> Part of [Loom](https://github.com/fram446742/loom).

One daemon with bounded-context crates, reachable over a Unix socket. This is
the first slice of the Loom companion plan: the shared protocol, the
parking/automation/hooks/system/idle/rotate contexts, the daemon and
`loomctl`.

It is its own Cargo workspace with its own lockfile and build.

## Crates

| Crate | Namespace | Responsibility |
| --- | --- | --- |
| `companion-core` | — | JSON-line protocol, action registry, event bus, state/socket/config paths |
| `companion-parking` | `parking.*` | Named lots of parked items (nsticky/staged workflow) |
| `companion-automation` | `automation.*` | Named action sequences executed through the registry |
| `companion-history` | `history.*` | Bounded persisted log of recent rule/automation runs |
| `companion-hooks` | `hooks.*` | Commands registered per event (resolution only; no implicit execution) |
| `companion-config` | `config.*` | Optional user config file, validation and hot reload |
| `companion-system` | `system.*` | Ping, status and paths |
| `companion-rules` | `rules.*` | Declarative window/workspace rules with explain/apply/run |
| `companion-adapters` | `session.*`, `monitor.*`, `audio.*`, `wallpaper.*`, `adapters.*` | Optional ecosystem adapters: event subscriptions, command templates, timeouts, dedup and failure policy |
| `companion-idle` | `idle.*` | Activity clock, idle/suspend policy, inhibitors, logind and DPMS actions |
| `companion-rotate` | `rotate.*` | Orientation source, hysteresis and niri output transform/position |
| `companion-niri` | `screentime.*` | Per-app usage time sampled from the focused window, persisted as seconds |
| `companion-niri` | `swallow.*` | Terminal swallowing: minimize a terminal while its launched child is open |
| `companion-niri` | `compositor.*`, `parker.*`, `windows.*`, `marks.*` | niri IPC bridge, event tracker, park/restore, run-or-raise/singleton, placement, focused-window cwd, native sticky and persistent window marks |
| `companion-niri` | `stage.*`, `scratchpad.*` | Staged windows and named scratchpads on the shared parking store |
| `companion-niri` | `nsticky.*` | Compatibility aliases for the standalone nsticky CLI verbs |
| `companion-daemon` | — | `loom-companion` Unix-socket server |
| `loomctl` | — | CLI client |

## Protocol

One JSON request per line, one JSON response per line:

```json
{"kind":"list"}
{"kind":"describe"}
{"kind":"call","namespace":"parking","action":"park","params":{"id":"win-1","value":{"title":"editor"}},"id":7}
{"kind":"status"}
{"kind":"subscribe","events":["park","restore"]}
```

Protocol version 2 adds three backward-compatible additions; every new field
is optional, so version-1 requests and responses keep working:

- `call.id` (optional `u64`) is echoed as `id` on the response. `list`,
  `status` and `describe` still accept their old shape.
- `error.code` is a stable machine-readable string (`unknown_namespace`,
  `unknown_action`, `invalid_request`, `request_too_large`, `unknown_topic`,
  `stream_required`, `slow_subscriber`, `recursive_run`, `action_failed`).
  `message` is unchanged.
- `subscribe` turns one connection into an event stream (see below).

Responses are tagged the same way: `contexts`, `value`, `status`, `description`,
`subscribed`, `event` or `error`. `describe` reports the protocol name
(`loom-companion`), its numeric version and every action with:

- `owner`: the namespace that owns it;
- `params`: argument names, JSON kinds, required flags and descriptions;
- `permissions`: `niri`, `shell` or empty, so clients can gate automation;
- `available`: false when the context cannot work in this build or host
  (for example `windows.*`/`marks.*` when `$NIRI_SOCKET` is missing).

Companions use `compositor.capabilities` to read the compositor's feature
report (`sticky`, `virtual_outputs`, `shaders`, `hdr`, `vulkan`, ...), which is
the same data as `niri msg capabilities --json`.
The default socket is `$XDG_RUNTIME_DIR/loom-companion.sock` (mode `0600`).
State is persisted under `$XDG_STATE_HOME/loom-companion/`.

### Event subscriptions

`{"kind":"subscribe","events":["park","window-focused"]}` replies with
`{"kind":"subscribed",...}` and then streams `{"kind":"event",...}` lines:

```json
{"kind":"event","event":"park","payload":{"window":70,"phase":"park"},"seq":41,"timestamp_ms":1764000000000}
```

An empty `events` array or `"*"` subscribes to every topic. Topics are the same
names as the adapter event table: `workspace-activated|added|removed|moved`,
`output-added|removed`, `window-opened|closed|focused|moved`, `park`, `restore`,
`stage`, `unstage`, `scratchpad-toggle|show|hide`, `idle-transition`,
`idle-active`, `rotation-applied` and `adapter-delivery` (one event per adapter
delivery report).

Subscriptions are per connection and never block the daemon: every subscriber
has a bounded queue, and a client that falls behind is disconnected with a
final `slow_subscriber` error rather than stalling the tracker or other
clients. A subscribed connection is read-only; there is no in-band
`unsubscribe` — close the connection, which ends the stream and removes the
subscriber. `loomctl subscribe park restore` is a small reference client.

## Usage

```sh
loom-companion                 # serve on the default socket
loom-companion --socket PATH   # serve on PATH
loom-companion --once          # serve one connection, for tests

loomctl list
loomctl describe
loomctl status
loomctl call system ping
loomctl call parking park '{"id":"win-1","value":{"title":"editor"}}'
loomctl call automation set '{"name":"focus","steps":[{"namespace":"system","action":"ping"}]}'
loomctl call automation run '{"name":"focus"}'
loomctl --dry-run call automation run '{"name":"focus"}'  # show steps, run nothing
NIRI_SOCKET=... loomctl call compositor version
loomctl call compositor action '{"action":{"FocusColumnRight":{}}}'

loomctl explain rules '{"window":10}'      # rules.explain
loomctl explain windows '{"app_id":"firefox"}'  # windows.find
loomctl explain adapters '{"event":"workspace-activated"}'
loomctl history 20
loomctl config show
loomctl config validate
loomctl config reload
loomctl subscribe park restore
```

### systemd user unit

The split packages install an optional unit at
`/usr/lib/systemd/user/loom-companion.service`. It is ordered after
`graphical-session.target` so niri has already imported `NIRI_SOCKET` and the
Wayland environment; it has no sandboxing because niri IPC and user hooks need
the session environment.

```sh
systemctl --user enable --now loom-companion.service
systemctl --user status loom-companion.service
```

The unit runs `loom-companion` with the default socket and restarts on
failure. To use a non-default socket or configuration, override the unit with
`systemctl --user edit loom-companion.service` rather than editing the
packaged file.

`--id N` echoes a request id on the response; `--dry-run` adds
`"dry_run": true` to `call`/`explain` params. `automation.run` and `rules.run`
honour it by returning their expanded steps without dispatching or recording a
history entry; idle/rotate/failing-adapter paths already understand it.

`compositor.*` connects to the running niri IPC socket (`$NIRI_SOCKET`, or
`--socket` on the daemon if it was started with a different environment).

## Auto-tracked lots

When `$NIRI_SOCKET` is set, the daemon subscribes to niri's event stream and
maintains two ordinary parking lots:

- `auto:workspaces` — every workspace keyed by stable id (name, output,
  active/focused/urgent/hidden, active window).
- `auto:windows` — every open window keyed by stable id (title, app id,
  workspace, focus, floating/sticky/minimized/urgent, layout).

Window closes remove entries; activation/focus/minimize/urgency events update
them. Use `loomctl call parking list '{"lot":"auto:windows"}'`. Start the
daemon with `--no-track` to disable the subscription.

## Parking and restoring on the compositor

`parker.*` consumes the auto lots through the niri bridge:

```sh
loomctl call parker park                                  # focused window
loomctl call parker park '{"window":70}'
loomctl call parker restore '{"window":70}'
loomctl call parker park-workspace '{"workspace":7}'
loomctl call parker restore-workspace '{"workspace":7,"focus":true}'
loomctl call parker list
```

Parking minimizes the window(s) and records their snapshots in the `parker`
lot; restoring unminimizes them (and optionally focuses the workspace).
`park-workspace` minimizes every window tracked for that workspace.

## Staged windows and scratchpads (nsticky absorption)

nsticky's staged-window workflow is part of the companion daemon. Its native
primitive stays in the compositor: `niri-ipc` already exposes
`Action::ToggleWindowSticky`, so the daemon only sends that action and never
runs a second sticky engine. Staging and scratchpads reuse the parking store and
the same niri bridge as `parker.*`; they do not open a second IPC connection.

### Staging (`stage.*`)

Staging parks a window out of the way. The implementation is deliberately the
existing parking primitive — the window is minimized and its snapshot is stored
in the `stage` lot — so a staged window behaves exactly like a parked one and
there is no parallel compositor path. Activating it unminimizes it.

```sh
loomctl call stage stage '{"window":70}'       # or omit `window` for focus
loomctl call stage list                        # numeric window-id order
loomctl call stage activate '{"window":70}'    # aliases: unstage, restore
loomctl call stage toggle '{"window":70}'      # stage ⇄ activate
```

`stage.list` returns `{lot, windows: [{window, snapshot}], count}` sorted by
numeric window id, so repeated calls and scripted diffs are stable.
`stage.activate` is idempotent: a window that is not staged reports
`"activated": false` and sends no extra niri action. Staged windows persist in
the `stage` lot of `$XDG_STATE_HOME/loom-companion/parking.json`, so they
survive a daemon restart.

### Named scratchpads (`scratchpad.*`)

Scratchpads are **named slots**, not nsticky's LIFO stack. Every slot holds at
most one window; the same window cannot live in two slots. Hiding a slot
minimizes its window (the shared parking primitive), showing it moves the window
to the focused workspace, unminimizes it and focuses it.

```sh
loomctl call scratchpad assign '{"name":"term","window":70}'  # alias: add
loomctl call scratchpad toggle '{"name":"term"}'
loomctl call scratchpad show   '{"name":"term"}'             # alias: restore
loomctl call scratchpad hide   '{"name":"term"}'
loomctl call scratchpad list
loomctl call scratchpad remove '{"name":"term"}'
loomctl call scratchpad reconcile
```

`name` (alias `slot`) defaults to `default`; an empty or over-64-byte name is
rejected. `toggle` with an empty slot assigns the focused window and hides it,
then shows and hides it on later presses. Assigning a different window over an
occupied slot is rejected unless `"replace": true` is passed; with `replace` the
old window is unminimized first. Re-assigning a window that already lives in
another slot moves it and reports `"moved_from"`.

```sh
# One key per scratchpad; multiple slots coexist.
loomctl call scratchpad toggle '{"name":"term"}'
loomctl call scratchpad toggle '{"name":"music"}'
```

`assign` returns `{action, slot, window, hidden, replaced, moved_from}`;
`toggle`/`show`/`hide` return the slot, window, `hidden` and whether the call
changed anything. `list` returns slots ordered by name. `show` accepts
`"focus": false` to place and unminimize without focusing.

Assignments are persisted in the `scratchpad` lot of the same
`$XDG_STATE_HOME/loom-companion/parking.json` and are readable immediately
after a daemon restart. `scratchpad.reconcile` drops slots whose window no
longer exists; if niri is unreachable it returns an error and leaves the store
untouched rather than wiping it. A removed slot restores its hidden window
before the assignment is dropped.

### Native sticky windows

The compositor primitive is exposed directly instead of being reimplemented:

```sh
loomctl call windows toggle-sticky '{"window":70}'   # or omit for focus
loomctl call windows set-sticky '{"window":70,"sticky":true}'
loomctl call windows list-sticky
loomctl call windows find '{"app_id":"firefox"}'     # includes `is_sticky`
```

`set-sticky` queries the live window list and sends `ToggleWindowSticky` only
when the flag must change, so repeated calls are no-ops. `toggle-sticky` reports
the previous and new state. `list-sticky` returns sticky windows in numeric id
order. The generic `compositor.action` escape hatch accepts
`{"ToggleWindowSticky":{"id":70}}` too.

### Compatibility (`nsticky.*`)

The `nsticky` namespace maps the old CLI verbs onto the canonical actions and
adds no state. It is safe to point existing keybindings at `loomctl call
nsticky ...` while the standalone nsticky repository remains untouched.

| nsticky command | Companion call |
| --- | --- |
| `nsticky sticky add <id>` | `nsticky sticky.add '{"window":<id>}'` → `windows.set-sticky` |
| `nsticky sticky remove <id>` | `nsticky sticky.remove '{"window":<id>}'` |
| `nsticky sticky toggle-active` | `nsticky sticky.toggle-active` → `windows.toggle-sticky` |
| `nsticky sticky list` | `nsticky sticky.list` → `windows.list-sticky` |
| `nsticky stage add <id>` | `nsticky stage.add '{"window":<id>}'` → `stage.stage` |
| `nsticky stage remove <id>` | `nsticky stage.remove '{"window":<id>}'` → `stage.activate` |
| `nsticky stage toggle-active` | `nsticky stage.toggle-active` → `stage.toggle` |
| `nsticky stage restore` | `nsticky stage.restore '{"window":<id>}'` (no selector: pass ids) |
| `nsticky stage list` | `nsticky stage.list` |
| `nsticky scratchpad add` | `nsticky scratchpad.add '{"name":"default"}'` → `scratchpad.assign` |
| `nsticky scratchpad restore` | `nsticky scratchpad.restore '{"name":"default"}'` → `scratchpad.show` |
| `nsticky scratchpad toggle [name]` | `nsticky scratchpad.toggle '{"name":...}'` |
| `nsticky scratchpad list` | `nsticky scratchpad.list` |

`sticky.add`/`sticky.remove` force their flag, so a stale script cannot turn
`add` into a removal. Unsupported verbs such as `stage.add-all`,
`stage.remove-all` and `sticky.toggle-appid` are rejected with an unknown-action
error; scripts should pass explicit window ids or use `windows.find` first. The
scratchpad aliases keep the named-slot policy, so `scratchpad.add` is not a LIFO
push and `scratchpad.restore` does not pop.

## Rules and run-or-raise (Phase B)

```sh
# Run-or-raise: focus a matching window, or spawn it.
loomctl call windows run-or-raise '{"app_id":"firefox","spawn":["firefox"]}'
loomctl call windows singleton '{"app_id":"Alacritty","spawn_sh":"alacritty"}'
loomctl call windows find '{"title":"Mozilla"}'

# Rules match auto-tracked windows and carry steps.
loomctl call rules set '{
  "name":"browser",
  "match":{"app_id":"firefox"},
  "priority":10,
  "steps":[{"namespace":"parker","action":"park","params":{}}]
}'
loomctl call rules explain '{"window":10}'
loomctl call rules apply '{"window":10}'
loomctl call rules run '{"window":10}'   # registry executes the steps

# Phase C adapters: optional commands and event subscriptions, no hard dependency.
loomctl call adapters status
loomctl call session enable
loomctl call session set '{"name":"lock","command":"loginctl lock-session"}'
loomctl call session dispatch '{"name":"lock"}'   # explicit, requires enable
loomctl call session set-hook '{"event":"workspace-activated","command":"notify-send switched"}'
loomctl call audio status
```

Adapters are disabled by default. An adapter never executes a command
automatically unless `enabled` is `true`; `dispatch` and `test` are the two
explicit entry points, and `test` is the documented way to try a command while
disabled. No `loom-*` binary is linked or required: every command is just a
string you configure.

### Adapter schema

Each adapter persists to
`$XDG_STATE_HOME/loom-companion/adapter-<name>.json`. Files written by older
companions (`enabled`, `commands`, `hooks`) keep loading unchanged; every new
field is optional and serde-defaulted. Legacy `hooks` still resolve to one
command per event and are treated as subscriptions with adapter defaults when
the adapter is enabled.

```json
{
  "enabled": true,
  "commands": { "apply": "loom-wallpaper apply" },
  "hooks": { "workspace-activated": "notify-send switched" },
  "subscriptions": {
    "workspace-activated": [
      {
        "command": "loom-wallpaper apply",
        "label": "wallpaper",
        "timeout_ms": 2000,
        "dedup_ms": 500,
        "failure_policy": "log"
      }
    ],
    "window-focused": [
      { "command": "loom-audio apply", "dedup_ms": 250 }
    ]
  },
  "timeout_ms": 5000,
  "dedup_ms": 1000,
  "max_concurrent": 2,
  "failure_policy": "disable-after-3"
}
```

| Field | Meaning |
| --- | --- |
| `enabled` | Master switch; default `false`. |
| `commands` | Named command templates used by `dispatch`. |
| `hooks` | Legacy `event -> command`; additive with subscriptions. |
| `subscriptions` | `event -> [subscription]`; each has `command`, optional `label`, `timeout_ms`, `dedup_ms`, `failure_policy`. |
| `timeout_ms` | Default hard timeout; default 5000. `0` is rejected for the adapter default. |
| `dedup_ms` | Suppress the same event + subscription + payload within this window; default 1000, `0` disables dedup. |
| `max_concurrent` | Simultaneous processes per adapter, 1–64; default 2. |
| `failure_policy` | `ignore`, `log`, `disable`, `disable-after-N`, or `{"kind":"disable_after","failures":N}`. Default `log`. |

`disable-after-N` counts consecutive failures and flips `enabled` to `false`
without restarting the daemon. A success resets the counter.

### Command templates and environment

Placeholders are replaced as one shell-quoted word, so a window title such as
`x'; rm -rf` cannot become shell syntax. Supported names:
`{{workspace_id}}`, `{{workspace_name}}`, `{{workspace_index}}`, `{{output}}`,
`{{window_id}}`, `{{app_id}}`, `{{title}}`, `{{focused}}`, `{{phase}}`,
`{{event}}`, `{{adapter}}`, plus any other top-level payload field. A missing
field expands to an empty quoted word and is reported in `unresolved`.

The same values are exported to the command:
`COMPANION_ADAPTER`, `COMPANION_EVENT`, `COMPANION_EVENT_JSON`,
`COMPANION_PHASE`, `COMPANION_WORKSPACE_ID`/`_NAME`/`_INDEX`,
`COMPANION_OUTPUT`, `COMPANION_WINDOW_ID`, `COMPANION_APP_ID`,
`COMPANION_TITLE` and `COMPANION_FOCUSED` (missing values are empty).

```sh
# One subscription with a dedup window and a self-disabling failure policy.
loomctl call audio subscribe '{"event":"window-focused","command":"loom-audio apply","dedup_ms":250}'
loomctl call audio configure '{"timeout_ms":2000,"max_concurrent":1,"failure_policy":"disable-after-3"}'

# Explain what would run (no execution), then try it explicitly while disabled.
loomctl call adapters explain '{"event":"workspace-activated","payload":{"workspace_name":"dev"}}'
loomctl call adapters test '{"adapter":"wallpaper","event":"workspace-activated","payload":{"workspace_name":"dev"},"dry_run":true}'
loomctl call adapters test '{"adapter":"wallpaper","event":"workspace-activated","payload":{"workspace_name":"dev"}}'

# Run one named command on one adapter (requires that adapter's enable).
loomctl call adapters dispatch '{"adapter":"monitor","name":"apply"}'
```

### Events

The daemon forwards niri events from the connection it already uses for the
parking tracker and activity clock, so adapters do not open another
subscription. niri has no `WorkspaceAdded`/`OutputRemoved` events: the adapter
mapper keeps the previous snapshot and derives them. The first snapshot after
each (re)connect only seeds state, so a restart does not replay every open
window. Companion actions are observed after they succeed, including actions
executed by `automation.run` or `rules.run`.

| Event | Source / payload |
| --- | --- |
| `workspace-activated` | niri `WorkspaceActivated`; workspace id/name/index, output, `focused`, `phase`. |
| `workspace-added` | Derived from `WorkspacesChanged`; workspace fields, `phase: added`. |
| `workspace-removed` | Derived from `WorkspacesChanged`; last known workspace fields. |
| `workspace-moved` | Output or index changed; also `previous_output`, `previous_index`. |
| `output-added` | New output name in the workspace snapshot; `output`, `phase`. |
| `output-removed` | No workspace remains on that output; `output`, `phase`. |
| `window-opened` | New window id; window id, app id, title, workspace fields, output, `focused`. |
| `window-closed` | Last known window fields, `phase: closed`. |
| `window-focused` | `WindowFocusChanged`; `window_id: null` and `focused: false` when focus is lost. |
| `window-moved` | Window changed workspace; also `previous_workspace_id`. |
| `park` | `parker.park*`; `phase` names the action. |
| `restore` | `parker.restore*`; `phase` names the action. |
| `stage` / `unstage` | `stage.stage|add|toggle` / `stage.activate|unstage|restore`. |
| `scratchpad-toggle` | `scratchpad.toggle`. |
| `scratchpad-show` / `scratchpad-hide` | `scratchpad.show|restore` / `assign|hide`. |
| `idle-transition` | Each executed idle timeout; `phase`, `idle_secs`, `after_secs`. |
| `idle-active` | `idle.reset`. |
| `rotation-applied` | `rotate.tick`/`rotate.set` when the result reports `applied: true`. |

`adapters.events` returns this table plus each adapter's subscribed event names.
`adapters.status` aggregates every adapter; per-adapter `status` also keeps the
legacy `namespace`, `enabled`, `commands` and `hooks` fields and adds
`subscriptions`, limits and runtime stats (`executed`, `failed`, `timed_out`,
`skipped_dedup`, `skipped_disabled`, `peak_concurrency`, last error).

### Built-in adapter presets

`preset` is read-only; `apply-preset` is the explicit write and never enables
the adapter unless you pass `"enable": true`.

```sh
loomctl call monitor apply-preset '{}'      # apply profile on output hotplug
loomctl call audio apply-preset '{}'        # route/default sink on focus changes
loomctl call wallpaper apply-preset '{}'    # apply scene on workspace focus
loomctl call session preset                 # capture/restore command suggestions
loomctl call monitor enable                 # still required for automatic events
```

Presets use documented commands (`loom-monitor apply`, `loom-audio apply`,
`loom-wallpaper apply`, `loom-session capture|restore`) and environment
placeholders such as `${LOOM_MONITOR_PROFILE:-default}`. They are examples,
not a runtime dependency.

Session capture/restore is deliberately opt-in because capture can be
expensive. Subscribe only the phases you want, for example:

```sh
loomctl call session subscribe '{"event":"output-added","command":"loom-session capture auto-{{output}}"}'
loomctl call session subscribe '{"event":"workspace-activated","command":"loom-session restore auto-{{workspace_name}} --wait-ms 800"}'
loomctl call session enable
```

### Adapter action summary

- Per adapter: `status`, `enable`, `disable`, `set`, `remove`, `list`,
  `dispatch`, `set-hook`, `remove-hook`, `hooks`, `subscribe`, `unsubscribe`,
  `subscriptions`, `configure`, `explain`, `test`, `preset`, `apply-preset`.
- Aggregate: `adapters.status`, `adapters.events`, `adapters.explain`,
  `adapters.dispatch`, `adapters.test`, `adapters.enable`, `adapters.disable`,
  `adapters.subscribe`, `adapters.unsubscribe`, `adapters.configure`,
  `adapters.preset`, `adapters.apply-preset`.

Safety notes: enabling is per adapter and persisted; commands are arbitrary
`sh -c` lines you write, so treat the state files as trusted configuration.
Payload values are always shell-quoted, timeouts are hard kills, concurrency is
bounded per adapter, repeated events are deduplicated, and failure policies can
disable an adapter automatically. Adapter commands run on a single background
worker, so a slow command cannot stall the niri tracker.


### Rule match conditions

Every populated field in `match` is ANDed; strings are case-insensitive.

| Field | Matches |
| --- | --- |
| `app_id` / `title` | substring |
| `app_id_regex` / `title_regex` | regular expression |
| `app_id_not` / `title_not` | negated substring |
| `app_id_not_regex` | negated regular expression |
| `mark` / `mark_regex` / `mark_not` | window mark from `marks.*` |
| `workspace` | stable workspace id |
| `workspace_name` / `workspace_name_regex` | name from `auto:workspaces` |
| `workspace_idx` | workspace index on its monitor (`idx`) |
| `workspace_output` | output name substring from `auto:workspaces` |
| `workspace_empty` | true when no tracked window belongs to the workspace |
| `is_floating` / `is_minimized` / `is_sticky` / `is_urgent` | exact boolean |

`rules.set` rejects invalid regular expressions before storing anything, and
older rule files keep loading because every new field is optional.

`rules.explain` reports each matching rule with its `match` object, `priority`
and a `window_state` snapshot (including the resolved `workspace_name`,
`workspace_idx`, `workspace_output` and `workspace_empty`), plus the steps. Matches are ordered by
numeric window id, then higher `priority` first (default `0`), then rule name,
so repeated calls return stable results. `rules.apply` reports the same
matcher/window state with a `step_count` instead of the steps:

```sh
loomctl call rules set '{
  "name":"dev-tools",
  "match":{
    "app_id_regex":"^(Alacritty|kitty)$",
    "title_regex":"nvim",
    "workspace_name":"dev",
    "app_id_not":"chrome"
  },
  "steps":[{"namespace":"parker","action":"park","params":{}}]
}'
loomctl call rules explain '{"window":10}'
```

### Run-or-raise selection

When several windows match, `windows.run-or-raise`/`windows.singleton` sort
matches by window id and raise the lowest-id focused match, or the lowest id
when none is focused. The response reports what happened:

```json
{"action":"raised","raised":11,"title":"firefox","matches":3,
 "candidate_ids":[10,11,12],"was_focused":true}
```

If nothing matched, the `spawn`/`spawn_sh` command runs and the response has
`"action":"spawned"` with `"matches":0` and an empty `candidate_ids`.
`windows.find` also returns matches sorted by id plus a `count`.

Repeated raises are idempotent: when the chosen window is already focused no
`FocusWindow` action is sent, and the response reports
`"already_focused": true`.

### Workspace and output placement

Workspace selectors accept a stable id (`workspace`), a case-insensitive name
(`workspace_name`) or the current per-monitor index (`workspace_idx`, alias
`workspace_index`). `workspace_output` restricts name/index resolution to one
output because indices are only unique per monitor. Supplying more than one of
id/name/index is rejected instead of guessing.

```sh
# Move a window by stable id, by name, or by monitor-local index.
loomctl call windows move-to-workspace '{"window":70,"workspace":7}'
loomctl call windows move-to-workspace '{"window":70,"workspace_name":"dev"}'
loomctl call windows move-to-workspace '{"window":70,"workspace_idx":2,"workspace_output":"HDMI-1"}'

# Move and switch the viewport to the destination ("follow the window").
loomctl call windows follow-to-workspace '{"window":70,"workspace_name":"web"}'

# Output targeting by output name (case-insensitive).
loomctl call windows move-to-output '{"window":70,"output":"HDMI-1"}'
loomctl call windows focus-output '{"output":"HDMI-1"}'
```

`move-to-workspace` follows focus by default (`"focus":true`); pass
`"focus":false` to leave the viewport where it is. `follow-to-workspace` always
follows. Both resolve names and indices to a stable id, then send niri's
`MoveWindowToWorkspace`, and report `window`, `workspace`, `workspace_name`,
`output` and `followed`.

`windows.find` (and therefore run-or-raise matching) accepts the same workspace
selectors plus an `output` filter. A window has no output of its own, so the
filter is resolved through the workspace it lives on:

```sh
loomctl call windows find '{"app_id":"firefox","workspace_name":"web"}'
loomctl call windows find '{"output":"HDMI-1"}'
```

`parker.park-workspace`/`parker.restore-workspace` accept
`workspace_name`/`workspace_idx` too, so scripts do not have to look up ids
first.

## Window marks (`marks.*`)

Marks are short unique names attached to window ids, persisted in
`$XDG_STATE_HOME/loom-companion/marks.json`. They are selectors for
`windows.find`/`run-or-raise`, are reported by `windows.find`/`list-sticky`,
and are a rule condition.

```sh
loomctl call marks set '{"window":70,"mark":"browser"}'   # or omit window for focus
loomctl call marks get '{"mark":"browser"}'
loomctl call marks list
loomctl call marks remove '{"window":70}'
loomctl call marks reconcile       # drop marks whose window is gone
loomctl call windows find '{"mark":"browser"}'
loomctl call windows run-or-raise '{"mark":"browser","spawn":["firefox"]}'
```

Re-using a mark moves it to the new window and reports `moved_from`. Marks are
not niri state: `reconcile` (and rule evaluation) tolerate a mark for a window
that no longer exists by simply not matching it. Rules match `mark` (exact),
`mark_regex` and `mark_not`.

## Workspace-triggered rules

Rules can be triggered by workspace events instead of window state. Add
`"trigger": "workspace-activated"` (or `workspace-added`, `workspace-removed`,
`workspace-moved`) and use workspace fields in `match`; window-only matchers
are rejected for workspace rules.

```sh
loomctl call rules set '{
  "name":"dev-layout",
  "trigger":"workspace-activated",
  "match":{"workspace_name":"dev","workspace_output":"eDP-1"},
  "steps":[{"namespace":"windows","action":"focus-output","params":{"output":"eDP-1"}}]
}'
loomctl call rules explain '{"event":"workspace-activated"}'
loomctl call rules run '{"event":"workspace-activated"}'
# For a removed workspace, pass its last known state explicitly.
loomctl call rules explain '{
  "event":"workspace-removed",
  "workspace_state":{"id":9,"name":"gone","idx":9,"output":"HDMI-1"}
}'
```

Without `workspace_state`, evaluation covers the current `auto:workspaces`
snapshot. Matches are ordered by numeric workspace id, then rule name, so
explain/run output is deterministic. `rules.run`/`--dry-run` never mixes window
rules into a workspace event (and vice versa).

Use `workspace_empty` for niri-empty style actions: it is true when no tracked
window belongs to the workspace. Niri always keeps one empty workspace, so this
fires when focus moves to it without a separate daemon.

```sh
loomctl call rules set '{
  "name":"empty-hint",
  "trigger":"workspace-activated",
  "match":{"workspace_empty":true},
  "steps":[{"namespace":"hooks","action":"fire","params":{"event":"empty-workspace"}}]
}'
```

## Automation history (`history.*`)

`automation.run` and `rules.run` append a record to a bounded (256-entry)
persisted log: timestamp, source, sequence/rule label, expanded steps and
`{ok, value}` or `{ok:false,error}`. Dry runs are not recorded.
`$XDG_STATE_HOME/loom-companion/history.json` keeps the log across restarts.

```sh
loomctl history 20
loomctl call history list '{"limit":5}'
loomctl call history clear
```

`history.list` is newest-first and reports `count`, `total` and `capacity`;
`limit` is capped at 1000.

## Config file and hot reload (`config.*`)

The daemon reads an optional declarative layer at
`$XDG_CONFIG_HOME/loom-companion/config.json` (override with `--config PATH`).
It is read-only for the daemon and never overwritten; without it every action
works exactly as before. Sections merge listed entries into live contexts:

```json
{
  "rules": {
    "browser": {
      "match": { "app_id": "firefox", "mark_not": "scratch" },
      "steps": [{ "namespace": "parker", "action": "park" }]
    }
  },
  "hooks": { "workspace-activated": ["notify-send switched"] },
  "automation": {
    "focus": [{ "namespace": "windows", "action": "focus-output", "params": { "output": "eDP-1" } }]
  },
  "adapters": { "audio": { "enabled": false, "dedup_ms": 250 } }
}
```

The file is validated at startup (an invalid file stops the daemon with the
error list) and on demand:

```sh
loomctl config show
loomctl config validate
loomctl config validate '{"config":{"rules":{}}}'
loomctl config reload
```

`show` reports the path, existence, parsed content and section counts;
`validate` returns `{valid, errors, sections}` without applying anything;
`reload` validates first, then merges rules, hooks, automation sequences and
adapter defaults into the same live contexts. Nothing is written back to the
config file, and unlisted stored entries are kept.

## Terminal swallowing

`swallow.*` is opt-in. When enabled, a newly opened window whose process is a
descendant of an allowlisted terminal window minimizes that terminal; closing
the child restores it. Swallowing also runs for windows that already exist when
the daemon connects, so a restart does not miss open children.

```sh
loomctl call swallow enable
loomctl call swallow status
loomctl call swallow add-terminal '{"app_id":"myterm"}'
loomctl call swallow list
loomctl call swallow restore '{"child":42}'
loomctl call swallow reconcile   # drop pairs whose child is gone
loomctl call swallow reload      # re-read swallow.json and enforce max_pairs
```

The parent chain is read from `/proc/<pid>/status`; the default terminal
allowlist covers Alacritty, foot, kitty, WezTerm, Ghostty, Konsole,
gnome-terminal and xterm. Multiple terminal app ids are supported through the
allowlist or per-rule matching.

### Swallow rules and ignore lists

`swallow.add-rule` adds an ordered rule; the first matching rule decides and
`add-rule` with the same `name` replaces it. Every populated field must match:

| Field | Matches |
| --- | --- |
| `terminal_app_id` | terminal app-id substrings |
| `terminal_app_id_regex` / `terminal_title_regex` | terminal regex |
| `child_app_id` | child app-id substrings |
| `child_app_id_regex` / `child_title_regex` | child regex |
| `ignore` | veto swallowing when the rule matches |
| `child_age_ms` | override the child process age window |
| `restore_on_exit` | override restoring the terminal on child exit |

A matching non-ignore rule allows swallowing even when the terminal is not in
the global allowlist. Rules are persisted with the rest of the swallow config.

```sh
# Media apps launched from any allowlisted terminal are swallowed.
loomctl call swallow add-rule '{
  "name":"media",
  "terminal_app_id":["alacritty","kitty"],
  "child_app_id":["mpv","vlc"]
}'

# Never swallow file dialogs by title.
loomctl call swallow add-rule '{
  "name":"dialogs",
  "child_title_regex":"^(Open|Save|Confirm)",
  "ignore":true
}'

# Veto children by app id/title before rules are evaluated.
loomctl call swallow set-ignore '{"ignore":["zenity","yad"]}'

# Only swallow children whose process started in the last 5 seconds.
loomctl call swallow configure '{"child_age_ms":5000}'
loomctl call swallow remove-rule '{"name":"dialogs"}'
```

`child_age_ms` is a time window measured from the child's process start
(`/proc/<pid>/stat`, via `/proc/uptime`). It prevents long-running processes
from being swallowed when they merely open another window; a per-rule
`child_age_ms` overrides the global value. `configure` also accepts
`"restore_on_exit": false` to leave terminals minimized when a child exits and
`"max_pairs"` to bound the number of live pairs.

Swallowed pairs are stored in the `swallow` parking lot, including the terminal,
child, matching rule and restore preference, so they survive a daemon restart.
Each pair also records the child/terminal workspace and output; when either
window moves while the pair is live, those fields follow the window without
breaking the pair. A child that opens on another workspace or output is still
paired, and restoration targets the terminal by window id, so moving it does
not orphan the pair.

Restoring is idempotent and serialised: the pair is removed atomically, so
repeated or concurrent `restore` calls report `"restored": false` after the
first and send no extra niri action. `reconcile` returns removed child ids in
ascending numeric order and a second call is a no-op. Closing the terminal first
drops its pairs without trying to restore it.

The pair store is bounded by `max_pairs` (default 512); the oldest child ids are
evicted when it is exceeded, so a missed close event cannot grow state forever.
Every config mutation and `swallow.reload` (which re-reads `swallow.json`)
enforces the bound immediately. `swallow.status` reports the allowlist, ignore
list, age window, restore preference, `max_pairs`, rules and live pairs.

## Idle and suspend (`idle.*`)

`idle` is a companion module, not a separate service. It owns a timeout policy,
inhibitors and a pluggable activity clock; it does not require a display, and
the default policy is disabled with no timeouts.

```sh
# Configure once; flat fields or a nested `policy` object both work.
loomctl call idle configure '{
  "policy": {
    "enabled": true,
    "timeouts": [
      {"after_secs": 300, "action": "lock"},
      {"after_secs": 600, "action": "dpms-off"},
      {"after_secs": 1800, "action": "suspend"}
    ],
    "audio_inhibitor": true,
    "fullscreen_inhibitor": true
  }
}'

loomctl call idle status
loomctl call idle explain '{"idle_secs": 700}'   # simulate, never executes
loomctl call idle tick                           # run due steps once
loomctl call idle inhibit '{"reason":"meeting","duration_secs":3600}'
loomctl call idle uninhibit
loomctl call idle reset                          # activity now
```

`idle.tick` is the only action that executes a timeout. It runs a due step only
when the policy is enabled, not in `dry_run`, and no inhibitor is active; fired
steps are remembered by `(after_secs, action)` so a repeated tick is
idempotent. `idle.reset` clears that set, resets the activity clock and powers
monitors back on if `dpms-off` had fired. `idle.status`/`idle.explain` never
execute and report each timeout as `pending`, `due`, `inhibited` or `fired`.

- **Activity**: the daemon marks every niri focus/window/workspace event as
  activity and `idle.reset` does so explicitly. The clock starts at daemon
  start; it is deliberately not persisted across restarts.
- **Inhibitors**: audio (a non-corked PulseAudio/PipeWire sink input via
  `pactl`), fullscreen (a focused window covering an output) and user-requested
  (persisted, optionally timed). niri IPC has no `is_fullscreen` flag, so the
  fullscreen check is a tile/output-size heuristic. A probe that cannot run is
  `unavailable` and does **not** block the policy; `audio_command` and
  `fullscreen_command` replace the built-ins (exit 0 active, 1 free, other
  unavailable).
- **Actions**: `lock` and `suspend` shell out to logind (`busctl` first, then
  `loginctl lock-sessions`/`systemctl suspend`). `dpms-off` uses niri's
  `PowerOffMonitors`; resuming sends `PowerOnMonitors`. `command` runs its own
  timeout command only when that timeout is configured.
- **Dry run**: set `"dry_run": true` or call `idle.tick` on a disabled policy;
  it reports the due steps and `skipped` reason without touching the session.
  `logind_idle_hint` adds logind's boolean `IdleHint` to status; it never resets
  the local idle clock.

The D-Bus integration is a shell-out on purpose: the cached `dbus` crate needs
libdbus at build time. If `busctl`, `loginctl`, `systemctl` or `pactl` is
missing, the corresponding step/probe degrades to an error or `unavailable`
instead of failing the daemon.

The daemon calls `idle.tick` every 5 seconds (`--once` skips timers). Policy is
stored in `$XDG_STATE_HOME/loom-companion/idle.json`; only the policy, user
inhibitor, fired set and DPMS flag are persisted. `--no-track` disables niri
events, so only `idle.reset` marks activity in that mode.

## Auto-rotation (`rotate.*`)

`rotate` maps an accelerometer orientation to a niri output transform (and an
optional position). It is disabled by default and never applies anything until
`tick` runs on an enabled, unlocked policy.

```sh
loomctl call rotate configure '{
  "enabled": true,
  "output": "eDP-1",
  "hysteresis_ms": 1000,
  "position": [0, 0]
}'
loomctl call rotate status
loomctl call rotate explain
loomctl call rotate tick
loomctl call rotate lock            # pause auto-rotation
loomctl call rotate unlock
loomctl call rotate set '{"orientation":"left-up"}'
loomctl call rotate set '{"transform":"270","output":"DP-2"}'
```

The orientation source is `sensor_command` when configured; otherwise
`iio-sensor-proxy` is read through `busctl`. A missing command/proxy reports
`source-unavailable` in status/tick instead of failing the daemon. Orientation
values are `normal`, `left-up`, `right-up`, `bottom-up`, `left-down` and
`right-down`; the mapping is fixed (`normal`→`normal`, `left-up`→`90`,
`bottom-up`→`180`, `right-up`→`270`, `left-down`→`flipped-90`,
`right-down`→`flipped-270`). Use `rotate.set` with a raw transform for a
different convention.

`rotate.tick` debounces: a new orientation must stay stable for `hysteresis_ms`
before it is applied, and a change inside the window restarts the timer.
Status/explain only observe and never mutate the debounce state. Auto-apply
requires `enabled`, `lock` off, a configured `output` and a stable orientation;
manual `rotate.set` bypasses `enabled`/`lock` but not `dry_run`. The last applied
transform is persisted in `rotate.json`; the debounce timer is process-local.
The daemon calls `rotate.tick` every second (`--once` skips timers), so the
configured hysteresis is evaluated without a separate service.

## Daemon robustness

The socket server keeps serving after bad input. Malformed JSON, unknown request
kinds and partial requests produce an `error` response (or a clean close when
the peer disappears) without stopping the accept loop. Request lines are capped
at 1 MiB; an oversized line is drained and rejected so one client cannot make
the daemon allocate without bound.

The niri event loop is similarly tolerant: malformed event lines, unknown event
kinds and a truncated final line are logged and skipped, and the loop continues
with the next event. Transport errors end that connection, and the tracker
reconnects. Each reconnect starts from niri's full snapshot and clears the
previous cache, so windows that vanished while disconnected are not retained.
The cache is also capped at 4096 windows, evicting the lowest (oldest) ids in a
deterministic order.

## Contexts

- `parking.park|unpark|list|lots|clear`: parked values keyed by lot and id.
- `automation.set|remove|list|get|run`: sequences of `(namespace, action,
  params)` steps; `run` dispatches through the registry, nested
  `automation.run` is rejected.
- `hooks.add|remove|list|fire`: event → commands. `fire` returns the matching
  commands and payload; the daemon never executes commands implicitly.
- `system.ping|status|paths`.
- `windows.run-or-raise|singleton|find|move-to-workspace|follow-to-workspace|
  move-to-output|focus-output|toggle-sticky|set-sticky|list-sticky`; `find`
  accepts `mark` and reports each window's current mark.
- `marks.set|remove|get|list|reconcile`: persistent unique window marks.
- `history.list|clear`: bounded record of completed rule/automation runs.
- `parker.park|restore|park-workspace|restore-workspace|list`.
- `stage.stage|list|activate|unstage|restore|toggle`.
- `scratchpad.assign|add|toggle|show|restore|hide|list|remove|reconcile`.
- `nsticky.sticky.add|remove|toggle-active|list`,
  `nsticky.stage.add|remove|restore|toggle-active|list`,
  `nsticky.scratchpad.add|restore|toggle|list|remove` (compatibility aliases).
- `swallow.enable|disable|status|list|restore|add-terminal|remove-terminal|
  add-rule|remove-rule|set-ignore|configure|reconcile|reload`.
- `rules.set|remove|list|explain|apply|run`: window rules and
  workspace-event rules (`trigger`, `event`/`workspace_state` for explain/run).
- `config.show|validate|reload`: optional config file without writing it.
- `idle.status|explain|tick|configure|enable|disable|inhibit|uninhibit|reset`.
- `rotate.status|explain|tick|configure|enable|disable|lock|unlock|set|reset`.
- `session.*`, `monitor.*`, `audio.*`, `wallpaper.*`, `adapters.*`: optional
  adapters with explicit commands, event subscriptions, templates, timeouts,
  dedup, concurrency limits and `ignore`/`log`/`disable-after-N` failure policy
  (see “Phase C adapters” above).

## Development

```sh
cd loom-companion
cargo test --workspace
cargo clippy --all-targets
```
