# Integration patches

## `nsticky-native-sticky.patch`

better-nsticky is an independent repository, so its native-sticky delegation is
kept here as a patch against the `source/` tree:

```sh
cd /path/to/better-nsticky/source
patch -p1 < /path/to/better-niri/contrib/patches/nsticky-native-sticky.patch
cargo test --all-targets
```

What it does:

- Adds `Niri::supports_native_sticky()`, `toggle_sticky()` and
  `sticky_windows()` to the compositor trait, implemented over
  `Request::Capabilities` and `Action::ToggleWindowSticky` (the Niri client
  already speaks that JSON protocol; no new dependency).
- When the compositor reports `sticky: true`, `nsticky sticky add|remove|list`
  and the focused-window/app-id/title toggles delegate to the compositor's
  native flag instead of moving windows across workspaces.
- Staging and scratchpads clear the native flag while a window is parked and
  restore it when the window comes back, keeping the "sticky or staged, never
  both" invariant.
- Reconcile ignores the stored sticky list in native mode: the compositor is the
  source of truth, and the daemon no longer resurrects a stale pin.
- On a stock niri (or a build without native sticky) the capability query fails
  and the existing move-based fallback runs unchanged.

Tests: three new cases cover add/remove/list/toggle delegation, stage/unstage
round-trips, and reconcile ignoring stored state, all with the fake compositor
reporting native sticky support. The full suite is 473 tests.
