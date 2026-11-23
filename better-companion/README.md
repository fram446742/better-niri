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
```

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
