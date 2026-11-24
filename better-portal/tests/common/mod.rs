//! Shared harness for the D-Bus integration tests.
//!
//! Every test gets its own `dbus-daemon` session bus, a fake niri backend on
//! that bus, and a `better-portal` process pointing at it. Tests are skipped
//! (not failed) when `dbus-daemon` is not installed.

#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_io::Timer;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy, fdo, interface};

pub const PORTAL_BUS: &str = "org.freedesktop.impl.portal.desktop.better";
pub const NIRI_PORTAL_BUS: &str = "org.freedesktop.impl.portal.desktop.niri";
pub const NIRI_SCREENSHOT_BUS: &str = "org.gnome.Shell.Screenshot";
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
pub const NIRI_PATH: &str = "/org/gnome/Shell/Screenshot";

pub const SCREENSHOT_INTERFACE: &str = "org.freedesktop.impl.portal.Screenshot";
pub const SCREENCAST_INTERFACE: &str = "org.freedesktop.impl.portal.ScreenCast";
pub const SESSION_INTERFACE: &str = "org.freedesktop.impl.portal.Session";
pub const REQUEST_INTERFACE: &str = "org.freedesktop.impl.portal.Request";

pub const RESPONSE_SUCCESS: u32 = 0;
pub const RESPONSE_CANCELLED: u32 = 1;
pub const RESPONSE_OTHER: u32 = 2;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub fn owned<T>(value: T) -> OwnedValue
where
    T: Into<Value<'static>>,
{
    OwnedValue::try_from(value.into()).expect("owned value")
}

pub fn options(types: Option<u32>) -> HashMap<String, OwnedValue> {
    let mut options = HashMap::new();
    if let Some(types) = types {
        options.insert("types".to_owned(), owned(types));
    }
    options
}

pub fn path(value: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(value).expect("object path")
}

pub fn unique_name(prefix: &str) -> String {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("/org/freedesktop/portal/desktop/{prefix}/{id}")
}

pub async fn with_timeout<F: Future>(future: F, seconds: u64) -> F::Output {
    futures_util::pin_mut!(future);
    match futures_util::future::select(future, Timer::after(Duration::from_secs(seconds))).await {
        futures_util::future::Either::Left((output, _)) => output,
        futures_util::future::Either::Right(_) => panic!("timed out after {seconds}s"),
    }
}

pub async fn connect(address: &str) -> Connection {
    zbus::connection::Builder::address(address)
        .expect("valid bus address")
        .build()
        .await
        .expect("connect to private bus")
}

pub async fn portal_proxy(connection: &Connection, interface: &'static str) -> Proxy<'static> {
    Proxy::new_owned(connection.clone(), PORTAL_BUS, OBJECT_PATH, interface)
        .await
        .expect("portal proxy")
}

pub async fn session_proxy(connection: &Connection, session: &OwnedObjectPath) -> Proxy<'static> {
    Proxy::new_owned(
        connection.clone(),
        PORTAL_BUS,
        session.as_str().to_owned(),
        SESSION_INTERFACE,
    )
    .await
    .expect("session proxy")
}

pub async fn request_proxy(connection: &Connection, handle: &OwnedObjectPath) -> Proxy<'static> {
    Proxy::new_owned(
        connection.clone(),
        PORTAL_BUS,
        handle.as_str().to_owned(),
        REQUEST_INTERFACE,
    )
    .await
    .expect("request proxy")
}

/// Writes an executable shell script and returns its path.
pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

pub fn temp_dir(prefix: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "better-portal-test-{prefix}-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Private session bus
// ---------------------------------------------------------------------------

pub struct TestBus {
    child: Child,
    pub address: String,
}

impl TestBus {
    /// Starts a private session bus, or returns `None` when `dbus-daemon` is
    /// unavailable (the caller should then skip the test).
    pub fn start() -> Option<Self> {
        let probe = Command::new("dbus-daemon").arg("--version").output();
        if probe.is_err() {
            eprintln!("skipping test: dbus-daemon is not installed");
            return None;
        }

        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address", "--nopidfile"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start dbus-daemon");
        let stdout = child.stdout.take().expect("dbus-daemon stdout");
        let mut address = String::new();
        BufReader::new(stdout)
            .read_line(&mut address)
            .expect("read bus address");
        let address = address.trim().to_owned();
        if address.is_empty() {
            eprintln!("skipping test: dbus-daemon printed no address");
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(Self { child, address })
    }
}

impl Drop for TestBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// better-portal process
// ---------------------------------------------------------------------------

pub struct Portal {
    child: Child,
    log: PathBuf,
}

impl Portal {
    pub async fn start(bus: &TestBus, picker: Option<&Path>) -> Self {
        let exe = env!("CARGO_BIN_EXE_better-portal");
        let log = std::env::temp_dir().join(format!(
            "better-portal-daemon-{}-{}.log",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let stderr = std::fs::File::create(&log).unwrap();
        let no_picker = PathBuf::from("/nonexistent/better-portal-picker");
        let mut child = Command::new(exe)
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .env(
                "BETTER_PORTAL_PICKER",
                picker.map(Path::to_path_buf).unwrap_or(no_picker),
            )
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("WAYLAND_SOCKET")
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("spawn better-portal");

        let log_text = |log: &Path| std::fs::read_to_string(log).unwrap_or_default();
        let mut waited = Duration::ZERO;
        loop {
            if let Some(status) = child.try_wait().expect("poll better-portal") {
                panic!(
                    "better-portal exited early with {status}:\n{}",
                    log_text(&log)
                );
            }
            if name_has_owner(&bus.address, PORTAL_BUS).await {
                break;
            }
            if waited > Duration::from_secs(15) {
                let text = log_text(&log);
                let _ = child.kill();
                let _ = child.wait();
                panic!("better-portal did not claim {PORTAL_BUS}:\n{text}");
            }
            Timer::after(Duration::from_millis(20)).await;
            waited += Duration::from_millis(20);
        }

        Self { child, log }
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.log);
    }
}

async fn name_has_owner(address: &str, name: &str) -> bool {
    let Ok(connection) = zbus::connection::Builder::address(address)
        .expect("valid address")
        .build()
        .await
    else {
        return false;
    };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&connection).await else {
        return false;
    };
    let Ok(bus_name) = zbus::names::BusName::try_from(name.to_owned()) else {
        return false;
    };
    dbus.get_name_owner(bus_name).await.is_ok()
}

// ---------------------------------------------------------------------------
// Fake niri backend
// ---------------------------------------------------------------------------

/// One recorded `SetSelection` call: session, sources, persist mode.
pub type SetSelectionCall = (OwnedObjectPath, Vec<(u32, String)>, u32);

pub struct FakeNiri {
    pub connection: Connection,
    pub state: Arc<Mutex<FakeState>>,
}

pub struct FakeState {
    pub calls: Vec<String>,
    pub create_session_response: u32,
    pub select_sources_response: u32,
    pub start_response: u32,
    pub selected_types: Vec<u32>,
    pub set_selection: Vec<SetSelectionCall>,
    pub clear_selection: Vec<OwnedObjectPath>,
    pub session_close_calls: usize,
    pub screenshot_path: PathBuf,
    pub screenshot_error: Option<String>,
    pub area_calls: Vec<(i32, i32, i32, i32)>,
    pub cancel_calls: usize,
    /// A pending `InteractiveScreenshot` waits on this; `CancelScreenshot`
    /// sends the cancellation.
    pub interactive_cancel_tx: async_channel::Sender<()>,
    pub interactive_cancel_rx: async_channel::Receiver<()>,
    pub pick_color: Option<(f64, f64, f64)>,
    pub pick_color_error: Option<String>,
    pub pick_window: Option<u64>,
}

impl Default for FakeState {
    fn default() -> Self {
        let (interactive_cancel_tx, interactive_cancel_rx) = async_channel::unbounded();
        Self {
            calls: Vec::new(),
            create_session_response: RESPONSE_SUCCESS,
            select_sources_response: RESPONSE_SUCCESS,
            start_response: RESPONSE_SUCCESS,
            selected_types: Vec::new(),
            set_selection: Vec::new(),
            clear_selection: Vec::new(),
            session_close_calls: 0,
            screenshot_path: PathBuf::from("/tmp/better-portal-fake-screenshot.png"),
            screenshot_error: None,
            area_calls: Vec::new(),
            cancel_calls: 0,
            interactive_cancel_tx,
            interactive_cancel_rx,
            pick_color: Some((0.25, 0.5, 0.75)),
            pick_color_error: None,
            pick_window: Some(42),
        }
    }
}

impl FakeNiri {
    pub async fn start(bus: &TestBus) -> Self {
        let connection = connect(&bus.address).await;
        let state = Arc::new(Mutex::new(FakeState::default()));

        connection
            .object_server()
            .at(
                OBJECT_PATH,
                FakeScreenCast {
                    state: state.clone(),
                    connection: connection.clone(),
                },
            )
            .await
            .unwrap();
        connection
            .object_server()
            .at(
                OBJECT_PATH,
                FakeSelection {
                    state: state.clone(),
                },
            )
            .await
            .unwrap();
        connection
            .object_server()
            .at(
                NIRI_PATH,
                FakeShellScreenshot {
                    state: state.clone(),
                },
            )
            .await
            .unwrap();

        connection.request_name(NIRI_PORTAL_BUS).await.unwrap();
        connection.request_name(NIRI_SCREENSHOT_BUS).await.unwrap();

        Self { connection, state }
    }

    pub fn calls(&self) -> Vec<String> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn session_close_calls(&self) -> usize {
        self.state.lock().unwrap().session_close_calls
    }

    /// Emits a compositor-initiated `Closed` on a session object.
    pub async fn close_session(&self, session: &OwnedObjectPath) {
        let emitter = SignalEmitter::new(&self.connection, session.as_str()).unwrap();
        FakeSession::closed(&emitter).await.unwrap();
    }
}

struct FakeScreenCast {
    state: Arc<Mutex<FakeState>>,
    connection: Connection,
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl FakeScreenCast {
    async fn create_session(
        &self,
        _handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let response = {
            let mut state = self.state.lock().unwrap();
            state.calls.push("CreateSession".to_owned());
            state.create_session_response
        };
        if response != RESPONSE_SUCCESS {
            return Ok((response, HashMap::new()));
        }
        let _ = self
            .connection
            .object_server()
            .at(
                session_handle,
                FakeSession {
                    state: self.state.clone(),
                },
            )
            .await;
        let mut results = HashMap::new();
        results.insert("session_id".to_owned(), owned("niri-1".to_owned()));
        Ok((response, results))
    }

    async fn select_sources(
        &self,
        _handle: OwnedObjectPath,
        _session_handle: OwnedObjectPath,
        _app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let mut state = self.state.lock().unwrap();
        state.calls.push("SelectSources".to_owned());
        let types = options
            .get("types")
            .and_then(|value| value.downcast_ref::<u32>().ok())
            .unwrap_or(1);
        state.selected_types.push(types);
        Ok((state.select_sources_response, HashMap::new()))
    }

    async fn start(
        &self,
        _handle: OwnedObjectPath,
        _session_handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let response = {
            let mut state = self.state.lock().unwrap();
            state.calls.push("Start".to_owned());
            state.start_response
        };
        let mut results = HashMap::new();
        if response == RESPONSE_SUCCESS {
            let stream: HashMap<String, OwnedValue> = HashMap::new();
            let streams = vec![(7u32, stream)];
            results.insert(
                "streams".to_owned(),
                OwnedValue::try_from(Value::from(streams)).unwrap(),
            );
        }
        Ok((response, results))
    }
}

struct FakeSelection {
    state: Arc<Mutex<FakeState>>,
}

#[interface(name = "org.better.portal.ScreenCastSelection")]
impl FakeSelection {
    async fn set_selection(
        &self,
        session_handle: OwnedObjectPath,
        sources: Vec<(u32, String)>,
        persist_mode: u32,
    ) -> fdo::Result<()> {
        self.state
            .lock()
            .unwrap()
            .set_selection
            .push((session_handle, sources, persist_mode));
        Ok(())
    }

    async fn clear_selection(&self, session_handle: OwnedObjectPath) -> fdo::Result<()> {
        self.state
            .lock()
            .unwrap()
            .clear_selection
            .push(session_handle);
        Ok(())
    }
}

struct FakeSession {
    state: Arc<Mutex<FakeState>>,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl FakeSession {
    async fn close(&self) -> fdo::Result<()> {
        self.state.lock().unwrap().session_close_calls += 1;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct FakeShellScreenshot {
    state: Arc<Mutex<FakeState>>,
}

#[interface(name = "org.gnome.Shell.Screenshot")]
impl FakeShellScreenshot {
    async fn screenshot(
        &self,
        _include_cursor: bool,
        _flash: bool,
        _filename: String,
    ) -> fdo::Result<(bool, String)> {
        let state = self.state.lock().unwrap();
        if let Some(error) = &state.screenshot_error {
            return Err(fdo::Error::Failed(error.clone()));
        }
        Ok((true, state.screenshot_path.display().to_string()))
    }

    async fn interactive_screenshot(&self) -> fdo::Result<(bool, String)> {
        self.state
            .lock()
            .unwrap()
            .calls
            .push("InteractiveScreenshot".to_owned());
        let rx = self.state.lock().unwrap().interactive_cancel_rx.clone();
        let _ = rx.recv().await;
        Ok((false, String::new()))
    }

    async fn screenshot_area(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        _flash: bool,
        _filename: String,
    ) -> fdo::Result<(bool, String)> {
        let mut state = self.state.lock().unwrap();
        state.calls.push("ScreenshotArea".to_owned());
        state.area_calls.push((x, y, width, height));
        Ok((true, state.screenshot_path.display().to_string()))
    }

    async fn cancel_screenshot(&self) -> fdo::Result<()> {
        let mut state = self.state.lock().unwrap();
        state.calls.push("CancelScreenshot".to_owned());
        let _ = state.interactive_cancel_tx.try_send(());
        Ok(())
    }

    async fn pick_color(&self) -> fdo::Result<HashMap<String, OwnedValue>> {
        let state = self.state.lock().unwrap();
        if let Some(error) = &state.pick_color_error {
            return Err(fdo::Error::Failed(error.clone()));
        }
        let (r, g, b) = state.pick_color.expect("fake color");
        let mut results = HashMap::new();
        results.insert("color".to_owned(), owned((r, g, b)));
        Ok(results)
    }
}
