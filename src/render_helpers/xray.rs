//! Shared scene-wide xray backdrop.
//!
//! [`Xray`] keeps a per-render-target pair of [`EffectBuffer`]s: the `background` buffer holds the
//! output's Background layer surfaces, the `backdrop` buffer holds the scene backdrop (wallpaper)
//! plus the workspace background colors. `Niri::bake_xray_elements` bakes them through whichever
//! renderer drives the output (GLES or Vulkan), and every window/layer with an xray background
//! effect composites the shared baked texture with the postprocess program instead of capturing
//! its own framebuffer.
//!
//! On GLES the composite is a `GlesTexProgram` draw with the optional alpha mask bound to texture
//! unit 1. On Vulkan the same GLSL runs through the `xray_postprocess_and_clip` custom program,
//! with `v_coords` remapped from the element rectangle to full backdrop-texture UVs, and the mask
//! passed as a second sampler. The optional blur uses the GLES `Blur` or the Vulkan Dual-Kawase
//! ladder respectively.

use std::array;
use std::cell::RefCell;
use std::rc::Rc;

use glam::{Mat3, Vec2};
use niri_config::CornerRadius;
use smithay::backend::renderer::Color32F;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesTexProgram, Uniform, ffi};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::multigpu::Error as MultiError;
use smithay::backend::renderer::utils::{CommitCounter, OpaqueRegions};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{
    CustomUniform, CustomUniformValue, OwnedCustomUniform, VulkanError, VulkanFrame,
};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::backend::tty::{TtyFrame, TtyOffscreen, TtyRenderer, TtyRendererError};
use crate::render_helpers::RenderTarget;
use crate::render_helpers::background_effect::{AlphaMask, RenderParams};
use crate::render_helpers::blend::FrameBlendState;
use crate::render_helpers::effect_buffer::EffectBuffer;
use crate::render_helpers::liquid_glass::LiquidGlassOptions;
use crate::render_helpers::renderer::AsGlesFrame as _;
#[cfg(feature = "vulkan")]
use crate::render_helpers::shader_element::uniform_to_custom_owned;
#[cfg(feature = "vulkan")]
use crate::render_helpers::shaders::get_vulkan_frame;
use crate::render_helpers::shaders::mat3_uniform;
use crate::utils::region::TransformedRegion;

#[derive(Debug)]
pub struct Xray {
    // The buffers are per-render-target to avoid constant rerendering when screencasting.
    pub background: [Rc<RefCell<EffectBuffer>>; RenderTarget::COUNT],
    pub backdrop: [Rc<RefCell<EffectBuffer>>; RenderTarget::COUNT],
    pub backdrop_color: Color32F,
    pub workspaces: Vec<(Rectangle<f64, Logical>, Color32F)>,
}

/// Position for drawing xray background.
#[derive(Debug, Clone, Copy)]
pub struct XrayPos {
    /// Position of geometry relative to the backdrop in zoomed coordinates.
    ///
    /// Should be upscaled by `zoom` to get position in backdrop coordinates.
    pub pos_in_backdrop: Point<f64, Logical>,

    /// Zoom factor between backdrop coordinates and geometry.
    pub zoom: f64,
}

impl XrayPos {
    pub fn new(pos_in_backdrop: Point<f64, Logical>, zoom: f64) -> Self {
        Self {
            pos_in_backdrop: pos_in_backdrop.downscale(zoom),
            zoom,
        }
    }

    pub fn offset(mut self, offset: Point<f64, Logical>) -> Self {
        self.pos_in_backdrop += offset;
        self
    }
}

impl Default for XrayPos {
    fn default() -> Self {
        Self {
            pos_in_backdrop: Point::new(0., 0.),
            zoom: 1.,
        }
    }
}

#[derive(Debug)]
pub struct XrayElement {
    buffer: Rc<RefCell<EffectBuffer>>,
    id: Id,
    geometry: Rectangle<f64, Logical>,
    src: Rectangle<f64, Buffer>,
    subregion: Option<TransformedRegion>,
    input_to_clip_geo: Mat3,
    clip_geo_size: Vec2,
    corner_radius: CornerRadius,
    scale: f32,
    blur: bool,
    noise: f32,
    saturation: f32,
    bg_color: Color32F,
    /// GLES composite program. `None` on Vulkan, where the program is looked up from the frame.
    program: Option<GlesTexProgram>,
    liquid_glass: Option<LiquidGlassOptions>,
    padding_pixels: f32,
    alpha_mask: Option<XrayAlphaMask>,
}

#[derive(Debug, Clone)]
struct XrayAlphaMask {
    /// The surface texture sampled by the postprocess mask branch.
    texture: TtyOffscreen,
    surface_to_geo: Mat3,
    threshold: f32,
}

impl Xray {
    pub fn new() -> Self {
        Self {
            background: array::from_fn(|_| Rc::new(RefCell::new(EffectBuffer::new()))),
            backdrop: array::from_fn(|_| Rc::new(RefCell::new(EffectBuffer::new()))),
            backdrop_color: Color32F::TRANSPARENT,
            workspaces: Vec::new(),
        }
    }

    /// Whether anything consumed the buffers for `target` since they were last baked.
    ///
    /// `XrayElement`s hold an `Rc` clone of the buffer they sample, so a count above one means
    /// some element is still alive and the buffer will be drawn this frame.
    pub fn is_used(&self, target: RenderTarget) -> bool {
        let i = target as usize;
        Rc::strong_count(&self.background[i]) > 1 || Rc::strong_count(&self.backdrop[i]) > 1
    }

    /// Builds the backdrop/background elements for `params`.
    ///
    /// `program` is the GLES composite program; pass `None` on Vulkan, where the composite
    /// program is resolved from the Vulkan frame at draw time.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        target: RenderTarget,
        program: Option<GlesTexProgram>,
        params: RenderParams,
        xray_pos: XrayPos,
        blur: bool,
        noise: f32,
        saturation: f32,
        liquid_glass_options: Option<LiquidGlassOptions>,
        padding_pixels: f32,
        alpha_mask: Option<AlphaMask>,
        push: &mut dyn FnMut(XrayElement),
    ) {
        let zoom = xray_pos.zoom;
        let pos_in_backdrop = xray_pos.pos_in_backdrop.upscale(zoom);

        let (clip_geo, corner_radius) = params
            .clip
            .unwrap_or((params.geometry, CornerRadius::default()));

        let clip_offset = clip_geo.loc - params.geometry.loc;
        let clip_pos_in_backdrop = pos_in_backdrop + clip_offset.upscale(zoom);

        let geo_in_backdrop = Rectangle::new(pos_in_backdrop, params.geometry.size.upscale(zoom));

        let clip_to_surface = alpha_mask.as_ref().and_then(|mask| {
            if mask.surface_geo.size.w <= 0. || mask.surface_geo.size.h <= 0. {
                return None;
            }
            let surface_size = Vec2::new(
                mask.surface_geo.size.w as f32,
                mask.surface_geo.size.h as f32,
            );
            let clip_loc = Vec2::new(clip_geo.loc.x as f32, clip_geo.loc.y as f32);
            let surface_loc =
                Vec2::new(mask.surface_geo.loc.x as f32, mask.surface_geo.loc.y as f32);
            let clip_size_v = Vec2::new(clip_geo.size.w as f32, clip_geo.size.h as f32);
            // clip_to_surface(p) = (clip_geo.loc + p * clip_geo.size - surface_geo.loc) /
            // surface_geo.size
            let m = Mat3::from_scale(Vec2::ONE / surface_size)
                * Mat3::from_translation(clip_loc - surface_loc)
                * Mat3::from_scale(clip_size_v);
            Some((m, mask.texture.clone(), mask.threshold))
        });

        let backdrop = self.backdrop[target as usize].borrow_mut();
        let backdrop_geo = Rectangle::from_size(backdrop.logical_size());
        let intersection_with_backdrop = backdrop_geo.intersection(geo_in_backdrop);

        let mut skip_backdrop = intersection_with_backdrop.is_none();

        let background = self.background[target as usize].borrow_mut();

        let clip_geo_size = Vec2::new(clip_geo.size.w as f32, clip_geo.size.h as f32);
        let buf_size = background.logical_size();

        for (ws_geo, bg_color) in &self.workspaces {
            // If the background color is opaque, check if the workspace fully covers the
            // element. In this case, we will skip the backdrop element since it's fully
            // covered.
            //
            // FIXME: also implement some way to check if the background elements are fully
            // covered in opaque regions, and not just the niri background color is opaque
            let crop = if bg_color.is_opaque() && ws_geo.contains_rect(geo_in_backdrop) {
                skip_backdrop = true;
                // No need to intersect, we know it's fully covered.
                Some(geo_in_backdrop)
            } else {
                ws_geo.intersection(geo_in_backdrop)
            };

            let Some(crop) = crop else {
                continue;
            };

            // If crop contains the intersection with backdrop, then the workspace fully
            // covers the backdrop, so we can skip the backdrop.
            //
            // This can happen when the overview is closed (so workspaces align left/right with
            // the backdrop) and the window is peeking out off screen to the side. In this
            // case, this off-screen part is on top of nothing, neither workspace nor backdrop,
            // but since the window doesn't fully cover the workspace, the check above doesn't
            // skip the backdrop.
            if bg_color.is_opaque()
                && intersection_with_backdrop.is_some_and(|backdrop| crop.contains_rect(backdrop))
            {
                skip_backdrop = true;
            }

            // This can be different from zoom for surfaces that do not scale with
            // workspaces, e.g. layer-shell top and overlay layer.
            let ws_zoom = ws_geo.size / buf_size;

            let src = Rectangle::new(crop.loc - ws_geo.loc, crop.size).downscale(ws_zoom);
            let src = src.to_buffer(background.scale(), Transform::Normal, &buf_size);

            let buf_size = Vec2::new(buf_size.w as f32, buf_size.h as f32);
            let pos_against_buf = (clip_pos_in_backdrop - ws_geo.loc).downscale(ws_zoom);
            let pos_against_buf = Vec2::new(pos_against_buf.x as f32, pos_against_buf.y as f32);
            let ws_zoom_vec = Vec2::new(ws_zoom.x as f32, ws_zoom.y as f32);
            let input_to_clip_geo = Mat3::from_scale(ws_zoom_vec / zoom as f32)
                * Mat3::from_scale(buf_size / clip_geo_size)
                * Mat3::from_translation(-pos_against_buf / buf_size);

            let mut geometry =
                Rectangle::new(crop.loc - geo_in_backdrop.loc, crop.size).downscale(zoom);
            geometry.loc += params.geometry.loc;

            let alpha_mask = clip_to_surface
                .as_ref()
                .map(|(m, tex, threshold)| XrayAlphaMask {
                    texture: tex.clone(),
                    surface_to_geo: *m * input_to_clip_geo,
                    threshold: *threshold,
                });

            let elem = XrayElement {
                buffer: self.background[target as usize].clone(),
                id: background.id().clone(),
                geometry,
                src,
                subregion: params.subregion.clone(),
                input_to_clip_geo,
                clip_geo_size,
                corner_radius,
                scale: params.scale as f32,
                blur,
                noise,
                saturation,
                bg_color: *bg_color,
                program: program.clone(),
                liquid_glass: liquid_glass_options,
                padding_pixels,
                alpha_mask,
            };
            push(elem);
        }

        // If the backdrop is fully covered by opaque background, we can skip it.
        if skip_backdrop {
            return;
        }

        let buf_size = backdrop.logical_size();
        let src = geo_in_backdrop.to_buffer(backdrop.scale(), Transform::Normal, &buf_size);

        let mut clip_geo_in_backdrop = Rectangle::new(clip_offset, clip_geo.size).upscale(zoom);
        clip_geo_in_backdrop.loc += geo_in_backdrop.loc;

        let clip_pos_in_backdrop = Vec2::new(
            clip_geo_in_backdrop.loc.x as f32,
            clip_geo_in_backdrop.loc.y as f32,
        );
        let clip_geo_size = Vec2::new(
            clip_geo_in_backdrop.size.w as f32,
            clip_geo_in_backdrop.size.h as f32,
        );

        let buf_size = Vec2::new(buf_size.w as f32, buf_size.h as f32);
        let input_to_clip_geo = Mat3::from_scale(buf_size / clip_geo_size)
            * Mat3::from_translation(-clip_pos_in_backdrop / buf_size);

        let alpha_mask = clip_to_surface
            .as_ref()
            .map(|(m, tex, threshold)| XrayAlphaMask {
                texture: tex.clone(),
                surface_to_geo: *m * input_to_clip_geo,
                threshold: *threshold,
            });

        let elem = XrayElement {
            buffer: self.backdrop[target as usize].clone(),
            id: backdrop.id().clone(),
            geometry: params.geometry,
            src,
            subregion: params.subregion.clone(),
            input_to_clip_geo,
            clip_geo_size,
            corner_radius: corner_radius.scaled_by(zoom as f32),
            scale: params.scale as f32,
            blur,
            noise,
            saturation,
            bg_color: self.backdrop_color,
            program: program.clone(),
            liquid_glass: liquid_glass_options,
            padding_pixels,
            alpha_mask,
        };
        push(elem);
    }
}

impl XrayElement {
    fn compute_uniforms(&self) -> Vec<Uniform<'static>> {
        let (surface_to_geo, alpha_mask_enabled, alpha_threshold) = match &self.alpha_mask {
            Some(mask) => (mask.surface_to_geo, 1i32, mask.threshold),
            None => (Mat3::IDENTITY, 0i32, 0.0),
        };

        let mut uniforms = vec![
            Uniform::new("niri_scale", self.scale),
            Uniform::new("geo_size", <[f32; 2]>::from(self.clip_geo_size)),
            Uniform::new("corner_radius", <[f32; 4]>::from(self.corner_radius)),
            mat3_uniform("input_to_geo", self.input_to_clip_geo),
            Uniform::new("noise", self.noise),
            Uniform::new("saturation", self.saturation),
            Uniform::new("bg_color", self.bg_color.components()),
            mat3_uniform("surface_to_geo", surface_to_geo),
            Uniform::new("alpha_mask_enabled", alpha_mask_enabled),
            Uniform::new("alpha_threshold", alpha_threshold),
            // The mask is bound to texture unit 1 by the GLES draw (GL texture units are not
            // part of the uniform block, so the sampler location must be set explicitly).
            Uniform::new("surface_tex", 1i32),
        ];

        if let Some(lg) = &self.liquid_glass {
            uniforms.extend([
                Uniform::new("lg_refraction_strength", lg.refraction_strength as f32),
                Uniform::new("lg_power_factor", lg.power_factor as f32),
                Uniform::new("lg_refraction_a", lg.refraction_a as f32),
                Uniform::new("lg_refraction_b", lg.refraction_b as f32),
                Uniform::new("lg_refraction_c", lg.refraction_c as f32),
                Uniform::new("lg_refraction_d", lg.refraction_d as f32),
                Uniform::new("lg_refraction_power", lg.refraction_power as f32),
                Uniform::new("lg_physical_refraction", lg.physical_refraction as f32),
                Uniform::new("lg_glow_weight", lg.glow_weight as f32),
                Uniform::new("lg_glow_bias", lg.glow_bias as f32),
                Uniform::new("lg_glow_edge0", lg.glow_edge0 as f32),
                Uniform::new("lg_glow_edge1", lg.glow_edge1 as f32),
                Uniform::new("lg_edge_lighting", lg.edge_lighting as f32),
                Uniform::new("lg_fringing", lg.fringing as f32),
                Uniform::new("lg_refraction_dilute", lg.refraction_dilute as f32),
                Uniform::new("lg_dilute_strength", lg.dilute_strength as f32),
                Uniform::new("lg_dilute_fringing", lg.dilute_fringing as f32),
                Uniform::new("lg_lens_distortion", lg.lens_distortion as f32),
                Uniform::new("lg_brightness", lg.brightness as f32),
                Uniform::new("lg_contrast", lg.contrast as f32),
                Uniform::new("lg_saturation", lg.saturation as f32),
                Uniform::new("lg_vibrancy", lg.vibrancy as f32),
                Uniform::new("lg_adaptive_dim", lg.adaptive_dim as f32),
                Uniform::new("lg_adaptive_boost", lg.adaptive_boost as f32),
                Uniform::new("lg_edge_thickness", lg.edge_thickness as f32),
                Uniform::new("lg_padding_pixels", self.padding_pixels),
            ]);
        } else {
            uniforms.extend([
                Uniform::new("lg_refraction_strength", 0.0f32),
                Uniform::new("lg_power_factor", 0.0f32),
                Uniform::new("lg_refraction_a", 0.0f32),
                Uniform::new("lg_refraction_b", 0.0f32),
                Uniform::new("lg_refraction_c", 0.0f32),
                Uniform::new("lg_refraction_d", 0.0f32),
                Uniform::new("lg_refraction_power", 0.0f32),
                Uniform::new("lg_physical_refraction", 0.0f32),
                Uniform::new("lg_glow_weight", 0.0f32),
                Uniform::new("lg_glow_bias", 0.0f32),
                Uniform::new("lg_glow_edge0", 0.0f32),
                Uniform::new("lg_glow_edge1", 0.0f32),
                Uniform::new("lg_edge_lighting", 0.0f32),
                Uniform::new("lg_fringing", 0.0f32),
                Uniform::new("lg_refraction_dilute", 0.0f32),
                Uniform::new("lg_dilute_strength", 0.0f32),
                Uniform::new("lg_dilute_fringing", 0.0f32),
                Uniform::new("lg_lens_distortion", 0.0f32),
                Uniform::new("lg_brightness", 0.0f32),
                Uniform::new("lg_contrast", 0.0f32),
                Uniform::new("lg_saturation", 0.0f32),
                Uniform::new("lg_vibrancy", 0.0f32),
                Uniform::new("lg_adaptive_dim", 0.0f32),
                Uniform::new("lg_adaptive_boost", 0.0f32),
                Uniform::new("lg_edge_thickness", 0.15f32),
                Uniform::new("lg_padding_pixels", self.padding_pixels),
            ]);
        }

        uniforms
    }

    /// Filters `damage` by the optional subregion, returning `None` when nothing is left.
    fn filter_damage<'a>(
        &self,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &'a [Rectangle<i32, Physical>],
        storage: &'a mut Vec<Rectangle<i32, Physical>>,
    ) -> Option<&'a [Rectangle<i32, Physical>]> {
        let Some(subregion) = &self.subregion else {
            return Some(damage);
        };

        let src_to_geo = self.geometry.size / self.src.size;

        // Compute crop in geometry coordinates.
        let mut crop = src;
        crop.loc -= self.src.loc;
        crop = crop.upscale(src_to_geo);
        let mut crop = crop.to_logical(1., Transform::Normal, &Size::default());

        // Then convert to subregion coordinates.
        crop.loc += self.geometry.loc;

        storage.clear();
        subregion.filter_damage(crop, dst, damage, storage);

        (!storage.is_empty()).then_some(&storage[..])
    }
}

impl Element for XrayElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.buffer.borrow().commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // FIXME: if bg_color alpha is 1 then compute opaque regions here taking corners into
        // account
        OpaqueRegions::default()
    }
}

impl RenderElement<smithay::backend::renderer::gles::GlesRenderer> for XrayElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let mut buffer = self.buffer.borrow_mut();
        let texture = match buffer.render(frame, self.blur) {
            Ok(x) => x,
            Err(err) => {
                warn!("error rendering effect buffer: {err:?}");
                return Ok(());
            }
        };

        let mut filtered_damage = Vec::new();
        let Some(damage) = self.filter_damage(src, dst, damage, &mut filtered_damage) else {
            return Ok(());
        };

        let uniforms = self.program.is_some().then(|| {
            let mut uniforms = self.compute_uniforms().to_vec();
            uniforms.extend(FrameBlendState::uniforms(frame));
            uniforms
        });
        let uniforms = uniforms.as_ref().map_or(&[][..], |x| &x[..]);

        let surface_tex_id = self
            .alpha_mask
            .as_ref()
            .filter(|_| self.program.is_some())
            .and_then(|m| m.texture.as_gles())
            .map(|t| t.tex_id());
        if let Some(tex_id) = surface_tex_id {
            frame.with_context(|gl| unsafe {
                gl.ActiveTexture(ffi::TEXTURE1);
                gl.BindTexture(ffi::TEXTURE_2D, tex_id);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_S,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_T,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.ActiveTexture(ffi::TEXTURE0);
            })?;
        }

        let result = frame.render_texture_from_to(
            &texture,
            src,
            dst,
            damage,
            // FIXME: opaque regions need to be filtered like damage.
            &[],
            Transform::Normal,
            1.,
            self.program.as_ref(),
            uniforms,
        );

        if surface_tex_id.is_some() {
            frame.with_context(|gl| unsafe {
                gl.ActiveTexture(ffi::TEXTURE1);
                gl.BindTexture(ffi::TEXTURE_2D, 0);
                gl.ActiveTexture(ffi::TEXTURE0);
            })?;
        }

        result
    }
}

#[cfg(feature = "vulkan")]
impl XrayElement {
    /// Vulkan composite: sample the shared baked backdrop through the `xray_postprocess_and_clip`
    /// program, with `v_coords` remapped from this element's rectangle to full backdrop UVs.
    fn draw_vulkan(
        &self,
        frame: &mut VulkanFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), VulkanError> {
        let texture = {
            let mut buffer = self.buffer.borrow_mut();
            match buffer.render_vulkan(frame, self.blur) {
                Ok(texture) => texture,
                Err(err) => {
                    warn!("error rendering effect buffer: {err:?}");
                    return Ok(());
                }
            }
        };

        let mut filtered_damage = Vec::new();
        let Some(damage) = self.filter_damage(src, dst, damage, &mut filtered_damage) else {
            return Ok(());
        };

        let (program, black_texture) = {
            let Some(shaders) = get_vulkan_frame(frame) else {
                return Ok(());
            };
            let Some(program) = shaders.xray_postprocess_and_clip.clone() else {
                return Ok(());
            };
            (program, shaders.black_texture.clone())
        };

        // The custom-pass vertex shader gives `v_coords` as 0..1 across `dst`; the shared shader
        // expects full backdrop-texture UVs (like the GLES `render_texture_from_to` path, where
        // `v_coords` is built from `src` and the texture size).
        let tex_size = smithay::backend::renderer::Texture::size(&texture);
        let tex_size = Vec2::new(tex_size.w as f32, tex_size.h as f32);
        let src_size = Vec2::new(src.size.w as f32, src.size.h as f32);
        let src_loc = Vec2::new(src.loc.x as f32, src.loc.y as f32);
        let xray_uv =
            Mat3::from_scale(src_size / tex_size) * Mat3::from_translation(src_loc / tex_size);

        let mut owned: Vec<OwnedCustomUniform> = self
            .compute_uniforms()
            .iter()
            .filter_map(uniform_to_custom_owned)
            .collect();
        owned.push(OwnedCustomUniform {
            name: "xray_uv".to_owned(),
            value: CustomUniformValue::Mat3(xray_uv.to_cols_array()),
        });
        // `alpha_mask_enabled` ships as an int on GLES; the Vulkan program declares it as float.
        let alpha_mask_enabled = if self.alpha_mask.is_some() { 1.0 } else { 0.0 };
        // The shader's own `alpha`/`tint` inputs (built-ins on GLES).
        for (name, value) in [
            ("alpha", 1.0f32),
            ("tint", 0.0f32),
            ("alpha_mask_enabled", alpha_mask_enabled),
        ] {
            owned.push(OwnedCustomUniform {
                name: name.to_owned(),
                value: CustomUniformValue::Float(value),
            });
        }
        // SDR blend-space defaults; Vulkan HDR output is not wired yet.
        for (name, value) in [
            ("niri_hdr_pq", 0.0),
            ("niri_ref_lum_scale", 1.0),
            ("niri_linear", 0.0),
            ("niri_linear_scale", 0.0),
            ("niri_linear_to_ref", 0.0),
            ("niri_hdr_to_sdr", 0.0),
        ] {
            owned.push(OwnedCustomUniform {
                name: name.to_owned(),
                value: CustomUniformValue::Float(value),
            });
        }
        let uniforms: Vec<CustomUniform<'_>> = owned.iter().map(|u| u.borrow()).collect();

        // The mask branch is disabled without a mask, but the sampler is still declared; sample
        // the backdrop itself as the harmless fallback.
        let mask_texture = self
            .alpha_mask
            .as_ref()
            .and_then(|mask| mask.texture.as_vulkan());
        let surface_tex = mask_texture.or(black_texture.as_ref()).unwrap_or(&texture);
        let textures = [("tex", &texture), ("surface_tex", surface_tex)];

        frame.render_custom(&program, dst, damage, &uniforms, &textures, 1.0)
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for XrayElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        #[cfg(feature = "vulkan")]
        if let TtyFrame::Vulkan(multi) = frame {
            let vk_frame: &mut VulkanFrame<'_, '_> = multi.as_mut();
            return self
                .draw_vulkan(vk_frame, src, dst, damage)
                .map_err(|err| TtyRendererError::Vulkan(MultiError::Render(err)));
        }

        let Some(gles_frame) = frame.as_gles_frame() else {
            return Ok(());
        };
        RenderElement::<smithay::backend::renderer::gles::GlesRenderer>::draw(
            &self,
            gles_frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        Ok(())
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::vulkan::{VulkanRenderer, VulkanTexture};
    use smithay::backend::renderer::{
        Bind, Color32F, ExportMem, Frame as _, ImportMem, Offscreen, Renderer, Texture,
    };
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Scale, Size, Transform};

    use super::*;
    use crate::render_helpers::blur::BlurOptions;
    use crate::render_helpers::shaders::init_vulkan;
    use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};

    /// Runs `test` on the first Vulkan device that can create a renderer, silently skipping when
    /// there is no Vulkan loader/device/driver (CI without a GPU).
    fn for_each_vulkan_renderer(mut test: impl FnMut(&mut VulkanRenderer)) {
        let Ok(instance) = Instance::new(Version::VERSION_1_3, None) else {
            return;
        };
        let Ok(devices) = PhysicalDevice::enumerate(&instance) else {
            return;
        };
        for device in devices {
            if let Ok(mut renderer) = VulkanRenderer::new(&device) {
                test(&mut renderer);
                return;
            }
        }
    }

    fn read_back(
        renderer: &mut VulkanRenderer,
        target: &mut VulkanTexture,
        size: Size<i32, Buffer>,
    ) -> Vec<u8> {
        let format = target.format().expect("target texture has a format");
        let fb = renderer.bind(target).expect("bind readback target");
        let mapping = renderer
            .copy_framebuffer(&fb, Rectangle::from_size(size), format)
            .expect("copy framebuffer");
        let pixels = renderer.map_texture(&mapping).expect("map framebuffer");
        let bytes = pixels.to_vec();
        drop(fb);
        bytes
    }

    /// The shared xray backdrop composite on Vulkan: bake a red/blue backdrop through the generic
    /// `EffectBuffer`, then draw it through `XrayElement` with blur and an alpha mask. The
    /// composite must blur the edge, keep unmasked pixels opaque, and zero masked pixels.
    #[test]
    fn vulkan_xray_backdrop_blurs_and_applies_the_alpha_mask() {
        for_each_vulkan_renderer(|renderer| {
            init_vulkan(renderer, &niri_config::Config::default());

            let size = Size::<i32, Buffer>::from((16, 16));
            let geo = Rectangle::from_size(Size::from((16., 16.)));

            let xray = Xray::new();
            {
                let mut background = xray.background[RenderTarget::Output as usize].borrow_mut();
                background.update_size(Size::from((16, 16)), Scale::from(1.));
                let mut backdrop = xray.backdrop[RenderTarget::Output as usize].borrow_mut();
                backdrop.update_size(Size::from((16, 16)), Scale::from(1.));
                backdrop.update_blur_options(BlurOptions {
                    passes: 3,
                    offset: 1.0,
                });

                let red_buf = SolidColorBuffer::new((8., 16.), [1., 0., 0., 1.]);
                let blue_buf = SolidColorBuffer::new((8., 16.), [0., 0., 1., 1.]);
                let red = SolidColorRenderElement::from_buffer(
                    &red_buf,
                    Point::from((0., 0.)),
                    1.,
                    Kind::Unspecified,
                );
                let blue = SolidColorRenderElement::from_buffer(
                    &blue_buf,
                    Point::from((8., 0.)),
                    1.,
                    Kind::Unspecified,
                );
                backdrop
                    .render_elements(renderer, &[red, blue])
                    .expect("bake the backdrop");
            }

            // Surface alpha mask: opaque on the left 12 columns, transparent on the right 4.
            let mut mask_data = vec![255u8; 16 * 16 * 4];
            for y in 0..16 {
                for x in 12..16 {
                    let i = (y * 16 + x) * 4;
                    mask_data[i + 3] = 0;
                }
            }
            let mask_texture = renderer
                .import_memory(&mask_data, Fourcc::Abgr8888, Size::from((16, 16)), false)
                .expect("import the mask texture");
            let alpha_mask = AlphaMask {
                texture: TtyOffscreen::Vulkan(mask_texture),
                surface_geo: geo,
                threshold: 0.0,
            };

            let params = RenderParams {
                geometry: geo,
                original_geometry: geo,
                subregion: None,
                clip: Some((geo, CornerRadius::default())),
                scale: 1.0,
            };

            let mut elements = Vec::new();
            xray.render(
                RenderTarget::Output,
                None,
                params,
                XrayPos::default(),
                true,
                0.0,
                1.0,
                None,
                0.0,
                Some(alpha_mask),
                &mut |elem| elements.push(elem),
            );
            assert_eq!(elements.len(), 1, "expected the backdrop element");

            let element = &elements[0];
            let mut target = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .expect("readback target");
            {
                let mut fb = renderer.bind(&mut target).expect("bind target");
                let mut frame = renderer
                    .render(&mut fb, Size::from((16, 16)), Transform::Normal)
                    .expect("begin frame");
                let dst = Rectangle::from_size(Size::from((16, 16)));
                frame.clear(Color32F::TRANSPARENT, &[dst]).expect("clear");
                element
                    .draw_vulkan(&mut frame, element.src(), dst, &[dst])
                    .expect("draw the xray element");
                frame.finish().expect("finish frame");
            }

            let pixels = read_back(renderer, &mut target, size);
            // Read back as R, G, B, A.

            // Centre of the blurred red/blue edge: both colours substantially present.
            let center = (8 * 16 + 8) * 4;
            let (r, g, b, a) = (
                pixels[center],
                pixels[center + 1],
                pixels[center + 2],
                pixels[center + 3],
            );
            assert_eq!(g, 0, "unexpected green channel: {g}");
            assert_eq!(a, 255, "unmasked backdrop should be opaque, got alpha {a}");
            assert!(
                (24..=231).contains(&b) && (24..=231).contains(&r),
                "xray blur did not mix the backdrop edge: R={r} B={b}"
            );

            // Unmasked red half (near the left edge; the blur bleeds in from the blue side).
            let left = (8 * 16 + 1) * 4;
            let (r, g, b, a) = (
                pixels[left],
                pixels[left + 1],
                pixels[left + 2],
                pixels[left + 3],
            );
            assert_eq!(g, 0, "unexpected green channel: {g}");
            assert_eq!(a, 255, "unmasked backdrop should be opaque, got alpha {a}");
            assert!(
                r > b && r > 100,
                "unmasked backdrop should be predominantly red: R={r} B={b}"
            );

            // Masked (transparent surface) columns must be zeroed so the unblurred backdrop shows
            // through.
            let masked = (8 * 16 + 14) * 4;
            assert_eq!(
                &pixels[masked..masked + 4],
                &[0, 0, 0, 0],
                "masked pixels must be zeroed"
            );
        });
    }
}
