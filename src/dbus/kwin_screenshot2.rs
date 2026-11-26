use std::collections::HashMap;
use std::io::Write as _;
use std::os::fd::{AsRawFd, OwnedFd};

use log::warn;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{fdo, interface};

use super::Start;
use super::kwin_screenshot::portal_error;
use crate::ui::screenshot_ui::ScreenshotPortalError;
use crate::window::mapped::MappedId;

/// QImage::Format_RGBA8888.
const QIMAGE_FORMAT_RGBA8888: u32 = 17;

/// Reply from the compositor with the encoded image and the metadata that only the compositor
/// knows (screen scale and names).
pub struct CapturedScreenshot {
    pub png: std::sync::Arc<[u8]>,
    pub scale: f64,
    pub screen: Option<String>,
    pub window_id: Option<String>,
}

pub type Screenshot2ReplySender =
    async_channel::Sender<Result<CapturedScreenshot, ScreenshotPortalError>>;

/// What to capture. Resolved against the live compositor state in the niri event loop.
pub enum CaptureTarget {
    /// All outputs united into one image.
    Workspace,
    /// The output the user is currently working on.
    ActiveScreen,
    /// A named output.
    Output(String),
    /// The focused window.
    ActiveWindow,
    /// A specific window, used after an interactive pick.
    Window(MappedId),
    /// A global-logical rectangle contained by a single output.
    Area {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
}

pub enum KwinScreenshot2ToNiri {
    Capture {
        target: CaptureTarget,
        include_cursor: bool,
        reply: Screenshot2ReplySender,
    },
    PickWindow(async_channel::Sender<Option<MappedId>>),
    PickOutput(async_channel::Sender<Option<String>>),
}

/// KDE KWin fd-based screenshot interface (`org.kde.KWin.ScreenShot2`).
///
/// Spectacle prefers this interface over the older `org.kde.KWin.Screenshot` one because it
/// receives raw pixels through a pipe instead of a saved file. We capture through niri's existing
/// screenshot pipeline, but ask it for the encoded PNG in memory (the same reply path the IPC
/// `screenshot` request uses), decode it and hand the client raw RGBA rows.
pub struct KwinScreenshot2 {
    to_niri: calloop::channel::Sender<KwinScreenshot2ToNiri>,
}

impl KwinScreenshot2 {
    pub fn new(to_niri: calloop::channel::Sender<KwinScreenshot2ToNiri>) -> Self {
        Self { to_niri }
    }

    fn send(&self, msg: KwinScreenshot2ToNiri) -> fdo::Result<()> {
        self.to_niri
            .send(msg)
            .map_err(|err| fdo::Error::Failed(format!("error sending message to niri: {err}")))
    }

    async fn capture(
        &self,
        target: CaptureTarget,
        options: &HashMap<String, OwnedValue>,
        pipe: zbus::zvariant::OwnedFd,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        // Decorations and shadows are part of the window in niri and captures are always at native
        // resolution, so the other KWin options need no handling.
        let include_cursor = options
            .get("include-cursor")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or_default();

        let (reply, receiver) = async_channel::bounded(1);
        self.send(KwinScreenshot2ToNiri::Capture {
            target,
            include_cursor,
            reply,
        })?;

        let captured = receiver
            .recv()
            .await
            .map_err(|err| fdo::Error::Failed(format!("screenshot reply channel closed: {err}")))?
            .map_err(portal_error)?;

        let (width, height, pixels) = decode_png(&captured.png)?;
        let results = image_metadata(
            width,
            height,
            captured.scale,
            captured.screen.as_deref(),
            captured.window_id.as_deref(),
        );
        write_image_to_pipe(pipe.into(), pixels);
        Ok(results)
    }
}

/// https://github.com/KDE/kwin/blob/b3d8b7085a5186744807300e122f2ef687e943fe/src/plugins/screenshot/org.kde.KWin.ScreenShot2.xml
#[interface(name = "org.kde.KWin.ScreenShot2")]
impl KwinScreenshot2 {
    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }

    #[zbus(name = "CaptureWorkspace")]
    async fn capture_workspace(
        &self,
        options: HashMap<String, OwnedValue>,
        pipe: zbus::zvariant::OwnedFd,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        self.capture(CaptureTarget::Workspace, &options, pipe).await
    }

    #[zbus(name = "CaptureActiveScreen")]
    async fn capture_active_screen(
        &self,
        options: HashMap<String, OwnedValue>,
        pipe: zbus::zvariant::OwnedFd,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        self.capture(CaptureTarget::ActiveScreen, &options, pipe)
            .await
    }

    #[zbus(name = "CaptureScreen")]
    async fn capture_screen(
        &self,
        name: String,
        options: HashMap<String, OwnedValue>,
        pipe: zbus::zvariant::OwnedFd,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        self.capture(CaptureTarget::Output(name), &options, pipe)
            .await
    }

    #[zbus(name = "CaptureActiveWindow")]
    async fn capture_active_window(
        &self,
        options: HashMap<String, OwnedValue>,
        pipe: zbus::zvariant::OwnedFd,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        self.capture(CaptureTarget::ActiveWindow, &options, pipe)
            .await
    }

    #[zbus(name = "CaptureArea")]
    async fn capture_area(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        options: HashMap<String, OwnedValue>,
        pipe: zbus::zvariant::OwnedFd,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        self.capture(
            CaptureTarget::Area {
                x,
                y,
                width,
                height,
            },
            &options,
            pipe,
        )
        .await
    }

    #[zbus(name = "CaptureInteractive")]
    async fn capture_interactive(
        &self,
        kind: u32,
        options: HashMap<String, OwnedValue>,
        pipe: zbus::zvariant::OwnedFd,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        match kind {
            // KWin's kinds: 0 asks for a window, 1 for a screen.
            0 => {
                let (reply, receiver) = async_channel::bounded(1);
                self.send(KwinScreenshot2ToNiri::PickWindow(reply))?;
                let Some(window) = receiver.recv().await.map_err(|err| {
                    fdo::Error::Failed(format!("compositor failed to pick a window: {err}"))
                })?
                else {
                    return Err(fdo::Error::Failed(String::from(
                        "screenshot selection was cancelled",
                    )));
                };
                self.capture(CaptureTarget::Window(window), &options, pipe)
                    .await
            }
            1 => {
                let (reply, receiver) = async_channel::bounded(1);
                self.send(KwinScreenshot2ToNiri::PickOutput(reply))?;
                let Some(output) = receiver.recv().await.map_err(|err| {
                    fdo::Error::Failed(format!("compositor failed to pick an output: {err}"))
                })?
                else {
                    return Err(fdo::Error::Failed(String::from(
                        "screenshot selection was cancelled",
                    )));
                };
                self.capture(CaptureTarget::Output(output), &options, pipe)
                    .await
            }
            _ => Err(fdo::Error::Failed(format!(
                "unsupported interactive screenshot kind {kind}"
            ))),
        }
    }
}

fn image_metadata(
    width: u32,
    height: u32,
    scale: f64,
    screen: Option<&str>,
    window_id: Option<&str>,
) -> HashMap<String, OwnedValue> {
    let mut results = HashMap::new();
    let mut insert = |key: &str, value: Value<'_>| {
        results.insert(key.to_owned(), OwnedValue::try_from(value).unwrap());
    };
    insert("type", Value::from("raw"));
    insert("width", Value::from(width));
    insert("height", Value::from(height));
    insert("stride", Value::from(width * 4));
    insert("scale", Value::from(scale));
    insert("format", Value::from(QIMAGE_FORMAT_RGBA8888));
    if let Some(screen) = screen {
        insert("screen", Value::from(screen));
    }
    if let Some(window_id) = window_id {
        insert("windowId", Value::from(window_id));
    }
    results
}

/// Decodes the PNG produced by the screenshot pipeline into tightly packed RGBA8888 rows.
fn decode_png(png: &[u8]) -> fdo::Result<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    // The default decoder limit is 64 MiB, which a workspace-sized screenshot can exceed.
    decoder.set_limits(png::Limits { bytes: usize::MAX });
    let mut reader = decoder
        .read_info()
        .map_err(|err| fdo::Error::Failed(format!("error decoding screenshot: {err}")))?;
    let buffer_size = reader.output_buffer_size().ok_or_else(|| {
        fdo::Error::Failed(String::from("screenshot dimensions are out of range"))
    })?;
    let mut pixels = vec![0; buffer_size];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|err| fdo::Error::Failed(format!("error decoding screenshot: {err}")))?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(fdo::Error::Failed(format!(
            "unsupported screenshot pixel format: {:?}/{:?}",
            info.color_type, info.bit_depth
        )));
    }
    pixels.truncate(info.buffer_size());
    Ok((info.width, info.height, pixels))
}

/// The client only starts reading the pipe after it receives the metadata reply, so the write
/// must not happen on the D-Bus dispatch thread: a large image would fill the pipe buffer and
/// deadlock the reply. Write errors can no longer be reported through D-Bus at that point, which
/// matches KWin's own pipe writer. Like KWin, the pipe is made non-blocking and given a minute to
/// drain so that a client which never reads cannot pin this thread forever.
fn write_image_to_pipe(fd: OwnedFd, pixels: Vec<u8>) {
    const WRITE_TIMEOUT_MS: libc::c_int = 60_000;

    std::thread::spawn(move || {
        let file = std::fs::File::from(fd);
        let raw_fd = file.as_raw_fd();

        // SAFETY: raw_fd is a valid open descriptor and fcntl only reads or updates its flags.
        unsafe {
            let flags = libc::fcntl(raw_fd, libc::F_GETFL);
            if flags == -1 || libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
                warn!(
                    "error preparing the screenshot pipe: {:?}",
                    std::io::Error::last_os_error()
                );
                return;
            }
        }

        let mut written = 0;
        while written < pixels.len() {
            match (&file).write(&pixels[written..]) {
                Ok(0) => {
                    warn!("screenshot pipe was closed before the image was written");
                    return;
                }
                Ok(count) => written += count,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => (),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut poll_fd = libc::pollfd {
                        fd: raw_fd,
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // SAFETY: poll_fd is a valid single-element pollfd array for this call.
                    let ready = unsafe { libc::poll(&mut poll_fd, 1, WRITE_TIMEOUT_MS) };
                    if ready == 0 {
                        warn!("timed out writing the screenshot to the client");
                        return;
                    }
                    if ready < 0 {
                        let err = std::io::Error::last_os_error();
                        if err.kind() != std::io::ErrorKind::Interrupted {
                            warn!("error waiting on the screenshot pipe: {err:?}");
                            return;
                        }
                    }
                }
                Err(err) => {
                    warn!("error writing screenshot to the client pipe: {err:?}");
                    return;
                }
            }
        }
    });
}

impl Start for KwinScreenshot2 {
    fn start(self, monitor: bool) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        conn.object_server().at("/org/kde/KWin/ScreenShot2", self)?;
        super::request_name(&conn, "org.kde.KWin.ScreenShot2", monitor)?;
        Ok(conn)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;
    use crate::utils::write_png_rgba8;

    fn take<T>(results: &mut HashMap<String, OwnedValue>, key: &str) -> T
    where
        T: TryFrom<OwnedValue>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(results.remove(key).unwrap()).unwrap()
    }

    #[test]
    fn private_bus_serves_raw_screenshot() {
        // The reference implementation has no tests for this interface, so exercise the whole
        // D-Bus path against a private daemon: method signature, fd transfer, metadata reply and
        // the pipe writer. Skipped when dbus-daemon is not installed.
        struct Daemon(std::process::Child);

        impl Drop for Daemon {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        struct RestoreAddress(Option<std::ffi::OsString>);

        impl Drop for RestoreAddress {
            fn drop(&mut self) {
                // SAFETY: this test owns the process-wide session address while it runs.
                unsafe {
                    match &self.0 {
                        Some(address) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address),
                        None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
                    }
                }
            }
        }

        let Ok(child) = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            eprintln!("skipping private bus test: dbus-daemon is not installed");
            return;
        };
        let mut daemon = Daemon(child);

        let address = {
            use std::io::BufRead as _;
            let stdout = daemon.0.stdout.take().unwrap();
            let mut lines = std::io::BufReader::new(stdout).lines();
            let Some(Ok(address)) = lines.next() else {
                eprintln!("skipping private bus test: dbus-daemon did not print an address");
                return;
            };
            address
        };
        let _restore = RestoreAddress(std::env::var_os("DBUS_SESSION_BUS_ADDRESS"));
        // SAFETY: the address is restored when the guard drops at the end of this test.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };

        let pixels = [
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let (to_niri, from_niri) = calloop::channel::channel();
        let mut event_loop = calloop::EventLoop::<()>::try_new().unwrap();
        let handle = event_loop.handle();
        handle
            .insert_source(from_niri, move |event, _, _| {
                let calloop::channel::Event::Msg(KwinScreenshot2ToNiri::Capture { reply, .. }) =
                    event
                else {
                    return;
                };
                let mut png = Vec::new();
                write_png_rgba8(&mut png, 2, 2, &pixels).unwrap();
                let _ = reply.try_send(Ok(CapturedScreenshot {
                    png: png.into(),
                    scale: 2.0,
                    screen: Some(String::from("TEST-1")),
                    window_id: None,
                }));
            })
            .unwrap();

        let (ping, ping_source) = calloop::ping::make_ping().unwrap();
        let signal = event_loop.get_signal();
        handle
            .insert_source(ping_source, move |_, _, _| signal.stop())
            .unwrap();

        let interface = KwinScreenshot2::new(to_niri);
        let _connection = <KwinScreenshot2 as crate::dbus::Start>::start(interface, false).unwrap();

        let client = std::thread::spawn(move || {
            // Ping even if an assertion below panics, so the event loop always stops.
            struct PingOnDrop(calloop::ping::Ping);

            impl Drop for PingOnDrop {
                fn drop(&mut self) {
                    self.0.ping();
                }
            }

            let _ping = PingOnDrop(ping);
            let connection = zbus::blocking::Connection::session().unwrap();
            let (mut reader, writer) = std::io::pipe().unwrap();
            let options = HashMap::<String, OwnedValue>::new();
            let pipe = std::os::fd::OwnedFd::from(writer);
            let message = connection
                .call_method(
                    Some("org.kde.KWin.ScreenShot2"),
                    "/org/kde/KWin/ScreenShot2",
                    Some("org.kde.KWin.ScreenShot2"),
                    "CaptureWorkspace",
                    &(options, zbus::zvariant::OwnedFd::from(pipe)),
                )
                .unwrap();
            let results: HashMap<String, OwnedValue> = message.body().deserialize().unwrap();
            let mut received = vec![0; pixels.len()];
            reader.read_exact(&mut received).unwrap();
            (results, received)
        });

        event_loop.run(None, &mut (), |_| {}).unwrap();
        let (mut results, received) = client.join().unwrap();

        assert_eq!(take::<String>(&mut results, "type"), "raw");
        assert_eq!(take::<u32>(&mut results, "width"), 2);
        assert_eq!(take::<u32>(&mut results, "height"), 2);
        assert_eq!(take::<u32>(&mut results, "stride"), 2 * 4);
        assert_eq!(take::<f64>(&mut results, "scale"), 2.0);
        assert_eq!(take::<u32>(&mut results, "format"), QIMAGE_FORMAT_RGBA8888);
        assert_eq!(take::<String>(&mut results, "screen"), "TEST-1");
        assert_eq!(received, pixels);
    }

    #[test]
    fn interface_exposes_screenshot2_methods() {
        use zbus::object_server::Interface as _;

        let (to_niri, _from_niri) = calloop::channel::channel();
        let interface = KwinScreenshot2::new(to_niri);
        let mut xml = String::new();
        interface.introspect_to_writer(&mut xml, 0);

        for method in [
            "CaptureWorkspace",
            "CaptureActiveScreen",
            "CaptureScreen",
            "CaptureActiveWindow",
            "CaptureArea",
            "CaptureInteractive",
        ] {
            assert!(
                xml.contains(&format!("<method name=\"{method}\">")),
                "missing {method} in:\n{xml}"
            );
        }
        assert!(xml.contains("<property name=\"Version\" type=\"u\" access=\"read\"/>"));
        assert!(xml.contains("<arg name=\"pipe\" type=\"h\" direction=\"in\"/>"));
        assert!(xml.contains("<arg name=\"options\" type=\"a{sv}\" direction=\"in\"/>"));
        assert!(xml.contains("type=\"a{sv}\" direction=\"out\""));
    }

    #[test]
    fn metadata_reports_raw_rgba() {
        let mut results = image_metadata(800, 600, 1.5, Some("DP-1"), None);

        assert_eq!(take::<String>(&mut results, "type"), "raw");
        assert_eq!(take::<u32>(&mut results, "width"), 800);
        assert_eq!(take::<u32>(&mut results, "height"), 600);
        assert_eq!(take::<u32>(&mut results, "stride"), 800 * 4);
        assert_eq!(take::<f64>(&mut results, "scale"), 1.5);
        assert_eq!(take::<u32>(&mut results, "format"), QIMAGE_FORMAT_RGBA8888);
        assert_eq!(take::<String>(&mut results, "screen"), "DP-1");
        assert!(!results.contains_key("windowId"));
    }

    #[test]
    fn metadata_reports_window_id() {
        let mut results = image_metadata(100, 50, 1.0, None, Some("42"));

        assert_eq!(take::<String>(&mut results, "windowId"), "42");
        assert!(!results.contains_key("screen"));
    }

    #[test]
    fn decoded_png_is_raw_rgba() {
        let pixels: Vec<u8> = (0..2 * 2 * 4).map(|byte| byte as u8).collect();
        let mut png = Vec::new();
        write_png_rgba8(&mut png, 2, 2, &pixels).unwrap();

        let (width, height, decoded) = decode_png(&png).unwrap();

        assert_eq!((width, height), (2, 2));
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn pixels_are_written_to_the_pipe() {
        let pixels: Vec<u8> = (0..64 * 1024).map(|byte| byte as u8).collect();
        let (mut reader, writer) = std::io::pipe().unwrap();
        write_image_to_pipe(writer.into(), pixels.clone());

        let mut received = vec![0; pixels.len()];
        reader.read_exact(&mut received).unwrap();
        assert_eq!(received, pixels);
    }
}
