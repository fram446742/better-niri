# better-portal

A standalone portal backend for **Screenshot** and **PickColor**. The
compositor already serves the capture interfaces; this daemon exposes the
standard `org.freedesktop.impl.portal.Screenshot` backend on its own bus name
and delegates to those interfaces, so the portal stack can run as a separate
process.

It lives inside this repository so no new repository is created; it is excluded
from niri's Cargo workspace and keeps its own lockfile.

## Bus interface

- Name: `org.freedesktop.impl.portal.desktop.better`
- Object: `/org/freedesktop/portal/desktop`
- Interface: `org.freedesktop.impl.portal.Screenshot` (version 2)
  - `Screenshot(handle, app_id, parent_window, options) -> (response, results)`
  - `PickColor(handle, app_id, parent_window, options) -> (response, results)`
  - property `Version = 2`
- Requests expose `org.freedesktop.impl.portal.Request.Close`, which cancels an
  interactive capture through niri's `CancelScreenshot`.

Response codes: `0` success, `1` cancelled, `2` other failure. Successful
screenshots return `uri`; colour picks return `color` as `(ddd)`.

## Delegation

`Screenshot` with `interactive: true` calls niri's
`org.gnome.Shell.Screenshot.InteractiveScreenshot`; otherwise it calls
`Screenshot`. `PickColor` calls `PickColor`. The compositor must be running with
that interface available (niri does).

ScreenCast is not implemented here; keep using the compositor's existing portal
backend for it.

## Run

```sh
cargo build
./target/debug/better-portal
```

Register it with xdg-desktop-portal, for example in
`~/.config/xdg-desktop-portal/portals.conf`:

```ini
[preferred]
org.freedesktop.impl.portal.Screenshot=better
```

and install `portals/better.portal` into
`$XDG_DATA_DIRS/xdg-desktop-portal/portals/`.

## Test

```sh
cargo test
```

A private session bus smoke test:

```sh
dbus-run-session -- bash -c './target/debug/better-portal & sleep 1; busctl --user introspect org.freedesktop.impl.portal.desktop.better /org/freedesktop/portal/desktop; kill %1'
```
