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
