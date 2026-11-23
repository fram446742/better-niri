# better-monitor

Output profiles for niri, with safe apply and rollback. Part of the Better
Desktop ecosystem; it lives inside this repository so no new repository is
created (excluded from niri's Cargo workspace).

## Features

- Snapshot the current output settings into a named profile: mode, scale,
  transform, position, VRR and on/off.
- Apply a profile through niri's IPC (`Request::Output`), one action per output
  in a safe order (on/off → mode → scale → transform → position → VRR).
- `--dry-run` prints the planned actions without touching the compositor.
- Applying stores the pre-apply state as `__previous`; `restore` re-applies it.
- Profiles are plain JSON under `$XDG_STATE_HOME/better-monitor/profiles.json`
  (override with `BETTER_MONITOR_STATE`).

## Usage

```sh
export NIRI_SOCKET=...            # normally set inside niri

better-monitor save desk          # all outputs
better-monitor save desk eDP-1    # just one output
better-monitor apply desk --dry-run
better-monitor apply desk
better-monitor restore            # undo the last apply
better-monitor list
better-monitor show desk
better-monitor delete desk
```

`--socket PATH` overrides `$NIRI_SOCKET`.

## Profile shape

```json
{
  "outputs": {
    "eDP-1": {
      "off": false,
      "scale": 1.5,
      "mode": [1920, 1080, 60000],
      "transform": "Normal",
      "position": [0, 0],
      "vrr": true
    }
  }
}
```

## Test

```sh
cargo test
cargo clippy --all-targets
```
