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
| `companion-niri` | `compositor.*`, `parker.*`, `windows.*` | niri IPC bridge, event tracker, park/restore and run-or-raise/singleton |
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
| `is_floating` / `is_minimized` / `is_sticky` / `is_urgent` | exact boolean |

`rules.set` rejects invalid regular expressions before storing anything, and
older rule files keep loading because every new field is optional.

`rules.explain` reports each matching rule with its `match` object and a
`window_state` snapshot (including the resolved `workspace_name`), plus the
steps. `rules.apply` reports the same matcher/window state with a `step_count`
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
`"restore_on_exit": false` to leave terminals minimized when a child exits.

Swallowed pairs are stored in the `swallow` parking lot, including the terminal,
child, matching rule and restore preference, so they survive a daemon restart.
Restoring is idempotent: a missing pair reports `"restored": false` instead of
failing. Closing the terminal first drops its pairs without trying to restore
it; on reconnect (or `swallow reconcile`) pairs whose child no longer exists are
removed and a still-open terminal is unminimized. `swallow.status` reports the
allowlist, ignore list, age window, restore preference, rules and live pairs.

## Contexts

- `parking.park|unpark|list|lots|clear`: parked values keyed by lot and id.
- `automation.set|remove|list|get|run`: sequences of `(namespace, action,
  params)` steps; `run` dispatches through the registry, nested
  `automation.run` is rejected.
- `hooks.add|remove|list|fire`: event → commands. `fire` returns the matching
  commands and payload; the daemon never executes commands implicitly.
- `system.ping|status|paths`.

## Development

```sh
cd better-companion
cargo test --workspace
cargo clippy --workspace --all-targets
```
