use std::sync::{Arc, Mutex};

use niri_config::CornerRadius;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::utils::{RendererSurfaceStateUserData, import_surface};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::VulkanTexture;
use smithay::utils::{Logical, Point, Rectangle, Scale};
use smithay::wayland::compositor::{SurfaceData, with_states};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::backend::tty::TtyOffscreen;
use crate::handlers::background_effect::get_cached_blur_region;
use crate::niri_render_elements;
use crate::render_helpers::RenderCtx;
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::damage::ExtraDamage;
use crate::render_helpers::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement};
use crate::render_helpers::liquid_glass::LiquidGlassOptions;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::shaders::Shaders;
use crate::render_helpers::xray::{XrayElement, XrayPos};
use crate::utils::region::TransformedRegion;
use crate::utils::surface_geo;

#[derive(Debug)]
pub struct BackgroundEffect {
    nonxray: FramebufferEffect,
    /// Damage when options change.
    damage: ExtraDamage,
    /// Corner radius for clipping.
    ///
    /// Stored here in addition to `RenderParams` to damage when it changes.
    // FIXME: would be good to remove this duplication of radius.
    corner_radius: CornerRadius,
    blur_config: niri_config::Blur,
    options: Options,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Options {
    pub blur: bool,
    pub xray: bool,
    pub ignore_opacity: Option<f64>,
    pub noise: Option<f64>,
    pub saturation: Option<f64>,
    pub liquid_glass: Option<LiquidGlassOptions>,
}

impl Options {
    fn is_visible(&self) -> bool {
        self.xray
            || self.blur
            || self.noise.is_some_and(|x| x > 0.)
            || self.saturation.is_some_and(|x| x != 1.)
            || self.liquid_glass.is_some()
    }
}

/// Render-time parameters.
#[derive(Debug)]
pub struct RenderParams {
    /// Geometry of the background effect (may be expanded by edge_padding).
    pub geometry: Rectangle<f64, Logical>,
    /// Original window geometry before expansion (used by xray shader for SDF).
    pub original_geometry: Rectangle<f64, Logical>,
    /// Effect subregion, will be clipped to `geometry`.
    ///
    /// `subregion.iter()` should return `geometry`-relative rectangles.
    pub subregion: Option<TransformedRegion>,
    /// Geometry and radius for clipping in the same coordinate space as `geometry`.
    pub clip: Option<(Rectangle<f64, Logical>, CornerRadius)>,
    /// Scale to use for rounding to physical pixels.
    pub scale: f64,
}

impl RenderParams {
    fn fit_clip_radius(&mut self) {
        if let Some((geo, radius)) = &mut self.clip {
            // HACK: increase radius to avoid slight bleed on rounded corners.
            *radius = radius.expanded_by(1.);

            *radius = radius.fit_to(geo.size.w as f32, geo.size.h as f32);
        }
    }
}

niri_render_elements! {
    BackgroundEffectElement => {
        FramebufferEffect = FramebufferEffectElement,
        Xray = XrayElement,
        ExtraDamage = ExtraDamage,
    }
}

impl BackgroundEffect {
    pub fn new() -> Self {
        Self {
            nonxray: FramebufferEffect::new(),
            damage: ExtraDamage::new(),
            corner_radius: CornerRadius::default(),
            blur_config: niri_config::Blur::default(),
            options: Options::default(),
        }
    }

    /// Damage the background effect, for example when a blur subregion changes.
    pub fn damage(&mut self) {
        self.damage.damage_all();
        self.nonxray.damage();
    }

    pub fn update_config(&mut self, config: niri_config::Blur) {
        if self.blur_config == config {
            return;
        }

        self.blur_config = config;
        self.damage.damage_all();
        self.nonxray.damage();
    }

    pub fn update_render_elements(
        &mut self,
        corner_radius: CornerRadius,
        effect: niri_config::BackgroundEffect,
        has_blur_region: bool,
    ) {
        // If the surface explicitly requests a blur region, default blur to true.
        let blur = if has_blur_region {
            effect.blur != Some(false)
        } else {
            effect.blur == Some(true)
        };

        let liquid_glass = effect.liquid_glass.map(LiquidGlassOptions::from);

        trace!("liquid glass options: {liquid_glass:?}");

        let mut options = Options {
            blur,
            xray: effect.xray == Some(true),
            ignore_opacity: effect.ignore_opacity,
            noise: effect.noise,
            saturation: effect.saturation,
            liquid_glass,
        };

        // If we have some background effect but xray wasn't explicitly set, default it to true
        // since it's cheaper.
        if options.is_visible() && effect.xray.is_none() {
            options.xray = true;
        }

        if self.options == options && self.corner_radius == corner_radius {
            return;
        }

        self.options = options;
        self.corner_radius = corner_radius;
        self.damage.damage_all();
        self.nonxray.damage();
    }

    pub fn is_visible(&self) -> bool {
        self.options.is_visible()
    }

    pub fn render<R: NiriRenderer>(
        &self,
        ctx: RenderCtx<R>,
        ns: Option<usize>,
        mut params: RenderParams,
        xray_pos: XrayPos,
        padding_pixels: f32,
        alpha_mask: Option<AlphaMask>,
        push: &mut dyn FnMut(BackgroundEffectElement),
    ) {
        if !self.is_visible() {
            return;
        }

        if let Some(clip) = &mut params.clip {
            clip.1 = self.corner_radius;
        }
        params.fit_clip_radius();

        let damage = self.damage.render(params.geometry);

        let blur = self.options.blur && !self.blur_config.off;
        let blur_options = blur.then_some(BlurOptions::from(self.blur_config));
        let noise = if blur { self.blur_config.noise } else { 0. };
        let noise = self.options.noise.unwrap_or(noise) as f32;
        let saturation = if blur {
            self.blur_config.saturation
        } else {
            1.
        };
        let saturation = self.options.saturation.unwrap_or(saturation) as f32;

        let alpha_mask = match (self.options.ignore_opacity, alpha_mask) {
            (Some(threshold), Some(mut m)) => {
                m.threshold = threshold as f32;
                Some(m)
            }
            _ => None,
        };

        // Xray samples the shared, scene-wide backdrop baked by `Xray`/`EffectBuffer` through
        // whichever renderer drives this output: GLES (the winit and GLES TTY paths) or Vulkan
        // (the TTY Vulkan path). `Niri::bake_xray_elements` bakes the buffers with that same
        // renderer before the frame is drawn. On a renderer with neither backend (and when no
        // xray buffer is available), fall back to the per-window framebuffer effect below, which
        // captures the composited scene under the window.
        let xray_gles_program = ctx
            .renderer
            .as_gles_renderer()
            .map(|gles| Shaders::get(gles).postprocess_and_clip.clone());
        #[cfg(feature = "vulkan")]
        let xray_vulkan_available = ctx.renderer.as_vulkan_renderer().is_some();
        #[cfg(not(feature = "vulkan"))]
        let xray_vulkan_available = false;
        let use_xray = self.options.xray
            && ctx.xray.is_some()
            && (xray_gles_program.is_some() || xray_vulkan_available);

        if use_xray {
            // Xray doesn't need expanded geometry — it samples from the scene-wide
            // background buffer. Use original window geometry for correct SDF.
            // Undo the xray_pos offset that render_for_tile added for expanded geometry.
            let expansion_offset = params.geometry.loc - params.original_geometry.loc;
            let xray_xray_pos =
                xray_pos.offset(Point::new(-expansion_offset.x, -expansion_offset.y));
            params.geometry = params.original_geometry;

            let xray = ctx.xray.unwrap();
            push(damage.into());
            xray.render(
                ctx.target,
                xray_gles_program.flatten(),
                params,
                xray_xray_pos,
                blur,
                noise,
                saturation,
                self.options.liquid_glass,
                padding_pixels,
                alpha_mask,
                &mut |elem| push(elem.into()),
            );
        } else {
            trace!(
                "liquid glass on the non-xray path, padding_pixels={padding_pixels}, options={:?}",
                self.options.liquid_glass
            );
            let elem = self.nonxray.render(
                ns,
                params,
                blur_options,
                noise,
                saturation,
                self.options.liquid_glass,
                padding_pixels,
                alpha_mask,
            );
            push(elem.into());
        }
    }
}

/// Texture mask for `background-effect ignore-opacity`, plus where it maps to in the effect
/// geometry.
///
/// The texture is backend-agnostic ([`TtyOffscreen`]): GLES producers store the surface's
/// `GlesTexture`, Vulkan producers the surface's `VulkanTexture`. Consumers pick the variant
/// matching their draw backend.
#[derive(Debug, Clone)]
pub struct AlphaMask {
    pub texture: TtyOffscreen,
    pub surface_geo: Rectangle<f64, Logical>,
    pub threshold: f32,
}

impl AlphaMask {
    /// The mask texture as a raw GLES texture, if it was produced by a GLES renderer.
    pub fn gles_texture(&self) -> Option<&GlesTexture> {
        self.texture.as_gles()
    }

    /// The mask texture as a raw Vulkan texture, if it was produced by a Vulkan renderer.
    #[cfg(feature = "vulkan")]
    pub fn vulkan_texture(&self) -> Option<&VulkanTexture> {
        self.texture.as_vulkan()
    }
}

/// Imports the surface through `renderer` and wraps its texture into an [`AlphaMask`].
fn import_alpha_mask<R: NiriRenderer>(
    renderer: &mut R,
    states: &SurfaceData,
    surface_geo: Rectangle<f64, Logical>,
    threshold: f32,
) -> Option<AlphaMask> {
    let _ = import_surface(renderer, states);
    let data = states.data_map.get::<RendererSurfaceStateUserData>();
    let texture =
        data.and_then(|data| data.lock().unwrap().texture(renderer.context_id()).cloned())?;

    Some(AlphaMask {
        texture: R::wrap_texture(texture),
        surface_geo,
        threshold,
    })
}

fn render_params_for_tile(
    geometry: Rectangle<f64, Logical>,
    scale: f64,
    clip_to_geometry: bool,
    block_out: bool,
    blur_region: Option<Arc<Vec<Rectangle<i32, Logical>>>>,
    surface_geo: Rectangle<f64, Logical>,
    surface_anim_scale: Scale<f64>,
    padding: f64,
) -> Option<RenderParams> {
    // Effects not requested by the surface itself are drawn to match the geometry.
    let mut clip = true;

    let mut effect_geometry = geometry;

    // Expand geometry by padding for liquid glass (captures content beyond window)
    if padding > 0.0 {
        effect_geometry.loc.x -= padding;
        effect_geometry.loc.y -= padding;
        effect_geometry.size.w += padding * 2.0;
        effect_geometry.size.h += padding * 2.0;
    }

    let mut subregion = None;
    if let Some(rects) = blur_region {
        if rects.is_empty() {
            // Surface has a set, but empty blur region.
            return None;
        } else {
            // If the surface itself requests the effects, apply different defaults.
            clip = clip_to_geometry;

            // Use geometry-shaped blur for blocked-out windows to avoid unintentionally
            // leaking any surface shapes. We render those windows as geometry-shaped solid
            // rectangles anyway.
            if block_out {
                clip = true;
            } else {
                let mut surface_geo = surface_geo.upscale(surface_anim_scale);
                surface_geo.loc += geometry.loc;

                subregion = Some(TransformedRegion {
                    rects,
                    scale: surface_anim_scale,
                    offset: surface_geo.loc,
                });

                surface_geo = surface_geo
                    .to_physical_precise_round(scale)
                    .to_logical(scale);
                effect_geometry = surface_geo;
            }
        }
    }

    // This corner radius is reset to self.corner_radius in render().
    let clip = clip.then_some((geometry, CornerRadius::default()));

    Some(RenderParams {
        geometry: effect_geometry,
        original_geometry: geometry,
        subregion,
        clip,
        scale,
    })
}

/// Per-surface background effect stored in its data map.
struct SurfaceBackgroundEffect(Mutex<BackgroundEffect>);

impl SurfaceBackgroundEffect {
    fn get(states: &SurfaceData) -> &Self {
        states
            .data_map
            .get_or_insert(|| SurfaceBackgroundEffect(Mutex::new(BackgroundEffect::new())))
    }
}

pub fn damage_surface(states: &SurfaceData) {
    if let Some(effect) = states.data_map.get::<SurfaceBackgroundEffect>() {
        effect.0.lock().unwrap().damage();
    }
}

pub fn render_for_tile<R: NiriRenderer>(
    ctx: RenderCtx<R>,
    ns: Option<usize>,
    geometry: Rectangle<f64, Logical>,
    scale: f64,
    clip_to_geometry: bool,
    surface: &WlSurface,
    surface_off: Point<f64, Logical>,
    surface_anim_scale: Scale<f64>,
    blur_config: niri_config::Blur,
    radius: CornerRadius,
    effect: niri_config::BackgroundEffect,
    should_block_out: bool,
    xray_pos: XrayPos,
    push: &mut dyn FnMut(BackgroundEffectElement),
) {
    with_states(surface, |states| {
        let background_effect = SurfaceBackgroundEffect::get(states);
        let mut background_effect = background_effect.0.lock().unwrap();

        let blur_region = get_cached_blur_region(states);
        let has_blur_region = blur_region.as_ref().is_some_and(|r| !r.is_empty());

        background_effect.update_config(blur_config);
        background_effect.update_render_elements(radius, effect, has_blur_region);

        if !background_effect.is_visible() {
            return;
        }

        let mut surface_geo_logical = surface_geo(states).unwrap_or_default().to_f64();
        surface_geo_logical.loc += surface_off;

        let alpha_mask = if background_effect.options.ignore_opacity.is_some() && !should_block_out
        {
            // The mask is sampled by the composite: through texture unit 1 on GLES, and as a
            // named sampler in the Vulkan custom program. Import (and wrap) the surface texture
            // with the raw renderer of the active backend.
            let mut mask_geo = surface_geo_logical.upscale(surface_anim_scale);
            mask_geo.loc += geometry.loc;

            if let Some(gles) = ctx.renderer.as_gles_renderer() {
                import_alpha_mask(gles, states, mask_geo, 0.0)
            } else {
                #[cfg(feature = "vulkan")]
                {
                    if let Some(vulkan) = ctx.renderer.as_vulkan_renderer() {
                        import_alpha_mask(vulkan, states, mask_geo, 0.0)
                    } else {
                        None
                    }
                }
                #[cfg(not(feature = "vulkan"))]
                {
                    None
                }
            }
        } else {
            None
        };

        // Compute padding from liquid glass edge_padding
        let padding = if let Some(lg) = &background_effect.options.liquid_glass {
            let min_dim = geometry.size.w.min(geometry.size.h);
            lg.edge_padding * min_dim
        } else {
            0.0
        };

        let Some(params) = render_params_for_tile(
            geometry,
            scale,
            clip_to_geometry,
            should_block_out,
            blur_region,
            surface_geo_logical,
            surface_anim_scale,
            padding,
        ) else {
            return;
        };

        let xray_pos = xray_pos.offset(params.geometry.loc - geometry.loc);
        background_effect.render(ctx, ns, params, xray_pos, padding as f32, alpha_mask, push);
    });
}
