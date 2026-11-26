//! `loom-portal`: a standalone portal backend for screenshots, colour
//! picking, and screen casting.
//!
//! The compositor already serves GNOME-Shell-style screenshot and colour-picker
//! D-Bus interfaces, and its own `org.freedesktop.impl.portal.desktop.niri`
//! ScreenCast backend. This daemon exposes the standard
//! `org.freedesktop.impl.portal.*` interfaces on its own bus name and delegates
//! to those compositor interfaces, so the portal stack can run against a
//! separate process.

// D-Bus method signatures are fixed by the portal interfaces.
#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::future::pending;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::StreamExt as _;
use zbus::message::Header;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
use zbus::{Connection, ObjectServer, Proxy, fdo, interface};

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.loom";
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
pub const SCREENSHOT_INTERFACE: &str = "org.freedesktop.impl.portal.Screenshot";
pub const SCREENCAST_INTERFACE: &str = "org.freedesktop.impl.portal.ScreenCast";

/// A logical, global screenshot selection returned by `loom-portal-picker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SelectionRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl SelectionRect {
    pub fn from_drag(output_pos: (i32, i32), start: (f64, f64), end: (f64, f64)) -> Option<Self> {
        let x0 = start.0.min(end.0);
        let y0 = start.1.min(end.1);
        let x1 = start.0.max(end.0);
        let y1 = start.1.max(end.1);
        let width = (x1 - x0).round() as i32;
        let height = (y1 - y0).round() as i32;
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(Self {
            x: output_pos.0 + x0.round() as i32,
            y: output_pos.1 + y0.round() as i32,
            width,
            height,
        })
    }
}

pub const SOURCE_TYPE_MONITOR: u32 = 1;
pub const SOURCE_TYPE_WINDOW: u32 = 2;
/// Virtual sources are not advertised (niri creates none), but a frontend may
/// still request them; they select an output like a monitor.
pub const SOURCE_TYPE_VIRTUAL: u32 = 4;
const SOURCE_TYPE_ALL: u32 = SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW | SOURCE_TYPE_VIRTUAL;

const CURSOR_MODE_HIDDEN: u32 = 1;
const CURSOR_MODE_EMBEDDED: u32 = 2;
const CURSOR_MODE_METADATA: u32 = 4;

pub const RESPONSE_SUCCESS: u32 = 0;
pub const RESPONSE_CANCELLED: u32 = 1;
pub const RESPONSE_OTHER: u32 = 2;

const NIRI_BUS: &str = "org.gnome.Shell.Screenshot";
const NIRI_PATH: &str = "/org/gnome/Shell/Screenshot";

const NIRI_PORTAL_BUS: &str = "org.freedesktop.impl.portal.desktop.niri";
const NIRI_PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENCAST_SELECTION_INTERFACE: &str = "org.loom.portal.ScreenCastSelection";

pub type PortalResult = (u32, HashMap<String, OwnedValue>);

#[derive(Debug)]
pub enum Failure {
    Cancelled,
    Other(String),
}

/// Reads a boolean portal option.
pub fn option_bool(options: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    options.get(key)?.downcast_ref::<bool>().ok()
}

/// Percent-encodes a path into a `file://` URI.
pub fn file_uri(path: &Path) -> Result<String, String> {
    let bytes = path.as_os_str().as_bytes();
    if !bytes.starts_with(b"/") {
        return Err(format!("not an absolute path: {}", path.display()));
    }
    let mut uri = String::from("file://");
    for &byte in bytes {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(byte as char)
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    Ok(uri)
}

fn owned(value: impl Into<zbus::zvariant::Value<'static>>) -> OwnedValue {
    OwnedValue::try_from(value.into()).expect("simple portal result value")
}

pub fn uri_result(uri: String) -> PortalResult {
    let mut results = HashMap::new();
    results.insert("uri".to_owned(), owned(uri));
    (RESPONSE_SUCCESS, results)
}

pub fn color_result(rgb: (f64, f64, f64)) -> PortalResult {
    let mut results = HashMap::new();
    results.insert(
        "color".to_owned(),
        owned(zbus::zvariant::Value::from((rgb.0, rgb.1, rgb.2))),
    );
    (RESPONSE_SUCCESS, results)
}

pub fn cancelled_result() -> PortalResult {
    (RESPONSE_CANCELLED, HashMap::new())
}

pub fn other_result(message: impl Into<String>) -> PortalResult {
    let mut results = HashMap::new();
    results.insert("error".to_owned(), owned(message.into()));
    (RESPONSE_OTHER, results)
}

fn failure_result(failure: Failure) -> PortalResult {
    match failure {
        Failure::Cancelled => cancelled_result(),
        Failure::Other(message) => other_result(message),
    }
}

/// Maps a remote compositor error to a cancellation when the compositor uses
/// its "user went away" error; every other error is a real failure.
fn remote_failure(err: zbus::Error, what: &str) -> Failure {
    match &err {
        zbus::Error::MethodError(_, Some(message), _)
            if message.contains("canceled")
                || message.contains("cancelled")
                || message.contains("no color picked") =>
        {
            Failure::Cancelled
        }
        _ => Failure::Other(format!("{what}: {err}")),
    }
}

async fn niri_proxy(connection: &Connection) -> Result<Proxy<'static>, String> {
    Proxy::new_owned(connection.clone(), NIRI_BUS, NIRI_PATH, NIRI_BUS)
        .await
        .map_err(|err| format!("screenshot proxy: {err}"))
}

async fn niri_cancel(connection: &Connection) {
    if let Ok(proxy) = niri_proxy(connection).await {
        let _: Result<(), _> = proxy.call("CancelScreenshot", &()).await;
    }
}

async fn fullscreen_screenshot(connection: &Connection) -> Result<String, Failure> {
    let proxy = niri_proxy(connection).await.map_err(Failure::Other)?;
    let (ok, path): (bool, PathBuf) = proxy
        .call("Screenshot", &(false, false, String::new()))
        .await
        .map_err(|err| remote_failure(err, "niri Screenshot"))?;
    if !ok {
        return Err(Failure::Cancelled);
    }
    file_uri(&path).map_err(Failure::Other)
}

async fn area_screenshot(connection: &Connection, rect: SelectionRect) -> Result<String, Failure> {
    let proxy = niri_proxy(connection).await.map_err(Failure::Other)?;
    let (ok, path): (bool, PathBuf) = proxy
        .call(
            "ScreenshotArea",
            &(
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                false,
                String::new(),
            ),
        )
        .await
        .map_err(|err| remote_failure(err, "niri ScreenshotArea"))?;
    if !ok {
        return Err(Failure::Cancelled);
    }
    file_uri(&path).map_err(Failure::Other)
}

/// Outcome of running the external layer-shell picker.
#[derive(Debug, PartialEq)]
pub enum PickerOutcome {
    Selected(SelectionRect),
    SelectedOutput(String),
    SelectedWindow(u64),
    Cancelled,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    Region,
    Output,
    Window,
}

impl PickerMode {
    /// Parses the picker command line. Unknown arguments are ignored, matching
    /// the historical behaviour.
    pub fn from_args<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut mode = Self::Region;
        for arg in args {
            match arg.as_ref() {
                "--screencast-window" => return Self::Window,
                "--screencast" => mode = Self::Output,
                _ => (),
            }
        }
        mode
    }
}

/// Picks the interaction that satisfies a ScreenCast `types` mask.
///
/// A request for windows only uses the window picker; anything that also
/// includes outputs (monitors or virtual) uses the output picker.
pub fn picker_mode_for_types(types: u32) -> PickerMode {
    let windows = types & SOURCE_TYPE_WINDOW != 0;
    let outputs = types & (SOURCE_TYPE_MONITOR | SOURCE_TYPE_VIRTUAL) != 0;
    if windows && !outputs {
        PickerMode::Window
    } else {
        PickerMode::Output
    }
}

/// Validates a picker result against the source types the app requested and
/// converts it to niri's `SetSelection` sources.
pub fn validate_picker_selection(
    types: u32,
    outcome: PickerOutcome,
) -> Result<Vec<(u32, String)>, String> {
    match outcome {
        PickerOutcome::SelectedOutput(output) => {
            if types & (SOURCE_TYPE_MONITOR | SOURCE_TYPE_VIRTUAL) == 0 {
                return Err("picker selected an output but only windows were requested".to_owned());
            }
            if output.trim().is_empty() {
                return Err("picker selected an empty output name".to_owned());
            }
            Ok(vec![(SOURCE_TYPE_MONITOR, output)])
        }
        PickerOutcome::SelectedWindow(window) => {
            if types & SOURCE_TYPE_WINDOW == 0 {
                return Err("picker selected a window but only outputs were requested".to_owned());
            }
            if window == 0 {
                return Err("picker selected an invalid window id 0".to_owned());
            }
            Ok(vec![(SOURCE_TYPE_WINDOW, window.to_string())])
        }
        PickerOutcome::Selected(_) => {
            Err("picker returned a screenshot region for a screen cast".to_owned())
        }
        PickerOutcome::Cancelled | PickerOutcome::Unavailable => {
            Err("picker did not return a selection".to_owned())
        }
    }
}

/// Resolves `loom-portal-picker`: `$LOOM_PORTAL_PICKER`, then a sibling of
/// this executable, then `$PATH`.
pub fn picker_program() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LOOM_PORTAL_PICKER") {
        return Some(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("loom-portal-picker");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("loom-portal-picker"))
        .find(|candidate| candidate.is_file())
}

fn picker_command(program: &Path, mode: PickerMode) -> Command {
    let mut command = Command::new(program);
    match mode {
        PickerMode::Region => {}
        PickerMode::Output => {
            command.arg("--screencast");
        }
        PickerMode::Window => {
            command.arg("--screencast-window");
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Runs the picker in region mode and waits for it, without cancellation
/// support. Kept for callers that do not have a request object (and tests).
pub fn run_picker(program: &Path) -> Result<PickerOutcome, String> {
    run_picker_mode(program, PickerMode::Region)
}

/// Runs the picker and waits for it, without cancellation support. Kept for
/// callers that do not have a request object (and for unit tests).
pub fn run_picker_mode(program: &Path, mode: PickerMode) -> Result<PickerOutcome, String> {
    run_picker_mode_cancellable(program, mode, None)
}

fn run_picker_mode_cancellable(
    program: &Path,
    mode: PickerMode,
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<PickerOutcome, String> {
    let mut command = picker_command(program, mode);
    // A fork racing with a file being written can transiently fail exec with
    // ETXTBSY; retry briefly instead of treating the picker as broken.
    let mut attempts = 0;
    let mut child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PickerOutcome::Unavailable);
            }
            Err(err) if err.raw_os_error() == Some(26) && attempts < 3 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return Err(format!("cannot run {}: {err}", program.display())),
        }
    };

    let status = loop {
        if cancelled
            .as_ref()
            .is_some_and(|cancelled| cancelled.load(Ordering::SeqCst))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(PickerOutcome::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for {}: {err}", program.display()));
            }
        }
    };

    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout)
            .map_err(|err| format!("reading picker output: {err}"))?;
    }
    if !status.success() {
        return Ok(PickerOutcome::Cancelled);
    }
    parse_picker_output(mode, stdout.trim())
}

fn parse_picker_output(mode: PickerMode, stdout: &str) -> Result<PickerOutcome, String> {
    if mode != PickerMode::Region {
        #[derive(serde::Deserialize)]
        struct Source {
            output: Option<String>,
            window: Option<u64>,
        }
        let source: Source = serde_json::from_str(stdout)
            .map_err(|err| format!("invalid screencast picker output: {err}"))?;
        return match (source.output, source.window) {
            (Some(output), _) => Ok(PickerOutcome::SelectedOutput(output)),
            (_, Some(window)) => Ok(PickerOutcome::SelectedWindow(window)),
            _ => Err("picker returned neither output nor window".to_owned()),
        };
    }
    let rect: SelectionRect =
        serde_json::from_str(stdout).map_err(|err| format!("invalid picker output: {err}"))?;
    Ok(PickerOutcome::Selected(rect))
}

/// Runs the picker on a worker thread so D-Bus messages (notably
/// `Request.Close`) keep flowing while the overlay is open, and kills the
/// picker process when the request is cancelled.
async fn run_picker_mode_async(
    program: PathBuf,
    mode: PickerMode,
    cancelled: Arc<AtomicBool>,
) -> Result<PickerOutcome, String> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("loom-portal picker".to_owned())
        .spawn(move || {
            let outcome = run_picker_mode_cancellable(&program, mode, Some(cancelled));
            let _ = tx.send_blocking(outcome);
        })
        .map_err(|err| format!("cannot start picker thread: {err}"))?;
    rx.recv()
        .await
        .map_err(|err| format!("picker worker stopped: {err}"))?
}

/// Asks niri's IPC to pick a window (crosshair) and returns its id.
pub fn pick_window_via_niri(path: &Path) -> Result<Option<u64>, String> {
    use niri_ipc::socket::Socket;
    use niri_ipc::{Request, Response};

    let mut socket =
        Socket::connect_to(path).map_err(|err| format!("cannot connect to niri IPC: {err}"))?;
    let reply = socket
        .send(Request::PickWindow)
        .map_err(|err| format!("niri PickWindow: {err}"))?;
    match reply.map_err(|err| format!("niri returned an error: {err}"))? {
        Response::PickedWindow(window) => Ok(window.map(|window| window.id)),
        other => Err(format!("unexpected niri response: {other:?}")),
    }
}

async fn interactive_screenshot(
    connection: &Connection,
    cancelled: Arc<AtomicBool>,
) -> Result<String, Failure> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(Failure::Cancelled);
    }

    // Prefer the external layer-shell picker when it is installed. Anything
    // other than a clean selection falls back to the compositor's own UI.
    if let Some(program) = picker_program() {
        match run_picker_mode_async(program, PickerMode::Region, cancelled.clone()).await {
            Ok(PickerOutcome::Selected(rect)) => {
                if cancelled.load(Ordering::SeqCst) {
                    return Err(Failure::Cancelled);
                }
                return area_screenshot(connection, rect).await;
            }
            Ok(PickerOutcome::Cancelled) => return Err(Failure::Cancelled),
            Ok(PickerOutcome::Unavailable)
            | Ok(PickerOutcome::SelectedOutput(_))
            | Ok(PickerOutcome::SelectedWindow(_))
            | Err(_) => (),
        }
    }
    if cancelled.load(Ordering::SeqCst) {
        return Err(Failure::Cancelled);
    }

    let proxy = niri_proxy(connection).await.map_err(Failure::Other)?;
    let (ok, uri): (bool, String) = proxy
        .call("InteractiveScreenshot", &())
        .await
        .map_err(|err| remote_failure(err, "niri InteractiveScreenshot"))?;
    if cancelled.load(Ordering::SeqCst) || !ok || uri.is_empty() {
        return Err(Failure::Cancelled);
    }
    Ok(uri)
}

async fn pick_color_from_niri(connection: &Connection) -> Result<(f64, f64, f64), Failure> {
    let proxy = niri_proxy(connection).await.map_err(Failure::Other)?;
    let result: HashMap<String, OwnedValue> = proxy
        .call("PickColor", &())
        .await
        .map_err(|err| remote_failure(err, "niri PickColor"))?;
    let color = result
        .get("color")
        .ok_or_else(|| Failure::Other("niri returned no colour".to_owned()))?
        .try_clone()
        .map_err(|err| Failure::Other(format!("colour decoding: {err}")))?;
    let rgb = <(f64, f64, f64)>::try_from(color)
        .map_err(|err| Failure::Other(format!("unexpected colour shape: {err}")))?;
    Ok(rgb)
}

/// Cancellable request object the portal calls to close an in-flight request.
pub struct PortalRequest {
    connection: Connection,
    cancelled: Arc<AtomicBool>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl PortalRequest {
    async fn close(&self) -> fdo::Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        niri_cancel(&self.connection).await;
        Ok(())
    }
}

pub struct ScreenshotBackend {
    connection: OnceLock<Connection>,
}

impl Default for ScreenshotBackend {
    fn default() -> Self {
        Self {
            connection: OnceLock::new(),
        }
    }
}

impl ScreenshotBackend {
    pub fn new(connection: Connection) -> Self {
        let backend = Self::default();
        let _ = backend.connection.set(connection);
        backend
    }

    async fn connection(&self) -> Result<Connection, String> {
        if let Some(connection) = self.connection.get() {
            return Ok(connection.clone());
        }
        Connection::session()
            .await
            .map_err(|err| format!("session bus: {err}"))
    }

    async fn register_request(
        server: &ObjectServer,
        connection: Connection,
        handle: ObjectPath<'_>,
        cancelled: Arc<AtomicBool>,
    ) -> OwnedObjectPath {
        let path = OwnedObjectPath::from(handle.clone());
        let request = PortalRequest {
            connection,
            cancelled,
        };
        if let Err(err) = server.at(path.clone(), request).await {
            eprintln!(
                "loom-portal: cannot register request {}: {err}",
                path.as_str()
            );
        }
        path
    }

    async fn unregister_request(server: &ObjectServer, path: &OwnedObjectPath) {
        let _ = server.remove::<PortalRequest, _>(path).await;
    }
}

#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl ScreenshotBackend {
    async fn screenshot(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: ObjectPath<'_>,
        _app_id: String,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> PortalResult {
        let interactive = option_bool(&options, "interactive").unwrap_or(false);
        let cancelled = Arc::new(AtomicBool::new(false));
        let connection = match self.connection().await {
            Ok(connection) => connection,
            Err(err) => return other_result(err),
        };
        let path =
            Self::register_request(server, connection.clone(), handle, cancelled.clone()).await;

        let result = if interactive {
            interactive_screenshot(&connection, cancelled).await
        } else {
            fullscreen_screenshot(&connection).await
        };

        Self::unregister_request(server, &path).await;
        match result {
            Ok(uri) => uri_result(uri),
            Err(failure) => failure_result(failure),
        }
    }

    async fn pick_color(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: ObjectPath<'_>,
        _app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> PortalResult {
        let cancelled = Arc::new(AtomicBool::new(false));
        let connection = match self.connection().await {
            Ok(connection) => connection,
            Err(err) => return other_result(err),
        };
        let path =
            Self::register_request(server, connection.clone(), handle, cancelled.clone()).await;
        let result = pick_color_from_niri(&connection).await;
        Self::unregister_request(server, &path).await;
        if cancelled.load(Ordering::SeqCst) {
            return cancelled_result();
        }
        match result {
            Ok(rgb) => color_result(rgb),
            Err(failure) => failure_result(failure),
        }
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

// ---------------------------------------------------------------------------
// ScreenCast
// ---------------------------------------------------------------------------

/// Proxy backend for `org.freedesktop.impl.portal.ScreenCast`.
///
/// The compositor owns the capture sessions and PipeWire nodes; this backend
/// forwards CreateSession/SelectSources/Start unchanged and re-exports the
/// session and request objects on its own bus name so the portal frontend can
/// close them. The PipeWire node ids in Start's results stay valid because the
/// frontend connects to the same PipeWire daemon.
pub struct ScreenCastBackend {
    shared: Arc<ScreenCastShared>,
}

impl Default for ScreenCastBackend {
    fn default() -> Self {
        Self {
            shared: Arc::new(ScreenCastShared::default()),
        }
    }
}

impl Clone for ScreenCastBackend {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

#[derive(Default)]
struct ScreenCastShared {
    connection: OnceLock<Connection>,
    sessions: Mutex<HashMap<OwnedObjectPath, SessionEntry>>,
}

struct SessionEntry {
    owner: Option<OwnedUniqueName>,
    /// Source types requested by the most recent SelectSources call.
    types: u32,
    started: bool,
}

impl ScreenCastBackend {
    pub fn new(connection: Connection) -> Self {
        let backend = Self::default();
        let _ = backend.shared.connection.set(connection);
        backend
    }

    async fn connection(&self) -> Result<Connection, String> {
        if let Some(connection) = self.shared.connection.get() {
            return Ok(connection.clone());
        }
        Connection::session()
            .await
            .map_err(|err| format!("session bus: {err}"))
    }

    fn owner_of(header: &Header<'_>) -> Option<OwnedUniqueName> {
        header
            .sender()
            .map(|sender| OwnedUniqueName::from(sender.to_owned()))
    }

    fn check_owner(entry: &SessionEntry, header: &Header<'_>) -> fdo::Result<()> {
        let Some(owner) = &entry.owner else {
            return Ok(());
        };
        match header.sender() {
            Some(sender) if owner.as_str() == sender.as_str() => Ok(()),
            _ => Err(fdo::Error::AccessDenied(
                "session belongs to another D-Bus client".to_owned(),
            )),
        }
    }

    /// Records a session and starts watching niri for its closure.
    fn track_session(&self, session_handle: &OwnedObjectPath, owner: Option<OwnedUniqueName>) {
        if let Ok(mut sessions) = self.shared.sessions.lock() {
            sessions.insert(
                session_handle.clone(),
                SessionEntry {
                    owner,
                    types: SOURCE_TYPE_MONITOR,
                    started: false,
                },
            );
        }
        self.spawn_session_monitor(session_handle.clone());
    }

    fn spawn_session_monitor(&self, path: OwnedObjectPath) {
        let Some(connection) = self.shared.connection.get().cloned() else {
            return;
        };
        let backend = self.clone();
        let task_connection = connection.clone();
        connection
            .executor()
            .spawn(
                async move {
                    if wait_for_niri_session_end(task_connection, path.clone()).await {
                        backend.close_session_from_compositor(&path).await;
                    }
                },
                "loom-portal niri session monitor",
            )
            .detach();
    }

    /// Closes a session because niri (or the compositor) closed it.
    async fn close_session_from_compositor(&self, path: &OwnedObjectPath) {
        if self
            .shared
            .sessions
            .lock()
            .map(|mut sessions| sessions.remove(path))
            .ok()
            .flatten()
            .is_none()
        {
            return;
        }
        let Some(connection) = self.shared.connection.get() else {
            return;
        };
        if let Ok(emitter) = SignalEmitter::new(connection, path.as_str()) {
            let _ = ProxySession::closed(&emitter).await;
        }
        let _ = connection
            .object_server()
            .remove::<ProxySession, _>(path)
            .await;
    }

    async fn forward(
        connection: &Connection,
        method: &str,
        body: impl serde::Serialize + zbus::zvariant::DynamicType,
    ) -> Result<PortalResult, String> {
        let proxy = niri_portal_proxy(connection, SCREENCAST_INTERFACE).await?;
        proxy
            .call(method, &body)
            .await
            .map_err(|err| format!("niri {method}: {err}"))
    }

    async fn export_request(
        server: &ObjectServer,
        connection: &Connection,
        handle: &OwnedObjectPath,
        cancelled: Arc<AtomicBool>,
        forward: bool,
    ) {
        let request = ProxyRequest {
            path: handle.clone(),
            connection: connection.clone(),
            cancelled: Some(cancelled),
            forward,
        };
        if let Err(err) = server.at(handle.clone(), request).await {
            eprintln!(
                "loom-portal: cannot register request {}: {err}",
                handle.as_str()
            );
        }
    }

    async fn unexport_request(server: &ObjectServer, handle: &OwnedObjectPath) {
        let _ = server.remove::<ProxyRequest, _>(handle).await;
    }

    async fn export_session(
        connection: &Connection,
        server: &ObjectServer,
        shared: &Arc<ScreenCastShared>,
        path: &OwnedObjectPath,
    ) {
        let session = ProxySession {
            path: path.clone(),
            connection: connection.clone(),
            shared: shared.clone(),
        };
        if let Err(err) = server.at(path.clone(), session).await {
            eprintln!(
                "loom-portal: cannot export session {}: {err}",
                path.as_str()
            );
        }
    }

    async fn unexport_session(server: &ObjectServer, path: &OwnedObjectPath) {
        let _ = server.remove::<ProxySession, _>(path).await;
    }

    async fn set_screen_cast_selection(
        connection: &Connection,
        session_handle: &OwnedObjectPath,
        sources: Vec<(u32, String)>,
    ) -> Result<(), String> {
        let proxy = Proxy::new_owned(
            connection.clone(),
            NIRI_PORTAL_BUS,
            NIRI_PORTAL_PATH,
            SCREENCAST_SELECTION_INTERFACE,
        )
        .await
        .map_err(|err| format!("niri selection proxy: {err}"))?;
        let _: () = proxy
            .call("SetSelection", &(session_handle.clone(), sources, 0u32))
            .await
            .map_err(|err| format!("niri SetSelection: {err}"))?;
        Ok(())
    }

    async fn clear_screen_cast_selection(
        connection: &Connection,
        session_handle: &OwnedObjectPath,
    ) {
        let Ok(proxy) = Proxy::new_owned(
            connection.clone(),
            NIRI_PORTAL_BUS,
            NIRI_PORTAL_PATH,
            SCREENCAST_SELECTION_INTERFACE,
        )
        .await
        else {
            return;
        };
        let _: Result<(), _> = proxy
            .call("ClearSelection", &(session_handle.clone(),))
            .await;
    }
}

async fn niri_portal_proxy(
    connection: &Connection,
    interface: &'static str,
) -> Result<Proxy<'static>, String> {
    Proxy::new_owned(
        connection.clone(),
        NIRI_PORTAL_BUS,
        NIRI_PORTAL_PATH,
        interface,
    )
    .await
    .map_err(|err| format!("niri portal proxy: {err}"))
}

async fn forward_close(connection: &Connection, path: &OwnedObjectPath, interface: &'static str) {
    let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        NIRI_PORTAL_BUS,
        path.as_str().to_owned(),
        interface,
    )
    .await
    else {
        return;
    };
    let _: Result<(), _> = proxy.call("Close", &()).await;
}

/// Waits until niri closes the session or niri leaves the bus.
async fn wait_for_niri_session_end(connection: Connection, path: OwnedObjectPath) -> bool {
    let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        NIRI_PORTAL_BUS,
        path.as_str().to_owned(),
        "org.freedesktop.impl.portal.Session",
    )
    .await
    else {
        return true;
    };
    let Ok(closed) = proxy.receive_signal("Closed").await else {
        return true;
    };

    let Ok(dbus) = zbus::fdo::DBusProxy::new(&connection).await else {
        // Without a name-owner watch, still clean up when Closed arrives.
        let mut closed = closed;
        return closed.next().await.is_some();
    };
    let Ok(owner_changed) = dbus.receive_name_owner_changed().await else {
        let mut closed = closed;
        return closed.next().await.is_some();
    };

    let niri_gone = async {
        let mut owner_changed = owner_changed;
        while let Some(signal) = owner_changed.next().await {
            let Ok(args) = signal.args() else {
                continue;
            };
            if args.name().as_str() == NIRI_PORTAL_BUS && args.new_owner().is_none() {
                return true;
            }
        }
        pending::<bool>().await
    };

    let mut closed = closed;
    let closed_next = closed.next();
    futures_util::pin_mut!(closed_next, niri_gone);
    match futures_util::future::select(closed_next, niri_gone).await {
        futures_util::future::Either::Left((signal, _)) => signal.is_some(),
        futures_util::future::Either::Right(_) => true,
    }
}

/// Session object re-exported on the loom-portal bus.
struct ProxySession {
    path: OwnedObjectPath,
    connection: Connection,
    shared: Arc<ScreenCastShared>,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl ProxySession {
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let was_tracked = self
            .shared
            .sessions
            .lock()
            .map(|mut sessions| sessions.remove(&self.path).is_some())
            .unwrap_or(false);
        if was_tracked {
            forward_close(
                &self.connection,
                &self.path,
                "org.freedesktop.impl.portal.Session",
            )
            .await;
            let _ = Self::closed(&emitter).await;
        }
        let _ = server.remove::<Self, _>(&self.path).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Request object re-exported on the loom-portal bus.
struct ProxyRequest {
    path: OwnedObjectPath,
    connection: Connection,
    cancelled: Option<Arc<AtomicBool>>,
    /// Whether niri owns a request object at the same path that must be closed.
    forward: bool,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl ProxyRequest {
    async fn close(&self) -> fdo::Result<()> {
        if let Some(cancelled) = &self.cancelled {
            cancelled.store(true, Ordering::SeqCst);
        }
        if self.forward {
            forward_close(
                &self.connection,
                &self.path,
                "org.freedesktop.impl.portal.Request",
            )
            .await;
        }
        Ok(())
    }
}

fn source_types_from_options(options: &HashMap<String, OwnedValue>) -> Result<u32, String> {
    let types = match options.get("types") {
        Some(value) => value
            .downcast_ref::<u32>()
            .map_err(|_| "ScreenCast types option must be a u32".to_owned())?,
        None => SOURCE_TYPE_MONITOR,
    };
    if types == 0 || types & !SOURCE_TYPE_ALL != 0 {
        return Err(format!("unsupported ScreenCast source types {types:#x}"));
    }
    Ok(types)
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastBackend {
    async fn create_session(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<PortalResult> {
        let owner = Self::owner_of(&header);
        let cancelled = Arc::new(AtomicBool::new(false));
        let connection = match self.connection().await {
            Ok(connection) => connection,
            Err(err) => return Ok(other_result(err)),
        };
        Self::export_request(server, &connection, &handle, cancelled.clone(), false).await;

        if self
            .shared
            .sessions
            .lock()
            .map(|sessions| sessions.contains_key(&session_handle))
            .unwrap_or(false)
        {
            Self::unexport_request(server, &handle).await;
            return Err(fdo::Error::Failed("session already exists".to_owned()));
        }
        Self::export_session(&connection, server, &self.shared, &session_handle).await;

        let result = Self::forward(
            &connection,
            "CreateSession",
            (handle.clone(), session_handle.clone(), app_id, options),
        )
        .await;
        let was_cancelled = cancelled.load(Ordering::SeqCst);
        Self::unexport_request(server, &handle).await;

        match result {
            Ok((RESPONSE_SUCCESS, results)) if !was_cancelled => {
                self.track_session(&session_handle, owner);
                Ok((RESPONSE_SUCCESS, results))
            }
            Ok((RESPONSE_SUCCESS, _)) => {
                Self::unexport_session(server, &session_handle).await;
                Ok(cancelled_result())
            }
            Ok(other) => {
                Self::unexport_session(server, &session_handle).await;
                Ok(other)
            }
            Err(err) => {
                Self::unexport_session(server, &session_handle).await;
                Ok(other_result(err))
            }
        }
    }

    async fn select_sources(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<PortalResult> {
        let types = match source_types_from_options(&options) {
            Ok(types) => types,
            Err(err) => return Ok(other_result(err)),
        };
        {
            let sessions = self
                .shared
                .sessions
                .lock()
                .map_err(|_| fdo::Error::Failed("session registry poisoned".to_owned()))?;
            let Some(entry) = sessions.get(&session_handle) else {
                return Err(fdo::Error::Failed("no such session".to_owned()));
            };
            Self::check_owner(entry, &header)?;
            if entry.started {
                return Err(fdo::Error::Failed("session was already started".to_owned()));
            }
        }

        let connection = match self.connection().await {
            Ok(connection) => connection,
            Err(err) => return Ok(other_result(err)),
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        Self::export_request(server, &connection, &handle, cancelled.clone(), false).await;
        let result = Self::forward(
            &connection,
            "SelectSources",
            (handle.clone(), session_handle.clone(), app_id, options),
        )
        .await;
        let was_cancelled = cancelled.load(Ordering::SeqCst);
        Self::unexport_request(server, &handle).await;

        match result {
            Ok((RESPONSE_SUCCESS, results)) if !was_cancelled => {
                if let Ok(mut sessions) = self.shared.sessions.lock()
                    && let Some(entry) = sessions.get_mut(&session_handle)
                {
                    entry.types = types;
                }
                Ok((RESPONSE_SUCCESS, results))
            }
            Ok((RESPONSE_SUCCESS, _)) => Ok(cancelled_result()),
            Ok(other) => Ok(other),
            Err(err) => Ok(other_result(err)),
        }
    }

    async fn start(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(header)] header: Header<'_>,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<PortalResult> {
        let types = {
            let sessions = self
                .shared
                .sessions
                .lock()
                .map_err(|_| fdo::Error::Failed("session registry poisoned".to_owned()))?;
            let Some(entry) = sessions.get(&session_handle) else {
                return Err(fdo::Error::Failed("no such session".to_owned()));
            };
            Self::check_owner(entry, &header)?;
            if entry.started {
                return Err(fdo::Error::Failed("session was already started".to_owned()));
            }
            entry.types
        };

        let connection = match self.connection().await {
            Ok(connection) => connection,
            Err(err) => return Ok(other_result(err)),
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        Self::export_request(server, &connection, &handle, cancelled.clone(), true).await;

        // Prefer the external picker when available. If the compositor does not
        // implement the selection API, fall back to its own picker.
        let mut forced_selection = false;
        if let Some(program) = picker_program() {
            let mode = picker_mode_for_types(types);
            match run_picker_mode_async(program, mode, cancelled.clone()).await {
                Ok(PickerOutcome::Cancelled) => {
                    Self::clear_screen_cast_selection(&connection, &session_handle).await;
                    Self::unexport_request(server, &handle).await;
                    return Ok(cancelled_result());
                }
                Ok(
                    outcome @ (PickerOutcome::SelectedOutput(_) | PickerOutcome::SelectedWindow(_)),
                ) => {
                    if cancelled.load(Ordering::SeqCst) {
                        Self::clear_screen_cast_selection(&connection, &session_handle).await;
                        Self::unexport_request(server, &handle).await;
                        return Ok(cancelled_result());
                    }
                    match validate_picker_selection(types, outcome) {
                        Ok(sources) => {
                            match Self::set_screen_cast_selection(
                                &connection,
                                &session_handle,
                                sources,
                            )
                            .await
                            {
                                Ok(()) => forced_selection = true,
                                Err(err) => eprintln!(
                                    "loom-portal: external ScreenCast selection failed: {err}"
                                ),
                            }
                        }
                        Err(err) => {
                            eprintln!("loom-portal: external ScreenCast selection invalid: {err}");
                        }
                    }
                }
                Ok(PickerOutcome::Unavailable) | Ok(PickerOutcome::Selected(_)) | Err(_) => (),
            }

            if cancelled.load(Ordering::SeqCst) {
                Self::clear_screen_cast_selection(&connection, &session_handle).await;
                Self::unexport_request(server, &handle).await;
                return Ok(cancelled_result());
            }
        }

        let session_still_tracked = {
            let mut sessions = self
                .shared
                .sessions
                .lock()
                .map_err(|_| fdo::Error::Failed("session registry poisoned".to_owned()))?;
            match sessions.get_mut(&session_handle) {
                Some(entry) => {
                    entry.started = true;
                    true
                }
                None => false,
            }
        };
        if !session_still_tracked {
            Self::unexport_request(server, &handle).await;
            return Ok(cancelled_result());
        }

        let result = Self::forward(
            &connection,
            "Start",
            (
                handle.clone(),
                session_handle.clone(),
                app_id,
                parent_window,
                options,
            ),
        )
        .await;

        // niri consumes the forced selection while starting. Clear it anyway
        // so a failed Start cannot leak a stale selection into the session.
        if forced_selection {
            Self::clear_screen_cast_selection(&connection, &session_handle).await;
        }
        Self::unexport_request(server, &handle).await;

        match result {
            Ok(response) => Ok(response),
            Err(err) => Ok(other_result(err)),
        }
    }

    #[zbus(property, name = "AvailableSourceTypes")]
    fn available_source_types(&self) -> u32 {
        // Match the compositor: it creates monitors and windows, not virtual
        // monitors. Virtual requests are still accepted defensively.
        SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW
    }

    #[zbus(property, name = "AvailableCursorModes")]
    fn available_cursor_modes(&self) -> u32 {
        CURSOR_MODE_HIDDEN | CURSOR_MODE_EMBEDDED | CURSOR_MODE_METADATA
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }
}

/// Starts the portal backend on the session bus.
pub async fn serve() -> anyhow::Result<Connection> {
    let connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .build()
        .await?;
    let server = connection.object_server();
    server
        .at(OBJECT_PATH, ScreenshotBackend::new(connection.clone()))
        .await?;
    server
        .at(OBJECT_PATH, ScreenCastBackend::new(connection.clone()))
        .await?;
    eprintln!("loom-portal listening on {BUS_NAME} at {OBJECT_PATH}");
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn file_uri_percent_encodes() {
        assert_eq!(
            file_uri(Path::new("/tmp/a screenshot #1.png")).unwrap(),
            "file:///tmp/a%20screenshot%20%231.png"
        );
        assert!(file_uri(Path::new("relative.png")).is_err());
    }

    #[test]
    fn reads_interactive_option() {
        let mut options = HashMap::new();
        options.insert("interactive".to_owned(), owned(true));
        assert_eq!(option_bool(&options, "interactive"), Some(true));
        assert_eq!(option_bool(&options, "missing"), None);
    }

    #[test]
    fn builds_portal_results() {
        let (code, results) = uri_result("file:///tmp/x.png".to_owned());
        assert_eq!(code, RESPONSE_SUCCESS);
        assert!(results.contains_key("uri"));

        let (code, results) = color_result((0.1, 0.2, 0.3));
        assert_eq!(code, RESPONSE_SUCCESS);
        assert!(results.contains_key("color"));

        assert_eq!(cancelled_result().0, RESPONSE_CANCELLED);
        assert_eq!(other_result("boom").0, RESPONSE_OTHER);
    }

    #[test]
    fn screencast_version_is_exposed() {
        let backend = ScreenCastBackend::default();
        assert_eq!(backend.version(), 4);
        assert_eq!(
            backend.available_source_types(),
            SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW
        );
        assert_eq!(
            backend.available_cursor_modes(),
            CURSOR_MODE_HIDDEN | CURSOR_MODE_EMBEDDED | CURSOR_MODE_METADATA
        );
    }

    #[test]
    fn selection_rect_normalizes_drag_and_output_offset() {
        let rect = SelectionRect::from_drag((1920, 0), (300.4, 200.6), (100.2, 50.1)).unwrap();
        assert_eq!(
            rect,
            SelectionRect {
                x: 1920 + 100,
                y: 50,
                width: 200,
                height: 151,
            }
        );
        assert!(SelectionRect::from_drag((0, 0), (10., 10.), (10., 10.)).is_none());
    }

    #[test]
    fn run_picker_parses_selection_and_cancellation() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("loom-portal-picker-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let selected = dir.join("selected");
        std::fs::write(
            &selected,
            "#!/bin/sh\nprintf '{\"x\":10,\"y\":20,\"width\":30,\"height\":40}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            run_picker(&selected).unwrap(),
            PickerOutcome::Selected(SelectionRect {
                x: 10,
                y: 20,
                width: 30,
                height: 40
            })
        );

        let cancelled = dir.join("cancelled");
        std::fs::write(&cancelled, "#!/bin/sh\nexit 2\n").unwrap();
        std::fs::set_permissions(&cancelled, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run_picker(&cancelled).unwrap(), PickerOutcome::Cancelled);

        let garbage = dir.join("garbage");
        std::fs::write(&garbage, "#!/bin/sh\necho not-json\n").unwrap();
        std::fs::set_permissions(&garbage, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(run_picker(&garbage).is_err());

        assert_eq!(
            run_picker(&dir.join("missing")).unwrap(),
            PickerOutcome::Unavailable
        );
    }

    #[test]
    fn screencast_picker_returns_an_output_name() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "loom-portal-screencast-picker-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("picker");
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$1\" = \"--screencast\" ]; then printf '{\"output\":\"eDP-1\"}'\nelse exit 9\nfi\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            run_picker_mode(&script, PickerMode::Output).unwrap(),
            PickerOutcome::SelectedOutput("eDP-1".to_owned())
        );

        let window_script = dir.join("window-picker");
        std::fs::write(&window_script, "#!/bin/sh\nprintf '{\"window\":42}'\n").unwrap();
        std::fs::set_permissions(&window_script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            run_picker_mode(&window_script, PickerMode::Window).unwrap(),
            PickerOutcome::SelectedWindow(42)
        );

        let empty = dir.join("empty");
        std::fs::write(&empty, "#!/bin/sh\nprintf '{}'\n").unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(run_picker_mode(&empty, PickerMode::Output).is_err());
    }

    #[test]
    fn picker_mode_is_selected_from_types_and_arguments() {
        assert_eq!(PickerMode::from_args(["--screencast"]), PickerMode::Output);
        assert_eq!(
            PickerMode::from_args(["ignored", "--screencast-window", "--screencast"]),
            PickerMode::Window
        );
        assert_eq!(
            PickerMode::from_args(Vec::<String>::new()),
            PickerMode::Region
        );

        assert_eq!(
            picker_mode_for_types(SOURCE_TYPE_MONITOR),
            PickerMode::Output
        );
        assert_eq!(
            picker_mode_for_types(SOURCE_TYPE_WINDOW),
            PickerMode::Window
        );
        assert_eq!(
            picker_mode_for_types(SOURCE_TYPE_VIRTUAL),
            PickerMode::Output
        );
        assert_eq!(
            picker_mode_for_types(SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW),
            PickerMode::Output
        );
        assert_eq!(
            picker_mode_for_types(SOURCE_TYPE_WINDOW | SOURCE_TYPE_VIRTUAL),
            PickerMode::Output
        );
    }

    #[test]
    fn validates_picker_selections_against_requested_types() {
        assert_eq!(
            validate_picker_selection(
                SOURCE_TYPE_MONITOR,
                PickerOutcome::SelectedOutput("eDP-1".to_owned())
            )
            .unwrap(),
            vec![(SOURCE_TYPE_MONITOR, "eDP-1".to_owned())]
        );
        assert_eq!(
            validate_picker_selection(
                SOURCE_TYPE_VIRTUAL,
                PickerOutcome::SelectedOutput("HEADLESS-1".to_owned())
            )
            .unwrap(),
            vec![(SOURCE_TYPE_MONITOR, "HEADLESS-1".to_owned())]
        );
        assert_eq!(
            validate_picker_selection(SOURCE_TYPE_WINDOW, PickerOutcome::SelectedWindow(42))
                .unwrap(),
            vec![(SOURCE_TYPE_WINDOW, "42".to_owned())]
        );

        assert!(
            validate_picker_selection(SOURCE_TYPE_MONITOR, PickerOutcome::SelectedWindow(42))
                .is_err()
        );
        assert!(
            validate_picker_selection(
                SOURCE_TYPE_WINDOW,
                PickerOutcome::SelectedOutput("eDP-1".to_owned())
            )
            .is_err()
        );
        assert!(
            validate_picker_selection(
                SOURCE_TYPE_MONITOR,
                PickerOutcome::SelectedOutput(String::new())
            )
            .is_err()
        );
        assert!(
            validate_picker_selection(SOURCE_TYPE_WINDOW, PickerOutcome::SelectedWindow(0))
                .is_err()
        );
        assert!(
            validate_picker_selection(
                SOURCE_TYPE_MONITOR,
                PickerOutcome::Selected(SelectionRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1
                })
            )
            .is_err()
        );
        assert!(validate_picker_selection(SOURCE_TYPE_MONITOR, PickerOutcome::Cancelled).is_err());
    }

    #[test]
    fn parses_screencast_source_types() {
        let mut options = HashMap::new();
        assert_eq!(
            source_types_from_options(&options).unwrap(),
            SOURCE_TYPE_MONITOR
        );

        options.insert("types".to_owned(), owned(SOURCE_TYPE_WINDOW));
        assert_eq!(
            source_types_from_options(&options).unwrap(),
            SOURCE_TYPE_WINDOW
        );

        options.insert("types".to_owned(), owned(SOURCE_TYPE_ALL));
        assert_eq!(
            source_types_from_options(&options).unwrap(),
            SOURCE_TYPE_ALL
        );

        options.insert("types".to_owned(), owned(0u32));
        assert!(source_types_from_options(&options).is_err());

        options.insert("types".to_owned(), owned(8u32));
        assert!(source_types_from_options(&options).is_err());

        options.insert("types".to_owned(), owned("monitor"));
        assert!(source_types_from_options(&options).is_err());
    }

    #[test]
    fn picker_worker_is_killed_when_the_request_is_cancelled() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;

        let dir =
            std::env::temp_dir().join(format!("loom-portal-picker-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("sleepy");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let outcome = run_picker_mode_cancellable(&script, PickerMode::Output, Some(cancelled))
            .expect("picker run");
        canceller.join().unwrap();
        assert_eq!(outcome, PickerOutcome::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancellation took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn picks_a_window_through_niri_ipc() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir =
            std::env::temp_dir().join(format!("loom-portal-pick-window-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("niri.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: niri_ipc::Request = serde_json::from_str(line.trim()).unwrap();
            assert!(matches!(request, niri_ipc::Request::PickWindow));
            let window = serde_json::json!({
                "id": 42,
                "title": "picked",
                "app_id": "app",
                "pid": 1,
                "workspace_id": 1,
                "is_focused": true,
                "is_floating": false,
                "is_sticky": false,
                "is_urgent": false,
                "is_minimized": false,
                "layout": {
                    "pos_in_scrolling_layout": [1, 1],
                    "tile_size": [100.0, 100.0],
                    "window_size": [100, 100],
                    "tile_pos_in_workspace_view": [0.0, 0.0],
                    "window_offset_in_tile": [0.0, 0.0]
                },
                "focus_timestamp": null
            });
            let response = serde_json::json!({ "Ok": { "PickedWindow": window } });
            let mut stream = stream;
            stream
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        });

        assert_eq!(pick_window_via_niri(&path).unwrap(), Some(42));
        server.join().unwrap();
    }

    #[test]
    fn cancelled_window_pick_is_reported_as_none() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!(
            "loom-portal-pick-window-cancel-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("niri.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let response = serde_json::json!({ "Ok": { "PickedWindow": null } });
            let mut stream = stream;
            stream
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        });

        assert_eq!(pick_window_via_niri(&path).unwrap(), None);
        server.join().unwrap();
    }

    #[test]
    fn encodes_absolute_paths_only() {
        assert!(file_uri(&PathBuf::from("/")).is_ok());
        assert!(file_uri(&PathBuf::from("")).is_err());
    }
}
