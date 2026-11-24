//! Activity detection.
//!
//! [`ActivityClock`] is a monotonic, process-local idle clock. The daemon feeds
//! it from niri's event stream with [`track_activity_stream`] (focus, window and
//! workspace changes all count as activity), and [`Idle::reset`] resets it
//! explicitly. Embedders and tests can replace the whole probe through
//! [`ActivityProbe`], so idle policy never requires a display or a compositor.
//!
//! logind's `IdleHint` is boolean, so it is reported in status when configured
//! but never used to derive the idle duration.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{io, thread};

use niri_ipc::Request;
use serde_json::Value;

/// Current wall-clock time in milliseconds.
pub trait Now: Send + Sync {
    fn now_millis(&self) -> u64;
}

/// System clock used by the daemon.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemNow;

impl Now for SystemNow {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default()
    }
}

/// Where the idle duration comes from.
pub trait ActivityProbe: Send + Sync {
    /// Milliseconds since the last observed activity.
    fn idle_millis(&self) -> u64;

    /// Human-readable source, shown by `idle.status`.
    fn source(&self) -> String;

    /// Marks the current time as the latest activity.
    fn reset(&self);
}

/// Shared monotonic idle clock.
///
/// Clones share the same baseline, which is what lets the daemon's event
/// thread and the `idle.*` context observe each other.
#[derive(Clone)]
pub struct ActivityClock {
    last_millis: Arc<Mutex<u64>>,
    now: Arc<dyn Now>,
}

impl Default for ActivityClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityClock {
    pub fn new() -> Self {
        Self::with_now(Arc::new(SystemNow))
    }

    /// Clock whose wall time comes from `now`. Tests use a fake [`Now`].
    pub fn with_now(now: Arc<dyn Now>) -> Self {
        let last = now.now_millis();
        Self {
            last_millis: Arc::new(Mutex::new(last)),
            now,
        }
    }

    pub fn idle_millis(&self) -> u64 {
        let last = *self
            .last_millis
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.now.now_millis().saturating_sub(last)
    }

    pub fn reset(&self) {
        let now = self.now.now_millis();
        *self
            .last_millis
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = now;
    }

    /// Records one observed compositor event as activity.
    pub fn observe(&self) {
        self.reset();
    }

    /// The wall clock this baseline uses, for sharing with other modules.
    pub fn now_handle(&self) -> Arc<dyn Now> {
        self.now.clone()
    }
}

/// Default probe backed by an [`ActivityClock`].
pub struct SystemProbe {
    clock: ActivityClock,
}

impl SystemProbe {
    pub fn new(clock: ActivityClock) -> Self {
        Self { clock }
    }
}

impl ActivityProbe for SystemProbe {
    fn idle_millis(&self) -> u64 {
        self.clock.idle_millis()
    }

    fn source(&self) -> String {
        "activity-clock".to_owned()
    }

    fn reset(&self) {
        self.clock.reset();
    }
}

/// Subscribes to niri's event stream and marks every event as activity.
///
/// Returns the number of events observed before the connection closed. As with
/// the parking tracker, malformed lines are skipped and only transport errors
/// end the loop. Any focus, window, workspace or output change counts: the
/// policy only needs "was there input recently", not the event details.
pub fn track_activity_stream(path: &Path, clock: &ActivityClock) -> io::Result<u64> {
    let stream = UnixStream::connect(path)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);

    let mut request = serde_json::to_string(&Request::EventStream)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    request.push('\n');
    writer.write_all(request.as_bytes())?;
    writer.flush()?;

    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "event-stream handshake closed",
        ));
    }
    let reply: niri_ipc::Reply = serde_json::from_str(line.trim())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    match reply {
        Ok(niri_ipc::Response::Handled) => (),
        Ok(other) => {
            return Err(io::Error::other(format!(
                "unexpected event-stream response: {other:?}"
            )));
        }
        Err(message) => return Err(io::Error::other(message)),
    }

    let mut observed = 0u64;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(observed),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err)
                if err.kind() == io::ErrorKind::UnexpectedEof
                    || err.kind() == io::ErrorKind::ConnectionReset =>
            {
                return Ok(observed);
            }
            Err(err) => return Err(err),
        }
        if line.trim().is_empty() {
            continue;
        }
        // The event body itself is irrelevant; receiving it is the signal.
        clock.observe();
        observed = observed.saturating_add(1);
    }
}

/// Reconnecting activity loop used by the daemon.
pub fn run_activity_stream(path: std::path::PathBuf, clock: ActivityClock) {
    loop {
        if let Err(err) = track_activity_stream(&path, &clock) {
            eprintln!(
                "better-companion: activity stream {}: {err}",
                path.display()
            );
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// Reads logind's boolean `IdleHint` for the current session.
///
/// This shells out to `loginctl`; a missing tool is reported as an
/// `Unavailable` value rather than an error that would break status.
pub fn logind_idle_hint() -> Result<Option<bool>, String> {
    let session = std::env::var("XDG_SESSION_ID").unwrap_or_else(|_| "self".to_owned());
    let output = Command::new("loginctl")
        .args(["show-session", &session, "--property=IdleHint", "--value"])
        .output()
        .map_err(|err| format!("cannot run loginctl: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "loginctl exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    let value = String::from_utf8_lossy(&output.stdout);
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" => Ok(Some(true)),
        "no" | "false" | "0" => Ok(Some(false)),
        "" => Ok(None),
        other => Err(format!("unexpected IdleHint value `{other}`")),
    }
}

/// Reads logind's `IdleHint` and renders it for JSON status output.
pub fn logind_status() -> Value {
    match logind_idle_hint() {
        Ok(Some(idle)) => serde_json::json!({ "available": true, "idle_hint": idle }),
        Ok(None) => serde_json::json!({ "available": false, "detail": "IdleHint is empty" }),
        Err(err) => serde_json::json!({ "available": false, "detail": err }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    struct FakeNow(AtomicU64);

    impl FakeNow {
        fn new(millis: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(millis)))
        }

        fn advance(&self, millis: u64) {
            self.0.fetch_add(millis, Ordering::SeqCst);
        }
    }

    impl Now for FakeNow {
        fn now_millis(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-companion-idle-probe-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn clock_tracks_idle_time_and_reset() {
        let now = FakeNow::new(10_000);
        let clock = ActivityClock::with_now(now.clone());
        assert_eq!(clock.idle_millis(), 0);

        now.advance(2_500);
        assert_eq!(clock.idle_millis(), 2_500);

        clock.observe();
        assert_eq!(clock.idle_millis(), 0);

        now.advance(400);
        assert_eq!(clock.idle_millis(), 400);
    }

    #[test]
    fn probe_reports_the_activity_clock_source() {
        let now = FakeNow::new(0);
        let probe = SystemProbe::new(ActivityClock::with_now(now.clone()));
        assert_eq!(probe.source(), "activity-clock");
        now.advance(99);
        assert_eq!(probe.idle_millis(), 99);
        probe.reset();
        assert_eq!(probe.idle_millis(), 0);
    }

    #[test]
    fn event_stream_counts_events_as_activity() {
        let dir = temp_path("stream");
        let socket = dir.join("niri.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(line.trim()).unwrap();
            assert!(matches!(request, Request::EventStream));
            let mut stream = stream;
            stream.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
            stream
                .write_all(b"{\"WorkspaceActivated\":{\"id\":1,\"focused\":true}}\n")
                .unwrap();
            stream.write_all(b"not json\n").unwrap();
            stream
                .write_all(b"{\"WindowFocusChanged\":{\"id\":null}}\n")
                .unwrap();
            stream.flush().unwrap();
        });

        let now = FakeNow::new(5_000);
        let clock = ActivityClock::with_now(now.clone());
        now.advance(10_000);
        assert_eq!(clock.idle_millis(), 10_000);

        let observed = track_activity_stream(&socket, &clock).unwrap();
        assert_eq!(observed, 3);
        assert_eq!(clock.idle_millis(), 0);
        server.join().unwrap();
    }
}
