//! `better-portal`: a standalone portal backend for screenshots and colour
//! picking.
//!
//! The compositor already serves GNOME-Shell-style screenshot and colour-picker
//! D-Bus interfaces. This daemon exposes the standard
//! `org.freedesktop.impl.portal.Screenshot` backend on its own bus name and
//! delegates capture to those compositor interfaces, so the portal stack can
//! run against a separate process.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value as JsonValue;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};
use zbus::{fdo, interface, ObjectServer, Proxy};

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.better";
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
pub const SCREENSHOT_INTERFACE: &str = "org.freedesktop.impl.portal.Screenshot";

/// A logical, global screenshot selection returned by `better-portal-picker`.
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

const SOURCE_TYPE_MONITOR: u32 = 1;

pub const RESPONSE_SUCCESS: u32 = 0;
pub const RESPONSE_CANCELLED: u32 = 1;
pub const RESPONSE_OTHER: u32 = 2;

const NIRI_BUS: &str = "org.gnome.Shell.Screenshot";
const NIRI_PATH: &str = "/org/gnome/Shell/Screenshot";

const NIRI_PORTAL_BUS: &str = "org.freedesktop.impl.portal.desktop.niri";
const NIRI_PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

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

async fn niri_proxy() -> Result<Proxy<'static>, String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|err| format!("session bus: {err}"))?;
    Proxy::new_owned(connection, NIRI_BUS, NIRI_PATH, NIRI_BUS)
        .await
        .map_err(|err| format!("screenshot proxy: {err}"))
}

async fn niri_cancel() {
    if let Ok(proxy) = niri_proxy().await {
        let _: Result<(), _> = proxy.call("CancelScreenshot", &()).await;
    }
}

async fn fullscreen_screenshot() -> Result<String, Failure> {
    let proxy = niri_proxy().await.map_err(Failure::Other)?;
    let (ok, path): (bool, std::path::PathBuf) = proxy
        .call("Screenshot", &(false, false, String::new()))
        .await
        .map_err(|err| Failure::Other(format!("niri Screenshot: {err}")))?;
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
    Cancelled,
    Unavailable,
}

/// Resolves `better-portal-picker`: `$BETTER_PORTAL_PICKER`, then a sibling of
/// this executable, then `$PATH`.
pub fn picker_program() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("BETTER_PORTAL_PICKER") {
        return Some(std::path::PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("better-portal-picker");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("better-portal-picker"))
        .find(|candidate| candidate.is_file())
}

fn run_picker(program: &std::path::Path) -> Result<PickerOutcome, String> {
    run_picker_mode(program, false)
}

fn run_picker_mode(program: &std::path::Path, screencast: bool) -> Result<PickerOutcome, String> {
    use std::process::Command;

    let mut command = Command::new(program);
    if screencast {
        command.arg("--screencast");
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PickerOutcome::Unavailable)
        }
        Err(err) => return Err(format!("cannot run {}: {err}", program.display())),
    };
    if !output.status.success() {
        return Ok(PickerOutcome::Cancelled);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if screencast {
        #[derive(serde::Deserialize)]
        struct Output {
            output: String,
        }
        let output: Output = serde_json::from_str(stdout.trim())
            .map_err(|err| format!("invalid screencast picker output: {err}"))?;
        return Ok(PickerOutcome::SelectedOutput(output.output));
    }
    let rect: SelectionRect = serde_json::from_str(stdout.trim())
        .map_err(|err| format!("invalid picker output: {err}"))?;
    Ok(PickerOutcome::Selected(rect))
}

async fn set_screen_cast_selection(
    session_handle: &OwnedObjectPath,
    output: &str,
) -> Result<(), String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|err| format!("session bus: {err}"))?;
    let proxy = Proxy::new_owned(
        connection,
        NIRI_PORTAL_BUS,
        NIRI_PORTAL_PATH,
        "org.better.portal.ScreenCastSelection",
    )
    .await
    .map_err(|err| format!("niri selection proxy: {err}"))?;
    let _: () = proxy
        .call(
            "SetSelection",
            &(
                session_handle.clone(),
                vec![(SOURCE_TYPE_MONITOR, output.to_owned())],
                0u32,
            ),
        )
        .await
        .map_err(|err| format!("niri SetSelection: {err}"))?;
    Ok(())
}

async fn area_screenshot(rect: SelectionRect) -> Result<String, Failure> {
    let proxy = niri_proxy().await.map_err(Failure::Other)?;
    let (ok, path): (bool, std::path::PathBuf) = proxy
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
        .map_err(|err| Failure::Other(format!("niri ScreenshotArea: {err}")))?;
    if !ok {
        return Err(Failure::Cancelled);
    }
    file_uri(&path).map_err(Failure::Other)
}

async fn interactive_screenshot(cancelled: Arc<AtomicBool>) -> Result<String, Failure> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(Failure::Cancelled);
    }

    // Prefer the external layer-shell picker when it is installed. Anything
    // other than a clean selection falls back to the compositor's own UI.
    if let Some(program) = picker_program() {
        match run_picker(&program) {
            Ok(PickerOutcome::Selected(rect)) => return area_screenshot(rect).await,
            Ok(PickerOutcome::Cancelled) => return Err(Failure::Cancelled),
            Ok(PickerOutcome::Unavailable) | Ok(PickerOutcome::SelectedOutput(_)) | Err(_) => (),
        }
    }

    let proxy = niri_proxy().await.map_err(Failure::Other)?;
    let (ok, uri): (bool, String) = proxy
        .call("InteractiveScreenshot", &())
        .await
        .map_err(|err| Failure::Other(format!("niri InteractiveScreenshot: {err}")))?;
    if cancelled.load(Ordering::SeqCst) || !ok || uri.is_empty() {
        return Err(Failure::Cancelled);
    }
    Ok(uri)
}

async fn pick_color_from_niri() -> Result<(f64, f64, f64), Failure> {
    let proxy = niri_proxy().await.map_err(Failure::Other)?;
    let result: HashMap<String, OwnedValue> = proxy
        .call("PickColor", &())
        .await
        .map_err(|err| Failure::Other(format!("niri PickColor: {err}")))?;
    let color = result
        .get("color")
        .ok_or_else(|| Failure::Other("niri returned no colour".to_owned()))?;
    let json: JsonValue = serde_json::to_value(color)
        .map_err(|err| Failure::Other(format!("colour encoding: {err}")))?;
    let rgb = json
        .as_array()
        .filter(|values| values.len() >= 3)
        .and_then(|values| {
            Some((
                values[0].as_f64()?,
                values[1].as_f64()?,
                values[2].as_f64()?,
            ))
        })
        .ok_or_else(|| Failure::Other(format!("unexpected colour shape: {json}")))?;
    Ok(rgb)
}

/// Cancellable request object the portal calls to close an in-flight request.
pub struct PortalRequest {
    cancelled: Arc<AtomicBool>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl PortalRequest {
    async fn close(&self) -> fdo::Result<()> {
        self.cancelled.store(true, Ordering::SeqCst);
        niri_cancel().await;
        Ok(())
    }
}

pub struct ScreenshotBackend;

impl ScreenshotBackend {
    async fn register_request(
        server: &ObjectServer,
        handle: ObjectPath<'_>,
        cancelled: Arc<AtomicBool>,
    ) -> OwnedObjectPath {
        let path = OwnedObjectPath::from(handle.clone());
        let request = PortalRequest { cancelled };
        if let Err(err) = server.at(path.clone(), request).await {
            eprintln!(
                "better-portal: cannot register request {}: {err}",
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
        let path = Self::register_request(server, handle, cancelled.clone()).await;

        let result = if interactive {
            interactive_screenshot(cancelled).await
        } else {
            fullscreen_screenshot().await
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
        let path = Self::register_request(server, handle, cancelled.clone()).await;
        let result = pick_color_from_niri().await;
        Self::unregister_request(server, &path).await;
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
        assert_eq!(ScreenCastBackend.version(), 4);
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
            std::env::temp_dir().join(format!("better-portal-picker-test-{}", std::process::id()));
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

        assert_eq!(
            run_picker(&dir.join("missing")).unwrap(),
            PickerOutcome::Unavailable
        );
    }

    #[test]
    fn screencast_picker_returns_an_output_name() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "better-portal-screencast-picker-{}",
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
            run_picker_mode(&script, true).unwrap(),
            PickerOutcome::SelectedOutput("eDP-1".to_owned())
        );
    }

    #[test]
    fn encodes_absolute_paths_only() {
        assert!(file_uri(&PathBuf::from("/")).is_ok());
        assert!(file_uri(&PathBuf::from("")).is_err());
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
pub struct ScreenCastBackend;

async fn portal_proxy(interface: &'static str) -> Result<Proxy<'static>, String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|err| format!("session bus: {err}"))?;
    Proxy::new_owned(connection, NIRI_PORTAL_BUS, NIRI_PORTAL_PATH, interface)
        .await
        .map_err(|err| format!("niri portal proxy: {err}"))
}

async fn forward_close(path: &OwnedObjectPath, interface: &'static str) {
    let Ok(connection) = zbus::Connection::session().await else {
        return;
    };
    let Ok(proxy) = Proxy::new_owned(
        connection,
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

/// Session object re-exported on the better-portal bus.
struct ProxySession {
    path: OwnedObjectPath,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl ProxySession {
    async fn close(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        forward_close(&self.path, "org.freedesktop.impl.portal.Session").await;
        let _ = server.remove::<ProxySession, _>(&self.path).await;
        let _ = Self::closed(&emitter).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Request object re-exported on the better-portal bus.
struct ProxyRequest {
    path: OwnedObjectPath,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl ProxyRequest {
    async fn close(&self) -> fdo::Result<()> {
        forward_close(&self.path, "org.freedesktop.impl.portal.Request").await;
        Ok(())
    }
}

async fn export_session(server: &ObjectServer, path: OwnedObjectPath) {
    let session = ProxySession { path: path.clone() };
    if let Err(err) = server.at(path.clone(), session).await {
        eprintln!(
            "better-portal: cannot export session {}: {err}",
            path.as_str()
        );
    }
}

async fn unexport_session(server: &ObjectServer, path: &OwnedObjectPath) {
    let _ = server.remove::<ProxySession, _>(path).await;
}

async fn export_request(server: &ObjectServer, path: OwnedObjectPath) {
    let request = ProxyRequest { path: path.clone() };
    if let Err(err) = server.at(path.clone(), request).await {
        eprintln!(
            "better-portal: cannot export request {}: {err}",
            path.as_str()
        );
    }
}

async fn unexport_request(server: &ObjectServer, path: &OwnedObjectPath) {
    let _ = server.remove::<ProxyRequest, _>(path).await;
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastBackend {
    async fn create_session(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        export_session(server, session_handle.clone()).await;
        let result = async {
            let proxy = portal_proxy("org.freedesktop.impl.portal.ScreenCast")
                .await
                .map_err(fdo::Error::Failed)?;
            proxy
                .call(
                    "CreateSession",
                    &(handle, session_handle.clone(), app_id, options),
                )
                .await
                .map_err(|err| fdo::Error::Failed(format!("niri CreateSession: {err}")))
        }
        .await;
        if !matches!(&result, Ok((RESPONSE_SUCCESS, _))) {
            unexport_session(server, &session_handle).await;
        }
        result
    }

    async fn select_sources(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        export_request(server, handle.clone()).await;
        let result = async {
            let proxy = portal_proxy("org.freedesktop.impl.portal.ScreenCast")
                .await
                .map_err(fdo::Error::Failed)?;
            proxy
                .call(
                    "SelectSources",
                    &(handle.clone(), session_handle, app_id, options),
                )
                .await
                .map_err(|err| fdo::Error::Failed(format!("niri SelectSources: {err}")))
        }
        .await;
        unexport_request(server, &handle).await;
        result
    }

    async fn start(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        export_request(server, handle.clone()).await;

        // Prefer the external picker when available. If the compositor does not
        // implement the selection API, fall back to its own picker.
        if let Some(program) = picker_program() {
            match run_picker_mode(&program, true) {
                Ok(PickerOutcome::SelectedOutput(output)) => {
                    if let Err(err) = set_screen_cast_selection(&session_handle, &output).await {
                        eprintln!("better-portal: external ScreenCast selection failed: {err}");
                    }
                }
                Ok(PickerOutcome::Cancelled) => {
                    unexport_request(server, &handle).await;
                    return Ok((RESPONSE_CANCELLED, HashMap::new()));
                }
                Ok(PickerOutcome::Unavailable) | Err(_) => (),
                Ok(PickerOutcome::Selected(_)) => (),
            }
        }

        let result = async {
            let proxy = portal_proxy("org.freedesktop.impl.portal.ScreenCast")
                .await
                .map_err(fdo::Error::Failed)?;
            proxy
                .call(
                    "Start",
                    &(
                        handle.clone(),
                        session_handle,
                        app_id,
                        parent_window,
                        options,
                    ),
                )
                .await
                .map_err(|err| fdo::Error::Failed(format!("niri Start: {err}")))
        }
        .await;
        unexport_request(server, &handle).await;
        result
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }
}
