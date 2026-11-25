//! Video wallpapers for the native backend.
//!
//! `ffmpeg` decodes the file into BGRA frames at the size of the output buffer;
//! a reader thread keeps the newest frame and the panel's frame timer commits
//! it. This is a CPU blit path by design: it needs no GPU decoder or Vulkan
//! surface, works on every compositor with layer-shell, and reuses the same
//! shm commit path as images.
//!
//! HDR video is tone-mapped to SDR through zscale/tonemap; the output surface
//! itself stays SDR, like the image backend.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::geom::Size;

pub const FPS: u32 = 30;

/// Extensions handled by the video path.
pub fn is_video(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("mp4" | "mkv" | "webm" | "mov" | "m4v" | "avi")
    )
}

pub struct Video {
    path: PathBuf,
    size: Size,
    child: Child,
    latest: Arc<Mutex<Arc<Vec<u8>>>>,
}

impl Video {
    /// Starts decoding `path` at `size`. Returns `None` when ffmpeg is missing
    /// or the process cannot be spawned; the caller paints the fallback colour.
    pub fn start(path: &Path, size: Size) -> Option<Self> {
        if size.is_empty() || !path.is_file() {
            return None;
        }
        let ffmpeg = std::env::var_os("BETTER_WALLPAPER_FFMPEG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let filter = filter_chain(path, size);
        let mut child = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-stream_loop",
                "-1",
                "-i",
            ])
            .arg(path)
            .args(["-vf", &filter, "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let frame_len = size.width as usize * size.height as usize * 4;
        let latest = Arc::new(Mutex::new(Arc::new(Vec::new())));
        let shared = Arc::clone(&latest);
        std::thread::Builder::new()
            .name(format!(
                "wallpaper-video-{}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("video")
            ))
            .spawn(move || {
                let mut buffer = vec![0u8; frame_len];
                while stdout.read_exact(&mut buffer).is_ok() {
                    if let Ok(mut latest) = shared.lock() {
                        *latest = Arc::new(buffer.clone());
                    }
                }
            })
            .ok()?;
        Some(Self {
            path: path.to_path_buf(),
            size,
            child,
            latest,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Newest decoded frame, or `None` until the first one arrives.
    pub fn frame(&self) -> Option<Arc<Vec<u8>>> {
        self.latest
            .lock()
            .ok()
            .map(|frame| Arc::clone(&frame))
            .filter(|frame| !frame.is_empty())
    }

    /// Whether the decoder is still running (used to restart it).
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `-vf` chain: optional HDR→SDR tone map, cover-scale/crop, fps, BGRA.
pub fn filter_chain(path: &Path, size: Size) -> String {
    let mut filters = Vec::new();
    if is_hdr(path) {
        filters.push(
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
             tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
                .to_owned(),
        );
    }
    filters.push(format!(
        "scale={}:{}:force_original_aspect_ratio=increase,crop={}:{}",
        size.width, size.height, size.width, size.height
    ));
    filters.push(format!("fps={FPS}"));
    filters.push("format=bgra".to_owned());
    filters.join(",")
}

fn is_hdr(path: &Path) -> bool {
    let ffprobe = std::env::var_os("BETTER_WALLPAPER_FFPROBE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffprobe"));
    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=color_transfer",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    let transfer = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    transfer.contains("smpte2084")
        || transfer.contains("arib-std-b67")
        || transfer.contains("smpte428")
}

/// Used by diagnostics to report the decode rate.
pub fn frame_interval() -> Duration {
    Duration::from_millis((1000 / FPS) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_video_extensions() {
        for name in ["a.mp4", "b.mkv", "c.WEBM", "d.mov"] {
            assert!(is_video(Path::new(name)), "{name}");
        }
        for name in ["a.png", "b.jpg", "c.svg", "noext"] {
            assert!(!is_video(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn filter_chain_covers_and_formats_bgra() {
        let chain = filter_chain(Path::new("/nonexistent.mp4"), Size::new(1920, 1080));
        assert!(
            chain.contains("scale=1920:1080:force_original_aspect_ratio=increase"),
            "{chain}"
        );
        assert!(chain.contains("crop=1920:1080"), "{chain}");
        assert!(chain.contains("fps=30"), "{chain}");
        assert!(chain.ends_with("format=bgra"), "{chain}");
    }

    #[test]
    fn decodes_frames_when_ffmpeg_is_present() {
        let dir =
            std::env::temp_dir().join(format!("better-wallpaper-video-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        let made = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=16x16:d=0.3",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&clip)
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let mut video = Video::start(&clip, Size::new(8, 8)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while video.frame().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            assert!(video.alive(), "decoder exited before the first frame");
        }
        let frame = video.frame().expect("no frame decoded");
        assert_eq!(frame.len(), 8 * 8 * 4);
        // Opaque red in BGRA (YUV conversion may round by one).
        assert_eq!(frame[3], 255);
        assert!(frame[2] > 250, "red={}", frame[2]);
        assert!(frame[0] < 5 && frame[1] < 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frame_interval_matches_fps() {
        assert_eq!(frame_interval(), Duration::from_millis(33));
    }
}
