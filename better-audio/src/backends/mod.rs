//! Audio backends.
//!
//! `better-audio` can talk to PipeWire in three ways:
//!
//! - [`native`]: a direct `libpipewire-0.3` connection through the `pipewire` crate. This is the
//!   preferred backend.
//! - [`pactl`]: the `pactl` command line tool from PipeWire's PulseAudio compatibility layer.
//! - [`wpctl`]: JSON snapshots from `pw-dump` combined with `pw-cli` for relinking and `wpctl` for
//!   defaults/volume/mute. This is the CLI-only fallback for systems where the native crate is not
//!   available at build time.
//!
//! All backends return the same [`crate::Sink`] and [`crate::Stream`] model,
//! so rule matching and planning are backend independent.

pub mod dump;
pub mod pactl;
pub mod wpctl;

#[cfg(feature = "native")]
pub mod native;

use crate::{Sink, Stream};

/// A backend the user can ask for with `--backend`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Probe in the order native, pactl, wpctl.
    Auto,
    Native,
    Pactl,
    Wpctl,
}

impl BackendKind {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "native" | "pipewire" => Ok(Self::Native),
            "pactl" | "pulse" => Ok(Self::Pactl),
            "wpctl" | "cli" | "pw-dump" => Ok(Self::Wpctl),
            other => Err(format!(
                "unknown backend `{other}` (expected auto, native, pactl or wpctl)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::Pactl => "pactl",
            Self::Wpctl => "wpctl",
        }
    }
}

/// Paths used by the command line backends. The native backend ignores them.
#[derive(Debug, Clone)]
pub struct BackendConfig {
    pub pactl: String,
    pub wpctl: String,
    pub pw_dump: String,
    pub pw_cli: String,
}

impl Default for BackendConfig {
    fn default() -> Self {
        fn env(name: &str, fallback: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
        }
        Self {
            pactl: env("BETTER_AUDIO_PACTL", "pactl"),
            wpctl: env("BETTER_AUDIO_WPCTL", "wpctl"),
            pw_dump: env("BETTER_AUDIO_PW_DUMP", "pw-dump"),
            pw_cli: env("BETTER_AUDIO_PW_CLI", "pw-cli"),
        }
    }
}

/// Operations needed to route playback streams.
pub trait AudioBackend {
    fn kind(&self) -> BackendKind;

    /// Human readable backend description, including the executable path.
    fn description(&self) -> String;

    fn sinks(&self) -> Result<Vec<Sink>, String>;
    fn streams(&self) -> Result<Vec<Stream>, String>;
    fn default_sink(&self) -> Result<Option<String>, String>;

    fn move_stream(&self, stream_id: u32, sink: &str) -> Result<(), String>;
    fn set_default_sink(&self, sink: &str) -> Result<(), String>;
    fn set_stream_volume(&self, stream_id: u32, volume: f32) -> Result<(), String>;
    fn set_stream_mute(&self, stream_id: u32, mute: bool) -> Result<(), String>;

    /// Verifies that the backend can talk to a server. Used by backend
    /// selection; the default reads sinks.
    fn probe(&self) -> Result<(), String> {
        self.sinks().map(|_| ())
    }
}

/// Creates a backend without talking to the server yet.
pub fn build(kind: BackendKind, config: &BackendConfig) -> Result<Box<dyn AudioBackend>, String> {
    match kind {
        BackendKind::Auto => Err("`auto` is not a concrete backend".to_owned()),
        BackendKind::Pactl => Ok(Box::new(pactl::PactlBackend::new(config.pactl.clone()))),
        BackendKind::Wpctl => Ok(Box::new(wpctl::WpctlBackend::new(
            config.pw_dump.clone(),
            config.pw_cli.clone(),
            config.wpctl.clone(),
        ))),
        BackendKind::Native => {
            #[cfg(feature = "native")]
            {
                Ok(Box::new(native::NativeBackend::new()))
            }
            #[cfg(not(feature = "native"))]
            {
                Err("native backend not compiled (feature `native` disabled)".to_owned())
            }
        }
    }
}

/// One backend probe during selection.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub kind: BackendKind,
    pub error: Option<String>,
}

/// Outcome of backend selection.
#[derive(Debug, Clone)]
pub struct BackendStatus {
    pub requested: BackendKind,
    pub active: BackendKind,
    pub active_description: String,
    pub degraded: bool,
    pub attempts: Vec<Attempt>,
}

impl BackendStatus {
    /// Short explanation of why this backend is active.
    pub fn explanation(&self) -> String {
        match self
            .attempts
            .iter()
            .find_map(|attempt| attempt.error.as_ref().map(|error| (attempt.kind, error)))
        {
            Some((kind, error)) => format!(
                "{} unavailable: {error}; using {}",
                kind.as_str(),
                self.active.as_str()
            ),
            None => format!("using {}", self.active.as_str()),
        }
    }
}

/// A selected backend plus the reasons for the choice.
pub struct Selection {
    pub backend: Box<dyn AudioBackend>,
    pub status: BackendStatus,
}

/// Selects a backend, probing candidates in order and degrading gracefully.
///
/// `make` must construct a backend and verify it can talk to the server. Tests
/// substitute fake factories to exercise selection without PipeWire.
pub fn select_backend<F>(requested: BackendKind, mut make: F) -> Result<Selection, String>
where
    F: FnMut(BackendKind) -> Result<Box<dyn AudioBackend>, String>,
{
    let mut order = Vec::new();
    if requested != BackendKind::Auto {
        order.push(requested);
    }
    for kind in [BackendKind::Native, BackendKind::Pactl, BackendKind::Wpctl] {
        if !order.contains(&kind) {
            order.push(kind);
        }
    }

    let mut attempts = Vec::new();
    for kind in order {
        match make(kind) {
            Ok(backend) => {
                attempts.push(Attempt { kind, error: None });
                let active = backend.kind();
                let degraded = requested != BackendKind::Auto && requested != active;
                return Ok(Selection {
                    status: BackendStatus {
                        requested,
                        active,
                        active_description: backend.description(),
                        degraded,
                        attempts,
                    },
                    backend,
                });
            }
            Err(error) => attempts.push(Attempt {
                kind,
                error: Some(error),
            }),
        }
    }

    let reasons = attempts
        .iter()
        .filter_map(|attempt| {
            attempt
                .error
                .as_ref()
                .map(|error| format!("{}: {error}", attempt.kind.as_str()))
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!("no audio backend available ({reasons})"))
}

/// Builds and probes backends from real executables.
pub fn select(requested: BackendKind, config: &BackendConfig) -> Result<Selection, String> {
    let config = config.clone();
    select_backend(requested, move |kind| {
        let backend = build(kind, &config)?;
        backend.probe()?;
        Ok(backend)
    })
}
