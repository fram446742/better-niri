# better-portal

A standalone portal backend for **Screenshot**, **PickColor**, and
**ScreenCast**. The compositor already serves the capture interfaces and its
own ScreenCast backend; this daemon exposes the standard
`org.freedesktop.impl.portal.*` interfaces on its own bus name and delegates to
those compositor interfaces, so the portal stack can run as a separate
process.

ScreenCast is proxied: the compositor owns the capture sessions and PipeWire
nodes, and better-portal forwards `CreateSession`/`SelectSources`/`Start`
unchanged while re-exporting Session/Request objects so the portal frontend can
close them. The PipeWire node ids remain valid because the frontend connects to
the same PipeWire daemon.

better-portal lives inside this repository so no new repository is created. It
is excluded from niri's Cargo workspace and keeps its own lockfile
(`better-portal/Cargo.lock`).

## Standalone build

```sh
# From the repository root:
cargo build --manifest-path better-portal/Cargo.toml

# Or from this directory:
cargo build
```

The build does not need niri's source tree beyond the `niri-ipc` path
dependency in this repository, and it does not build the compositor. Two
binaries are produced in `better-portal/target/debug/`:

- `better-portal` — the portal backend daemon.
- `better-portal-picker` — the layer-shell overlay used for interactive
  screenshots and ScreenCast source selection.

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
  - properties `Version = 4`, `AvailableSourceTypes = 3` (monitor + window),
    `AvailableCursorModes = 7` (hidden, embedded, metadata)
- Session objects at `session_handle`:
  - `org.freedesktop.impl.portal.Session.Close()`
  - `org.freedesktop.impl.portal.Session.Closed` signal
- Request objects at `handle`:
  - `org.freedesktop.impl.portal.Request.Close()`

Response codes: `0` success, `1` cancelled, `2` other failure. Successful
screenshots return `uri`; colour picks return `color` as `(ddd)`; ScreenCast
`Start` returns `streams` exactly as the compositor produced them.

The impl.Screenshot/ScreenCast methods return their response directly; the
`Response` signal seen by applications belongs to xdg-desktop-portal's
frontend Request object, not to this backend. The backend request objects only
need to implement `Close`, and they are removed when the method replies.

## Request lifecycle

Every Screenshot, PickColor, CreateSession, SelectSources, and Start call
exports a Request object at `handle` before doing work and removes it before
replying (including on cancellation and error paths). Calling `Request.Close`
while a call is in flight:

- Screenshot/PickColor: marks the request cancelled and calls the compositor's
  `CancelScreenshot`.
- ScreenCast CreateSession/SelectSources: marks the request cancelled; the
  forwarded call is short, so the reply becomes `1` when it completes.
- ScreenCast Start: sets the cancellation flag, kills a running
  `better-portal-picker` child, and forwards the Close to niri's own Start
  request so niri's built-in picker is cancelled too. A cancelled Start is not
  forwarded and replies `1`.

The external picker runs on a worker thread, so the daemon keeps servicing
D-Bus while the overlay is open; a blocking child process would otherwise
prevent `Request.Close` from ever being handled.

## ScreenCast lifecycle

1. `CreateSession` records the session (owner, requested source types, started
   flag), exports Session/Request objects, and forwards the call. On failure
   the session object is removed and the reply is `2` with an `error` result.
   A session also closes if niri disappears from the bus.
2. `SelectSources` validates the requested `types` bitmask (`monitor`,
   `window`, `virtual`), rejects an unknown or already-started session, stores
   the types on success, and forwards the call.
3. `Start` chooses the picker mode from the stored types:
   - windows only → `better-portal-picker --screencast-window`;
   - anything that includes monitor/virtual output → `better-portal-picker
     --screencast`.

   A selected output is recorded through niri's
   `org.better.portal.ScreenCastSelection.SetSelection` as type `1` (output);
   a selected window as type `2` (window id). `ClearSelection` is called after
   `Start` (including failures) so a stale forced selection cannot leak into a
   later call. If the selection API is unavailable, or the picker is missing,
   fails, or returns an invalid selection, the forwarded `Start` falls back to
   niri's own picker.
4. `Session.Close` removes the local session, forwards `Close` to niri,
   emits `Closed`, and removes the Session object. A `Closed` emitted by niri
   (compositor-initiated stop, niri shutdown) is re-emitted on better-portal's
   bus and cleans up the same state. `Closed` is emitted exactly once per
   session.

The source `types` bitmask supports `1` (monitor), `2` (window), and `4`
(virtual). Only monitor and window are advertised because niri creates neither
virtual monitors nor more than one source per pick; a virtual request is
accepted defensively and selects an output like a monitor.

## Screenshot and PickColor

- Interactive screenshots use `better-portal-picker` to drag a rectangle and
  capture it through niri's `ScreenshotArea`. If the picker is missing, exits
  non-zero, or cannot run, it falls back to niri's
  `InteractiveScreenshot`. The layer-shell picker requires a compositor that
  supports `wlr-layer-shell`; its window mode works without a compositor.
- Non-interactive screenshots call niri's `Screenshot`.
- `PickColor` calls niri's `PickColor`; a "no color picked" cancellation is
  mapped to response `1`.

Set `BETTER_PORTAL_PICKER=/path/to/picker` to override the picker binary; by
default it resolves a sibling `better-portal-picker`, then `$PATH`.

Picker modes and exit codes:

| invocation | output | exit |
| --- | --- | --- |
| no arguments | `{"x","y","width","height"}` region | `0` selected, `2` cancelled, `1` error |
| `--screencast` | `{"output":"eDP-1"}` | `0` selected, `2` cancelled, `1` error |
| `--screencast-window` | `{"window":42}` | `0` selected, `2` cancelled, `1` error |

Window mode uses niri IPC (`NIRI_SOCKET`) and works without a Wayland
connection; it prints `{"window":…}` from `PickWindow`.

## Install and register

Install the daemon somewhere stable, then point xdg-desktop-portal at it:

```sh
install -Dm755 better-portal/target/release/better-portal /usr/libexec/better-portal
install -Dm755 better-portal/target/release/better-portal-picker /usr/libexec/better-portal-picker
install -Dm644 better-portal/portals/better.portal \
    /usr/share/xdg-desktop-portal/portals/better.portal
install -Dm644 better-portal/packaging/org.freedesktop.impl.portal.desktop.better.service \
    /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.better.service
```

`packaging/org.freedesktop.impl.portal.desktop.better.service` lets
xdg-desktop-portal activate the backend over D-Bus. It references the optional
systemd user unit in `packaging/better-portal.service`; install that unit into
`~/.config/systemd/user/` if you prefer systemd supervision. Adjust `Exec=`
paths when installing elsewhere (for example `~/.local/bin`).

Select the backend in `~/.config/xdg-desktop-portal/portals.conf`:

```ini
[preferred]
org.freedesktop.impl.portal.Screenshot=better
org.freedesktop.impl.portal.ScreenCast=better
```

`portals/better.portal` uses `UseIn=niri`, so the backend is only selected on
niri sessions. The portal frontend restarts when the configuration changes.

Run manually for development:

```sh
cargo build
./target/debug/better-portal
```

## Test and smoke checks

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets
```

The tests start their own `dbus-daemon` session buses and skip when
`dbus-daemon` is unavailable. They cover:

- ScreenCast CreateSession → SelectSources → Start → Session.Close, local
  `Closed` emission, compositor-initiated `Closed`, niri disappearing from the
  bus, unknown sessions, invalid source types, unavailable niri, and
  cancellation while the picker is open.
- Monitor, window, and virtual source-type selection with a fake picker,
  including `SetSelection`/`ClearSelection` arguments and invalid-selection
  fallback.
- Screenshot/PickColor request-object lifecycle, results, region picker capture
  through `ScreenshotArea`, cancellation, and colour-cancellation mapping.
- `better-portal-picker` window mode, cancel exit code `2`, missing
  `NIRI_SOCKET`, and output mode without a compositor.

A manual private-bus smoke check:

```sh
cargo build
dbus-run-session -- bash -c '
  ./target/debug/better-portal &
  sleep 1
  busctl --user introspect \
    org.freedesktop.impl.portal.desktop.better \
    /org/freedesktop/portal/desktop
  kill %1
'
```

Expected introspection output includes the `org.freedesktop.impl.portal.Screenshot`
and `org.freedesktop.impl.portal.ScreenCast` interfaces at
`/org/freedesktop/portal/desktop`, with `Version = 2` and `Version = 4`
respectively.

## Known limitations

- ScreenCast is a proxy over niri's
  `org.freedesktop.impl.portal.desktop.niri` backend. better-portal contains no
  capture or PipeWire code; node ids and restore data depend on niri and the
  shared PipeWire daemon.
- niri sees better-portal as the D-Bus caller for forwarded ScreenCast calls
  and screenshots, so niri-side ownership checks cannot identify the original
  application. better-portal enforces its own session ownership for
  SelectSources/Start.
- Only one source is selected per Start. `multiple` and cursor modes are
  forwarded unchanged; better-portal does not synthesize multiple streams.
- Persist/restore is delegated: an external-picker selection uses persist mode
  `0`, and restore data returned by niri is forwarded untouched.
- Virtual sources are not advertised and are not created; a requested virtual
  source selects an existing output.
- The picker needs `wlr-layer-shell` and `wl_shm`, and must run on the same
  Wayland display as the compositor. Region/output modes fail with exit code
  `1` when no compositor is reachable.
- PickColor cancellation is inferred from niri's `"no color picked"` error,
  because the compositor's screenshot interface has no separate cancel reply.
- Only the Screenshot, PickColor, and ScreenCast impl interfaces are provided.
  Settings, Wallpaper, Inhibit, and the other portals remain with niri or
  another backend.
