# Repository scripts

## `check-workspaces.sh`

Runs `cargo fmt --check`, `cargo check --all-targets` and `cargo test` for every
`loom-*` Cargo workspace. The companion apps are independent workspaces
excluded from niri's root workspace, so the root CI jobs do not cover them.

```sh
scripts/check-workspaces.sh                    # every loom-* workspace
scripts/check-workspaces.sh loom-settings    # one workspace
STRICT_FMT=1 scripts/check-workspaces.sh       # fail on formatting drift
```

Workspaces whose system libraries are absent are skipped with a note instead of
failing the run. Detection uses `pkg-config` first (`gtk4`, `libpipewire-0.3`,
`wayland-client`, `cairo`, `xkbcommon`) and falls back to recognising a
dependency-related `cargo check` failure. `LIBCLANG_PATH` defaults to `/usr/lib`
for the PipeWire/SPA bindings.

`loom-noctalia-plugins` is not a Cargo workspace (Luau sources only) and is
reported as skipped. `loom-portal` is a single crate with its own lockfile
rather than a `[workspace]` manifest; the script still checks it.

Formatting drift is a warning by default because some workspaces were merged
from other integration branches and are not formatted like the root crate. Set
`STRICT_FMT=1` in CI or locally to make it a failure. The `loom-workspaces`
job in `.github/workflows/ci.yml` installs the Ubuntu development packages and
runs this script.

## `pending-checks/`

Scratch space for checks that are prepared but not wired into CI yet. Files
there are not executed automatically.
