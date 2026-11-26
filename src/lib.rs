#[macro_use]
extern crate tracing;

#[cfg(feature = "dbus")]
pub mod a11y;
#[cfg(feature = "agent")]
pub mod agent;
pub mod animation;
pub mod backend;
pub mod cli;
pub mod cursor;
#[cfg(feature = "dbus")]
pub mod dbus;
pub mod frame_clock;
pub mod handlers;
pub mod identity;
pub mod input;
pub mod ipc;
pub mod layer;
pub mod layout;
pub mod niri;
pub mod output_shader;
pub mod protocols;
pub mod render_helpers;
pub mod rubber_band;
#[cfg(feature = "xdp-gnome-screencast")]
pub mod screencasting;
pub mod ui;
pub mod utils;
pub mod window;

#[cfg(test)]
mod tests;

/// Serializes tests that create raw EGL/GLES contexts. The GPU driver and EGL state are global to
/// the test process, so running these tests in parallel makes them interfere (and fail
/// intermittently) even though each one creates its own context.
#[cfg(test)]
pub(crate) static EGL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
