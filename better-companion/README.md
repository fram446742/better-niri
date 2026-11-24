# better-companion

One daemon with bounded-context crates, reachable over a Unix socket. This is
the first slice of the Better Desktop companion plan: the shared protocol, the
parking/automation/hooks/system contexts, the daemon and `companionctl`.

The workspace lives inside `better-niri` so no new repositories are created; it
is excluded from the niri Cargo workspace and keeps its own lockfile/build.

## Crates

| Crate | Namespace | Responsibility |
| --- | --- | --- |
| `companion-core` | — | JSON-line protocol, action registry, state/socket paths |
| `companion-parking` | `parking.*` | Named lots of parked items (nsticky/staged workflow) |
| `companion-automation` | `automation.*` | Named action sequences executed through the registry |
| `companion-hooks` | `hooks.*` | Commands registered per event (resolution only; no implicit execution) |
| `companion-system` | `system.*` | Ping, status and paths |
| `companion-rules` | `rules.*` | Declarative window/workspace rules with explain/apply/run |
| `companion-adapters` | `session.*`, `monitor.*`, `audio.*`, `wallpaper.*` | Optional Phase C ecosystem adapters (commands + event hooks) |
| `companion-niri` | `swallow.*` | Terminal swallowing: minimize a terminal while its launched child is open |
| `companion-niri` | `compositor.*`, `parker.*`, `windows.*` | niri IPC bridge, event tracker, park/restore, run-or-raise/singleton, placement and native sticky |
| `companion-niri` | `stage.*`, `scratchpad.*` | Staged windows and named scratchpads on the shared parking store |
| `companion-niri` | `nsticky.*` | Compatibility aliases for the standalone nsticky CLI verbs |
| `companion-daemon` | — | `better-companion` Unix-socket server |
| `companionctl` | — | CLI client |

## Protocol

One JSON request per line, one JSON response per line:

```json
{"kind":"list"}
{"kind":"call","namespace":"parking","action":"park","params":{"id":"win-1","value":{"title":"editor"}}}
{"kind":"status"}
```

Responses are tagged the same way: `contexts`, `value`, `status` or `error`.
The default socket is `$XDG_RUNTIME_DIR/better-companion.sock` (mode `0600`).
State is persisted under `$XDG_STATE_HOME/better-companion/`.

## Usage

```sh
better-companion                 # serve on the default socket
better-companion --socket PATH   # serve on PATH
better-companion --once          # serve one connection, for tests

companionctl list
companionctl status
companionctl call system ping
companionctl call parking park '{"id":"win-1","value":{"title":"editor"}}'
companionctl call automation set '{"name":"focus","steps":[{"namespace":"system","action":"ping"}]}'
companionctl call automation run '{"name":"focus"}'
NIRI_SOCKET=... companionctl call compositor version
companionctl call compositor action '{"action":{"FocusColumnRight":{}}}'
```

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
them. Use `companionctl call parking list '{"lot":"auto:windows"}'`. Start the
daemon with `--no-track` to disable the subscription.

## Parking and restoring on the compositor

`parker.*` consumes the auto lots through the niri bridge:

```sh
companionctl call parker park                                  # focused window
companionctl call parker park '{"window":70}'
companionctl call parker restore '{"window":70}'
companionctl call parker park-workspace '{"workspace":7}'
companionctl call parker restore-workspace '{"workspace":7,"focus":true}'
companionctl call parker list
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
companionctl call stage stage '{"window":70}'       # or omit `window` for focus
companionctl call stage list                        # numeric window-id order
companionctl call stage activate '{"window":70}'    # aliases: unstage, restore
companionctl call stage toggle '{"window":70}'      # stage ⇄ activate
```

`stage.list` returns `{lot, windows: [{window, snapshot}], count}` sorted by
numeric window id, so repeated calls and scripted diffs are stable.
`stage.activate` is idempotent: a window that is not staged reports
`"activated": false` and sends no extra niri action. Staged windows persist in
the `stage` lot of `$XDG_STATE_HOME/better-companion/parking.json`, so they
survive a daemon restart.

### Named scratchpads (`scratchpad.*`)

Scratchpads are **named slots**, not nsticky's LIFO stack. Every slot holds at
most one window; the same window cannot live in two slots. Hiding a slot
minimizes its window (the shared parking primitive), showing it moves the window
to the focused workspace, unminimizes it and focuses it.

```sh
companionctl call scratchpad assign '{"name":"term","window":70}'  # alias: add
companionctl call scratchpad toggle '{"name":"term"}'
companionctl call scratchpad show   '{"name":"term"}'             # alias: restore
companionctl call scratchpad hide   '{"name":"term"}'
companionctl call scratchpad list
companionctl call scratchpad remove '{"name":"term"}'
companionctl call scratchpad reconcile
```

`name` (alias `slot`) defaults to `default`; an empty or over-64-byte name is
rejected. `toggle` with an empty slot assigns the focused window and hides it,
then shows and hides it on later presses. Assigning a different window over an
occupied slot is rejected unless `"replace": true` is passed; with `replace` the
old window is unminimized first. Re-assigning a window that already lives in
another slot moves it and reports `"moved_from"`.

```sh
# One key per scratchpad; multiple slots coexist.
companionctl call scratchpad toggle '{"name":"term"}'
companionctl call scratchpad toggle '{"name":"music"}'
```

`assign` returns `{action, slot, window, hidden, replaced, moved_from}`;
`toggle`/`show`/`hide` return the slot, window, `hidden` and whether the call
changed anything. `list` returns slots ordered by name. `show` accepts
`"focus": false` to place and unminimize without focusing.

Assignments are persisted in the `scratchpad` lot of the same
`$XDG_STATE_HOME/better-companion/parking.json` and are readable immediately
after a daemon restart. `scratchpad.reconcile` drops slots whose window no
longer exists; if niri is unreachable it returns an error and leaves the store
untouched rather than wiping it. A removed slot restores its hidden window
before the assignment is dropped.

### Native sticky windows

The compositor primitive is exposed directly instead of being reimplemented:

```sh
companionctl call windows toggle-sticky '{"window":70}'   # or omit for focus
companionctl call windows set-sticky '{"window":70,"sticky":true}'
companionctl call windows list-sticky
companionctl call windows find '{"app_id":"firefox"}'     # includes `is_sticky`
```

`set-sticky` queries the live window list and sends `ToggleWindowSticky` only
when the flag must change, so repeated calls are no-ops. `toggle-sticky` reports
the previous and new state. `list-sticky` returns sticky windows in numeric id
order. The generic `compositor.action` escape hatch accepts
`{"ToggleWindowSticky":{"id":70}}` too.

### Compatibility (`nsticky.*`)

The `nsticky` namespace maps the old CLI verbs onto the canonical actions and
adds no state. It is safe to point existing keybindings at `companionctl call
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
companionctl call windows run-or-raise '{"app_id":"firefox","spawn":["firefox"]}'
companionctl call windows singleton '{"app_id":"Alacritty","spawn_sh":"alacritty"}'
companionctl call windows find '{"title":"Mozilla"}'

# Rules match auto-tracked windows and carry steps.
companionctl call rules set '{
  "name":"browser",
  "match":{"app_id":"firefox"},
  "steps":[{"namespace":"parker","action":"park","params":{}}]
}'
companionctl call rules explain '{"window":10}'
companionctl call rules apply '{"window":10}'
companionctl call rules run '{"window":10}'   # registry executes the steps

# Phase C adapters: explicit commands and event hooks, no hard dependency.
companionctl call session enable
companionctl call session set '{"name":"lock","command":"loginctl lock-session"}'
companionctl call session dispatch '{"name":"lock"}'   # only runs when explicit
companionctl call monitor set-hook '{"event":"workspace-activated","command":"waybar reload"}'
companionctl call audio status
```

Adapters are disabled by default. `dispatch` is the only action that executes
a command; hooks just store/resolve them.

### Rule match conditions

Every populated field in `match` is ANDed; strings are case-insensitive.

| Field | Matches |
| --- | --- |
| `app_id` / `title` | substring |
| `app_id_regex` / `title_regex` | regular expression |
| `app_id_not` / `title_not` | negated substring |
| `app_id_not_regex` | negated regular expression |
| `workspace` | stable workspace id |
| `workspace_name` / `workspace_name_regex` | name from `auto:workspaces` |
| `workspace_idx` | workspace index on its monitor (`idx`) |
| `workspace_output` | output name substring from `auto:workspaces` |
| `is_floating` / `is_minimized` / `is_sticky` / `is_urgent` | exact boolean |

`rules.set` rejects invalid regular expressions before storing anything, and
older rule files keep loading because every new field is optional.

`rules.explain` reports each matching rule with its `match` object and a
`window_state` snapshot (including the resolved `workspace_name`,
`workspace_idx` and `workspace_output`), plus the steps. Matches are ordered by
numeric window id, then rule name, so repeated calls return stable results.
`rules.apply` reports the same matcher/window state with a `step_count`
instead of the steps:

```sh
companionctl call rules set '{
  "name":"dev-tools",
  "match":{
    "app_id_regex":"^(Alacritty|kitty)$",
    "title_regex":"nvim",
    "workspace_name":"dev",
    "app_id_not":"chrome"
  },
  "steps":[{"namespace":"parker","action":"park","params":{}}]
}'
companionctl call rules explain '{"window":10}'
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
companionctl call windows move-to-workspace '{"window":70,"workspace":7}'
companionctl call windows move-to-workspace '{"window":70,"workspace_name":"dev"}'
companionctl call windows move-to-workspace '{"window":70,"workspace_idx":2,"workspace_output":"HDMI-1"}'

# Move and switch the viewport to the destination ("follow the window").
companionctl call windows follow-to-workspace '{"window":70,"workspace_name":"web"}'

# Output targeting by output name (case-insensitive).
companionctl call windows move-to-output '{"window":70,"output":"HDMI-1"}'
companionctl call windows focus-output '{"output":"HDMI-1"}'
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
companionctl call windows find '{"app_id":"firefox","workspace_name":"web"}'
companionctl call windows find '{"output":"HDMI-1"}'
```

`parker.park-workspace`/`parker.restore-workspace` accept
`workspace_name`/`workspace_idx` too, so scripts do not have to look up ids
first.

## Terminal swallowing

`swallow.*` is opt-in. When enabled, a newly opened window whose process is a
descendant of an allowlisted terminal window minimizes that terminal; closing
the child restores it. Swallowing also runs for windows that already exist when
the daemon connects, so a restart does not miss open children.

```sh
companionctl call swallow enable
companionctl call swallow status
companionctl call swallow add-terminal '{"app_id":"myterm"}'
companionctl call swallow list
companionctl call swallow restore '{"child":42}'
companionctl call swallow reconcile   # drop pairs whose child is gone
companionctl call swallow reload      # re-read swallow.json and enforce max_pairs
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
companionctl call swallow add-rule '{
  "name":"media",
  "terminal_app_id":["alacritty","kitty"],
  "child_app_id":["mpv","vlc"]
}'

# Never swallow file dialogs by title.
companionctl call swallow add-rule '{
  "name":"dialogs",
  "child_title_regex":"^(Open|Save|Confirm)",
  "ignore":true
}'

# Veto children by app id/title before rules are evaluated.
companionctl call swallow set-ignore '{"ignore":["zenity","yad"]}'

# Only swallow children whose process started in the last 5 seconds.
companionctl call swallow configure '{"child_age_ms":5000}'
companionctl call swallow remove-rule '{"name":"dialogs"}'
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
  move-to-output|focus-output|toggle-sticky|set-sticky|list-sticky`.
- `parker.park|restore|park-workspace|restore-workspace|list`.
- `stage.stage|list|activate|unstage|restore|toggle`.
- `scratchpad.assign|add|toggle|show|restore|hide|list|remove|reconcile`.
- `nsticky.sticky.add|remove|toggle-active|list`,
  `nsticky.stage.add|remove|restore|toggle-active|list`,
  `nsticky.scratchpad.add|restore|toggle|list|remove` (compatibility aliases).
- `swallow.enable|disable|status|list|restore|add-terminal|remove-terminal|
  add-rule|remove-rule|set-ignore|configure|reconcile|reload`.
- `rules.set|remove|list|explain|apply|run`.

## Development

```sh
cd better-companion
cargo test --workspace
cargo clippy --workspace --all-targets
```
