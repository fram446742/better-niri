use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use niri_config::{Config, ModKey};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::drm::DrmNode;
use smithay::backend::renderer::Renderer;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::niri::Niri;
use crate::utils::id::IdCounter;

pub mod tty;
#[cfg(feature = "vulkan")]
pub mod tty_renderer;
pub use tty::Tty;

mod libinput_plugins;
mod virtual_output;

pub mod winit;
pub use winit::Winit;

pub mod headless;
pub use headless::Headless;

#[allow(clippy::large_enum_variant)]
pub enum Backend {
    Tty(Tty),
    Winit(Winit),
    Headless(Headless),
}

/// HDR capabilities of an output, inserted into the [`Output`]'s user data by the backend.
///
/// `supported` requires the DRM connector to expose the `Colorspace` (with BT2020_RGB) and
/// `HDR_OUTPUT_METADATA` properties, and the sink's EDID to advertise the PQ EOTF. On backends
/// without HDR support (winit, headless) the user data entry is absent, which reads as
/// unsupported.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputHdrCaps {
    pub supported: bool,
    /// Desired content max luminance from the EDID, in cd/m² (0 = not provided).
    pub max_luminance: u16,
    /// Desired content min luminance from the EDID, in 0.0001 cd/m² units (0 = not provided).
    pub min_luminance: u16,
    /// Desired content max frame-average luminance from the EDID, in cd/m² (0 = not provided).
    pub max_frame_avg_luminance: u16,
}

#[derive(PartialEq, Eq)]
pub enum RenderResult {
    /// The frame was submitted to the backend for presentation.
    Submitted,
    /// Rendering succeeded, but there was no damage.
    NoDamage,
    /// The frame was not rendered and submitted, due to an error or otherwise.
    Skipped,
}

pub type IpcOutputMap = HashMap<OutputId, niri_ipc::Output>;

/// Marker inserted into `Output::user_data()` for outputs that are not driven by a real scanout
/// pipeline (e.g. HEADLESS-* virtual outputs).
///
/// Such outputs require special-casing in a few places (notably frame callback delivery), because
/// they don't participate in the normal GPU pipeline that tracks `surface_primary_scanout_output`.
#[derive(Debug, Default)]
pub struct VirtualOutputMarker;

impl VirtualOutputMarker {
    pub(crate) fn is_virtual(output: &Output) -> bool {
        output.user_data().get::<Self>().is_some()
    }
}

static OUTPUT_ID_COUNTER: IdCounter = IdCounter::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputId(u64);

impl OutputId {
    fn next() -> OutputId {
        OutputId(OUTPUT_ID_COUNTER.next())
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl Backend {
    /// Reclaims dead buffer imports even when no output renders a frame.
    ///
    /// Importing a client buffer does not require a frame; a client can create and
    /// destroy dmabufs while the outputs are powered off or unplugged. Without this,
    /// their EGLImages stay pinned until the next rendered frame.
    pub fn cleanup_texture_cache(&mut self) {
        if let Self::Tty(tty) = self {
            tty.cleanup_texture_cache();
        } else {
            self.with_primary_renderer(|renderer| {
                if let Err(err) = renderer.cleanup_texture_cache() {
                    warn!("error cleaning up renderer texture cache: {err:?}");
                }
            });
        }
    }

    pub fn init(&mut self, niri: &mut Niri) {
        let _span = tracy_client::span!("Backend::init");
        match self {
            Backend::Tty(tty) => tty.init(niri),
            Backend::Winit(winit) => winit.init(niri),
            Backend::Headless(headless) => headless.init(niri),
        }
    }

    pub fn seat_name(&self) -> String {
        match self {
            Backend::Tty(tty) => tty.seat_name(),
            Backend::Winit(winit) => winit.seat_name(),
            Backend::Headless(headless) => headless.seat_name(),
        }
    }

    pub fn with_primary_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut GlesRenderer) -> T,
    ) -> Option<T> {
        match self {
            Backend::Tty(tty) => tty.with_primary_renderer(f),
            Backend::Winit(winit) => winit.with_primary_renderer(f),
            Backend::Headless(headless) => headless.with_primary_renderer(f),
        }
    }

    /// Runs `f` with the TTY backend's primary renderer abstraction, which also covers the Vulkan
    /// renderer. Returns `None` on non-TTY backends and when there is no primary renderer.
    pub fn with_primary_tty_renderer<T>(
        &mut self,
        f: impl FnOnce(&mut crate::backend::tty::TtyRenderer<'_>) -> T,
    ) -> Option<T> {
        match self {
            Backend::Tty(tty) => tty.with_primary_tty_renderer(f),
            Backend::Winit(_) | Backend::Headless(_) => None,
        }
    }

    /// DRM render node of the primary renderer, if it has one.
    ///
    /// This is the node clients should allocate dma-bufs on for the primary renderer to import
    /// and render into them directly.
    pub fn primary_render_node(&mut self) -> Option<DrmNode> {
        match self {
            Backend::Tty(tty) => tty.primary_render_node(),
            Backend::Winit(winit) => winit.primary_render_node(),
            Backend::Headless(headless) => headless.primary_render_node(),
        }
    }

    pub fn render(
        &mut self,
        niri: &mut Niri,
        output: &Output,
        target_presentation_time: Duration,
    ) -> RenderResult {
        match self {
            Backend::Tty(tty) => tty.render(niri, output, target_presentation_time),
            Backend::Winit(winit) => winit.render(niri, output),
            Backend::Headless(headless) => headless.render(niri, output),
        }
    }

    pub fn mod_key(&self, config: &Config) -> ModKey {
        match self {
            Backend::Winit(_) => config.input.mod_key_nested.unwrap_or({
                if let Some(ModKey::Alt) = config.input.mod_key {
                    ModKey::Super
                } else {
                    ModKey::Alt
                }
            }),
            Backend::Tty(_) | Backend::Headless(_) => config.input.mod_key.unwrap_or(ModKey::Super),
        }
    }

    pub fn change_vt(&mut self, vt: i32) {
        match self {
            Backend::Tty(tty) => tty.change_vt(vt),
            Backend::Winit(_) => (),
            Backend::Headless(_) => (),
        }
    }

    pub fn suspend(&mut self) {
        match self {
            Backend::Tty(tty) => tty.suspend(),
            Backend::Winit(_) => (),
            Backend::Headless(_) => (),
        }
    }

    pub fn toggle_debug_tint(&mut self) {
        match self {
            Backend::Tty(tty) => tty.toggle_debug_tint(),
            Backend::Winit(winit) => winit.toggle_debug_tint(),
            Backend::Headless(_) => (),
        }
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        match self {
            Backend::Tty(tty) => tty.import_dmabuf(dmabuf),
            Backend::Winit(winit) => winit.import_dmabuf(dmabuf),
            Backend::Headless(headless) => headless.import_dmabuf(dmabuf),
        }
    }

    pub fn early_import(&mut self, surface: &WlSurface) {
        match self {
            Backend::Tty(tty) => tty.early_import(surface),
            Backend::Winit(_) => (),
            Backend::Headless(_) => (),
        }
    }

    pub fn ipc_outputs(&self) -> Arc<Mutex<IpcOutputMap>> {
        match self {
            Backend::Tty(tty) => tty.ipc_outputs(),
            Backend::Winit(winit) => winit.ipc_outputs(),
            Backend::Headless(headless) => headless.ipc_outputs(),
        }
    }

    #[cfg(feature = "xdp-gnome-screencast")]
    pub fn gbm_device(
        &self,
    ) -> Option<smithay::backend::allocator::gbm::GbmDevice<smithay::utils::DeviceFd>> {
        match self {
            Backend::Tty(tty) => tty.primary_gbm_device(),
            Backend::Winit(winit) => winit.gbm_device(),
            Backend::Headless(headless) => headless.gbm_device(),
        }
    }

    pub fn set_monitors_active(&mut self, active: bool, skip_isolated: bool) {
        match self {
            Backend::Tty(tty) => tty.set_monitors_active(active, skip_isolated),
            Backend::Winit(_) => (),
            Backend::Headless(_) => (),
        }
    }

    pub fn set_output_on_demand_vrr(&mut self, niri: &mut Niri, output: &Output, enable_vrr: bool) {
        match self {
            Backend::Tty(tty) => tty.set_output_on_demand_vrr(niri, output, enable_vrr),
            Backend::Winit(_) => (),
            Backend::Headless(_) => (),
        }
    }

    pub fn update_ignored_nodes_config(&mut self, niri: &mut Niri) {
        match self {
            Backend::Tty(tty) => tty.update_ignored_nodes_config(niri),
            Backend::Winit(_) => (),
            Backend::Headless(_) => (),
        }
    }

    pub fn on_output_config_changed(&mut self, niri: &mut Niri) {
        match self {
            Backend::Tty(tty) => tty.on_output_config_changed(niri),
            Backend::Winit(_) => (),
            Backend::Headless(headless) => headless.on_output_config_changed(niri),
        }
    }

    pub fn set_host_cursor_visible(&mut self, visible: bool) {
        match self {
            Backend::Winit(winit) => winit.set_cursor_visible(visible),
            Backend::Tty(_) | Backend::Headless(_) => (),
        }
    }

    pub fn tty_checked(&mut self) -> Option<&mut Tty> {
        if let Self::Tty(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub fn tty(&mut self) -> &mut Tty {
        if let Self::Tty(v) = self {
            v
        } else {
            panic!("backend is not Tty");
        }
    }

    pub fn winit(&mut self) -> &mut Winit {
        if let Self::Winit(v) = self {
            v
        } else {
            panic!("backend is not Winit")
        }
    }

    pub fn headless(&mut self) -> &mut Headless {
        if let Self::Headless(v) = self {
            v
        } else {
            panic!("backend is not Headless")
        }
    }

    /// Create a new virtual output and return its name.
    /// Only supported on TTY and Headless backends.
    pub fn create_virtual_output(
        &mut self,
        niri: &mut Niri,
        width: u16,
        height: u16,
        refresh_rate: u32,
        name: Option<String>,
    ) -> Result<String, String> {
        match self {
            Backend::Headless(headless) => {
                headless.create_virtual_output(niri, width, height, refresh_rate, name)
            }
            Backend::Tty(tty) => tty.create_virtual_output(niri, width, height, refresh_rate, name),
            Backend::Winit(_) => {
                Err("virtual outputs are not supported on the Winit backend".to_string())
            }
        }
    }

    /// Remove a virtual output by name.
    pub fn remove_virtual_output(&mut self, niri: &mut Niri, name: &str) -> Result<(), String> {
        match self {
            Backend::Headless(headless) => headless.remove_virtual_output(niri, name),
            Backend::Tty(tty) => tty.remove_virtual_output(niri, name),
            Backend::Winit(_) => {
                Err("virtual outputs are not supported on the Winit backend".to_string())
            }
        }
    }
}
