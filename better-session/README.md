# better-session

Named **contexts** for niri: capture workspaces, windows, output configuration,
hooks and optional links to the other Better Desktop apps, then restore them
predictably. Part of the Better Desktop ecosystem; it lives inside this
repository so no new repository is created (excluded from niri's Cargo
workspace).

## Features

- Capture workspaces (name/index/output/active/focused), windows (app id, title,
  workspace, floating state, focus) and outputs (mode, scale, transform,
  position, VRR, on/off) into a named context.
- Restore in a fixed order: **outputs → monitor profile → workspaces →
  windows → audio/wallpaper/companion → activate hook → post hook**. Output
  changes are sent as niri `Request::Output` requests, so the core does not
  depend on the `better-monitor` binary.
- Optional, no-hard-dependency profiles for `better-monitor`, `better-audio`,
  `better-wallpaper` and `better-companion`. Every profile reports
  `applied`, `skipped`, `missing` or `failed`; restore continues with partial
  success.
- `pre`, `activate`, `post` and `deactivate` hooks, plus per-profile
  `activate`/`deactivate` hooks. Hooks receive the context name and phase, and
  profile hooks also receive the profile name.
- Wait for apps that appear late by polling `Request::Windows` until the
  deadline instead of sleeping for a fixed time (`--wait-ms`).
- Resolve missing apps from `command_map`, installed Flatpak ids, XDG `.desktop`
  files (`Exec=` with field codes stripped) or executables on `PATH`. A given
  app is spawned at most once per restore, existing windows are matched by app
  id/title before spawning, windows an app opens extra are reported and left
  alone, and late windows keep being polled.
- `--dry-run`, `explain`, JSON reports with `applied`/`skipped`/`unmatched`,
  `extra` windows and per-profile outcomes, and stable ordering.
- Reconnect to niri during a restore (`--retry`, exponential backoff), skip
  steps whose target a user already changed when that is observable over IPC,
  and write state atomically.
- Bounded session history under `$XDG_STATE_HOME/better-session/history/` with
  `history list/show/restore/prune` and `snapshot --auto` (manual or periodic,
  no daemon).
- Import/export a single context or the whole store, with schema validation and
  `--merge`/`--replace`.
- Built-in `development`, `gaming` and `work` templates made of placeholders,
  never guessed app names. Apply them or use them as `capture --template`
  defaults.
- Versioned JSON state under `$XDG_STATE_HOME/better-session/contexts.json`
  (override with `BETTER_SESSION_STATE`). The old `sessions.json` snapshot
  format is still read and migrated in memory.

## Usage

```sh
export NIRI_SOCKET=...          # normally set inside niri

better-session capture work --launch editor=alacritty \
  --hook-pre 'notify-send "restoring work"' \
  --hook-activate 'notify-send "work active"' \
  --hook-post 'notify-send "work restored"' \
  --monitor-profile desk --audio-profile work --wallpaper-scene night \
  --companion-call rules.run

better-session save work        # alias for capture; keeps existing settings
better-session restore work --dry-run
better-session explain work
better-session restore work --wait-ms 5000 --retry 5
better-session restore work --force   # continue after a failing pre hook
better-session restore work --snapshot-before
better-session deactivate work
better-session list
better-session show work
better-session rename work desk
better-session delete work
```

`--socket PATH` overrides `$NIRI_SOCKET`.

### Commands

| Command | Meaning |
| --- | --- |
| `capture <name>` / `save <name>` | Snapshot live niri state into a context. Preserves existing commands, hooks and profiles; flags append/override. |
| `list` | Print context names (sorted). |
| `show <name>` | Print one context as JSON. |
| `restore <name>` | Apply the context. `--dry-run`, `--explain`, `--wait-ms`, `--poll-ms`, `--retry`, `--force`, `--no-preserve-user-changes`, `--snapshot-before`. |
| `explain <name>` | Print the restore plan without applying it (implies `--dry-run`). |
| `deactivate <name>` | Run profile then context `deactivate` hooks. `--dry-run` prevents execution. No niri state changes. |
| `rename <old> <new>` | Rename a context; fails if `<new>` exists. |
| `delete <name>` | Delete a context. |
| `history list/show/restore/prune` | Work with bounded snapshots. |
| `snapshot` | Record the current state in history, once or periodically. |
| `export` / `import` | Move one context or the whole store. |
| `template list/show/apply` | Inspect or instantiate built-in shapes. |

## Context schema

`contexts.json` is versioned. `version` must not be newer than this build's
schema version (`1`); newer files are rejected instead of being partially read.
All fields added after v1 are optional, so old files load unchanged.

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
        "activate": ["notify-send 'work active'"],
        "post": ["notify-send 'work restored'"],
        "deactivate": ["notify-send 'leaving work'"]
      },
      "profiles": {
        "monitor": {
          "profile": "desk",
          "apply": "auto",
          "hooks": { "activate": ["notify-send 'monitor ready'"] }
        },
        "audio": {
          "profile": "work",
          "config": "/home/me/.config/better-audio/profiles/work.json"
        },
        "wallpaper": { "scene": "night" },
        "companion": {
          "rules": ["work"],
          "marks": { "project": "work" },
          "calls": ["rules.run"]
        }
      },
      "template": "development"
    }
  }
}
```

The `outputs` entry intentionally matches a `better-monitor` profile:
`off`, `mode` (`[width, height, refresh_millihz]`), `scale`, `transform`,
`position` and `vrr`. `adaptive_sync` and `variable_refresh_rate` are accepted
as input aliases for `vrr`.

Workspaces and windows are sorted at capture time (by output, index, name/app
id/title), so captures and plans are stable across runs.

### Optional profile fields

All `profiles` fields are optional. Omitting them keeps `better-session` fully
standalone.

| Field | Meaning |
| --- | --- |
| `profiles.monitor.profile` | `better-monitor` profile name. `apply` is `auto` (IPC when the profile is readable, else command), `ipc` or `command`. `command` is an optional shell override. |
| `profiles.audio.profile` | Named rules file. `config` overrides the path; the default is `$XDG_CONFIG_HOME/better-audio/profiles/<profile>.json` (profile `default` falls back to `rules.json`). `command` overrides. |
| `profiles.wallpaper.scene` | `better-wallpaper` scene. `command` overrides the default `scene use` + `apply` pair. |
| `profiles.companion.rules` | Rule names recorded as intent. Without `calls`/`command`, restore runs `companionctl call rules run` (all rules; companion has no subset selector). |
| `profiles.companion.calls` | `namespace.action` strings passed to `companionctl call`, in sorted order. |
| `profiles.companion.marks` | Marks exported to commands as `BETTER_SESSION_COMPANION_MARKS` (JSON). Marks alone do not call anything; the status is `skipped` with that explanation. |
| `hooks.activate` / `hooks.deactivate` | Per-profile hooks run when that profile is applied/deactivated. |

## Restore orchestration

The order is fixed and deterministic:

1. **Pre hook.** A failure aborts before touching niri unless `--force`.
2. **Outputs.** For each output, sorted by name: on/off, mode, scale,
   transform, position, VRR. Settings that already match are skipped. Outputs
   that are not connected are reported under `skipped` and left alone.
3. **Monitor profile.** `ipc` maps the profile's outputs to currently connected
   outputs (supports the profile-level `match` block) and sends niri `Output`
   requests. `command` runs `better-monitor --state <path> apply <profile>`.
   `auto` prefers IPC when the profile can be read and falls back to the
   command.
4. **Workspaces.** Missing named workspaces are focused so niri creates them;
   then each output's saved active workspace is focused; finally the saved
   focused workspace is focused. Existing/active/focused workspaces are
   skipped.
5. **Windows.** Missing apps are launched first (one launch per app id). Then
   `better-session` polls `Request::Windows` until all saved windows are matched
   or `--wait-ms` expires. Matched windows are moved to their saved workspace,
   switched between floating and tiling when needed, and the saved focused
   window is focused last.
6. **Audio, wallpaper and companion** profiles, in that order.
7. **Activate hook.** Runs after the context and its profiles are active.
8. **Post hook.** Runs last.

Profile ordering inside steps 3 and 6 is `monitor`, then `audio`, `wallpaper`,
`companion`. Each profile reports:

- `applied`: the command or niri requests ran; profile `activate` hooks ran if
  configured.
- `skipped`: already applied, dry-run, no deactivation hooks, or marks-only
  companion configuration.
- `missing`: executable, profile file, scene or companion daemon requirement is
  absent. Nothing fails hard.
- `failed`: a command, hook or niri request failed. Remaining profiles and the
  core restore continue.

`partial` is `true` when any optional profile is `missing` or `failed`. `ok`
still describes the core restore and hooks only; optional-app degradation does
not abort or fail the exit code.

`--wait-ms MS` defaults to `2000`; use `--wait-ms 0` to disable polling. The
poll interval defaults to 50 ms. Late windows are matched by app id/title
exactly like windows that were already running.

Window ids are session-local, so matching is by app id/title. An exact title
match wins; when only the title changed, a live window with the same app id is
used rather than launching a duplicate. When several live windows match, the
planner prefers one already on the saved workspace, then the focused one, then
the lowest id, and never assigns one live window to two saved windows. Unnamed
workspaces fall back to their index. Live windows that no saved window claims
are listed under `extra` and are not moved or closed.

### Launch resolution

For a saved window with no live match, the first available source wins:

1. `command_map[<app_id>]` (from `--launch app=command`).
2. An installed Flatpak app id, when no native executable with that name exists:
   `flatpak run <app_id>`. Installed apps are detected from Flatpak export
   `.desktop` files (including `$HOME/.local/share/flatpak/exports/share` and
   `/var/lib/flatpak/exports/share`); `flatpak` itself is not executed.
3. An XDG `.desktop` file: exact `<app_id>.desktop` first, then an entry whose
   `StartupWMClass` equals the app id. The first `Exec=` in `[Desktop Entry]` is
   used after stripping `%f`, `%F`, `%u`, `%U`, `%i`, `%c`, `%k` and similar
   field codes (`%%` becomes `%`). Hidden/NoDisplay entries are ignored.
4. An executable named `<app_id>` on `PATH`.

If none resolve, the app is listed under `unmatched` with
`"no live window and no launch command"`. A single app id is spawned only once
per plan even if several saved windows share it; the poll then waits for the
extra windows.

### Idempotency and user changes

Every action is planned against current niri state and settings that already
match are skipped, so running `restore` twice leaves the second report with no
`applied` entries. Hooks are user commands and always run on a real restore
(they are skipped by `--dry-run`/`explain`), so make them idempotent.

By default each guarded step is re-checked immediately before it is applied.
A window that disappeared, is already focused, already has the saved floating
state or already reached the saved workspace is skipped with a reason instead of
being forced back. `--no-preserve-user-changes` disables these checks.

`--retry N` (default `3`) reconnects to niri with exponential backoff (10 ms,
20 ms, ... capped) when a request cannot connect, which makes a restore survive
a compositor restart. `--retry 0` disables retries.

State is written through a temporary file plus rename, so an interrupted write
cannot leave a half-written `contexts.json` or history entry.

## Hooks

`hooks.pre`, `hooks.activate`, `hooks.post` and `hooks.deactivate` are lists of
shell commands (`sh -c`). Each command receives:

- `BETTER_SESSION_CONTEXT` — the context name (or history name).
- `BETTER_SESSION_PHASE` — `pre`, `activate`, `post` or `deactivate`.
- `BETTER_SESSION_PROFILE` / `BETTER_SESSION_PROFILE_NAME` — for profile hooks.
- `BETTER_SESSION_COMPANION_MARKS` — JSON marks for companion commands when set.

Failure semantics:

- A failing **pre** hook aborts the restore before any output/workspace/window
  change. The report has `"aborted": true`, `"ok": false` and the failing
  command. `--force` continues anyway and reports the failure.
- A failing **activate** or **post** hook does **not** roll back the restore. It
  is reported in `"hooks"` with `"status": "failed"`, and the process exits
  non-zero after printing the JSON report.
- A failing per-profile activate hook turns that profile's status into
  `failed` and sets `partial`.
- `deactivate` runs the profile deactivate hooks (monitor, audio, wallpaper,
  companion order) and then `hooks.deactivate`. It never changes niri state.
- `--dry-run` and `explain` do not run hooks; they are reported as `skipped`
  with reason `dry-run`.

## JSON report

`restore`, `explain` and `history restore` print one object:

```json
{
  "context": "work",
  "dry_run": false,
  "explain": false,
  "wait_ms": 2000,
  "aborted": false,
  "ok": true,
  "partial": false,
  "applied": [
    { "phase": "output", "target": "eDP-1", "action": "set scale", "request": { "...": "..." } }
  ],
  "skipped": [
    { "phase": "workspace", "target": "work", "reason": "already active" }
  ],
  "unmatched": [
    { "phase": "window", "target": "slack", "reason": "no live window and no launch command" }
  ],
  "extra": [
    { "id": 42, "app_id": "firefox", "title": "Docs" }
  ],
  "profiles": [
    { "profile": "audio", "name": "work", "status": "applied" },
    { "profile": "monitor", "name": "desk", "status": "missing", "detail": "..." }
  ],
  "hooks": {
    "pre": { "status": "ok", "commands": ["..."] },
    "activate": { "status": "ok", "commands": ["..."] },
    "post": { "status": "ok", "commands": ["..."] }
  }
}
```

`applied` contains planned actions in dry-run mode and actually sent requests in
a real restore. `skipped` covers already-matching targets, disconnected outputs,
existing workspaces and state that changed before a step ran. `unmatched` lists
saved windows with no live match and no launch command, including spawned apps
that did not appear before the wait deadline. `extra` lists live windows left
untouched.

`deactivate` prints `{ "context", "dry_run", "ok", "profiles", "hooks", "error" }`.

## Session history

History is bounded and file-based: one JSON file per snapshot in
`$XDG_STATE_HOME/better-session/history/` (override with
`BETTER_SESSION_HISTORY`, or a `history` sibling of `--state`). There is no
daemon and no background process.

```json
{
  "version": 1,
  "id": "1795557696206-0000",
  "name": "work",
  "created_at": 1795557696,
  "trigger": "auto",
  "context": { "workspaces": [], "windows": [] }
}
```

```sh
better-session snapshot --auto                 # one automatic snapshot
better-session snapshot --auto --name work --interval-ms 60000 --count 5
better-session history list
better-session history show <id-or-name>
better-session history restore <id-or-name> [--dry-run]
better-session history prune --keep 20 --max-age-days 30
```

- `snapshot` records the live state. `--auto` marks the trigger. With
  `--interval-ms` it sleeps in the foreground, writing until `--count` (or
  forever without it; Ctrl-C stops it). It is opt-in and has no daemon.
- `--keep N` (default `50`) prunes after each snapshot; `snapshot` keeps at
  least one entry per call. `history prune` accepts `--keep` (including `0` to
  clear the directory) and `--max-age-days`; retention is per-directory.
- `history show` matches an exact id first, otherwise the newest entry for that
  name. `history restore` uses the entry's own context; the named context store
  is not modified.
- Malformed or newer-version history files are skipped with a warning instead
  of failing the whole listing.
- Ids are monotonic even when the clock moves backwards or a low sequence was
  pruned.

`restore --snapshot-before` records the current state as a `pre-restore` history
entry before changing anything (not in dry-run mode).

## Templates

Built-in templates are shapes, not app prescriptions. Every app id and launch
command is a placeholder, so the templates never claim that a particular
desktop uses a specific program.

| Template | Workspaces | Placeholders |
| --- | --- | --- |
| `development` | `code`, `web`, `term` | `code_app`, `code_cmd`, `browser_app`, `browser_cmd`, `terminal_app`, `terminal_cmd` |
| `gaming` | `game`, `chat` | `game_app`, `game_cmd`, `chat_app`, `chat_cmd` |
| `work` | `mail`, `meet`, `docs` | `mail_app`, `mail_cmd`, `meeting_app`, `meeting_cmd`, `docs_app`, `docs_cmd` |

```sh
better-session template list
better-session template show development
better-session template apply development dev \
  --set code_app=my-editor --set code_cmd=my-editor \
  --set browser_app=my-browser --set browser_cmd=my-browser \
  --set terminal_app=my-terminal --set terminal_cmd=my-terminal
```

`--set` values can also come from
`BETTER_SESSION_TEMPLATE_<UPPERCASE_PLACEHOLDER>` environment variables. All
placeholders must resolve; unresolved names are reported together. The created
context records `"template": "<name>"`.

`capture <name> --template <name> [--set ...]` attaches the template name and
fills missing `command_map` entries when a captured window's app id equals a
template placeholder value. Captured live state always wins; the template only
supplies launch commands that were not already known.

## Import and export

```sh
better-session export > all-contexts.json
better-session export work --file work.json --json
better-session import --file all-contexts.json --merge
better-session import --file work.json --replace
better-session import < context.json --merge --name recovered
```

- A whole-store export is `{"version": 1, "contexts": { ... }}`.
- A single-context export is
  `{"version": 1, "name": "work", "context": { ... }}`.
- `import` accepts either shape, or a plain context when `--name` is given.
- `--merge` (default) adds new names and overwrites collisions; `--replace`
  discards contexts not present in the import and reports them under `removed`.
- Versions newer than this build are rejected before any state is changed.
  Imported contexts are deserialized with the same schema as `contexts.json`.
- State is saved atomically only after validation succeeds.

## Migration from `sessions.json`

Older versions wrote `$XDG_STATE_HOME/better-session/sessions.json` with a
`{ "sessions": { ... } }` object and no schema version. `better-session` reads
that shape in two ways:

- If `contexts.json` does not exist, `Store::load` falls back to
  `sessions.json` next to it and treats each session as a context (empty
  `outputs`, `hooks` and `profiles`, existing windows/workspaces/commands kept).
- If `BETTER_SESSION_STATE` (or any explicit path) points at a legacy file, the
  legacy `sessions` key is migrated in memory.

The migrated store is written in the new versioned format on the next command
that saves state. The legacy file is not deleted automatically.

## Limitations

- Restoring output modes assumes the saved mode is still advertised by the
  output. A disconnected output is skipped, not queued.
- A `better-monitor` profile applied over IPC only carries fields niri exposes
  over IPC. Config-only fields (`max_bpc`, HDR, startup focus, isolation,
  mirroring) are ignored; use `apply = "command"` if they matter.
- When a profile-level `match` matches several outputs and the profile has one
  output block, the same settings are applied to each match.
- `better-audio` has rules files rather than named profiles in its CLI; the
  profile name maps to `profiles/<name>.json` by convention, or use `config`.
- A `better-wallpaper` scene must already exist; `better-session` does not
  create scenes. The default path runs `scene use` and then `apply`.
- Companion cannot select a subset of rules through `companionctl`; `rules`
  without `calls`/`command` means "run all matching rules". `marks` are
  metadata exported to commands, not a companion primitive.
- Unnamed workspaces are referenced by index, which is ambiguous across
  outputs; name workspaces for predictable multi-output restores.
- Windows are matched by app id/title. Multiple identical windows are matched
  deterministically but cannot carry per-window identity across restarts.
- `.desktop` parsing is intentionally small: the first `Exec=` and
  `StartupWMClass` in `[Desktop Entry]`; no localized keys, `TryExec` or shell
  expansion. The resulting command is handed to niri's `spawn-sh`.
- Flatpak installation detection uses exported desktop files; a Flatpak that
  exports no desktop entry will not be recognized automatically.
- Hooks run through `sh`; only the documented variables are added to the
  inherited environment.
- Restore does not launch apps that are missing unless one of the launch
  resolution sources above applies.
- Profile deactivation is hook-only. The optional apps expose no portable
  "deactivate" protocol.
- History stores snapshots, not a diff log; restoring a history entry does not
  create or update a named context.
- Template placeholder values are required. The built-in templates intentionally
  do not guess app names or commands.

## Test

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

Tests use in-process fake niri IPC sockets, fake `.desktop` trees, fake Flatpak
exports and fake command scripts; no display, real compositor or installed app
is required.
