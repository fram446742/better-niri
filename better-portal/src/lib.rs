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

pub const RESPONSE_SUCCESS: u32 = 0;
pub const RESPONSE_CANCELLED: u32 = 1;
pub const RESPONSE_OTHER: u32 = 2;

const NIRI_BUS: &str = "org.gnome.Shell.Screenshot";
const NIRI_PATH: &str = "/org/gnome/Shell/Screenshot";

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

async fn interactive_screenshot(cancelled: Arc<AtomicBool>) -> Result<String, Failure> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(Failure::Cancelled);
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
    fn encodes_absolute_paths_only() {
        assert!(file_uri(&PathBuf::from("/")).is_ok());
        assert!(file_uri(&PathBuf::from("")).is_err());
    }
}
