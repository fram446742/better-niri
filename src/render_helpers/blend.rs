//! Per-output blend space for windowed HDR support.
//!
//! An output is either SDR (electrical sRGB, the default) or HDR (the framebuffer holds
//! PQ/BT.2020 electrical values and the connector is signalled accordingly). On HDR outputs,
//! SDR content is encoded into the blend space at draw time by the shaders' `niri_blend`
//! stage; surfaces that already carry an HDR image description pass through numerically.
//!
//! Blending happens directly in PQ-encoded space. Alpha blending in an encoded space is an
//! approximation (the same class of error as regular sRGB-space blending).

use std::cell::Cell;

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{
    CustomUniformValue, OwnedCustomUniform, VulkanFrame, VulkanRenderer,
};
use smithay::backend::renderer::{Color32F, ImportAll, Renderer};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};
use smithay::wayland::color::management::{
    ImageDescription, Primaries as CmPrimaries, TransferFunction as CmTransferFunction,
};

use super::renderer::AsGlesFrame as _;
use super::shaders::Shaders;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

/// Default SDR reference white in cd/m² (BT.2408).
pub const DEFAULT_REFERENCE_LUMINANCE: f64 = 203.;

/// How a surface's content relates to the output blend space, derived from its committed
/// image description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentColor {
    /// Electrical sRGB content; encoded into the blend space on HDR outputs.
    #[default]
    Sdr,
    /// Client content already encoded as HDR PQ in the output blend space.
    /// Passes through numerically on HDR frames, and is converted back to SDR for capture.
    HdrPq,
    /// Extended-linear content: Windows scRGB or a parametric `ext_linear` image description
    /// (what Mesa's Vulkan WSI attaches for `VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT`
    /// swapchains). Linear light where encoded 1.0 = `max_lum` cd/m²; encoded into the blend
    /// space with the fixed absolute mapping. Immune to tone mapping by definition: only
    /// clamped to the output volume, never rescaled by the SDR reference luminance.
    ///
    /// Unlike other content this is also transformed on SDR outputs (reference white anchored
    /// to display white, HDR headroom clamped away): the raw linear values would otherwise
    /// blow bright colors out to white in the framebuffer.
    Linear {
        /// The container primaries are BT.2020 rather than BT.709/sRGB.
        bt2020: bool,
        /// Luminance of encoded 1.0 in cd/m² (80 for scRGB and default ext_linear).
        max_lum: u32,
        /// Reference white luminance in cd/m² (203 for scRGB, 80 for default ext_linear).
        ref_lum: u32,
    },
}

impl ContentColor {
    /// The content color of a surface with the given committed image description.
    pub fn from_description(desc: Option<ImageDescription>) -> Self {
        let Some(desc) = desc else {
            return ContentColor::Sdr;
        };
        if desc.windows_scrgb || desc.transfer == CmTransferFunction::ExtLinear {
            let (_, max_lum, ref_lum) = desc.luminances_or_default();
            return ContentColor::Linear {
                bt2020: desc.primaries.named == Some(CmPrimaries::Bt2020),
                max_lum: max_lum.max(1),
                ref_lum: ref_lum.max(1),
            };
        }
        if desc.is_hdr() {
            ContentColor::HdrPq
        } else {
            ContentColor::Sdr
        }
    }
}

/// The blend state of the frame currently being rendered, stored in the renderer's EGL user
/// data (like [`super::shaders::Shaders`]).
///
/// Shader uniform values persist in GL program objects across draws, so on HDR frames every
/// draw sets the blend uniforms from this state, and on SDR frames sets them back to zero.
#[derive(Debug, Default)]
pub struct FrameBlendState {
    hdr_pq: Cell<bool>,
    ref_lum_scale: Cell<f32>,
}

impl FrameBlendState {
    pub fn init(renderer: &mut GlesRenderer) {
        let data = renderer.egl_context().user_data();
        data.insert_if_missing(FrameBlendState::default);
    }

    fn get(renderer: &GlesRenderer) -> &Self {
        renderer
            .egl_context()
            .user_data()
            .get()
            .expect("FrameBlendState::init() must be called when creating the renderer")
    }

    /// Marks the frames rendered from now on as HDR with the given SDR reference luminance,
    /// or as SDR (`None`).
    pub fn set(renderer: &mut GlesRenderer, reference_luminance: Option<f64>) {
        let state = Self::get(renderer);
        match reference_luminance {
            Some(lum) => {
                state.hdr_pq.set(true);
                state.ref_lum_scale.set((lum / 10000.) as f32);
            }
            None => state.hdr_pq.set(false),
        }
    }

    pub fn set_sdr_capture(renderer: &mut GlesRenderer, reference_luminance: f64) {
        let state = Self::get(renderer);
        state.hdr_pq.set(false);
        state
            .ref_lum_scale
            .set((reference_luminance / 10000.) as f32);
    }

    fn values_from_frame(frame: &GlesFrame) -> (bool, f32) {
        let state: &Self = frame
            .egl_context()
            .user_data()
            .get()
            .expect("FrameBlendState::init() must be called when creating the renderer");
        (state.hdr_pq.get(), state.ref_lum_scale.get())
    }

    pub fn is_hdr_frame(frame: &GlesFrame) -> bool {
        Self::values_from_frame(frame).0
    }

    pub fn ref_lum_scale(frame: &GlesFrame) -> f32 {
        Self::values_from_frame(frame).1
    }

    /// The `niri_blend` uniform values for content already rendered in the frame blend space.
    pub fn uniforms_for_blend_space(frame: &GlesFrame) -> [Uniform<'static>; 6] {
        let (_, scale) = Self::values_from_frame(frame);
        [
            Uniform::new("niri_hdr_pq", 0.0f32),
            Uniform::new("niri_ref_lum_scale", scale),
            Uniform::new("niri_linear", 0.0f32),
            Uniform::new("niri_linear_scale", 0.0f32),
            Uniform::new("niri_linear_to_ref", 0.0f32),
            Uniform::new("niri_hdr_to_sdr", 0.0f32),
        ]
    }

    /// The `niri_blend` uniform values for a draw of SDR content in this frame.
    pub fn uniforms(frame: &GlesFrame) -> [Uniform<'static>; 6] {
        Self::uniforms_for_content(frame, ContentColor::Sdr)
    }

    /// The `niri_blend` uniform values for a draw in this frame; [`ContentColor::HdrPq`]
    /// exempts client content already encoded in HDR PQ from SDR-to-HDR conversion,
    /// [`ContentColor::Linear`] selects the absolute extended-linear encode. HDR PQ content is
    /// converted back to SDR when drawn into SDR capture buffers.
    pub fn uniforms_for_content(frame: &GlesFrame, content: ContentColor) -> [Uniform<'static>; 6] {
        let (hdr_pq, scale) = Self::values_from_frame(frame);
        let sdr_to_hdr = hdr_pq && content != ContentColor::HdrPq;
        let hdr_to_sdr = !hdr_pq && content == ContentColor::HdrPq;
        let (linear, linear_scale, linear_to_ref) = match content {
            ContentColor::Linear {
                bt2020,
                max_lum,
                ref_lum,
            } => (
                if bt2020 { 2.0f32 } else { 1.0 },
                max_lum as f32 / 10000.,
                max_lum as f32 / ref_lum as f32,
            ),
            _ => (0.0, 0.0, 0.0),
        };
        [
            Uniform::new("niri_hdr_pq", if sdr_to_hdr { 1.0f32 } else { 0.0 }),
            Uniform::new("niri_ref_lum_scale", scale),
            Uniform::new("niri_linear", linear),
            Uniform::new("niri_linear_scale", linear_scale),
            Uniform::new("niri_linear_to_ref", linear_to_ref),
            Uniform::new("niri_hdr_to_sdr", if hdr_to_sdr { 1.0f32 } else { 0.0 }),
        ]
    }
}

/// Configures the renderer for rendering frames in the given blend space: `Some(reference
/// luminance)` = HDR (PQ/BT.2020), `None` = SDR.
///
/// In HDR, texture draws using the default program go through the blend-space texture shader,
/// solid colors are encoded on the CPU, and niri's own shader programs read the frame blend
/// state for their `niri_blend` stage. Call with `None` after rendering the output so
/// screencasts, screenshots and other outputs stay SDR.
///
/// This is the GLES half; the Vulkan renderer has the symmetric [`set_frame_blend_vulkan`]
/// (its frames install the blend-space texture program in `TtyRenderer::render`, and solid
/// colors are encoded by the `TtyFrame` wrapper).
pub fn set_frame_blend(renderer: Option<&mut GlesRenderer>, reference_luminance: Option<f64>) {
    let Some(renderer) = renderer else {
        return;
    };
    FrameBlendState::set(renderer, reference_luminance);

    match reference_luminance {
        Some(lum) => {
            let scale = (lum / 10000.) as f32;
            let program = Shaders::get(renderer).texture_hdr.clone();
            if let Some(program) = program {
                renderer.set_default_tex_program_override(Some((
                    program,
                    vec![
                        Uniform::new("niri_hdr_pq", 1.0f32),
                        Uniform::new("niri_ref_lum_scale", scale),
                        // Uniform values persist in the program object; reset the
                        // extended-linear state that BlendSurfaceRenderElement sets for
                        // linear-content draws.
                        Uniform::new("niri_linear", 0.0f32),
                        Uniform::new("niri_linear_scale", 0.0f32),
                        Uniform::new("niri_linear_to_ref", 0.0f32),
                        Uniform::new("niri_hdr_to_sdr", 0.0f32),
                    ],
                )));
            } else {
                warn!("HDR texture shader missing; SDR content will render raw");
            }
            renderer
                .set_solid_color_transform(Some(Box::new(move |color| srgb_to_pq(color, scale))));
        }
        None => {
            renderer.set_default_tex_program_override(None);
            renderer.set_solid_color_transform(None);
        }
    }
}

/// Configures the renderer for rendering into an SDR capture buffer, while preserving the
/// reference luminance needed to convert HDR content back to SDR.
pub fn set_sdr_capture_blend(renderer: &mut GlesRenderer, reference_luminance: f64) {
    FrameBlendState::set_sdr_capture(renderer, reference_luminance);
    renderer.set_default_tex_program_override(None);
    renderer.set_solid_color_transform(None);
}

/// Vulkan counterpart of [`FrameBlendState`]: the blend space of the frames rendered from now
/// on, stored in the [`VulkanRenderer`]'s user data (see
/// [`super::shaders::VulkanShaders`], which lives in the same map).
///
/// Unlike GLES there is no renderer-level default texture program override; instead
/// [`apply_vulkan_frame_blend`] installs the HDR texture program on each frame as it is created
/// (see `TtyRenderer::render`). Solid colors are encoded by the TTY frame wrapper, and niri's
/// own Vulkan programs read these values for their `niri_blend` stage.
///
/// Deliberately still SDR: the blur passes are pure filters over already-encoded data and
/// never run `niri_blend`; the capture/readback and global-pass feedback paths produce SDR
/// intermediates; and the cursor plane is disabled on HDR outputs, so the cursor is drawn into
/// the primary plane and encoded by the frame-wide texture override.
#[cfg(feature = "vulkan")]
#[derive(Debug, Default)]
pub struct VulkanFrameBlendState {
    hdr_pq: Cell<bool>,
    ref_lum_scale: Cell<f32>,
}

#[cfg(feature = "vulkan")]
impl VulkanFrameBlendState {
    pub fn init(renderer: &mut VulkanRenderer) {
        renderer.user_data().insert_if_missing(Self::default);
    }

    fn get(renderer: &VulkanRenderer) -> Option<&Self> {
        renderer.user_data().get()
    }

    /// Marks the frames rendered from now on as HDR with the given SDR reference luminance,
    /// or as SDR (`None`).
    pub fn set(renderer: &mut VulkanRenderer, reference_luminance: Option<f64>) {
        if Self::get(renderer).is_none() {
            Self::init(renderer);
        }
        let state = Self::get(renderer).expect("just initialized");
        match reference_luminance {
            Some(lum) => {
                state.hdr_pq.set(true);
                state.ref_lum_scale.set((lum / 10000.) as f32);
            }
            None => state.hdr_pq.set(false),
        }
    }

    pub fn set_sdr_capture(renderer: &mut VulkanRenderer, reference_luminance: f64) {
        if Self::get(renderer).is_none() {
            Self::init(renderer);
        }
        let state = Self::get(renderer).expect("just initialized");
        state.hdr_pq.set(false);
        state
            .ref_lum_scale
            .set((reference_luminance / 10000.) as f32);
    }

    /// The `(hdr_pq, ref_lum_scale)` values for frames rendered by this renderer.
    pub fn values(renderer: &VulkanRenderer) -> (bool, f32) {
        Self::get(renderer).map_or((false, 0.), |state| {
            (state.hdr_pq.get(), state.ref_lum_scale.get())
        })
    }

    fn values_from_frame(frame: &VulkanFrame<'_, '_>) -> (bool, f32) {
        frame.user_data().get().map_or((false, 0.), |state: &Self| {
            (state.hdr_pq.get(), state.ref_lum_scale.get())
        })
    }

    pub fn is_hdr_frame(frame: &VulkanFrame<'_, '_>) -> bool {
        Self::values_from_frame(frame).0
    }

    pub fn ref_lum_scale(frame: &VulkanFrame<'_, '_>) -> f32 {
        Self::values_from_frame(frame).1
    }
}

/// Configures the Vulkan renderer for rendering frames in the given blend space. Symmetric with
/// [`set_frame_blend`]; the override is applied to the frames created afterwards by
/// `TtyRenderer::render`, and cleared by passing `None` after the output is rendered.
#[cfg(feature = "vulkan")]
pub fn set_frame_blend_vulkan(
    renderer: Option<&mut VulkanRenderer>,
    reference_luminance: Option<f64>,
) {
    let Some(renderer) = renderer else {
        return;
    };
    VulkanFrameBlendState::set(renderer, reference_luminance);
}

/// Configures the Vulkan renderer for rendering into an SDR capture buffer, preserving the
/// reference luminance needed to convert HDR content back to SDR. Symmetric with
/// [`set_sdr_capture_blend`].
#[cfg(feature = "vulkan")]
pub fn set_sdr_capture_blend_vulkan(renderer: &mut VulkanRenderer, reference_luminance: f64) {
    VulkanFrameBlendState::set_sdr_capture(renderer, reference_luminance);
}

/// The `niri_blend` custom-uniform values for content already rendered in the frame blend space.
#[cfg(feature = "vulkan")]
pub fn vulkan_uniforms_for_blend_space(frame: &VulkanFrame<'_, '_>) -> Vec<OwnedCustomUniform> {
    let (_, scale) = VulkanFrameBlendState::values_from_frame(frame);
    vulkan_blend_uniforms(0.0, scale, 0.0, 0.0, 0.0, 0.0)
}

/// The `niri_blend` custom-uniform values for a draw of SDR content in this frame.
#[cfg(feature = "vulkan")]
pub fn vulkan_uniforms(frame: &VulkanFrame<'_, '_>) -> Vec<OwnedCustomUniform> {
    vulkan_uniforms_for_content(frame, ContentColor::Sdr)
}

/// The Vulkan counterpart of [`FrameBlendState::uniforms_for_content`].
#[cfg(feature = "vulkan")]
pub fn vulkan_uniforms_for_content(
    frame: &VulkanFrame<'_, '_>,
    content: ContentColor,
) -> Vec<OwnedCustomUniform> {
    vulkan_blend_uniform_values_for_content(frame, content)
        .into_iter()
        .map(|(name, value)| OwnedCustomUniform {
            name: name.to_owned(),
            value: CustomUniformValue::Float(value),
        })
        .collect()
}

/// The `niri_blend` values for a draw of the given content, as `(name, value)` pairs with
/// static names (usable where the caller needs `CustomUniform<'static>`).
#[cfg(feature = "vulkan")]
pub fn vulkan_blend_uniform_values(frame: &VulkanFrame<'_, '_>) -> [(&'static str, f32); 6] {
    vulkan_blend_uniform_values_for_content(frame, ContentColor::Sdr)
}

/// The `niri_blend` values for a draw of the given content, as `(name, value)` pairs.
#[cfg(feature = "vulkan")]
pub fn vulkan_blend_uniform_values_for_content(
    frame: &VulkanFrame<'_, '_>,
    content: ContentColor,
) -> [(&'static str, f32); 6] {
    let (hdr_pq, scale) = VulkanFrameBlendState::values_from_frame(frame);
    let sdr_to_hdr = hdr_pq && content != ContentColor::HdrPq;
    let hdr_to_sdr = !hdr_pq && content == ContentColor::HdrPq;
    let (linear, linear_scale, linear_to_ref) = match content {
        ContentColor::Linear {
            bt2020,
            max_lum,
            ref_lum,
        } => (
            if bt2020 { 2.0f32 } else { 1.0 },
            max_lum as f32 / 10000.,
            max_lum as f32 / ref_lum as f32,
        ),
        _ => (0.0, 0.0, 0.0),
    };
    [
        ("niri_hdr_pq", if sdr_to_hdr { 1.0 } else { 0.0 }),
        ("niri_ref_lum_scale", scale),
        ("niri_linear", linear),
        ("niri_linear_scale", linear_scale),
        ("niri_linear_to_ref", linear_to_ref),
        ("niri_hdr_to_sdr", if hdr_to_sdr { 1.0 } else { 0.0 }),
    ]
}

#[cfg(feature = "vulkan")]
fn vulkan_blend_uniforms(
    hdr_pq: f32,
    ref_lum_scale: f32,
    linear: f32,
    linear_scale: f32,
    linear_to_ref: f32,
    hdr_to_sdr: f32,
) -> Vec<OwnedCustomUniform> {
    [
        ("niri_hdr_pq", hdr_pq),
        ("niri_ref_lum_scale", ref_lum_scale),
        ("niri_linear", linear),
        ("niri_linear_scale", linear_scale),
        ("niri_linear_to_ref", linear_to_ref),
        ("niri_hdr_to_sdr", hdr_to_sdr),
    ]
    .into_iter()
    .map(|(name, value)| OwnedCustomUniform {
        name: name.to_owned(),
        value: CustomUniformValue::Float(value),
    })
    .collect()
}

/// Installs the frame-wide default texture program override for the current Vulkan blend state.
///
/// Called on every frame created by `TtyRenderer::render`. On HDR frames, all default texture
/// draws (Wayland surfaces not wrapped by [`BlendSurfaceRenderElement`], cursors, snapshot
/// textures) are encoded into the output blend space, exactly like the GLES renderer-level
/// `set_default_tex_program_override`.
#[cfg(feature = "vulkan")]
pub fn apply_vulkan_frame_blend(frame: &mut VulkanFrame<'_, '_>) {
    let (hdr_pq, scale) = VulkanFrameBlendState::values_from_frame(frame);
    if !hdr_pq {
        return;
    }
    let Some(program) = super::shaders::get_vulkan_frame(frame).and_then(|s| s.texture_hdr.clone())
    else {
        warn!("HDR texture shader missing; SDR content will render raw on Vulkan");
        return;
    };
    let uniforms = vulkan_blend_uniforms(1.0, scale, 0.0, 0.0, 0.0, 0.0);
    frame.set_tex_program_override(Some((program, uniforms)));
}

/// The texture program override for a draw of the given content on Vulkan, or `None` when the
/// frame default (the HDR encode, or the built-in shader on SDR frames) already applies.
#[cfg(feature = "vulkan")]
pub fn vulkan_content_override(
    frame: &VulkanFrame<'_, '_>,
    content: ContentColor,
) -> Option<(
    smithay::backend::renderer::vulkan::VulkanPixelProgram,
    Vec<OwnedCustomUniform>,
)> {
    let shaders = super::shaders::get_vulkan_frame(frame)?;
    match content {
        ContentColor::Sdr => None,
        ContentColor::HdrPq => {
            if VulkanFrameBlendState::is_hdr_frame(frame) {
                // Numeric passthrough: the blend program with every transform disabled.
                let program = shaders.texture_hdr.clone()?;
                Some((program, vulkan_uniforms_for_blend_space(frame)))
            } else {
                let program = shaders.texture_hdr_to_sdr.clone()?;
                let scale = VulkanFrameBlendState::ref_lum_scale(frame);
                Some((
                    program,
                    vec![OwnedCustomUniform {
                        name: "niri_ref_lum_scale".to_owned(),
                        value: CustomUniformValue::Float(scale),
                    }],
                ))
            }
        }
        ContentColor::Linear { .. } => {
            let program = shaders.texture_hdr.clone()?;
            Some((program, vulkan_uniforms_for_content(frame, content)))
        }
    }
}

/// Encodes a solid color for the current Vulkan blend space (CPU counterpart of the shaders'
/// `niri_blend`), matching the GLES renderer-level solid color transform.
#[cfg(feature = "vulkan")]
pub fn vulkan_encode_solid(frame: &VulkanFrame<'_, '_>, color: Color32F) -> Color32F {
    let (hdr_pq, scale) = VulkanFrameBlendState::values_from_frame(frame);
    if hdr_pq {
        srgb_to_pq(color, scale)
    } else {
        color
    }
}

/// CPU counterpart of the shaders' `niri_blend`: encodes an electrical sRGB premultiplied
/// color into PQ/BT.2020 for the given SDR reference luminance scale (reference / 10000).
pub fn srgb_to_pq(color: Color32F, ref_lum_scale: f32) -> Color32F {
    let a = color.a();
    let unpremul = |c: f32| if a > 0. { c / a } else { c };

    let pq = |lin: f32| {
        const M1: f32 = 0.159_301_76;
        const M2: f32 = 78.84375;
        const C1: f32 = 0.8359375;
        const C2: f32 = 18.851_563;
        const C3: f32 = 18.6875;
        let y = lin.clamp(0., 1.).powf(M1);
        ((C1 + C2 * y) / (1. + C3 * y)).powf(M2)
    };

    let r = unpremul(color.r()).max(0.).powf(2.2);
    let g = unpremul(color.g()).max(0.).powf(2.2);
    let b = unpremul(color.b()).max(0.).powf(2.2);

    // BT.709 -> BT.2020, linear light, D65.
    let r2020 = 0.627404 * r + 0.329283 * g + 0.043313 * b;
    let g2020 = 0.069097 * r + 0.919540 * g + 0.011362 * b;
    let b2020 = 0.016391 * r + 0.088013 * g + 0.895595 * b;

    Color32F::new(
        pq(r2020 * ref_lum_scale) * a,
        pq(g2020 * ref_lum_scale) * a,
        pq(b2020 * ref_lum_scale) * a,
        a,
    )
}

/// A surface-tree render element that knows how its content relates to the output blend space
/// (from its committed image description).
///
/// For blend-space (PQ) content the frame-wide blend-space texture program is suspended around
/// the draw, so the client's PQ values pass through numerically, and underlying storage is
/// delegated so direct scanout keeps working. For scRGB content the frame program is swapped
/// for one applying the absolute scRGB encode, and direct scanout is prevented (the raw linear
/// buffer must not reach a PQ-signalled connector).
#[derive(Debug)]
pub struct BlendSurfaceRenderElement<R: Renderer> {
    inner: WaylandSurfaceRenderElement<R>,
    content: ContentColor,
}

impl<R: Renderer> BlendSurfaceRenderElement<R> {
    pub fn new(inner: WaylandSurfaceRenderElement<R>, content: ContentColor) -> Self {
        Self { inner, content }
    }

    pub fn inner(&self) -> &WaylandSurfaceRenderElement<R> {
        &self.inner
    }

    pub fn into_inner(self) -> WaylandSurfaceRenderElement<R> {
        self.inner
    }

    pub fn content(&self) -> ContentColor {
        self.content
    }
}

/// Adjusts the frame's default-texture-program override for a draw of the given content,
/// returning the previous override to restore afterwards (`None` = nothing was changed).
///
/// HDR PQ client content suspends the override on HDR frames (numeric passthrough) and installs
/// the HDR-to-SDR texture program on SDR capture frames. Extended-linear content installs the
/// blend texture program with the `niri_linear` state set — on HDR *and* SDR frames: raw
/// extended-linear values are meaningless on an SDR framebuffer (channels above 1.0 clamp to full
/// scale, blowing bright colors out to white), so SDR frames get the reference-white-anchored SDR
/// encode from the shader instead of a passthrough.
fn adjust_tex_program_for_content(
    frame: &mut GlesFrame,
    content: ContentColor,
) -> Option<
    Option<(
        smithay::backend::renderer::gles::GlesTexProgram,
        Vec<Uniform<'static>>,
    )>,
> {
    match content {
        ContentColor::Sdr => None,
        ContentColor::HdrPq => {
            let saved = frame.take_tex_program_override();
            if FrameBlendState::is_hdr_frame(frame) {
                saved.is_some().then_some(saved)
            } else if let Some(program) = Shaders::get_from_frame(frame).texture_hdr_to_sdr.clone()
            {
                let ref_lum_scale = FrameBlendState::ref_lum_scale(frame);
                frame.override_default_tex_program(
                    program,
                    vec![Uniform::new("niri_ref_lum_scale", ref_lum_scale)],
                );
                Some(saved)
            } else {
                warn!("HDR-to-SDR texture shader missing; HDR capture will render raw");
                Some(saved)
            }
        }
        ContentColor::Linear { .. } => {
            let program = Shaders::get_from_frame(frame).texture_hdr.clone();
            let saved = frame.take_tex_program_override();
            // Prefer the frame override's program; on SDR frames (no override) fall back to
            // the blend shader directly.
            let Some(program) = saved.as_ref().map(|(p, _)| p.clone()).or(program) else {
                // Shader failed to compile at startup (already warned); render raw.
                return None;
            };
            let uniforms = FrameBlendState::uniforms_for_content(frame, content).to_vec();
            frame.set_tex_program_override(Some((program, uniforms)));
            Some(saved)
        }
    }
}

impl<R: Renderer + ImportAll> Element for BlendSurfaceRenderElement<R>
where
    R::TextureId: Clone + 'static,
{
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for BlendSurfaceRenderElement<GlesRenderer> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let saved = adjust_tex_program_for_content(frame, self.content);
        let res = RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        );
        if let Some(saved) = saved {
            frame.set_tex_program_override(saved);
        }
        res
    }

    fn underlying_storage(&self, renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // Extended-linear buffers need the GL encode; their raw values must not be scanned
        // out.
        if matches!(self.content, ContentColor::Linear { .. }) {
            return None;
        }
        self.inner.underlying_storage(renderer)
    }
}

impl<'render> RenderElement<TtyRenderer<'render>>
    for BlendSurfaceRenderElement<TtyRenderer<'render>>
{
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        #[cfg(feature = "vulkan")]
        if let TtyFrame::Vulkan(multi) = frame {
            if self.content == ContentColor::Sdr {
                // The frame default installed by `TtyRenderer::render` already encodes SDR
                // content into the blend space (or is absent on SDR frames).
                return RenderElement::draw(
                    &self.inner,
                    frame,
                    src,
                    dst,
                    damage,
                    opaque_regions,
                    cache,
                );
            }
            // Blend-space / extended-linear content: swap the frame program around this draw,
            // exactly like the GLES path does with the default tex program override.
            let vk_frame: &mut VulkanFrame<'_, '_> = multi.as_mut();
            let saved = vk_frame.take_tex_program_override();
            let installed = vulkan_content_override(vk_frame, self.content);
            if let TtyFrame::Vulkan(multi) = frame {
                multi.as_mut().set_tex_program_override(installed);
            }
            let res =
                RenderElement::draw(&self.inner, frame, src, dst, damage, opaque_regions, cache);
            if let TtyFrame::Vulkan(multi) = frame {
                multi.as_mut().set_tex_program_override(saved);
            }
            return res;
        }

        let Some(gles_frame) = frame.as_gles_frame() else {
            return RenderElement::draw(
                &self.inner,
                frame,
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            );
        };
        let saved = adjust_tex_program_for_content(gles_frame, self.content);
        let res = RenderElement::draw(&self.inner, frame, src, dst, damage, opaque_regions, cache);
        if let Some(saved) = saved
            && let Some(frame) = frame.as_gles_frame()
        {
            frame.set_tex_program_override(saved);
        }
        res
    }

    fn underlying_storage(
        &self,
        renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        // Extended-linear buffers need the GL encode; their raw values must not be scanned
        // out.
        if matches!(self.content, ContentColor::Linear { .. }) {
            return None;
        }
        self.inner.underlying_storage(renderer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_color_classification() {
        use smithay::wayland::color::management::PrimariesOption;

        assert_eq!(ContentColor::from_description(None), ContentColor::Sdr);
        assert_eq!(
            ContentColor::from_description(Some(ImageDescription::SRGB)),
            ContentColor::Sdr
        );

        // Windows scRGB: 1.0 = 80 cd/m² with a 203 cd/m² reference white.
        assert_eq!(
            ContentColor::from_description(Some(ImageDescription::WINDOWS_SCRGB)),
            ContentColor::Linear {
                bt2020: false,
                max_lum: 80,
                ref_lum: 203,
            }
        );

        // A parametric ext_linear + sRGB description (Mesa's WSI mapping for
        // VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT) with default luminances.
        let mesa_scrgb = ImageDescription {
            transfer: CmTransferFunction::ExtLinear,
            ..ImageDescription::SRGB
        };
        assert_eq!(
            ContentColor::from_description(Some(mesa_scrgb)),
            ContentColor::Linear {
                bt2020: false,
                max_lum: 80,
                ref_lum: 80,
            }
        );

        // BT.2020 linear content skips the 709 -> 2020 conversion.
        let bt2020_linear = ImageDescription {
            transfer: CmTransferFunction::ExtLinear,
            primaries: PrimariesOption {
                named: Some(CmPrimaries::Bt2020),
                values: None,
            },
            ..ImageDescription::SRGB
        };
        assert!(matches!(
            ContentColor::from_description(Some(bt2020_linear)),
            ContentColor::Linear { bt2020: true, .. }
        ));

        // PQ content passes through the blend space numerically.
        let pq = ImageDescription {
            transfer: CmTransferFunction::St2084Pq,
            primaries: PrimariesOption {
                named: Some(CmPrimaries::Bt2020),
                values: None,
            },
            ..ImageDescription::SRGB
        };
        assert_eq!(
            ContentColor::from_description(Some(pq)),
            ContentColor::HdrPq
        );
        assert_eq!(
            ContentColor::from_description(Some(ImageDescription::WINDOWS_BT2100)),
            ContentColor::HdrPq
        );
    }

    #[test]
    fn srgb_to_pq_reference_values() {
        let scale = (203. / 10000.) as f32;

        // Opaque white at reference luminance 203 cd/m²: PQ(0.0203) ≈ 0.5806.
        let white = srgb_to_pq(Color32F::new(1., 1., 1., 1.), scale);
        assert!((white.r() - 0.5806).abs() < 0.002, "got {}", white.r());
        // BT.709 white maps to BT.2020 white (rows sum to 1) => neutral stays neutral.
        assert!((white.r() - white.g()).abs() < 0.0005);
        assert!((white.g() - white.b()).abs() < 0.0005);

        // Black stays (essentially) black — PQ(0) is ~4e-7 — and alpha is preserved.
        let black = srgb_to_pq(Color32F::new(0., 0., 0., 0.5), scale);
        assert!(black.r() < 1e-6, "got {}", black.r());
        assert_eq!(black.a(), 0.5);

        // Premultiplied 50% white: unpremultiplied value is 1.0, so the encoded result is
        // the white point rescaled by alpha.
        let half = srgb_to_pq(Color32F::new(0.5, 0.5, 0.5, 0.5), scale);
        assert!((half.r() - white.r() * 0.5).abs() < 0.0005);
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{
        Bind, Color32F, ExportMem, Frame as _, ImportMem, Offscreen, Renderer,
    };
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Rectangle, Size, Transform};

    use super::*;
    use crate::render_helpers::shaders::init_vulkan;

    /// Runs `f` on the first usable Vulkan device; skips silently when there is none.
    fn with_vulkan_device<T>(f: impl FnOnce(&mut VulkanRenderer, &str) -> T) -> Option<T> {
        let instance = Instance::new(Version::VERSION_1_3, None).ok()?;
        let devices = PhysicalDevice::enumerate(&instance).ok()?;
        for device in devices {
            let Ok(mut renderer) = VulkanRenderer::new(&device) else {
                continue;
            };
            init_vulkan(&mut renderer, &niri_config::Config::default());
            return Some(f(&mut renderer, device.name()));
        }
        None
    }

    /// Draws an opaque mid-grey SDR texture into an offscreen framebuffer in the given blend
    /// space and returns the first framebuffer pixel (Argb8888 little-endian: B, G, R, A).
    fn draw_mid_grey(renderer: &mut VulkanRenderer, reference_luminance: Option<f64>) -> [u8; 4] {
        let tex_size = Size::<i32, smithay::utils::Buffer>::from((1, 1));
        let data = [128u8, 128, 128, 255];
        let source = renderer
            .import_memory(&data, Fourcc::Abgr8888, tex_size, false)
            .expect("import the mid-grey texture");

        let buffer_size = Size::<i32, smithay::utils::Buffer>::from((4, 4));
        let output_size = Size::<i32, smithay::utils::Physical>::from((4, 4));
        let mut target = renderer
            .create_buffer(Fourcc::Argb8888, buffer_size)
            .expect("offscreen target");
        let mut framebuffer = renderer.bind(&mut target).expect("bind target");

        set_frame_blend_vulkan(Some(renderer), reference_luminance);

        // This mirrors `TtyRenderer::render`: the frame starts with the blend-space texture
        // program override on HDR frames and none on SDR frames.
        let mut frame = renderer
            .render(&mut framebuffer, output_size, Transform::Normal)
            .expect("begin frame");
        if VulkanFrameBlendState::is_hdr_frame(&frame) {
            apply_vulkan_frame_blend(&mut frame);
            // The override must be installed and be a real program.
            let installed = frame.take_tex_program_override();
            assert!(installed.is_some(), "HDR frame has no texture override");
            frame.set_tex_program_override(installed);
        } else {
            assert!(
                frame.take_tex_program_override().is_none(),
                "SDR frame must not install a texture override"
            );
        }

        let dst = Rectangle::from_size(output_size);
        frame
            .render_texture_from_to(
                &source,
                Rectangle::from_size(tex_size.to_f64()),
                dst,
                &[dst],
                &[],
                Transform::Normal,
                1.0,
            )
            .expect("draw mid-grey through the default texture path");
        frame.finish().expect("finish frame");

        let mapping = renderer
            .copy_framebuffer(
                &framebuffer,
                Rectangle::from_size(buffer_size),
                Fourcc::Argb8888,
            )
            .expect("copy framebuffer");
        let pixels = renderer.map_texture(&mapping).expect("map framebuffer");
        [pixels[0], pixels[1], pixels[2], pixels[3]]
    }

    /// The real renderer proves the SDR -> PQ encode: the same SDR mid-grey texture drawn
    /// through the HDR frame override lands at the PQ value that the GLES CPU helper
    /// (`srgb_to_pq`) computes, while an SDR frame leaves it unchanged.
    #[test]
    fn vulkan_hdr_texture_override_encodes_sdr_content() {
        let Some((hdr, sdr)) = with_vulkan_device(|renderer, name| {
            let hdr = draw_mid_grey(renderer, Some(203.));
            let sdr = draw_mid_grey(renderer, None);
            eprintln!("{name}: HDR frame pixel {hdr:?}, SDR frame pixel {sdr:?}");
            (hdr, sdr)
        }) else {
            return;
        };

        // The expected encoded value comes from the same CPU helper GLES uses for solid colors.
        let expected = (srgb_to_pq(Color32F::new(0.5, 0.5, 0.5, 1.0), (203. / 10000.) as f32).r()
            * 255.)
            .round() as i32;
        // Opaque mid-grey SDR output is unchanged.
        assert!(
            (sdr[0] as i32 - 128).abs() <= 2,
            "SDR frame changed the texture: {sdr:?}"
        );
        assert!(
            (hdr[0] as i32 - expected).abs() <= 4,
            "HDR frame pixel {} is not the expected PQ value {expected}",
            hdr[0]
        );
        // PQ-encoded reference grey is darker than its electrical sRGB value.
        assert!(hdr[0] < sdr[0], "HDR encode did not change the pixel");
        // Grey stays neutral through the BT.709 -> BT.2020 matrix.
        assert!((hdr[0] as i32 - hdr[1] as i32).abs() <= 1);
        assert!((hdr[1] as i32 - hdr[2] as i32).abs() <= 1);
        assert_eq!(hdr[3], 255, "alpha was not preserved");
    }

    /// The override and the blend state are frame/call scoped: restoring SDR must clear both,
    /// so screenshots and screencasts rendered afterwards stay SDR.
    #[test]
    fn vulkan_sdr_frames_clear_the_hdr_override() {
        let Some(()) = with_vulkan_device(|renderer, _name| {
            let hdr = draw_mid_grey(renderer, Some(203.));
            assert_ne!(hdr[0], 128, "HDR frame did not encode");

            // `set_frame_blend_vulkan(None)` is what the TTY backend calls after rendering the
            // output; the next frame must be SDR with no override.
            set_frame_blend_vulkan(Some(renderer), None);
            assert!(!VulkanFrameBlendState::values(renderer).0);
            let sdr = draw_mid_grey(renderer, None);
            assert!(
                (sdr[0] as i32 - 128).abs() <= 2,
                "capture is not SDR: {sdr:?}"
            );

            // The capture blend keeps the reference luminance but stays SDR (= HDR content is
            // converted back, SDR content passes through).
            set_sdr_capture_blend_vulkan(renderer, 203.);
            let (hdr_pq, scale) = VulkanFrameBlendState::values(renderer);
            assert!(!hdr_pq);
            assert!(
                (scale - 0.0203).abs() < 1e-6,
                "ref lum scale not kept: {scale}"
            );
            let capture = draw_mid_grey(renderer, None);
            assert!((capture[0] as i32 - 128).abs() <= 2, "capture is not SDR");
        }) else {
            return;
        };
    }

    /// HDR PQ content drawn into an SDR frame (capture) goes through the HDR-to-SDR texture
    /// program, so a screenshot of an HDR client is SDR-correct rather than raw PQ.
    #[test]
    fn vulkan_hdr_content_converts_back_on_sdr_capture() {
        let Some(pixel) = with_vulkan_device(|renderer, name| {
            // PQ-encoded reference mid-grey (what the HDR frame produced above).
            let tex_size = Size::<i32, smithay::utils::Buffer>::from((1, 1));
            let data = [109u8, 109, 109, 255];
            let source = renderer
                .import_memory(&data, Fourcc::Abgr8888, tex_size, false)
                .expect("import the PQ texture");

            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((4, 4));
            let output_size = Size::<i32, smithay::utils::Physical>::from((4, 4));
            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("offscreen target");
            let mut framebuffer = renderer.bind(&mut target).expect("bind target");

            set_frame_blend_vulkan(Some(renderer), None);
            let mut frame = renderer
                .render(&mut framebuffer, output_size, Transform::Normal)
                .expect("begin frame");
            // What `BlendSurfaceRenderElement` installs for HDR PQ content on an SDR frame.
            let installed = vulkan_content_override(&frame, ContentColor::HdrPq);
            assert!(installed.is_some(), "HDR-to-SDR program missing");
            frame.set_tex_program_override(installed);

            let dst = Rectangle::from_size(output_size);
            frame
                .render_texture_from_to(
                    &source,
                    Rectangle::from_size(tex_size.to_f64()),
                    dst,
                    &[dst],
                    &[],
                    Transform::Normal,
                    1.0,
                )
                .expect("draw PQ content into the SDR capture");
            frame.finish().expect("finish frame");

            let mapping = renderer
                .copy_framebuffer(
                    &framebuffer,
                    Rectangle::from_size(buffer_size),
                    Fourcc::Argb8888,
                )
                .expect("copy framebuffer");
            let pixels = renderer.map_texture(&mapping).expect("map framebuffer");
            let pixel = [pixels[0], pixels[1], pixels[2], pixels[3]];
            eprintln!("{name}: HDR->SDR capture pixel {pixel:?}");
            pixel
        }) else {
            return;
        };

        assert!(
            (pixel[0] as i32 - 128).abs() <= 4,
            "PQ content was not converted back to SDR: {pixel:?}"
        );
    }
}
