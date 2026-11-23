# better-portal

A standalone portal backend for **Screenshot** and **PickColor**. The
compositor already serves the capture interfaces; this daemon exposes the
standard `org.freedesktop.impl.portal.Screenshot` backend on its own bus name
and delegates to those interfaces, so the portal stack can run as a separate
process.

It also proxies **ScreenCast**: the compositor owns the capture sessions and
PipeWire nodes, and better-portal forwards CreateSession/SelectSources/Start
unchanged while re-exporting Session/Request objects so the portal frontend can
close them. The PipeWire node ids remain valid because the frontend connects to
the same PipeWire daemon.

It lives inside this repository so no new repository is created; it is excluded
from niri's Cargo workspace and keeps its own lockfile.

## Bus interface

- Name: `org.freedesktop.impl.portal.desktop.better`
- Object: `/org/freedesktop/portal/desktop`
- Interface: `org.freedesktop.impl.portal.Screenshot` (version 2)
  - `Screenshot(handle, app_id, parent_window, options) -> (response, results)`
  - `PickColor(handle, app_id, parent_window, options) -> (response, results)`
  - property `Version = 2`
- Interface: `org.freedesktop.impl.portal.ScreenCast` (version 4, proxy)
  - `CreateSession(handle, session_handle, app_id, options) -> (response, results)`
  - `SelectSources(handle, session_handle, app_id, options) -> (response, results)`
  - `Start(handle, session_handle, app_id, parent_window, options) -> (response, results)`
  - re-exported `Session.Close`/`Closed` and `Request.Close`
  - property `Version = 4`
- Requests expose `org.freedesktop.impl.portal.Request.Close`, which cancels an
  interactive capture through niri's `CancelScreenshot`.

Response codes: `0` success, `1` cancelled, `2` other failure. Successful
screenshots return `uri`; colour picks return `color` as `(ddd)`.

## Delegation

`Screenshot` with `interactive: true` uses **`better-portal-picker`**, a
layer-shell overlay that lets the user drag a rectangle on any output and then
captures that area through niri's `ScreenshotArea`. If the picker is missing,
exits non-zero, or cannot run, it falls back to niri's
`InteractiveScreenshot`. Non-interactive screenshots call `Screenshot`;
`PickColor` calls `PickColor`.

Set `BETTER_PORTAL_PICKER=/path/to/picker` to override the picker binary; by
default it resolves a sibling `better-portal-picker`, then `$PATH`.

```sh
cargo build
./target/debug/better-portal-picker   # prints {"x","y","width","height"} or exits 2
```

ScreenCast source selection still uses the compositor's picker (an external
ScreenCast picker needs a capture API from the compositor).

ScreenCast delegates to the compositor's existing
`org.freedesktop.impl.portal.desktop.niri` backend; it does not own capture or
PipeWire code. Session ownership checks therefore see better-portal as the
caller, and remote session closure relies on the local `Close` path.

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
org.freedesktop.impl.portal.ScreenCast=better
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
