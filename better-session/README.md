# better-session

Save and restore niri workspaces and windows. Part of the Better Desktop
ecosystem; lives inside this repository so no new repository is created
(excluded from niri's Cargo workspace).

## Features

- Snapshot workspaces (name/index/output/active/focused) and windows (app id,
  title, workspace, floating state, focus).
- Restore matches current windows by app id and title, focuses named
  workspaces, moves windows back to their saved workspace, restores
  floating/tiling state, spawns configured missing apps, and focuses the saved
  window.
- `--launch app=command` maps an app id to a shell command used when that app is
  missing on restore.
- `--dry-run` prints the planned actions without touching the compositor.
- JSON state under `$XDG_STATE_HOME/better-session/sessions.json` (override with
  `BETTER_SESSION_STATE`).

## Usage

```sh
export NIRI_SOCKET=...          # normally set inside niri

better-session save work --launch editor=alacritty
better-session restore work --dry-run
better-session restore work
better-session list
better-session show work
better-session delete work
```

`--socket PATH` overrides `$NIRI_SOCKET`.

## Restore semantics

1. Focus every named workspace in the session so it exists.
2. For each saved window, find a live window with the same `app_id` (and title
   when recorded).
3. Move it to the saved workspace, switch floating/tiling if needed.
4. If no match exists, run the configured launch command; otherwise report the
   app under `missing`.
5. Focus the saved focused window.

Window ids are session-local, so matching is by app id/title; unnamed
workspaces fall back to their index.

## Test

```sh
cargo test
cargo clippy --all-targets
```
