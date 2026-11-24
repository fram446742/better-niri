# better-session

Named **contexts** for niri: capture workspaces, windows, output configuration
and user hooks, then restore them predictably. Part of the Better Desktop
ecosystem; it lives inside this repository so no new repository is created
(excluded from niri's Cargo workspace).

## Features

- Capture workspaces (name/index/output/active/focused), windows (app id, title,
  workspace, floating state, focus) and outputs (mode, scale, transform,
  position, VRR, on/off) into a named context.
- Restore in a fixed order: **outputs → workspaces → windows**. Output changes
  are sent as niri `Request::Output` requests, so `better-session` does not
  depend on the `better-monitor` binary.
- Wait for apps that appear late by polling `Request::Windows` until the
  deadline instead of sleeping for a fixed time (`--wait-ms`).
- Match windows by app id/title, move them to the saved workspace, restore
  floating/tiling state, spawn configured missing apps, focus the saved window
  and report unmatched apps.
- User pre/post restore hooks with `BETTER_SESSION_CONTEXT` and
  `BETTER_SESSION_PHASE` in the environment.
- `--dry-run`, `explain`, JSON reports with `applied`/`skipped`/`unmatched`,
  and stable ordering for list/show and for restore plans.
- Versioned JSON state under `$XDG_STATE_HOME/better-session/contexts.json`
  (override with `BETTER_SESSION_STATE`). The old `sessions.json` snapshot
  format is still read and migrated in memory.

## Usage

```sh
export NIRI_SOCKET=...          # normally set inside niri

better-session capture work --launch editor=alacritty \
  --hook-pre 'notify-send "restoring work"' \
  --hook-post 'notify-send "work restored"'
better-session save work        # alias for capture; keeps existing hooks/commands
better-session restore work --dry-run
better-session explain work
better-session restore work --wait-ms 5000
better-session restore work --force   # continue after a failing pre hook
better-session list
better-session show work
better-session rename work desk
better-session delete work
```

`--socket PATH` overrides `$NIRI_SOCKET`.

### Commands

| Command | Meaning |
| --- | --- |
| `capture <name>` / `save <name>` | Snapshot live niri state into a context. Preserves existing launch commands and hooks; `--launch`, `--hook-pre`, `--hook-post` append. |
| `list` | Print context names (sorted). |
| `show <name>` | Print one context as JSON. |
| `restore <name>` | Apply the context. `--dry-run`, `--explain`, `--wait-ms MS`, `--force`. |
| `explain <name>` | Print the restore plan without applying it (implies `--dry-run`). |
| `rename <old> <new>` | Rename a context; fails if `<new>` exists. |
| `delete <name>` | Delete a context. |

## Context schema

`contexts.json` is versioned. `version` must not be newer than this build's
schema version (`1`); newer files are rejected instead of being partially read.

```json
{
  "version": 1,
  "contexts": {
    "work": {
      "created_at": 1730000000,
      "updated_at": 1730000000,
      "command_map": { "editor": "alacritty" },
      "workspaces": [
        {
          "id": 1,
          "name": "work",
          "idx": 1,
          "output": "eDP-1",
          "is_active": true,
          "is_focused": true
        }
      ],
      "windows": [
        {
          "app_id": "firefox",
          "title": "Mail",
          "workspace_id": 1,
          "workspace_name": "work",
          "workspace_idx": 1,
          "workspace_output": "eDP-1",
          "is_floating": false,
          "is_focused": true
        }
      ],
      "outputs": {
        "eDP-1": {
          "off": false,
          "mode": [1920, 1080, 60000],
          "scale": 1.5,
          "transform": "Normal",
          "position": [0, 0],
          "vrr": true
        }
      },
      "hooks": {
        "pre": ["notify-send 'restoring work'"],
        "post": ["notify-send 'work restored'"]
      }
    }
  }
}
```

The `outputs` entry intentionally matches a `better-monitor` profile:
`off`, `mode` (`[width, height, refresh_millihz]`), `scale`, `transform`,
`position` and `vrr`. A monitor profile can be pasted into a context; restore
emits niri `Output` requests directly.

Workspaces and windows are sorted at capture time (by output, index, name/app
id/title), so captures and plans are stable across runs.

## Restore semantics

The restore order is fixed:

1. **Outputs.** For each output, sorted by name: on/off, mode, scale,
   transform, position, VRR. Settings that already match are skipped. Outputs
   that are not connected are reported under `skipped` and left alone.
2. **Workspaces.** Missing named workspaces are focused so niri creates them;
   then each output's saved active workspace is focused; finally the saved
   focused workspace is focused. Existing/active/focused workspaces are
   skipped.
3. **Windows.** Missing apps with a `command_map` entry are spawned first. Then
   `better-session` polls `Request::Windows` until all saved windows are matched
   or `--wait-ms` expires. Matched windows are moved to their saved workspace,
   switched between floating and tiling when needed, and the saved focused
   window is focused last.

`--wait-ms MS` defaults to `2000`; use `--wait-ms 0` to disable polling. The
poll interval is 50 ms. Late windows are matched by app id/title exactly like
windows that were already running.

Window ids are session-local, so matching is by app id/title. When several live
windows match, the planner prefers one already on the saved workspace, then the
focused one, then the lowest id, and never assigns one live window to two saved
windows. Unnamed workspaces fall back to their index.

### Idempotency

Every action is planned against current niri state and settings that already
match are skipped, so running `restore` twice leaves the second report with no
`applied` entries. Hooks are user commands and always run on a real restore
(they are skipped by `--dry-run`/`explain`), so make them idempotent. The saved
focused window is expected to live on the saved focused workspace.

## Hooks

`hooks.pre` and `hooks.post` are lists of shell commands (`sh -c`). Each command
receives:

- `BETTER_SESSION_CONTEXT` — the context name.
- `BETTER_SESSION_PHASE` — `pre` or `post`.

Failure semantics:

- A failing **pre** hook aborts the restore before any output/workspace/window
  change. The report has `"aborted": true`, `"ok": false` and the failing
  command. `--force` continues anyway and reports the failure.
- A failing **post** hook does **not** roll back the restore. It is reported in
  `"hooks.post"` with `"status": "failed"`, and the process exits non-zero after
  printing the JSON report.
- `--dry-run` and `explain` do not run hooks; they are reported as `skipped`
  with reason `dry-run`.

## JSON report

`restore` and `explain` print one object:

```json
{
  "context": "work",
  "dry_run": false,
  "explain": false,
  "wait_ms": 2000,
  "aborted": false,
  "ok": true,
  "applied": [
    { "phase": "output", "target": "eDP-1", "action": "set scale", "request": { "...": "..." } }
  ],
  "skipped": [
    { "phase": "workspace", "target": "work", "reason": "already active" }
  ],
  "unmatched": [
    { "phase": "window", "target": "slack", "reason": "no live window and no launch command" }
  ],
  "hooks": {
    "pre": { "status": "ok", "commands": ["..."] },
    "post": { "status": "ok", "commands": ["..."] }
  }
}
```

`applied` contains planned actions in dry-run mode and actually sent requests in
a real restore. `skipped` covers already-matching targets, disconnected outputs
and existing workspaces. `unmatched` lists saved windows with no live match and
no launch command, including spawned apps that did not appear before the wait
deadline.

## Migration from `sessions.json`

Older versions wrote `$XDG_STATE_HOME/better-session/sessions.json` with a
`{ "sessions": { ... } }` object and no schema version. `better-session` reads
that shape in two ways:

- If `contexts.json` does not exist, `Store::load` falls back to
  `sessions.json` next to it and treats each session as a context (empty
  `outputs` and `hooks`, existing windows/workspaces/commands kept).
- If `BETTER_SESSION_STATE` (or any explicit path) points at a legacy file, the
  legacy `sessions` key is migrated in memory.

The migrated store is written in the new versioned format on the next command
that saves state. The legacy file is not deleted automatically.

## Limitations

- Restoring output modes assumes the saved mode is still advertised by the
  output. A disconnected output is skipped, not queued.
- Unnamed workspaces are referenced by index, which is ambiguous across
  outputs; name workspaces for predictable multi-output restores.
- Windows are matched by app id/title. Multiple identical windows are matched
  deterministically but cannot carry per-window identity across restarts.
- Hooks run through `sh`; only `BETTER_SESSION_CONTEXT` and
  `BETTER_SESSION_PHASE` are added to the inherited environment.
- Restore does not launch apps that are missing unless a `command_map` entry
  exists (`--launch app=command` at capture time).

## Test

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

Tests use an in-process fake niri IPC socket; no display or real compositor is
required.
