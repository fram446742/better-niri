use std::path::PathBuf;

use zbus::{fdo, interface};

use super::Start;
use crate::dbus::gnome_shell_screenshot::ScreenshotToNiri;
use crate::ui::screenshot_ui::{ScreenshotPathReplySender, ScreenshotPortalError};

/// KDE KWin screenshot D-Bus interface (`org.kde.kwin.Screenshot`).
///
/// KWin serves this interface from the `org.kde.KWin` name at `/Screenshot`, so Spectacle and
/// other KDE tools talk to it there. We reuse niri's existing screenshot capture and save-to-disk
/// pipeline (the same one behind the GNOME Shell and portal screenshot interfaces) and return the
/// saved path like KWin does.
pub struct KwinScreenshot {
    to_niri: calloop::channel::Sender<ScreenshotToNiri>,
}

impl KwinScreenshot {
    pub fn new(to_niri: calloop::channel::Sender<ScreenshotToNiri>) -> Self {
        Self { to_niri }
    }

    fn send(&self, msg: ScreenshotToNiri) -> fdo::Result<()> {
        self.to_niri
            .send(msg)
            .map_err(|err| fdo::Error::Failed(format!("error sending message to niri: {err}")))
    }
}

fn portal_error(error: ScreenshotPortalError) -> fdo::Error {
    match error {
        ScreenshotPortalError::Cancelled => {
            fdo::Error::Failed("screenshot was canceled".to_owned())
        }
        ScreenshotPortalError::Failed(message) => fdo::Error::Failed(message),
    }
}

async fn receive_path(
    receiver: async_channel::Receiver<Result<PathBuf, ScreenshotPortalError>>,
) -> fdo::Result<String> {
    let path = receiver
        .recv()
        .await
        .map_err(|err| fdo::Error::Failed(format!("screenshot reply channel closed: {err}")))?
        .map_err(portal_error)?;
    Ok(path.to_string_lossy().into_owned())
}

#[interface(name = "org.kde.kwin.Screenshot")]
impl KwinScreenshot {
    /// Capture the whole desktop (all outputs composited into one image).
    #[zbus(name = "screenshotFullscreen")]
    async fn screenshot_fullscreen(&self, include_cursor: bool) -> fdo::Result<String> {
        let (reply, receiver): (
            ScreenshotPathReplySender,
            async_channel::Receiver<Result<PathBuf, ScreenshotPortalError>>,
        ) = async_channel::bounded(1);
        self.send(ScreenshotToNiri::TakeScreenshot {
            include_cursor,
            reply,
        })?;
        receive_path(receiver).await
    }

    /// Capture a global-logical rectangle. As in KWin, the area must be contained by one output.
    #[zbus(name = "screenshotArea")]
    async fn screenshot_area(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        include_cursor: bool,
    ) -> fdo::Result<String> {
        let _ = include_cursor;
        let (reply, receiver): (
            ScreenshotPathReplySender,
            async_channel::Receiver<Result<PathBuf, ScreenshotPortalError>>,
        ) = async_channel::bounded(1);
        self.send(ScreenshotToNiri::TakeArea {
            x,
            y,
            width,
            height,
            reply,
        })?;
        receive_path(receiver).await
    }

    /// Capture the focused window.
    #[zbus(name = "screenshotWindow")]
    async fn screenshot_window(
        &self,
        include_cursor: bool,
        _internal: bool,
    ) -> fdo::Result<String> {
        let (reply, receiver): (
            ScreenshotPathReplySender,
            async_channel::Receiver<Result<PathBuf, ScreenshotPortalError>>,
        ) = async_channel::bounded(1);
        self.send(ScreenshotToNiri::TakeWindow {
            include_cursor,
            reply,
        })?;
        receive_path(receiver).await
    }

    /// Capture the window under the cursor. niri's capture path targets the focused window, which
    /// is the window under the cursor in the normal tiling interaction model.
    #[zbus(name = "screenshotWindowUnderCursor")]
    async fn screenshot_window_under_cursor(
        &self,
        include_cursor: bool,
        internal: bool,
    ) -> fdo::Result<String> {
        self.screenshot_window(include_cursor, internal).await
    }

    /// Open niri's interactive screenshot UI and return the saved path once the user saves.
    #[zbus(name = "screenshotInteractive")]
    async fn screenshot_interactive(
        &self,
        _include_cursor: bool,
        _internal: bool,
    ) -> fdo::Result<String> {
        let (reply, receiver): (
            ScreenshotPathReplySender,
            async_channel::Receiver<Result<PathBuf, ScreenshotPortalError>>,
        ) = async_channel::bounded(1);
        self.send(ScreenshotToNiri::InteractiveScreenshot(reply))?;
        receive_path(receiver).await
    }
}

impl Start for KwinScreenshot {
    fn start(self, monitor: bool) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        conn.object_server().at("/Screenshot", self)?;
        super::request_name(&conn, "org.kde.KWin", monitor)?;
        Ok(conn)
    }
}
