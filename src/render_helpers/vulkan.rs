//! Vulkan renderer support: device probe, renderer preference and the startup
//! plan.
//!
//! better-smithay ships a Vulkan renderer behind its `renderer_vulkan` feature,
//! which niri's `vulkan` feature enables. This module owns the compositor-side
//! selection policy:
//!
//! 1. `probe()` creates a Vulkan 1.3 instance, enumerates physical devices and checks which of them
//!    can actually instantiate [`VulkanRenderer`]. Systems without a Vulkan loader, driver or the
//!    required extensions simply report why, instead of failing startup.
//! 2. `decide()` turns the user preference (`NIRI_RENDERER`) plus the probe into a [`Plan`]. Today
//!    niri's render tree is typed over `GlesRenderer`, so the plan always keeps the EGL/GLES path
//!    active and records either the Vulkan-readiness or the reason for the fallback. When the
//!    renderer-generic refactor lands, `Path::Vulkan` becomes reachable without changing the policy
//!    interface.
//! 3. `niri --check-vulkan` prints the plan as JSON, and startup logs it once.
//!
//! Requesting Vulkan never leaves the user without a compositor: an unavailable
//! device or an unsupported build is a logged fallback, not an abort.

use serde::Serialize;
use smithay::backend::renderer::vulkan::VulkanRenderer;
use smithay::backend::vulkan::version::Version;
use smithay::backend::vulkan::{Instance, PhysicalDevice};

/// Environment variable that selects the renderer: `auto` (default), `gles` or
/// `vulkan`.
pub const ENV_RENDERER: &str = "NIRI_RENDERER";

/// Whether niri's render tree can render an output with `VulkanRenderer`.
///
/// The TTY backend renders through `TtyRenderer`, an enum over the GLES and
/// Vulkan multi-GPU renderers, so core output rendering (windows, layers, UI
/// textures) works on Vulkan. GLSL-only visuals (niri shader programs, blur,
/// xray, decoration light, carousel panels) are skipped on Vulkan and keep
/// their GLES path; see `docs/MODULES.md`.
pub const RENDER_TREE_SUPPORTS_VULKAN: bool = true;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Preference {
    Auto,
    Egl,
    Vulkan,
}

impl Preference {
    /// Parses `NIRI_RENDERER`. Unknown values fall back to `Auto` (and are
    /// reported by `decide`).
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("egl") | Some("gles") | Some("gl") => Self::Egl,
            Some("vulkan") | Some("vk") => Self::Vulkan,
            _ => Self::Auto,
        }
    }

    pub fn from_env() -> Self {
        Self::parse(std::env::var(ENV_RENDERER).ok().as_deref())
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Egl => "gles",
            Self::Vulkan => "vulkan",
        }
    }
}

/// One physical device as seen by the probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub api_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_node: Option<String>,
    /// Whether `VulkanRenderer::new()` succeeded for this device.
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of looking for a usable Vulkan device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Probe {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DeviceInfo>,
    /// Instance/driver level failure (no loader, unsupported version, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Probe {
    pub fn supported(&self) -> bool {
        self.devices.iter().any(|device| device.supported)
    }

    pub fn supported_devices(&self) -> impl Iterator<Item = &DeviceInfo> {
        self.devices.iter().filter(|device| device.supported)
    }
}

/// Which renderer path the compositor will actually use for outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Path {
    EglGles,
    Vulkan,
}

/// Resolved startup plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub preference: Preference,
    /// Renderer used for output rendering in this build.
    pub path: Path,
    /// Whether the render tree can use Vulkan at all (see
    /// [`RENDER_TREE_SUPPORTS_VULKAN`]).
    pub render_tree_supports_vulkan: bool,
    /// Whether at least one physical device can instantiate the Vulkan renderer.
    pub vulkan_available: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DeviceInfo>,
    /// Why the plan did not use Vulkan, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

/// Creates a Vulkan instance, enumerates devices and checks whether
/// `VulkanRenderer` can be constructed on each of them.
///
/// Never panics and never aborts startup: failures are reported in the returned
/// probe.
pub fn probe() -> Probe {
    let instance = match Instance::new(Version::VERSION_1_3, None) {
        Ok(instance) => instance,
        Err(err) => {
            return Probe {
                devices: Vec::new(),
                error: Some(format!("cannot create Vulkan instance: {err}")),
            };
        }
    };

    let physical_devices = match PhysicalDevice::enumerate(&instance) {
        Ok(devices) => devices,
        Err(err) => {
            return Probe {
                devices: Vec::new(),
                error: Some(format!("cannot enumerate Vulkan physical devices: {err}")),
            };
        }
    };

    let mut devices = Vec::new();
    for device in physical_devices {
        let mut info = DeviceInfo {
            name: device.name().to_owned(),
            api_version: device.api_version().to_string(),
            render_node: device
                .render_node()
                .ok()
                .flatten()
                .map(|node| node.to_string()),
            supported: false,
            reason: None,
        };
        match VulkanRenderer::new(&device) {
            Ok(renderer) => {
                info.supported = true;
                // Drop right away: the probe only validates construction.
                drop(renderer);
            }
            Err(err) => info.reason = Some(err.to_string()),
        }
        devices.push(info);
    }

    Probe {
        devices,
        error: None,
    }
}

/// Applies the preference to a probe result.
pub fn decide(preference: Preference, probe: &Probe) -> Plan {
    let vulkan_available = probe.supported();
    let fallback = match preference {
        Preference::Egl => None,
        Preference::Auto => None,
        Preference::Vulkan if !vulkan_available => Some(match &probe.error {
            Some(err) => {
                format!("Vulkan requested but unavailable ({err}); falling back to EGL/GLES")
            }
            None => "Vulkan requested but no physical device can instantiate the Vulkan \
                     renderer; falling back to EGL/GLES"
                .to_owned(),
        }),
        Preference::Vulkan if !RENDER_TREE_SUPPORTS_VULKAN => Some(
            "Vulkan requested and a usable device was found, but this build's render tree is \
             EGL/GLES; continuing with EGL/GLES"
                .to_owned(),
        ),
        Preference::Vulkan => None,
    };

    let path =
        if preference == Preference::Vulkan && vulkan_available && RENDER_TREE_SUPPORTS_VULKAN {
            Path::Vulkan
        } else {
            Path::EglGles
        };

    Plan {
        preference,
        path,
        render_tree_supports_vulkan: RENDER_TREE_SUPPORTS_VULKAN,
        vulkan_available,
        devices: probe.devices.clone(),
        fallback,
    }
}

/// Probes the system and resolves the plan from the environment.
pub fn startup_plan() -> Plan {
    decide(Preference::from_env(), &probe())
}

/// Logs the startup plan once.
pub fn log_startup_plan() {
    if let Ok(raw) = std::env::var(ENV_RENDERER) {
        let trimmed = raw.trim();
        let known = matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "" | "auto" | "egl" | "gles" | "gl" | "vulkan" | "vk"
        );
        if !known {
            warn!(
                "unknown {ENV_RENDERER} value `{trimmed}`; expected auto, gles or vulkan. Using auto"
            );
        }
    }

    let plan = startup_plan();

    if let Some(fallback) = &plan.fallback {
        if plan.preference == Preference::Vulkan {
            warn!("{fallback}");
        } else {
            info!("{fallback}");
        }
    }

    if plan.vulkan_available {
        let names: Vec<&str> = plan
            .devices
            .iter()
            .filter(|device| device.supported)
            .map(|device| device.name.as_str())
            .collect();
        info!(
            "Vulkan renderer available on {} device(s): {}",
            names.len(),
            names.join(", ")
        );
    } else if let Some(err) = plan.error() {
        debug!("Vulkan renderer unavailable: {err}");
    }

    debug!(
        "renderer plan: preference={}, path={:?}, render-tree-vulkan={}",
        plan.preference.as_str(),
        plan.path,
        plan.render_tree_supports_vulkan
    );
}

impl Plan {
    fn error(&self) -> Option<&str> {
        // The probe error is only kept through the fallback message; expose the
        // first device reason otherwise for logging.
        self.devices
            .iter()
            .find_map(|device| device.reason.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, supported: bool) -> DeviceInfo {
        DeviceInfo {
            name: name.to_owned(),
            api_version: "1.3.0".to_owned(),
            render_node: Some("/dev/dri/renderD128".to_owned()),
            supported,
            reason: (!supported).then(|| "missing VK_KHR_dynamic_rendering".to_owned()),
        }
    }

    #[test]
    fn parses_renderer_preference() {
        assert_eq!(Preference::parse(None), Preference::Auto);
        assert_eq!(Preference::parse(Some("")), Preference::Auto);
        assert_eq!(Preference::parse(Some("auto")), Preference::Auto);
        assert_eq!(Preference::parse(Some(" gles ")), Preference::Egl);
        assert_eq!(Preference::parse(Some("EGL")), Preference::Egl);
        assert_eq!(Preference::parse(Some("gl")), Preference::Egl);
        assert_eq!(Preference::parse(Some("vulkan")), Preference::Vulkan);
        assert_eq!(Preference::parse(Some("VK")), Preference::Vulkan);
        assert_eq!(Preference::parse(Some("metal")), Preference::Auto);
        assert_eq!(Preference::Auto.as_str(), "auto");
        assert_eq!(Preference::Egl.as_str(), "gles");
        assert_eq!(Preference::Vulkan.as_str(), "vulkan");
    }

    #[test]
    fn gles_preference_is_kept_without_fallback() {
        let probe = Probe {
            devices: vec![device("GPU", true)],
            error: None,
        };
        let plan = decide(Preference::Egl, &probe);
        assert_eq!(plan.path, Path::EglGles);
        assert!(plan.vulkan_available);
        assert_eq!(plan.fallback, None);
    }

    #[test]
    fn auto_stays_on_gles_and_reports_vulkan_availability() {
        let probe = Probe {
            devices: vec![device("GPU", true)],
            error: None,
        };
        let plan = decide(Preference::Auto, &probe);
        assert_eq!(plan.path, Path::EglGles);
        assert!(plan.vulkan_available);
        assert!(plan.render_tree_supports_vulkan);
        assert_eq!(plan.fallback, None);
    }

    #[test]
    fn auto_without_vulkan_has_no_fallback_noise() {
        let plan = decide(Preference::Auto, &Probe::default());
        assert_eq!(plan.path, Path::EglGles);
        assert!(!plan.vulkan_available);
        assert_eq!(plan.fallback, None);
    }

    #[test]
    fn requested_vulkan_without_device_falls_back_with_reason() {
        let probe = Probe {
            devices: vec![device("GPU", false)],
            error: None,
        };
        let plan = decide(Preference::Vulkan, &probe);
        assert_eq!(plan.path, Path::EglGles);
        let fallback = plan.fallback.unwrap();
        assert!(fallback.contains("no physical device"), "{fallback}");
        assert!(fallback.contains("falling back"), "{fallback}");
    }

    #[test]
    fn requested_vulkan_with_driver_error_reports_it() {
        let probe = Probe {
            devices: Vec::new(),
            error: Some("no Vulkan loader".to_owned()),
        };
        let plan = decide(Preference::Vulkan, &probe);
        let fallback = plan.fallback.unwrap();
        assert!(fallback.contains("no Vulkan loader"), "{fallback}");
    }

    #[test]
    fn requested_vulkan_with_device_selects_vulkan() {
        let probe = Probe {
            devices: vec![device("GPU", true)],
            error: None,
        };
        let plan = decide(Preference::Vulkan, &probe);
        assert_eq!(plan.path, Path::Vulkan);
        assert!(plan.vulkan_available);
        assert_eq!(plan.fallback, None);
    }

    #[test]
    fn plan_serializes_for_check_vulkan() {
        let probe = Probe {
            devices: vec![device("GPU", true)],
            error: None,
        };
        let plan = decide(Preference::Auto, &probe);
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["preference"], "auto");
        assert_eq!(json["path"], "eglgles");
        assert_eq!(json["render_tree_supports_vulkan"], true);
        assert_eq!(json["vulkan_available"], true);
        assert_eq!(json["devices"][0]["name"], "GPU");
        assert_eq!(json["devices"][0]["supported"], true);
    }

    #[test]
    fn probe_never_panics_without_a_gpu() {
        // On machines without a Vulkan loader this returns `error`; with a
        // loader it returns devices. Either way it must be safe to call.
        let probe = probe();
        if probe.error.is_some() {
            assert!(probe.devices.is_empty());
        }
        for device in &probe.devices {
            assert!(!device.name.is_empty());
            if device.supported {
                assert!(device.reason.is_none());
            } else {
                assert!(device.reason.is_some());
            }
        }
    }
}
