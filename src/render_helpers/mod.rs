use std::ptr;

use anyhow::{Context as _, bail, ensure};
use niri_config::{BlockOutFrom, BlockOutKind, BlockOutTarget};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Fourcc};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{Element, Kind, RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::{
    GlesError, GlesMapping, GlesRenderer, GlesTarget, GlesTexture,
};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{
    Bind, Color32F, ErasedContextId, ExportMem, Frame, Offscreen, Renderer, Texture as _,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::shm;
use solid_color::{SolidColorBuffer, SolidColorRenderElement};

use self::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use self::texture::{TextureBuffer, TextureRenderElement};
use crate::backend::tty::{TtyOffscreen, TtyRenderer};
use crate::render_helpers::blend::{DEFAULT_REFERENCE_LUMINANCE, set_sdr_capture_blend};
use crate::render_helpers::renderer::{AsGlesRenderer, NiriRenderer};
use crate::render_helpers::xray::Xray;

pub mod background_effect;
pub mod blend;
pub mod blur;
#[cfg(feature = "vulkan")]
pub mod blur_vulkan;
pub mod border;
pub mod capture;
pub mod clipped_surface;
pub mod damage;
pub mod debug;
#[cfg(feature = "effects")]
pub mod decoration_light;
pub mod effect_buffer;
pub mod framebuffer_effect;
#[cfg(feature = "effects")]
pub mod global_shader_element;
pub mod gradient_fade_texture;
pub mod liquid_glass;
pub mod memory;
pub mod offscreen;
pub mod overview_rescale;
#[cfg(feature = "navigation")]
pub mod panel;
#[cfg(feature = "navigation")]
pub mod panel_quad;
pub mod primary_gpu_texture;
pub mod render_elements;
pub mod renderer;
pub mod resize;
pub mod resources;
#[cfg(feature = "effects")]
pub mod scoped_shader_element;
pub mod shader_element;
pub mod shaders;
pub mod shadow;
pub mod snapshot;
pub mod solid_color;
pub mod surface;
#[cfg(feature = "cursor")]
pub mod svg;
pub mod texture;
#[cfg(feature = "vulkan")]
pub mod vulkan;
pub mod xray;

/// A rendering context.
///
/// Bundles together things needed by most rendering code.
pub struct RenderCtx<'a, R> {
    pub renderer: &'a mut R,
    pub target: RenderTarget,
    pub intent: RenderIntent,
    pub xray: Option<&'a Xray>,
    /// Seconds since the compositor's window-shader time origin, bound as `niri_time` for
    /// per-window scoped shaders. `0.0` on incidental render paths that don't animate windows.
    pub shader_time: f32,
}

impl<'a, R> RenderCtx<'a, R> {
    /// Reborrows this context with a smaller lifetime.
    #[inline]
    pub fn r<'b>(&'b mut self) -> RenderCtx<'b, R> {
        RenderCtx {
            renderer: self.renderer,
            target: self.target,
            intent: self.intent,
            xray: self.xray,
            shader_time: self.shader_time,
        }
    }
}

impl<'a, R: AsGlesRenderer> RenderCtx<'a, R> {
    /// Reborrows the context for GLES-only code. Returns `None` on renderers
    /// without GLES (Vulkan), so elite GLSL-only paths degrade gracefully.
    pub fn as_gles<'b>(&'b mut self) -> Option<RenderCtx<'b, GlesRenderer>> {
        Some(RenderCtx {
            renderer: self.renderer.as_gles_renderer()?,
            target: self.target,
            intent: self.intent,
            xray: self.xray,
            shader_time: self.shader_time,
        })
    }
}

/// What we're rendering for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTarget {
    /// Rendering to display on screen.
    Output = 0,
    /// Rendering for a screencast.
    Screencast,
    /// Rendering for any other screen capture.
    ScreenCapture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderIntent {
    Normal,
    PickerPreview,
}

/// Buffer with location, src and dst.
#[derive(Debug)]
pub struct BakedBuffer<B> {
    pub buffer: B,
    pub location: Point<f64, Logical>,
    pub src: Option<Rectangle<f64, Logical>>,
    pub dst: Option<Size<i32, Logical>>,
}

/// Buffer backed by a wl_buffer.
#[derive(Clone)]
pub enum CaptureBuffer {
    // The Dmabuf that is contained inside the WlBuffer.
    Dma(Dmabuf),
    Shm(WlBuffer),
}

pub trait ToRenderElement {
    type RenderElement;

    fn to_render_element(
        &self,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        kind: Kind,
    ) -> Self::RenderElement;
}

impl RenderTarget {
    pub const COUNT: usize = 3;

    pub fn should_block_out(self, block_out_from: Option<BlockOutFrom>) -> bool {
        match block_out_from.map(|rule| rule.target) {
            None => false,
            Some(BlockOutTarget::Screencast) => self == RenderTarget::Screencast,
            Some(BlockOutTarget::ScreenCapture) => self != RenderTarget::Output,
        }
    }

    pub fn should_hide(self, block_out_from: Option<BlockOutFrom>) -> bool {
        self.should_block_out(block_out_from)
            && block_out_from.is_some_and(|rule| rule.kind == BlockOutKind::Hide)
    }
}

/// Whether shaders (global / region / window) should render for this target. Always on for the
/// real Output; on for capture targets (Screencast / ScreenCapture) only when opted in.
pub fn target_renders_shaders(target: RenderTarget, capture_enabled: bool) -> bool {
    target == RenderTarget::Output || capture_enabled
}

impl ToRenderElement for BakedBuffer<TextureBuffer<TtyOffscreen>> {
    type RenderElement = PrimaryGpuTextureRenderElement;

    fn to_render_element(
        &self,
        location: Point<f64, Logical>,
        _scale: Scale<f64>,
        alpha: f32,
        kind: Kind,
    ) -> Self::RenderElement {
        let elem = TextureRenderElement::from_texture_buffer(
            self.buffer.clone(),
            location + self.location,
            alpha,
            self.src,
            self.dst.map(|dst| dst.to_f64()),
            kind,
        );
        PrimaryGpuTextureRenderElement(elem)
    }
}

impl ToRenderElement for BakedBuffer<SolidColorBuffer> {
    type RenderElement = SolidColorRenderElement;

    fn to_render_element(
        &self,
        location: Point<f64, Logical>,
        _scale: Scale<f64>,
        alpha: f32,
        kind: Kind,
    ) -> Self::RenderElement {
        SolidColorRenderElement::from_buffer(&self.buffer, location + self.location, alpha, kind)
    }
}

pub fn encompassing_geo(
    scale: Scale<f64>,
    elements: impl Iterator<Item = impl Element>,
) -> Rectangle<i32, Physical> {
    elements
        .map(|ele| ele.geometry(scale))
        .reduce(|a, b| a.merge(b))
        .unwrap_or_default()
}

pub fn create_texture(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    fourcc: Fourcc,
) -> Result<GlesTexture, GlesError> {
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    renderer.create_buffer(fourcc, buffer_size)
}

pub fn copy_framebuffer(
    renderer: &mut GlesRenderer,
    target: &GlesTarget,
    fourcc: Fourcc,
) -> Result<GlesMapping, GlesError> {
    renderer.copy_framebuffer(target, Rectangle::from_size(target.size()), fourcc)
}

pub fn render_to_encompassing_texture(
    renderer: &mut GlesRenderer,
    scale: Scale<f64>,
    transform: Transform,
    fourcc: Fourcc,
    elements: &[impl RenderElement<GlesRenderer>],
) -> anyhow::Result<(GlesTexture, SyncPoint, Rectangle<i32, Physical>)> {
    let geo = encompassing_geo(scale, elements.iter());
    let elements = elements.iter().rev().map(|ele| {
        RelocateRenderElement::from_element(ele, geo.loc.upscale(-1), Relocate::Relative)
    });

    let (texture, sync_point) =
        render_to_texture(renderer, geo.size, scale, transform, fourcc, elements)?;

    Ok((texture, sync_point, geo))
}

pub fn render_to_texture(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    fourcc: Fourcc,
    elements: impl Iterator<Item = impl RenderElement<GlesRenderer>>,
) -> anyhow::Result<(GlesTexture, SyncPoint)> {
    let _span = tracy_client::span!();

    let mut texture = create_texture(renderer, size, fourcc).context("error creating texture")?;

    let sync_point = {
        let mut target = renderer
            .bind(&mut texture)
            .context("error binding texture")?;

        render_elements(renderer, &mut target, size, scale, transform, elements)?
    };

    Ok((texture, sync_point))
}

pub fn render_and_download(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    fourcc: Fourcc,
    elements: impl Iterator<Item = impl RenderElement<GlesRenderer>>,
) -> anyhow::Result<GlesMapping> {
    let _span = tracy_client::span!();

    let mut texture = create_texture(renderer, size, fourcc).context("error creating texture")?;
    let mut target = renderer
        .bind(&mut texture)
        .context("error binding texture")?;

    let _sync = render_elements(renderer, &mut target, size, scale, transform, elements)
        .context("error rendering")?;

    copy_framebuffer(renderer, &target, fourcc).context("error copying framebuffer")
}

pub fn render_and_download_with_damage(
    renderer: &mut GlesRenderer,
    damage_tracker: &mut OutputDamageTracker,
    fourcc: Fourcc,
    elements: &[impl RenderElement<GlesRenderer>],
    states: RenderElementStates,
) -> anyhow::Result<GlesMapping> {
    let _span = tracy_client::span!();

    let (size, _scale, _transform) = damage_tracker.mode().try_into().unwrap();
    let mut texture = create_texture(renderer, size, fourcc).context("error creating texture")?;
    let mut target = renderer
        .bind(&mut texture)
        .context("error binding texture")?;

    let _res = damage_tracker
        .render_output_with_states(
            renderer,
            &mut target,
            0,
            elements,
            Color32F::TRANSPARENT,
            states,
        )
        .context("error rendering")?;

    copy_framebuffer(renderer, &target, fourcc).context("error copying framebuffer")
}

pub fn render_to_vec(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    fourcc: Fourcc,
    elements: impl Iterator<Item = impl RenderElement<GlesRenderer>>,
) -> anyhow::Result<Vec<u8>> {
    let _span = tracy_client::span!();

    let mapping = render_and_download(renderer, size, scale, transform, fourcc, elements)
        .context("error rendering")?;
    let copy = renderer
        .map_texture(&mapping)
        .context("error mapping texture")?;
    Ok(copy.to_vec())
}

/// Renders `elements` into a backend-agnostic offscreen texture covering `bounds`.
///
/// Unlike [`render_to_texture`], this works for every [`NiriRenderer`]: the render goes through
/// the generic [`offscreen::OffscreenBuffer`], which stores a [`TtyOffscreen`]. A GLES renderer
/// produces `TtyOffscreen::Gles`, a Vulkan renderer `TtyOffscreen::Vulkan`.
///
/// `elements` must be in front-to-back order, like the vectors returned by `Niri::render_to_vec`.
pub fn render_to_tty_texture<R: NiriRenderer>(
    renderer: &mut R,
    bounds: Rectangle<i32, Physical>,
    scale: Scale<f64>,
    elements: &[impl RenderElement<R>],
) -> anyhow::Result<(TtyOffscreen, ErasedContextId, SyncPoint)> {
    let _span = tracy_client::span!();

    let buffer = offscreen::OffscreenBuffer::default();
    let (element, sync, _data) = buffer
        .render_with_bounds(renderer, scale, bounds, elements)
        .context("error rendering to offscreen texture")?;

    Ok((element.texture().clone(), element.context_id(), sync))
}

/// Renders `elements` into a backend-agnostic offscreen texture encompassing their geometry.
///
/// The returned offset is where the texture should be placed in logical output coordinates.
/// `elements` must be in front-to-back order.
pub fn render_to_encompassing_tty_texture<R: NiriRenderer>(
    renderer: &mut R,
    scale: Scale<f64>,
    elements: &[impl RenderElement<R>],
) -> anyhow::Result<(TtyOffscreen, ErasedContextId, Point<f64, Logical>)> {
    let _span = tracy_client::span!();

    let buffer = offscreen::OffscreenBuffer::default();
    let (element, _sync, _data) = buffer
        .render(renderer, scale, elements)
        .context("error rendering to encompassing offscreen texture")?;

    Ok((
        element.texture().clone(),
        element.context_id(),
        element.offset(),
    ))
}

/// Reads a backend-agnostic offscreen texture back into CPU memory.
///
/// Uses the concrete GLES or Vulkan renderer behind `renderer`, so it works for both the plain
/// renderers and the TTY renderer abstraction.
pub fn copy_tty_texture_to_vec<R: NiriRenderer>(
    renderer: &mut R,
    texture: &TtyOffscreen,
    region: Rectangle<i32, smithay::utils::Buffer>,
    fourcc: Fourcc,
) -> anyhow::Result<Vec<u8>> {
    let _span = tracy_client::span!();

    if let Some(gles) = renderer.as_gles_renderer() {
        let TtyOffscreen::Gles(texture) = texture else {
            bail!("trying to read a non-GLES texture with the GLES renderer");
        };

        let mapping = gles
            .copy_texture(texture, region, fourcc)
            .context("error copying GLES texture")?;
        let copy = gles
            .map_texture(&mapping)
            .context("error mapping GLES texture")?;
        return Ok(copy.to_vec());
    }

    #[cfg(feature = "vulkan")]
    if let Some(vulkan) = renderer.as_vulkan_renderer() {
        let TtyOffscreen::Vulkan(texture) = texture else {
            bail!("trying to read a non-Vulkan texture with the Vulkan renderer");
        };

        let mapping = vulkan
            .copy_texture(texture, region, fourcc)
            .context("error copying Vulkan texture")?;
        let copy = vulkan
            .map_texture(&mapping)
            .context("error mapping Vulkan texture")?;
        return Ok(copy.to_vec());
    }

    bail!("renderer does not support texture readback")
}

/// Applies the SDR capture blend to the concrete backend behind `renderer`.
///
/// Capture buffers always hold SDR content on both backends; on Vulkan this also carries the
/// reference luminance used to tone-map HDR scene content back into SDR.
pub fn set_capture_blend<R: NiriRenderer>(renderer: &mut R, reference_luminance: f64) {
    if let Some(gles) = renderer.as_gles_renderer() {
        set_sdr_capture_blend(gles, reference_luminance);
    }
    #[cfg(feature = "vulkan")]
    if let Some(vulkan) = renderer.as_vulkan_renderer() {
        crate::render_helpers::blend::set_sdr_capture_blend_vulkan(vulkan, reference_luminance);
    }
}

/// Renders `elements` with the state of `damage_tracker` into a fresh offscreen texture and reads
/// it back as `fourcc` pixels.
///
/// Returns `None` when the tracker produced no damage and the render was skipped, mirroring the
/// GLES direct-to-target path where the client buffer is left untouched in that case.
pub(crate) fn render_capture_frame_to_vec<R: NiriRenderer>(
    renderer: &mut R,
    damage_tracker: &mut OutputDamageTracker,
    elements: &[impl RenderElement<R>],
    states: RenderElementStates,
    fourcc: Fourcc,
) -> anyhow::Result<Option<Vec<u8>>> {
    let (size, _scale, _transform) = damage_tracker.mode().try_into().unwrap();
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);

    let mut texture = renderer
        .create_buffer(fourcc, buffer_size)
        .context("error creating capture texture")?;

    let result = {
        let mut target = renderer
            .bind(&mut texture)
            .context("error binding capture texture")?;
        damage_tracker
            .render_output_with_states(
                renderer,
                &mut target,
                0,
                elements,
                Color32F::TRANSPARENT,
                states,
            )
            .context("error rendering capture frame")?
    };
    if result.damage.is_none() {
        return Ok(None);
    }

    let texture = R::wrap_offscreen(texture);
    let pixels = copy_tty_texture_to_vec(
        renderer,
        &texture,
        Rectangle::from_size(buffer_size),
        fourcc,
    )?;
    Ok(Some(pixels))
}

/// A renderer that can render capture frames into client buffers.
///
/// This exists because the dma-buf path cannot be a plain generic function over
/// [`NiriRenderer`]: the renderer binds a dma-buf through [`Bind`], and that trait does not relate
/// `Bind::Framebuffer` to `Renderer::Framebuffer`. The two concrete capture renderers of niri
/// (GLES and the TTY renderer, which covers both backends) implement this trait, so generic
/// capture paths only need `R: CaptureRenderer` while `render_output_with_states()` still gets
/// the right target type.
///
/// GLES keeps its direct-to-client-buffer paths; the TTY/Vulkan implementation uses
/// [`render_to_tty_shm`] for `wl_shm` targets and imports dma-bufs as render targets.
pub trait CaptureRenderer: NiriRenderer + Sized {
    fn render_capture_to_dmabuf(
        &mut self,
        damage_tracker: &mut OutputDamageTracker,
        dmabuf: Dmabuf,
        elements: &[impl RenderElement<Self>],
        states: RenderElementStates,
        reference_luminance: f64,
    ) -> anyhow::Result<SyncPoint>;

    /// Renders a capture frame into a client `wl_shm` buffer.
    ///
    /// GLES keeps its direct-to-buffer render; other backends use the generic offscreen readback
    /// in [`render_to_tty_shm`].
    fn render_capture_to_shm(
        &mut self,
        damage_tracker: &mut OutputDamageTracker,
        buffer: &WlBuffer,
        elements: &[impl RenderElement<Self>],
        states: RenderElementStates,
        reference_luminance: f64,
    ) -> anyhow::Result<()>;

    /// Clears a dma-buf capture buffer to transparent black.
    fn clear_capture_dmabuf(&mut self, dmabuf: Dmabuf) -> anyhow::Result<SyncPoint>;
}

impl CaptureRenderer for GlesRenderer {
    fn render_capture_to_dmabuf(
        &mut self,
        damage_tracker: &mut OutputDamageTracker,
        dmabuf: Dmabuf,
        elements: &[impl RenderElement<Self>],
        states: RenderElementStates,
        reference_luminance: f64,
    ) -> anyhow::Result<SyncPoint> {
        render_to_dmabuf(
            self,
            damage_tracker,
            dmabuf,
            elements,
            states,
            reference_luminance,
        )
    }

    fn render_capture_to_shm(
        &mut self,
        damage_tracker: &mut OutputDamageTracker,
        buffer: &WlBuffer,
        elements: &[impl RenderElement<Self>],
        states: RenderElementStates,
        reference_luminance: f64,
    ) -> anyhow::Result<()> {
        render_to_shm(
            self,
            damage_tracker,
            buffer,
            elements,
            states,
            reference_luminance,
        )
    }

    fn clear_capture_dmabuf(&mut self, dmabuf: Dmabuf) -> anyhow::Result<SyncPoint> {
        clear_dmabuf(self, dmabuf)
    }
}

impl CaptureRenderer for TtyRenderer<'_> {
    fn render_capture_to_dmabuf(
        &mut self,
        damage_tracker: &mut OutputDamageTracker,
        mut dmabuf: Dmabuf,
        elements: &[impl RenderElement<Self>],
        states: RenderElementStates,
        reference_luminance: f64,
    ) -> anyhow::Result<SyncPoint> {
        let _span = tracy_client::span!();
        set_capture_blend(self, reference_luminance);

        let (size, _scale, _transform) = damage_tracker.mode().try_into().unwrap();
        ensure!(
            dmabuf.width() == size.w as u32 && dmabuf.height() == size.h as u32,
            "invalid buffer size"
        );

        let mut target = self.bind(&mut dmabuf).context("error binding dmabuf")?;
        let result = damage_tracker
            .render_output_with_states(
                self,
                &mut target,
                0,
                elements,
                Color32F::TRANSPARENT,
                states,
            )
            .context("error rendering to dmabuf")?;
        Ok(result.sync)
    }

    fn render_capture_to_shm(
        &mut self,
        damage_tracker: &mut OutputDamageTracker,
        buffer: &WlBuffer,
        elements: &[impl RenderElement<Self>],
        states: RenderElementStates,
        reference_luminance: f64,
    ) -> anyhow::Result<()> {
        render_to_tty_shm(
            self,
            damage_tracker,
            buffer,
            elements,
            states,
            reference_luminance,
        )
    }

    fn clear_capture_dmabuf(&mut self, mut dmabuf: Dmabuf) -> anyhow::Result<SyncPoint> {
        let size = dmabuf.size();
        let size = size.to_logical(1, Transform::Normal).to_physical(1);
        let mut target = self.bind(&mut dmabuf).context("error binding dmabuf")?;
        let mut frame = self
            .render(&mut target, size, Transform::Normal)
            .context("error starting frame")?;
        frame
            .clear(Color32F::TRANSPARENT, &[Rectangle::from_size(size)])
            .context("error clearing")?;
        frame.finish().context("error finishing frame")
    }
}

/// Generic counterpart of [`render_to_shm`] for any [`NiriRenderer`].
///
/// The frame is rendered to an offscreen texture on the active backend, read back, and copied
/// into the client's `wl_shm` pool honoring the client-chosen format, stride and pool offset.
/// On Vulkan this is the only option because the renderer cannot bind a client `wl_shm` buffer
/// as a render target; [`CaptureRenderer`] keeps GLES on the original direct path.
pub fn render_to_tty_shm<R: NiriRenderer>(
    renderer: &mut R,
    damage_tracker: &mut OutputDamageTracker,
    buffer: &WlBuffer,
    elements: &[impl RenderElement<R>],
    states: RenderElementStates,
    reference_luminance: f64,
) -> anyhow::Result<()> {
    let _span = tracy_client::span!();
    set_capture_blend(renderer, reference_luminance);

    shm::with_buffer_contents_mut(buffer, |pool, pool_len, buffer_data| {
        let (size, _scale, _transform) = damage_tracker.mode().try_into().unwrap();
        let fourcc = match buffer_data.format {
            wl_shm::Format::Xrgb8888 => Fourcc::Xrgb8888,
            wl_shm::Format::Argb8888 => Fourcc::Argb8888,
            _ => bail!("unsupported shm format: {:?}", buffer_data.format),
        };

        ensure!(
            buffer_data.width == size.w && buffer_data.height == size.h,
            "invalid buffer size"
        );

        let row_len = size.w as usize * 4;
        let height = size.h as usize;
        let offset = usize::try_from(buffer_data.offset).context("negative buffer offset")?;
        let stride = usize::try_from(buffer_data.stride).context("negative buffer stride")?;

        let end = stride
            .checked_mul(height.saturating_sub(1))
            .and_then(|len| len.checked_add(row_len))
            .and_then(|len| len.checked_add(offset));
        ensure!(
            stride >= row_len && end.is_some_and(|end| end <= pool_len),
            "buffer does not fit in its shm pool"
        );

        let Some(bytes) =
            render_capture_frame_to_vec(renderer, damage_tracker, elements, states, fourcc)
                .context("error rendering capture frame")?
        else {
            // No damage: leave the client's buffer untouched, like the GLES path.
            return Ok(());
        };

        ensure!(bytes.len() >= row_len * height, "short texture mapping");

        // SAFETY: `pool`, `offset`, `stride` and `height` were validated against `pool_len`
        // above, and `bytes` is at least `row_len * height` long.
        unsafe {
            copy_frame_to_shm_pool(&bytes, pool, offset, stride, row_len, height);
        }

        Ok(())
    })
    .context("expected shm buffer, but didn't get one")?
}

/// Copies a tightly packed frame into the destination region of a client shm pool.
///
/// The client chooses the pool offset and stride, and rows may have padding, so only the first
/// `row_len` bytes of each source row are copied.
///
/// # Safety
/// `dst` must be valid for writes covering
/// `offset + stride * (height - 1) + row_len` bytes.
unsafe fn copy_frame_to_shm_pool(
    bytes: &[u8],
    dst: *mut u8,
    offset: usize,
    stride: usize,
    row_len: usize,
    height: usize,
) {
    let _span = tracy_client::span!("copy_nonoverlapping");
    let dst = unsafe { dst.add(offset) };
    if stride == row_len {
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), dst, row_len * height) };
    } else {
        for y in 0..height {
            unsafe {
                ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(y * row_len),
                    dst.add(y * stride),
                    row_len,
                );
            }
        }
    }
}

pub fn render_to_dmabuf(
    renderer: &mut GlesRenderer,
    damage_tracker: &mut OutputDamageTracker,
    mut dmabuf: Dmabuf,
    elements: &[impl RenderElement<GlesRenderer>],
    states: RenderElementStates,
    reference_luminance: f64,
) -> anyhow::Result<SyncPoint> {
    let _span = tracy_client::span!();
    set_sdr_capture_blend(renderer, reference_luminance);
    let (size, _scale, _transform) = damage_tracker.mode().try_into().unwrap();
    ensure!(
        dmabuf.width() == size.w as u32 && dmabuf.height() == size.h as u32,
        "invalid buffer size"
    );

    let mut target = renderer.bind(&mut dmabuf).context("error binding dmabuf")?;
    let res = damage_tracker
        .render_output_with_states(
            renderer,
            &mut target,
            0,
            elements,
            Color32F::TRANSPARENT,
            states,
        )
        .context("error rendering to dmabuf")?;
    Ok(res.sync)
}

pub fn render_to_shm(
    renderer: &mut GlesRenderer,
    damage_tracker: &mut OutputDamageTracker,
    buffer: &WlBuffer,
    elements: &[impl RenderElement<GlesRenderer>],
    states: RenderElementStates,
    reference_luminance: f64,
) -> anyhow::Result<()> {
    let _span = tracy_client::span!();
    set_sdr_capture_blend(renderer, reference_luminance);

    // The pointer and length are the client's entire pool, which may hold other
    // buffers besides this one... buffer_data is the one we want.
    shm::with_buffer_contents_mut(buffer, |pool, pool_len, buffer_data| {
        let (size, _scale, _transform) = damage_tracker.mode().try_into().unwrap();
        let fourcc = match buffer_data.format {
            wl_shm::Format::Xrgb8888 => Fourcc::Xrgb8888,
            wl_shm::Format::Argb8888 => Fourcc::Argb8888,
            _ => bail!("unsupported shm format: {:?}", buffer_data.format),
        };

        ensure!(
            // The buffer prefers pixels in little endian ...
            buffer_data.width == size.w && buffer_data.height == size.h,
            "invalid buffer size"
        );

        // The client chooses the stride and may pad rows, so only the first
        // row_len bytes can be used here.
        let row_len = size.w as usize * 4;
        let height = size.h as usize;
        let offset = usize::try_from(buffer_data.offset).context("negative buffer offset")?;
        let stride = usize::try_from(buffer_data.stride).context("negative buffer stride")?;

        // This should have already been validated by wl_shm, and a pool can
        // only grow, but check again just in case.
        let end = stride
            .checked_mul(height.saturating_sub(1))
            .and_then(|len| len.checked_add(row_len))
            .and_then(|len| len.checked_add(offset));
        ensure!(
            stride >= row_len && end.is_some_and(|end| end <= pool_len),
            "buffer does not fit in its shm pool"
        );

        let mut texture =
            create_texture(renderer, size, fourcc).context("error creating texture")?;
        let mut target = renderer
            .bind(&mut texture)
            .context("error binding texture")?;

        let _res = damage_tracker
            .render_output_with_states(
                renderer,
                &mut target,
                0,
                elements,
                Color32F::TRANSPARENT,
                states,
            )
            .context("error rendering")?;

        let mapping =
            copy_framebuffer(renderer, &target, fourcc).context("error copying framebuffer")?;
        let bytes = renderer
            .map_texture(&mapping)
            .context("error mapping texture")?;

        ensure!(bytes.len() >= row_len * height, "short texture mapping");

        unsafe {
            let _span = tracy_client::span!("copy_nonoverlapping");
            let dst = pool.add(offset);
            if stride == row_len {
                ptr::copy_nonoverlapping(bytes.as_ptr(), dst, row_len * height);
            } else {
                for y in 0..height {
                    ptr::copy_nonoverlapping(
                        bytes.as_ptr().add(y * row_len),
                        dst.add(y * stride),
                        row_len,
                    );
                }
            }
        }

        Ok(())
    })
    .context("expected shm buffer, but didn't get one")?
}

pub fn clear_dmabuf(renderer: &mut GlesRenderer, mut dmabuf: Dmabuf) -> anyhow::Result<SyncPoint> {
    let size = dmabuf.size();
    let size = size.to_logical(1, Transform::Normal).to_physical(1);
    let mut target = renderer.bind(&mut dmabuf).context("error binding dmabuf")?;
    let mut frame = renderer
        .render(&mut target, size, Transform::Normal)
        .context("error starting frame")?;
    frame
        .clear(Color32F::TRANSPARENT, &[Rectangle::from_size(size)])
        .context("error clearing")?;
    frame.finish().context("error finishing frame")
}

fn render_elements(
    renderer: &mut GlesRenderer,
    target: &mut GlesTarget,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    elements: impl Iterator<Item = impl RenderElement<GlesRenderer>>,
) -> anyhow::Result<SyncPoint> {
    let transform = transform.invert();
    let output_rect = Rectangle::from_size(transform.transform_size(size));

    let mut frame = renderer
        .render(target, size, transform)
        .context("error starting frame")?;

    frame
        .clear(Color32F::TRANSPARENT, &[output_rect])
        .context("error clearing")?;

    for element in elements {
        let src = element.src();
        let dst = element.geometry(scale);

        if let Some(mut damage) = output_rect.intersection(dst) {
            damage.loc -= dst.loc;

            let cache = UserDataMap::new();
            if element.is_framebuffer_effect() {
                element
                    .capture_framebuffer(&mut frame, src, dst, &cache)
                    .context("error in capture_framebuffer()")?;
            }
            element
                .draw(&mut frame, src, dst, &[damage], &[], Some(&cache))
                .context("error drawing element")?;
        }
    }

    frame.finish().context("error finishing frame")
}

impl CaptureBuffer {
    pub fn render<R>(
        &self,
        renderer: &mut R,
        damage_tracker: &mut OutputDamageTracker,
        elements: &[impl RenderElement<R>],
        states: RenderElementStates,
    ) -> anyhow::Result<SyncPoint>
    where
        R: CaptureRenderer,
    {
        match self {
            CaptureBuffer::Dma(dmabuf) => {
                let sync = renderer
                    .render_capture_to_dmabuf(
                        damage_tracker,
                        dmabuf.clone(),
                        elements,
                        states,
                        DEFAULT_REFERENCE_LUMINANCE,
                    )
                    .context("error rendering to screencopy dmabuf")?;
                Ok(sync)
            }
            CaptureBuffer::Shm(wl_buffer) => {
                renderer
                    .render_capture_to_shm(
                        damage_tracker,
                        wl_buffer,
                        elements,
                        states,
                        DEFAULT_REFERENCE_LUMINANCE,
                    )
                    .context("error rendering to screencopy shm buffer")?;
                Ok(SyncPoint::signaled())
            }
        }
    }
}

/// Real-renderer tests for the backend-agnostic screenshot/readback plumbing used by the
/// portal's full-desktop and area captures. They skip silently on machines without a usable
/// Vulkan device.
#[cfg(all(test, feature = "vulkan"))]
mod capture_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::Texture as _;
    use smithay::backend::renderer::damage::OutputDamageTracker;
    use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
    use smithay::backend::renderer::element::{Kind, RenderElement};
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{
        Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform,
    };

    use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
    use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
    use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
    use crate::render_helpers::{
        copy_frame_to_shm_pool, copy_tty_texture_to_vec, encompassing_geo,
        render_capture_frame_to_vec, render_to_encompassing_tty_texture, render_to_tty_texture,
    };

    fn vulkan_renderer() -> Option<VulkanRenderer> {
        let instance = Instance::new(Version::VERSION_1_3, None).ok()?;
        let devices = PhysicalDevice::enumerate(&instance).ok()?;
        for device in devices {
            if let Ok(renderer) = VulkanRenderer::new(&device) {
                return Some(renderer);
            }
        }
        None
    }

    /// Little-endian RGBA byte order for `Fourcc::Abgr8888`.
    fn assert_rgba(pixels: &[u8], expected: [u8; 3], what: &str) {
        assert!(!pixels.is_empty(), "{what}: empty readback");
        assert_eq!(pixels.len() % 4, 0, "{what}: not RGBA8");
        for px in pixels.as_chunks::<4>().0 {
            assert_eq!(&px[..3], &expected, "{what}: unexpected pixel {px:?}");
        }
    }

    #[test]
    fn vulkan_full_output_capture_readback() {
        let Some(mut renderer) = vulkan_renderer() else {
            return;
        };

        let size = Size::<i32, Physical>::from((8, 8));
        let scale = Scale::<f64>::from(1.);
        let rect = Rectangle::from_size(size);

        let render_solid = |renderer: &mut VulkanRenderer, color: [f32; 4]| {
            let buffer = SolidColorBuffer::new((8., 8.), color);
            let elem =
                SolidColorRenderElement::from_buffer(&buffer, (0., 0.), 1., Kind::Unspecified);
            render_to_tty_texture(renderer, rect, scale, &[elem]).expect("render solid output")
        };

        let (red, red_ctx, _) = render_solid(&mut renderer, [1., 0., 0., 1.]);
        let (blue, blue_ctx, _) = render_solid(&mut renderer, [0., 0., 1., 1.]);
        assert_eq!(
            red.size(),
            Size::<i32, BufferCoords>::from((8, 8)),
            "unexpected capture texture size"
        );

        // Mirror `Niri::screenshot_all_outputs`: place the per-output textures side by side and
        // combine them into one encompassing texture.
        let textures = vec![
            PrimaryGpuTextureRenderElement(TextureRenderElement::from_texture_buffer(
                TextureBuffer::from_tty_texture(red, red_ctx, scale, Transform::Normal, Vec::new()),
                (0., 0.),
                1.,
                None,
                None,
                Kind::Unspecified,
            )),
            PrimaryGpuTextureRenderElement(TextureRenderElement::from_texture_buffer(
                TextureBuffer::from_tty_texture(
                    blue,
                    blue_ctx,
                    scale,
                    Transform::Normal,
                    Vec::new(),
                ),
                (8., 0.),
                1.,
                None,
                None,
                Kind::Unspecified,
            )),
        ];

        let geo = encompassing_geo(scale, textures.iter());
        assert_eq!(
            geo,
            Rectangle::new(Point::from((0, 0)), Size::from((16, 8)))
        );

        let (texture, _ctx, _offset) =
            render_to_encompassing_tty_texture(&mut renderer, scale, &textures)
                .expect("combine outputs");
        let pixels = copy_tty_texture_to_vec(
            &mut renderer,
            &texture,
            Rectangle::from_size(Size::<i32, BufferCoords>::from((16, 8))),
            Fourcc::Abgr8888,
        )
        .expect("read back combined output");

        assert_eq!(pixels.len(), 16 * 8 * 4);
        let pixel = |x: usize, y: usize| &pixels[(y * 16 + x) * 4..][..4];
        assert_eq!(pixel(0, 0), &[255, 0, 0, 255], "left output should be red");
        assert_eq!(
            pixel(15, 0),
            &[0, 0, 255, 255],
            "right output should be blue"
        );
    }

    #[test]
    fn vulkan_region_capture_readback() {
        let Some(mut renderer) = vulkan_renderer() else {
            return;
        };

        let scale = Scale::<f64>::from(1.);
        let red = SolidColorBuffer::new((16., 16.), [1., 0., 0., 1.]);
        let blue = SolidColorBuffer::new((8., 16.), [0., 0., 1., 1.]);
        // Front-to-back order, as produced by `Niri::render_to_vec()`.
        let elements = [
            SolidColorRenderElement::from_buffer(&blue, (8., 0.), 1., Kind::Unspecified),
            SolidColorRenderElement::from_buffer(&red, (0., 0.), 1., Kind::Unspecified),
        ];

        let read_region = |renderer: &mut VulkanRenderer, origin: Point<i32, Physical>| {
            let bounds = Rectangle::new(origin, Size::from((8, 8)));
            let (texture, _ctx, _sync) =
                render_to_tty_texture(renderer, bounds, scale, &elements).expect("render region");
            copy_tty_texture_to_vec(
                renderer,
                &texture,
                Rectangle::from_size(Size::<i32, BufferCoords>::from((8, 8))),
                Fourcc::Abgr8888,
            )
            .expect("read back region")
        };

        // The capture origin offsets the rendered elements; this is what
        // `Niri::capture_pick_color_region` and the portal area crop rely on.
        let left = read_region(&mut renderer, Point::from((0, 4)));
        assert_rgba(&left, [255, 0, 0], "left region");

        let right = read_region(&mut renderer, Point::from((8, 4)));
        assert_rgba(&right, [0, 0, 255], "right region");
    }

    /// Renders one frame through the damage tracker used by the capture paths and reads it back.
    fn capture_frame(
        renderer: &mut VulkanRenderer,
        size: Size<i32, Physical>,
        scale: Scale<f64>,
        elements: &[impl RenderElement<VulkanRenderer>],
        fourcc: Fourcc,
    ) -> Vec<u8> {
        let mut tracker = OutputDamageTracker::new(size, scale, Transform::Normal);
        let (damage, states) = tracker.damage_output(1, elements).unwrap();
        assert!(damage.is_some(), "fresh tracker must report damage");
        render_capture_frame_to_vec(renderer, &mut tracker, elements, states, fourcc)
            .expect("render capture frame")
            .expect("fresh tracker must not skip the render")
    }

    #[test]
    fn vulkan_output_capture_into_client_shaped_shm() {
        let Some(mut renderer) = vulkan_renderer() else {
            return;
        };

        let size = Size::<i32, Physical>::from((8, 8));
        let scale = Scale::<f64>::from(1.);
        let green = SolidColorBuffer::new((8., 8.), [0., 1., 0., 1.]);
        let elements = [SolidColorRenderElement::from_buffer(
            &green,
            (0., 0.),
            1.,
            Kind::Unspecified,
        )];

        // Capture as XRGB8888, the wlr-screencopy SHM offering.
        let bytes = capture_frame(&mut renderer, size, scale, &elements, Fourcc::Xrgb8888);
        assert_eq!(bytes.len(), 8 * 8 * 4);

        // Lay the frame out like a client wl_shm buffer: a non-zero pool offset, rows padded to
        // a larger client-chosen stride, and guard bytes around the used region.
        let row_len = 8 * 4;
        let stride = 40;
        let offset = 16;
        let mut pool = vec![0x5Au8; offset + stride * 8 + 16];
        // SAFETY: `pool` is large enough for `offset + stride * (height - 1) + row_len`.
        unsafe {
            copy_frame_to_shm_pool(&bytes, pool.as_mut_ptr(), offset, stride, row_len, 8);
        }

        let pixel = |x: usize, y: usize| &pool[offset + y * stride + x * 4..][..4];
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(
                    &pixel(x, y)[..3],
                    &[0, 255, 0],
                    "pixel ({x}, {y}) should be green"
                );
            }
            assert_eq!(
                &pool[offset + y * stride + row_len..offset + (y + 1) * stride],
                &[0x5A; 8],
                "row padding must stay untouched"
            );
        }
        assert!(pool[..offset].iter().all(|b| *b == 0x5A));
        assert!(pool[offset + stride * 8..].iter().all(|b| *b == 0x5A));
    }

    #[test]
    fn vulkan_region_and_window_capture_offsets() {
        let Some(mut renderer) = vulkan_renderer() else {
            return;
        };

        let scale = Scale::<f64>::from(1.);
        let size = Size::<i32, Physical>::from((8, 8));
        let red = SolidColorBuffer::new((8., 8.), [1., 0., 0., 1.]);
        let blue = SolidColorBuffer::new((8., 8.), [0., 0., 1., 1.]);
        let red = SolidColorRenderElement::from_buffer(&red, (8., 0.), 1., Kind::Unspecified);
        let blue = SolidColorRenderElement::from_buffer(&blue, (8., 0.), 1., Kind::Unspecified);

        // Region capture: `render_for_screencopy_*` relocates the output-local elements by
        // `-region_loc` and tracks a region-sized buffer.
        let shifted_blue =
            RelocateRenderElement::from_element(&blue, Point::from((-8, 0)), Relocate::Relative);
        let bytes = capture_frame(
            &mut renderer,
            size,
            scale,
            std::slice::from_ref(&shifted_blue),
            Fourcc::Abgr8888,
        );
        assert_rgba(&bytes, [0, 0, 255], "region at x=8");

        // Window capture: window content lives at the window bbox origin and is rendered into a
        // bbox-sized buffer with the same relocation.
        let shifted_red =
            RelocateRenderElement::from_element(&red, Point::from((-8, 0)), Relocate::Relative);
        let bytes = capture_frame(
            &mut renderer,
            size,
            scale,
            std::slice::from_ref(&shifted_red),
            Fourcc::Abgr8888,
        );
        assert_rgba(&bytes, [255, 0, 0], "window at bbox x=8");
    }

    #[test]
    fn vulkan_screencast_frame_generation() {
        let Some(mut renderer) = vulkan_renderer() else {
            return;
        };

        let size = Size::<i32, Physical>::from((16, 16));
        let scale = Scale::<f64>::from(1.);
        let background = SolidColorBuffer::new((16., 16.), [0., 0., 1., 1.]);
        let cursor = SolidColorBuffer::new((4., 4.), [1., 0., 0., 1.]);

        // Front-to-back order, like `CastRenderElement`: the cursor first, main content after.
        let elements = [
            SolidColorRenderElement::from_buffer(&cursor, (8., 8.), 1., Kind::Unspecified),
            SolidColorRenderElement::from_buffer(&background, (0., 0.), 1., Kind::Unspecified),
        ];

        let bytes = capture_frame(&mut renderer, size, scale, &elements, Fourcc::Xrgb8888);
        assert_eq!(bytes.len(), 16 * 16 * 4);

        // Screencast SHM buffers are tightly packed; reuse the client copy for the layout.
        let row_len = 16 * 4;
        let mut cast_buf = vec![0u8; row_len * 16];
        // SAFETY: `cast_buf` is exactly `row_len * height` bytes.
        unsafe {
            copy_frame_to_shm_pool(&bytes, cast_buf.as_mut_ptr(), 0, row_len, row_len, 16);
        }

        // XRGB8888 is little-endian BGRA on the wire.
        let pixel = |x: usize, y: usize| &cast_buf[(y * 16 + x) * 4..][..4];
        assert_eq!(&pixel(0, 0)[..3], &[255, 0, 0], "background should be blue");
        assert_eq!(&pixel(8, 8)[..3], &[0, 0, 255], "cursor should be red");
        assert_eq!(
            &pixel(15, 15)[..3],
            &[255, 0, 0],
            "background should be blue"
        );
    }
}
