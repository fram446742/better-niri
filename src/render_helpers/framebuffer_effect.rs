use std::cell::RefCell;
use std::sync::Once;

use glam::{Mat3, Vec2};
use niri_config::CornerRadius;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{
    Capability, GlesError, GlesFrame, GlesRenderer, GlesTexture, Uniform, ffi,
};
use smithay::backend::renderer::utils::CommitCounter;
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{
    CustomUniform, CustomUniformValue, VulkanError, VulkanFrame, VulkanTexture,
};
use smithay::backend::renderer::{Frame as _, FrameContext, Offscreen, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::background_effect::{AlphaMask, RenderParams};
use crate::render_helpers::blend::FrameBlendState;
use crate::render_helpers::blur::{Blur, BlurOptions};
#[cfg(feature = "vulkan")]
use crate::render_helpers::blur_vulkan::VulkanBlur;
use crate::render_helpers::capture;
use crate::render_helpers::liquid_glass::LiquidGlassOptions;
use crate::render_helpers::renderer::AsGlesFrame as _;
#[cfg(feature = "vulkan")]
use crate::render_helpers::shader_element::uniform_to_custom_owned;
#[cfg(feature = "vulkan")]
use crate::render_helpers::shaders::get_vulkan_frame;
use crate::render_helpers::shaders::{Shaders, mat3_uniform};
use crate::utils::region::TransformedRegion;

#[derive(Debug)]
pub struct FramebufferEffect {
    id: Id,
    commit: CommitCounter,
}

#[derive(Debug)]
pub struct FramebufferEffectElement {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<f64, Logical>,
    clip_geo: Rectangle<f64, Logical>,
    corner_radius: CornerRadius,
    subregion: Option<TransformedRegion>,
    scale: f32,
    blur_options: Option<BlurOptions>,
    noise: f32,
    saturation: f32,
    brightness: f32,
    contrast: f32,
    liquid_glass: Option<LiquidGlassOptions>,
    padding_pixels: f32,
    alpha_mask: Option<AlphaMask>,
}

#[derive(Debug)]
struct Inner {
    framebuffer: Option<GlesTexture>,
    blur: Option<Blur>,
    intermediate: Option<GlesTexture>,
    /// Reusable storage for subregion-filtered damage rects.
    subregion_damage: Vec<Rectangle<i32, Physical>>,
}

/// Vulkan counterpart of [`Inner`]: renderer-owned textures captured, blurred and composited
/// through loom-smithay's custom-pass pipeline. Kept in the same per-element effects cache
/// as [`Inner`] (the two types never mix, since an output uses one renderer).
#[cfg(feature = "vulkan")]
#[derive(Debug, Default)]
struct VulkanInner {
    /// Captured framebuffer region (the unblurred backdrop).
    framebuffer: Option<VulkanTexture>,
    /// The texture sampled by the composite (`framebuffer` itself, or the blur output).
    intermediate: Option<VulkanTexture>,
    /// Cached Dual-Kawase blur ladder for `framebuffer`.
    blur: VulkanBlur,
    /// Reusable storage for subregion-filtered damage rects.
    subregion_damage: Vec<Rectangle<i32, Physical>>,
}

impl FramebufferEffect {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
        }
    }

    pub fn damage(&mut self) {
        self.commit.increment();
    }

    pub fn render(
        &self,
        ns: Option<usize>,
        params: RenderParams,
        blur_options: Option<BlurOptions>,
        noise: f32,
        saturation: f32,
        brightness: f32,
        contrast: f32,
        liquid_glass: Option<LiquidGlassOptions>,
        padding_pixels: f32,
        alpha_mask: Option<AlphaMask>,
    ) -> FramebufferEffectElement {
        let (clip_geo, corner_radius) = params
            .clip
            .unwrap_or((params.geometry, CornerRadius::default()));

        let mut id = self.id.clone();
        if let Some(ns) = ns {
            id = id.namespaced(ns);
        }

        FramebufferEffectElement {
            id,
            commit: self.commit,
            geometry: params.geometry,
            clip_geo,
            corner_radius,
            subregion: params.subregion,
            scale: params.scale as f32,
            blur_options,
            noise,
            saturation,
            brightness,
            contrast,
            liquid_glass,
            padding_pixels,
            alpha_mask,
        }
    }
}

impl FramebufferEffectElement {
    fn compute_uniforms(
        &self,
        crop: Rectangle<f64, Logical>,
        transform: Transform,
    ) -> Vec<Uniform<'static>> {
        let offset = crop.loc - (self.clip_geo.loc - self.geometry.loc);
        let offset = Vec2::new(offset.x as f32, offset.y as f32);
        let crop_size = Vec2::new(crop.size.w as f32, crop.size.h as f32);
        let clip_size = Vec2::new(self.clip_geo.size.w as f32, self.clip_geo.size.h as f32);

        // Our v_coords are [0, 1] inside crop. We want them to be [0, 1] inside clip_geo.
        let input_to_clip_geo =
            Mat3::from_scale(crop_size / clip_size) * Mat3::from_translation(offset / crop_size);

        // Revert the effect of the texture transform.
        let transform_mat = Mat3::from_translation(Vec2::new(0.5, 0.5))
            * transform.matrix()
            * Mat3::from_translation(Vec2::new(-0.5, -0.5));
        let input_to_clip_geo = input_to_clip_geo * transform_mat;

        let clip_geo_size = (self.clip_geo.size.w as f32, self.clip_geo.size.h as f32);

        let (_surface_to_geo, _alpha_mask_enabled, _alpha_threshold) = match &self.alpha_mask {
            Some(mask) if mask.surface_geo.size.w > 0. && mask.surface_geo.size.h > 0. => {
                let surface_offset = crop.loc - (mask.surface_geo.loc - self.geometry.loc);
                let surface_offset = Vec2::new(surface_offset.x as f32, surface_offset.y as f32);
                let surface_size = Vec2::new(
                    mask.surface_geo.size.w as f32,
                    mask.surface_geo.size.h as f32,
                );
                let s2g = Mat3::from_scale(crop_size / surface_size)
                    * Mat3::from_translation(surface_offset / crop_size);
                (s2g * transform_mat, 1i32, mask.threshold)
            }
            _ => (Mat3::IDENTITY, 0i32, 0.0),
        };

        let mut uniforms = vec![
            Uniform::new("niri_scale", self.scale),
            Uniform::new("geo_size", clip_geo_size),
            Uniform::new("corner_radius", <[f32; 4]>::from(self.corner_radius)),
            mat3_uniform("input_to_geo", input_to_clip_geo),
            Uniform::new("noise", self.noise),
            Uniform::new("saturation", self.saturation),
            Uniform::new("brightness", self.brightness),
            Uniform::new("contrast", self.contrast),
            Uniform::new("bg_color", [0f32, 0., 0., 0.]),
            Uniform::new("lg_padding_pixels", self.padding_pixels),
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
            ]);
        }

        uniforms
    }
}

impl Element for FramebufferEffectElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        // We don't use src for drawing but we can use it to figure out how we were cropped.
        let size = self.geometry.size.to_buffer(1., Transform::Normal);
        Rectangle::from_size(size)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("FramebufferEffectElement::capture_framebuffer");
        let location = gpu_span_location!("FramebufferEffectElement::capture_framebuffer");
        frame.with_gpu_span(location, |frame| {
            let output_rect = Rectangle::from_size(frame.output_size());
            let transform = frame.transformation();

            let supports_blit = frame.capabilities().contains(&Capability::Blit);
            let mut guard = frame.renderer();

            let inner = cache
                .get_or_insert::<RefCell<Inner>, _>(|| RefCell::new(Inner::new(guard.as_mut())));
            let mut inner = inner.borrow_mut();
            let inner = &mut *inner;

            inner.intermediate = None;

            if !supports_blit {
                static WARN: Once = Once::new();
                WARN.call_once(|| {
                    warn!("non-xray background effects disabled: renderer does not support framebuffer blitting");
                });
                return Ok(());
            }

            // We want clamp-to-edge behavior for out-of-bounds pixels. However, glBlitFramebuffer
            // seems to skip out-of-bounds pixels, even though my reading of the docs suggests
            // otherwise (we use GL_LINEAR filter). So, clamp dst to the framebuffer bounds
            // ourselves.
            let clamped_dst = match dst.intersection(output_rect) {
                Some(clamped) => clamped,
                None => return Ok(()),
            };
            let clamp_scale = clamped_dst.size.to_f64() / dst.size.to_f64();

            let dst = transform.transform_rect_in(clamped_dst, &output_rect.size);

            // Compute size from our geometry and scale.
            //
            // The "correct" size is always dst.size since that's the pixel region we're actually
            // blitting. However, using dst.size causes two undesirable things when zooming out for
            // the overview:
            // 1. dst.size shrinks every frame, causing a texture realloaction for every fb effect
            //    element every frame.
            // 2. The underlying blur visually expands. This is technically correct, since the
            //    underlying contents shrink, but it's not what you visually expect: you expect the
            //    blur to also shrink as the windows zoom out, to give the zooming out effect.
            //
            // Using size computed from geometry and scale solves both of those problems (even
            // though there's a bit of a cost in that zoomed-out elements still blur the entire
            // unzoomed texture size, and even though the blur ends up slightly wrong as there's two
            // layers of texture resampling, up and back down).
            //
            // Here we use src.size rather than geometry directly because src takes into account
            // cropping.
            let size = src
                .size
                .to_logical(1., Transform::Normal)
                .upscale(clamp_scale)
                .to_physical_precise_round(self.scale);
            let size = transform.transform_size(size);

            let size = size.to_logical(1).to_buffer(1, Transform::Normal);

            // Recreate framebuffer if needed.
            if inner
                .framebuffer
                .as_ref()
                .is_some_and(|fb| fb.size() != size)
            {
                inner.framebuffer = None;
            }
            let framebuffer = if let Some(fb) = &inner.framebuffer {
                fb
            } else {
                trace!("creating framebuffer texture sized {} × {}", size.w, size.h);
                let renderer = guard.as_mut();
                let texture = renderer.create_buffer(Fourcc::Abgr8888, size)?;
                inner.framebuffer.insert(texture)
            };

            // Prepare blur textures.
            let mut blur = Option::zip(inner.blur.as_mut(), self.blur_options);
            if let Some((b, options)) = &mut blur {
                let renderer = guard.as_mut();
                if let Err(err) = b.prepare_textures(
                    |fourcc, size| renderer.create_buffer(fourcc, size),
                    framebuffer,
                    *options,
                ) {
                    warn!("error preparing blur textures: {err:?}");
                    blur = None;
                }
            }

            // We can't use renderer.with_context() as that will reset the GlesFrame binding that we
            // want to blit from.
            let framebuffer = framebuffer.clone();
            drop(guard);

            // Blit the framebuffer contents.
            capture::capture_framebuffer_region(frame, dst, &framebuffer)?;

            // If blur is off, use the unblurred texture.
            if self.blur_options.is_none() {
                inner.intermediate = Some(framebuffer.clone());
                return Ok(());
            }

            if let Some((blur, options)) = blur {
                let mut guard = frame.renderer();
                let renderer = guard.as_mut();
                match blur.render(renderer, &framebuffer, options) {
                    Ok(blurred) => inner.intermediate = Some(blurred),
                    Err(err) => {
                        warn!("error rendering blur: {err:?}");
                    }
                }
            }

            Ok(())
        })
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let Some(cache) = cache else {
            return Ok(());
        };
        let Some(inner) = cache.get::<RefCell<Inner>>() else {
            return Ok(());
        };
        let mut inner = inner.borrow_mut();
        let inner = &mut *inner;

        let Some(texture) = &inner.intermediate else {
            return Ok(());
        };

        // Clamp the same way as in capture_framebuffer().
        let output_rect = Rectangle::from_size(frame.output_size());
        let clamped_dst = match dst.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let clamp_offset = clamped_dst.loc - dst.loc;

        // Filter damage by subregion, reusing the stored Vec to avoid allocation.
        let filtered = &mut inner.subregion_damage;
        filtered.clear();

        if let Some(subregion) = &self.subregion {
            // Convert to subregion coordinates.
            let mut crop = src.to_logical(1., Transform::Normal, &src.size);
            crop.loc += self.geometry.loc;
            subregion.filter_damage(crop, dst, damage, filtered);
        } else {
            filtered.extend(damage.iter());
        };

        // Adjust for clamped dst.
        if clamped_dst != dst {
            let r = Rectangle::new(clamp_offset, clamped_dst.size);
            filtered.retain_mut(|d| {
                if let Some(mut crop) = d.intersection(r) {
                    crop.loc -= clamp_offset;
                    *d = crop;
                    true
                } else {
                    false
                }
            });
        }

        if filtered.is_empty() {
            return Ok(());
        }
        let damage = &filtered[..];

        // Adjust src proportionally to the dst clamping.
        let src_loc = src.loc.to_logical(1., Transform::Normal, &src.size);
        let dst_to_src = src.size / dst.size.to_f64();
        let crop = Rectangle::new(
            src_loc + clamp_offset.to_f64().upscale(dst_to_src).to_logical(1.),
            clamped_dst.size.to_f64().upscale(dst_to_src).to_logical(1.),
        );

        let program = Shaders::get_from_frame(frame).postprocess_and_clip.clone();
        let uniforms = program.is_some().then(|| {
            let mut uniforms = self.compute_uniforms(crop, frame.transformation()).to_vec();
            // The sampled framebuffer content is already in the frame blend space.
            uniforms.extend(FrameBlendState::uniforms_for_blend_space(frame));
            uniforms
        });
        let uniforms = uniforms.as_ref().map_or(&[][..], |x| &x[..]);

        let surface_tex_id = self
            .alpha_mask
            .as_ref()
            .filter(|_| program.is_some())
            .and_then(|mask| mask.gles_texture())
            .map(|texture| texture.tex_id());
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
            texture,
            Rectangle::from_size(texture.size().to_f64()),
            clamped_dst,
            damage,
            &[],
            // The intermediate texture has the same transform as the frame.
            frame.transformation().invert(),
            1.,
            program.as_ref(),
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

impl FramebufferEffectElement {
    /// Vulkan capture: blit the covered framebuffer region into a renderer-owned texture, run
    /// the blur ladder over it, and keep the result for [`Self::draw_vulkan`].
    #[cfg(feature = "vulkan")]
    fn capture_framebuffer_vulkan(
        &self,
        frame: &mut VulkanFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), VulkanError> {
        let _span = tracy_client::span!("FramebufferEffectElement::capture_framebuffer_vulkan");

        let output_rect = Rectangle::from_size(frame.output_size());
        let transform = frame.transformation();

        // Same clamping math as the GLES path: the renderer skips out-of-bounds
        // framebuffer pixels, so clamp the destination to the output ourselves.
        let clamped_dst = match dst.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let clamp_scale = clamped_dst.size.to_f64() / dst.size.to_f64();
        let dst = transform.transform_rect_in(clamped_dst, &output_rect.size);

        let size = src
            .size
            .to_logical(1., Transform::Normal)
            .upscale(clamp_scale)
            .to_physical_precise_round(self.scale);
        let size = transform.transform_size(size);
        let size = size.to_logical(1).to_buffer(1, Transform::Normal);

        let inner_cell =
            cache.get_or_insert::<RefCell<VulkanInner>, _>(|| RefCell::new(VulkanInner::default()));
        // Drop the previous composite clone so the blur output texture is unique again below.
        let _ = inner_cell.borrow_mut().intermediate.take();

        let framebuffer = {
            let mut inner = inner_cell.borrow_mut();
            if inner
                .framebuffer
                .as_ref()
                .is_some_and(|fb| fb.size() != size)
            {
                inner.framebuffer = None;
            }
            if let Some(fb) = &inner.framebuffer {
                fb.clone()
            } else {
                let mut guard = frame.renderer();
                let texture = guard.as_mut().create_buffer(Fourcc::Abgr8888, size)?;
                inner.framebuffer = Some(texture.clone());
                texture
            }
        };

        capture::capture_framebuffer_region_vulkan(frame, dst, &framebuffer)?;

        let Some(options) = self.blur_options else {
            inner_cell.borrow_mut().intermediate = Some(framebuffer);
            return Ok(());
        };

        let blurred = {
            let mut inner = inner_cell.borrow_mut();
            let mut blur = std::mem::take(&mut inner.blur);
            let result = blur.render_in_frame(frame, &framebuffer, options);
            inner.blur = blur;
            result?
        };

        inner_cell.borrow_mut().intermediate = blurred.or(Some(framebuffer));
        Ok(())
    }

    /// Vulkan composite: draws the captured/blurred backdrop through the background-effect
    /// program (which also carries the liquid-glass uniforms).
    #[cfg(feature = "vulkan")]
    fn draw_vulkan(
        &self,
        frame: &mut VulkanFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), VulkanError> {
        let _span = tracy_client::span!("FramebufferEffectElement::draw_vulkan");

        let Some(cache) = cache else {
            return Ok(());
        };
        let Some(inner) = cache.get::<RefCell<VulkanInner>>() else {
            return Ok(());
        };

        let Some(texture) = inner.borrow().intermediate.clone() else {
            return Ok(());
        };

        let (program, surface_fallback) = {
            let Some(shaders) = get_vulkan_frame(frame) else {
                return Ok(());
            };
            let Some(program) = shaders.postprocess_and_clip.clone() else {
                return Ok(());
            };
            (program, shaders.black_texture.clone())
        };

        // Clamp the same way as the capture path.
        let output_rect = Rectangle::from_size(frame.output_size());
        let clamped_dst = match dst.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let clamp_offset = clamped_dst.loc - dst.loc;

        let subregion_damage = &mut inner.borrow_mut().subregion_damage;
        subregion_damage.clear();
        if let Some(subregion) = &self.subregion {
            let mut crop = src.to_logical(1., Transform::Normal, &src.size);
            crop.loc += self.geometry.loc;
            subregion.filter_damage(crop, dst, damage, subregion_damage);
        } else {
            subregion_damage.extend(damage.iter());
        }
        if clamped_dst != dst {
            let r = Rectangle::new(clamp_offset, clamped_dst.size);
            subregion_damage.retain_mut(|d| {
                if let Some(mut crop) = d.intersection(r) {
                    crop.loc -= clamp_offset;
                    *d = crop;
                    true
                } else {
                    false
                }
            });
        }
        if subregion_damage.is_empty() {
            return Ok(());
        }
        let damage = &subregion_damage[..];

        let src_loc = src.loc.to_logical(1., Transform::Normal, &src.size);
        let dst_to_src = src.size / dst.size.to_f64();
        let crop = Rectangle::new(
            src_loc + clamp_offset.to_f64().upscale(dst_to_src).to_logical(1.),
            clamped_dst.size.to_f64().upscale(dst_to_src).to_logical(1.),
        );

        let mut owned: Vec<smithay::backend::renderer::vulkan::OwnedCustomUniform> = self
            .compute_uniforms(crop, frame.transformation())
            .iter()
            .filter_map(uniform_to_custom_owned)
            .collect();
        // The shader's own `alpha` / `tint` inputs (built-ins on GLES) plus the disabled
        // alpha-mask branch.
        for (name, value) in [
            ("alpha", 1.0f32),
            ("tint", 0.0f32),
            ("alpha_mask_enabled", 0.0f32),
            ("alpha_threshold", 0.0f32),
        ] {
            owned.push(smithay::backend::renderer::vulkan::OwnedCustomUniform {
                name: name.to_owned(),
                value: CustomUniformValue::Float(value),
            });
        }
        // The sampled framebuffer content is already in the frame blend space.
        owned.extend(
            super::blend::vulkan_blend_space_custom_uniforms(frame)
                .into_iter()
                .map(|u| smithay::backend::renderer::vulkan::OwnedCustomUniform {
                    name: u.name.to_owned(),
                    value: u.value,
                }),
        );
        let uniforms: Vec<CustomUniform<'_>> = owned.iter().map(|u| u.borrow()).collect();

        // The alpha-mask branch is disabled above, but the program still declares the sampler.
        let surface_tex = surface_fallback.unwrap_or_else(|| texture.clone());
        let textures = [("tex", &texture), ("surface_tex", &surface_tex)];

        frame.render_custom(&program, clamped_dst, damage, &uniforms, &textures, 1.0)
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        #[cfg(feature = "vulkan")]
        if let TtyFrame::Vulkan(multi) = frame {
            let vk_frame: &mut VulkanFrame<'_, '_> = multi.as_mut();
            return self
                .capture_framebuffer_vulkan(vk_frame, src, dst, cache)
                .map_err(|err| {
                    TtyRendererError::Vulkan(smithay::backend::renderer::multigpu::Error::Render(
                        err,
                    ))
                });
        }

        let Some(gles_frame) = frame.as_gles_frame() else {
            return Ok(());
        };
        RenderElement::<GlesRenderer>::capture_framebuffer(&self, gles_frame, src, dst, cache)?;
        Ok(())
    }

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
                .draw_vulkan(vk_frame, src, dst, damage, cache)
                .map_err(|err| {
                    TtyRendererError::Vulkan(smithay::backend::renderer::multigpu::Error::Render(
                        err,
                    ))
                });
        }

        let Some(gles_frame) = frame.as_gles_frame() else {
            return Ok(());
        };
        RenderElement::<GlesRenderer>::draw(
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

impl Inner {
    fn new(renderer: &mut GlesRenderer) -> Self {
        Inner {
            framebuffer: None,
            blur: Blur::new(renderer),
            intermediate: None,
            subregion_damage: Vec::new(),
        }
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame as _, Offscreen, Renderer};
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Point, Rectangle, Size, Transform};

    use super::*;
    use crate::render_helpers::background_effect::{BackgroundEffect, BackgroundEffectElement};
    use crate::render_helpers::shaders::init_vulkan;
    use crate::render_helpers::xray::XrayPos;
    use crate::render_helpers::{RenderCtx, RenderIntent, RenderTarget};

    /// End-to-end Vulkan background blur: capture the framebuffer, run the Dual-Kawase
    /// down/up ladder, composite through the background-effect program, and check that a
    /// sharp red/blue boundary comes back mixed. Skips silently without a device.
    #[test]
    fn vulkan_framebuffer_blur_composites_a_mixed_pixel() {
        let Ok(instance) = Instance::new(Version::VERSION_1_3, None) else {
            return;
        };
        let Ok(devices) = PhysicalDevice::enumerate(&instance) else {
            return;
        };
        for device in devices {
            let Ok(mut renderer) = VulkanRenderer::new(&device) else {
                continue;
            };
            init_vulkan(&mut renderer, &niri_config::Config::default());

            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((8, 8));
            let output_size = Size::<i32, smithay::utils::Physical>::from((8, 8));
            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("offscreen target");
            let mut fb = renderer.bind(&mut target).expect("bind target");
            let mut frame = renderer
                .render(&mut fb, output_size, Transform::Normal)
                .expect("begin frame");

            let dst = Rectangle::from_size(output_size);
            let left = Rectangle::new(Point::from((0, 0)), Size::from((4, 8)));
            let right = Rectangle::new(Point::from((4, 0)), Size::from((4, 8)));
            frame
                .clear(Color32F::new(1., 0., 0., 1.), &[left])
                .expect("clear red half");
            frame
                .clear(Color32F::new(0., 0., 1., 1.), &[right])
                .expect("clear blue half");

            let geometry = Rectangle::from_size(Size::from((8., 8.)));
            let params = RenderParams {
                geometry,
                original_geometry: geometry,
                subregion: None,
                clip: None,
                scale: 1.0,
            };
            let effect = FramebufferEffect::new();
            let element = effect.render(
                None,
                params,
                Some(BlurOptions {
                    passes: 3,
                    offset: 1.0,
                }),
                0.0,
                1.0,
                1.0,
                1.0,
                None,
                0.0,
                None,
            );

            let cache = UserDataMap::default();
            let src = element.src();
            element
                .capture_framebuffer_vulkan(&mut frame, src, dst, &cache)
                .expect("vulkan framebuffer capture + blur");
            element
                .draw_vulkan(&mut frame, src, dst, &[dst], Some(&cache))
                .expect("vulkan background composite");
            frame.finish().expect("finish frame");

            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Argb8888)
                .expect("copy framebuffer");
            let pixels = renderer.map_texture(&mapping).expect("map framebuffer");
            // Argb8888 little-endian: B, G, R, A. The blur smashed the boundary together, so
            // the middle pixel must contain a substantial amount of BOTH colours.
            let center = (4 * 8 + 4) * 4;
            let (b, g, r, a) = (
                pixels[center],
                pixels[center + 1],
                pixels[center + 2],
                pixels[center + 3],
            );
            assert_eq!(g, 0, "unexpected green channel: {g}");
            assert_eq!(a, 255, "composite should be opaque, got alpha {a}");
            assert!(
                (24..=231).contains(&b) && (24..=231).contains(&r),
                "blur did not mix the boundary: B={b} R={r}"
            );
            eprintln!(
                "Vulkan framebuffer blur composite OK on {} (B={b} R={r})",
                device.name()
            );
            return;
        }
    }

    /// Renders the same scene through the Vulkan background composite with and without liquid
    /// glass and checks that the liquid-glass uniforms actually reach the shader. This is the
    /// evidence that liquid glass rides the `postprocess_and_clip` program on Vulkan.
    #[test]
    fn vulkan_liquid_glass_uniforms_change_the_composite() {
        fn render_scene(
            renderer: &mut VulkanRenderer,
            liquid_glass: Option<LiquidGlassOptions>,
        ) -> Vec<u8> {
            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((32, 32));
            let output_size = Size::<i32, smithay::utils::Physical>::from((32, 32));
            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("offscreen target");
            let mut fb = renderer.bind(&mut target).expect("bind target");
            let mut frame = renderer
                .render(&mut fb, output_size, Transform::Normal)
                .expect("begin frame");

            let dst = Rectangle::from_size(output_size);
            frame
                .clear(
                    Color32F::new(1., 0., 0., 1.),
                    &[Rectangle::new(Point::from((0, 0)), Size::from((16, 32)))],
                )
                .expect("clear red half");
            frame
                .clear(
                    Color32F::new(0., 0., 1., 1.),
                    &[Rectangle::new(Point::from((16, 0)), Size::from((16, 32)))],
                )
                .expect("clear blue half");

            let geometry = Rectangle::from_size(Size::from((32., 32.)));
            let params = RenderParams {
                geometry,
                original_geometry: geometry,
                subregion: None,
                clip: None,
                scale: 1.0,
            };
            let effect = FramebufferEffect::new();
            let element = effect.render(
                None,
                params,
                None,
                0.0,
                1.0,
                1.0,
                1.0,
                liquid_glass,
                0.0,
                None,
            );
            let cache = UserDataMap::default();
            let src = element.src();
            element
                .capture_framebuffer_vulkan(&mut frame, src, dst, &cache)
                .expect("vulkan framebuffer capture");
            element
                .draw_vulkan(&mut frame, src, dst, &[dst], Some(&cache))
                .expect("vulkan background composite");
            frame.finish().expect("finish frame");

            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Argb8888)
                .expect("copy framebuffer");
            renderer
                .map_texture(&mapping)
                .expect("map framebuffer")
                .to_vec()
        }

        let Ok(instance) = Instance::new(Version::VERSION_1_3, None) else {
            return;
        };
        let Ok(devices) = PhysicalDevice::enumerate(&instance) else {
            return;
        };
        for device in devices {
            let Ok(mut renderer) = VulkanRenderer::new(&device) else {
                continue;
            };
            init_vulkan(&mut renderer, &niri_config::Config::default());

            let plain = render_scene(&mut renderer, None);
            let glass = render_scene(
                &mut renderer,
                Some(LiquidGlassOptions {
                    refraction_strength: 100.0,
                    power_factor: 2.0,
                    refraction_power: 1.0,
                    glow_weight: 1.0,
                    edge_lighting: 1.0,
                    edge_thickness: 1.0,
                    brightness: 1.0,
                    contrast: 1.0,
                    saturation: 1.0,
                    ..Default::default()
                }),
            );

            let differing = plain
                .as_chunks::<4>()
                .0
                .iter()
                .zip(glass.chunks_exact(4))
                .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
                .count();
            assert!(
                differing > 0,
                "liquid-glass uniforms had no visible effect on the Vulkan composite"
            );
            eprintln!(
                "Vulkan liquid-glass uniforms affect {differing} pixels on {}",
                device.name()
            );
            return;
        }
    }

    /// Popup-style background effect: blur on, xray off, no surface blur region. This goes
    /// through the same `BackgroundEffect` wrapper the popup and layer-popup render path uses,
    /// so it exercises the non-xray `FramebufferEffectElement` Vulkan capture/composite that
    /// used to be gated behind a GLES renderer. Skips silently without a device.
    #[test]
    fn vulkan_popup_background_effect_draws_blur() {
        let Ok(instance) = Instance::new(Version::VERSION_1_3, None) else {
            return;
        };
        let Ok(devices) = PhysicalDevice::enumerate(&instance) else {
            return;
        };
        for device in devices {
            let Ok(mut renderer) = VulkanRenderer::new(&device) else {
                continue;
            };
            init_vulkan(&mut renderer, &niri_config::Config::default());

            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((8, 8));
            let output_size = Size::<i32, smithay::utils::Physical>::from((8, 8));

            let mut effect = BackgroundEffect::new();
            effect.update_config(niri_config::Blur {
                noise: 0.,
                saturation: 1.,
                brightness: 1.,
                contrast: 1.,
                ..Default::default()
            });
            effect.update_render_elements(
                niri_config::CornerRadius::default(),
                niri_config::BackgroundEffect {
                    blur: Some(true),
                    xray: Some(false),
                    ..Default::default()
                },
                false,
            );
            assert!(effect.is_visible(), "popup blur effect should be visible");

            let geometry = Rectangle::from_size(Size::from((8., 8.)));
            let params = RenderParams {
                geometry,
                original_geometry: geometry,
                subregion: None,
                // Popups render with `clip_to_geometry = false`; without a surface blur
                // region that means no clip, which `FramebufferEffect::render` fills in.
                clip: None,
                scale: 1.0,
            };
            let ctx = RenderCtx {
                renderer: &mut renderer,
                target: RenderTarget::Output,
                intent: RenderIntent::Normal,
                xray: None,
                shader_time: 0.,
            };

            let mut elements = Vec::new();
            effect.render(
                ctx,
                None,
                params,
                XrayPos::default(),
                0.,
                None,
                &mut |elem| elements.push(elem),
            );
            assert_eq!(elements.len(), 1, "popup effect should push one element");
            let BackgroundEffectElement::FramebufferEffect(elem) = &elements[0] else {
                panic!("popup effect with xray off must use the framebuffer path");
            };

            // Build the scene only after the element exists: `RenderCtx` borrows the renderer
            // mutably, while a live frame holds that same borrow.
            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("offscreen target");
            let mut fb = renderer.bind(&mut target).expect("bind target");
            let mut frame = renderer
                .render(&mut fb, output_size, Transform::Normal)
                .expect("begin frame");
            let dst = Rectangle::from_size(output_size);
            frame
                .clear(
                    Color32F::new(1., 0., 0., 1.),
                    &[Rectangle::new(Point::from((0, 0)), Size::from((4, 8)))],
                )
                .expect("clear red half");
            frame
                .clear(
                    Color32F::new(0., 0., 1., 1.),
                    &[Rectangle::new(Point::from((4, 0)), Size::from((4, 8)))],
                )
                .expect("clear blue half");

            let cache = UserDataMap::default();
            let src = elem.src();
            elem.capture_framebuffer_vulkan(&mut frame, src, dst, &cache)
                .expect("vulkan popup background capture");
            elem.draw_vulkan(&mut frame, src, dst, &[dst], Some(&cache))
                .expect("vulkan popup background composite");
            frame.finish().expect("finish frame");

            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Argb8888)
                .expect("copy framebuffer");
            let pixels = renderer.map_texture(&mapping).expect("map framebuffer");
            let center = (4 * 8 + 4) * 4;
            let (b, g, r, a) = (
                pixels[center],
                pixels[center + 1],
                pixels[center + 2],
                pixels[center + 3],
            );
            assert_eq!(g, 0, "unexpected green channel: {g}");
            assert_eq!(a, 255, "popup composite should be opaque, got alpha {a}");
            assert!(
                (24..=231).contains(&b) && (24..=231).contains(&r),
                "popup blur did not mix the boundary: B={b} R={r}"
            );
            eprintln!(
                "Vulkan popup background blur composite OK on {} (B={b} R={r})",
                device.name()
            );
            return;
        }
    }
}
