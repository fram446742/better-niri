use std::collections::HashMap;
use std::rc::Rc;

use niri_config::decoration_shader::DecorationLight as LightConfig;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::Uniform;
use smithay::backend::renderer::{
    Bind as _, ContextId, ErasedContextId, Offscreen as _, Renderer as _, Texture as _,
};
use smithay::utils::{Buffer, Logical, Point, Size};

use super::blur::{Blur, BlurOptions};
#[cfg(feature = "vulkan")]
use super::blur_vulkan::VulkanBlur;
use super::offscreen::{OffscreenBuffer, OffscreenRenderElement};
use super::renderer::NiriRenderer;
#[cfg(feature = "vulkan")]
use super::shader_element::vulkan_texture_map;
use super::shader_element::{ShaderRenderElement, gles_texture_map};
use super::shaders::ProgramType;
use super::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::backend::tty::TtyOffscreen;
use crate::layout::focus_ring::FocusRingRenderElement;

/// Cached emission and blur textures for one decoration. The light never samples client content.
#[derive(Debug)]
pub struct DecorationLight {
    emission: OffscreenBuffer,
    bounds: SolidColorBuffer,
    /// GLES Dual-Kawase blur, cached against [`Self::renderer_context`].
    blur: Option<Blur>,
    blur_format: Option<Fourcc>,
    /// Vulkan Dual-Kawase blur, cached against [`Self::renderer_context`].
    #[cfg(feature = "vulkan")]
    vulkan_blur: VulkanBlur,
    /// Context of the renderer that produced the cached blur textures. Backend-agnostic so a
    /// switch between GLES and Vulkan (or between renderers) invalidates the cached blur.
    renderer_context: Option<ErasedContextId>,
    composite: ShaderRenderElement,
    config: Option<(LightConfig, f64)>,
    offset: Point<f64, Logical>,
}

impl Default for DecorationLight {
    fn default() -> Self {
        Self {
            emission: OffscreenBuffer::default(),
            bounds: SolidColorBuffer::new((0., 0.), [0.; 4]),
            blur: None,
            blur_format: None,
            #[cfg(feature = "vulkan")]
            vulkan_blur: VulkanBlur::default(),
            renderer_context: None,
            composite: ShaderRenderElement::empty(ProgramType::DecorationLight, Kind::Unspecified)
                .with_screen_blend(),
            config: None,
            offset: Point::default(),
        }
    }
}

impl DecorationLight {
    pub fn id(&self) -> &Id {
        self.composite.id()
    }

    pub fn clear(&mut self) {
        self.emission.clear();
        self.blur = None;
        self.blur_format = None;
        #[cfg(feature = "vulkan")]
        self.vulkan_blur.clear();
        self.renderer_context = None;
        self.composite.clear_textures();
        self.config = None;
    }

    /// Uses half-resolution logical pixels regardless of monitor DPI. The shader itself
    /// retains its original AA scale and exact frame time. No fullscreen capture is needed.
    ///
    /// The emission prepass is backend-agnostic: GLES renderers blur it with the GLES
    /// Dual-Kawase implementation, Vulkan renderers with [`VulkanBlur`], and the composite
    /// samples whichever texture the active renderer produced.
    pub fn render<R: NiriRenderer>(
        &mut self,
        renderer: &mut R,
        mut sources: Vec<FocusRingRenderElement>,
        config: LightConfig,
        width: f64,
        location: Point<f64, Logical>,
        alpha: f32,
    ) -> anyhow::Result<ShaderRenderElement>
    where
        FocusRingRenderElement: RenderElement<R>,
    {
        const SCALE: f64 = 0.5;
        let geo = super::encompassing_geo(SCALE.into(), sources.iter())
            .to_f64()
            .to_logical(SCALE);
        // Include the blur taps and bilinear footprints, with a transparent guard band so
        // clamped texture sampling cannot smear lit pixels across the boundary.
        let margin = (config.spread.0 * 2. + 8.).ceil();
        let size = geo.size + Size::from((margin * 2., margin * 2.));
        self.bounds.resize(size);
        sources.push(
            SolidColorRenderElement::from_buffer(
                &self.bounds,
                geo.loc - Point::from((margin, margin)),
                1.,
                Kind::Unspecified,
            )
            .into(),
        );
        if self
            .config
            .is_some_and(|(previous, _)| previous.threshold != config.threshold)
        {
            self.emission.clear();
        }
        let (emission, sync, data) = self.emission.render(renderer, SCALE.into(), &sources)?;
        // The Vulkan blur runs in its own short submission, so make it (and the composite
        // frame) wait for the emission prepass. This only schedules the wait on Vulkan.
        renderer.wait(&sync).map_err(anyhow::Error::from)?;
        self.build_composite(renderer, &emission, data.damaged, config, width)?;
        self.composite.set_alpha(alpha);
        Ok(self.composite.clone().with_location(location + self.offset))
    }

    /// Blurs the emission texture and caches the composite element for it.
    ///
    /// Dispatches on the texture the emission buffer produced, so the same code serves GLES and
    /// Vulkan callers.
    fn build_composite<R: NiriRenderer>(
        &mut self,
        renderer: &mut R,
        emission: &OffscreenRenderElement,
        damaged: bool,
        config: LightConfig,
        width: f64,
    ) -> anyhow::Result<()> {
        // Dual Kawase: keep offsets small and use the mip ladder for wide spill. For P passes,
        // conservative support is 3 * offset * (2^P - 1) source pixels.
        let radius = config.spread.0 * 0.5;
        let passes = ((radius / 3. + 1.).log2().ceil() as u8).clamp(1, 6);
        let offset = radius / (3. * (2_f64.powi(passes as i32) - 1.));
        let options = BlurOptions { passes, offset };
        let params = (config, width);

        #[cfg(feature = "vulkan")]
        if let Some(source) = emission.texture_vulkan() {
            let context = {
                let Some(vk) = renderer.as_vulkan_renderer() else {
                    anyhow::bail!(
                        "decoration light emission is a Vulkan texture, but the renderer has no Vulkan backend"
                    );
                };
                ContextId::erased(&vk.context_id())
            };
            let context_changed = self.renderer_context.as_ref() != Some(&context);
            let changed = self.config != Some(params) || damaged || context_changed;
            if changed {
                self.composite.clear_textures();
                if context_changed {
                    // Cached textures belong to the old renderer.
                    self.vulkan_blur.clear();
                }
                let mut blur = std::mem::take(&mut self.vulkan_blur);
                let result = match renderer.as_vulkan_renderer() {
                    Some(vk) => blur.render(vk, source, options),
                    None => unreachable!("checked above"),
                };
                self.vulkan_blur = blur;
                let texture = result?.ok_or_else(|| anyhow::anyhow!("blur shader unavailable"))?;
                let texture_size = texture.size();
                let textures =
                    vulkan_texture_map(HashMap::from([("light_texture".to_owned(), texture)]));
                self.update_composite(emission, config, width, texture_size, textures);
                self.renderer_context = Some(context);
            }
            return Ok(());
        }

        let Some(source) = emission.texture_gles() else {
            anyhow::bail!("decoration light emission is not a texture of the active renderer");
        };
        let Some(gles) = renderer.as_gles_renderer() else {
            anyhow::bail!(
                "decoration light emission is a GLES texture, but the renderer has no GLES backend"
            );
        };
        let context = ContextId::erased(&gles.context_id());
        let changed = self.config != Some(params)
            || damaged
            || self.renderer_context.as_ref() != Some(&context)
            || self
                .blur
                .as_ref()
                .is_none_or(|b| b.context_id() != gles.context_id());
        if changed {
            self.composite.clear_textures();
            if self
                .blur
                .as_ref()
                .is_none_or(|b| b.context_id() != gles.context_id())
            {
                self.blur = Blur::new(gles);
                self.blur_format = None;
            }
            let blur = self
                .blur
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("blur shader unavailable"))?;
            blur.prepare_textures(
                |_, size| {
                    if let Some(format) = self.blur_format {
                        return gles.create_buffer(format, size);
                    }
                    // Bloom amplifies very small values. Eight-bit intermediates produce
                    // visible coloured bands; prefer float, with compatible fallbacks.
                    for format in [Fourcc::Abgr16161616f, Fourcc::Abgr2101010] {
                        if let Ok(mut texture) = gles.create_buffer(format, size)
                            && gles.bind(&mut texture).is_ok()
                        {
                            self.blur_format = Some(format);
                            return Ok(texture);
                        }
                    }
                    self.blur_format = Some(Fourcc::Abgr8888);
                    gles.create_buffer(Fourcc::Abgr8888, size)
                },
                source,
                options,
            )?;
            let texture = blur.render(gles, source, options)?;
            let texture_size = texture.size();
            let textures = gles_texture_map(HashMap::from([("light_texture".to_owned(), texture)]));
            self.update_composite(emission, config, width, texture_size, textures);
            self.renderer_context = Some(context);
        }
        Ok(())
    }

    /// Publishes the blurred texture and uniforms to the cached composite element.
    fn update_composite(
        &mut self,
        emission: &OffscreenRenderElement,
        config: LightConfig,
        width: f64,
        texture_size: Size<i32, Buffer>,
        textures: HashMap<String, TtyOffscreen>,
    ) {
        let logical_size = emission.logical_size();
        // OffscreenBuffer may reuse a larger allocation; use its source rectangle only.
        let tex_scale = [
            emission.src().size.w as f32 / texture_size.w as f32,
            emission.src().size.h as f32 / texture_size.h as f32,
        ];
        let gain = config.intensity.0 * (config.spread.0 / width.max(1.)).max(1.);
        self.composite.update(
            logical_size,
            None,
            1.,
            1.,
            Rc::new([
                Uniform::new("light_gain", gain as f32),
                Uniform::new("light_tex_scale", tex_scale),
            ]),
            textures,
        );
        self.offset = emission.offset();
        self.config = Some((config, width));
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use niri_config::FloatOrInt;
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{Color32F, ExportMem, Frame as _};
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Physical, Rectangle, Scale, Size, Transform};

    use super::*;
    use crate::render_helpers::shaders::init_vulkan;

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

    /// Renders a decoration-light emission through the Vulkan blur and composites it with the
    /// real `decoration_light` program. The blurred light must spill outside the sharp red
    /// source, while far pixels stay dark. Skips silently without a Vulkan device.
    #[test]
    fn vulkan_decoration_light_blurs_emission_and_composites() {
        for_each_vulkan_renderer(|renderer| {
            init_vulkan(renderer, &niri_config::Config::default());

            let config = LightConfig {
                enable: true,
                spread: FloatOrInt(8.),
                intensity: FloatOrInt(1.),
                threshold: FloatOrInt(0.5),
            };

            // A sharp 2×2 red emission in a 32×32 bounds, so the blur can spill into empty
            // space and be sampled back.
            let color = SolidColorBuffer::new((2., 2.), [1., 0., 0., 1.]);
            let source = SolidColorRenderElement::from_buffer(
                &color,
                Point::from((0., 0.)),
                1.,
                Kind::Unspecified,
            );
            let bounds = Rectangle::<i32, Physical>::new((0, 0).into(), (32, 32).into());
            let buffer = OffscreenBuffer::default();
            let (emission, sync, data) = buffer
                .render_with_bounds(renderer, Scale::from(1.), bounds, &[source])
                .expect("render the emission prepass");
            renderer.wait(&sync).expect("wait for the emission prepass");
            assert!(
                emission.texture_vulkan().is_some(),
                "expected a Vulkan emission texture"
            );

            let mut light = DecorationLight::default();
            light
                .build_composite(renderer, &emission, data.damaged, config, 400.)
                .expect("build the decoration light composite");
            let composite = light.composite.clone().with_location(light.offset);

            let buffer_size = Size::<i32, Buffer>::from((32, 32));
            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("readback target");
            let mut fb = renderer.bind(&mut target).expect("bind readback target");
            let mut frame = renderer
                .render(&mut fb, Size::from((32, 32)), Transform::Normal)
                .expect("begin readback frame");
            let dst = composite.geometry(Scale::from(1.));
            frame
                .clear(Color32F::TRANSPARENT, &[dst])
                .expect("clear readback target");
            RenderElement::<VulkanRenderer>::draw(
                &composite,
                &mut frame,
                composite.src(),
                dst,
                &[dst],
                &[],
                None,
            )
            .expect("draw the decoration light composite");
            let _sync = frame.finish().expect("finish readback frame");

            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Argb8888)
                .expect("copy the readback framebuffer");
            let pixels = renderer.map_texture(&mapping).expect("map the framebuffer");
            // Argb8888 little-endian: B, G, R, A.
            let pixel = |x: usize, y: usize| {
                let i = (y * 32 + x) * 4;
                [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
            };

            let core = pixel(1, 1);
            assert!(
                core[2] > 40,
                "the red emission did not light up the composite: {core:?}"
            );
            assert_eq!((core[0], core[1]), (0, 0), "unexpected tint: {core:?}");

            // Just outside the sharp 2×2 source: only the blur can light this pixel.
            let spill = pixel(4, 4);
            assert!(
                spill[2] > 0,
                "the decoration light did not blur past the emission edge: {spill:?}"
            );

            let far = pixel(30, 30);
            assert!(far[2] == 0, "light leaked too far from its source: {far:?}");
        });
    }

    /// End-to-end Vulkan decoration light: a real focus ring (border emission program) is
    /// rendered into the generic offscreen, blurred by the extracted ladder and composited with
    /// the real `decoration_light` program. This exercises the whole producer that used to be
    /// gated to GLES. Skips silently without a Vulkan device.
    #[test]
    fn vulkan_decoration_light_renders_focus_ring_emission() {
        for_each_vulkan_renderer(|renderer| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("resources/shaders/focus-ring/lightning.frag");
            let config = niri_config::Config::parse_mem(&format!(
                r#"
                layout {{ focus-ring {{ width 6; shader {{ path {:?}; padding 24; light spread=80 intensity=1.0 threshold=0.5; }}; }}; }}
                "#,
                path.to_str().unwrap()
            ))
            .expect("parse the decoration-light config");
            init_vulkan(renderer, &config);

            let mut ring =
                crate::layout::focus_ring::FocusRing::new(config.layout.focus_ring.clone());
            ring.update_render_elements(
                (320., 180.).into(),
                true,
                false,
                false,
                Rectangle::from_size((800., 600.).into()),
                niri_config::CornerRadius {
                    top_left: 14.,
                    top_right: 14.,
                    bottom_right: 14.,
                    bottom_left: 14.,
                },
                1.,
                1.,
            );
            let light = ring
                .render_light(renderer, (240., 240.).into(), 1.)
                .expect("the decoration light renders on Vulkan");

            // Draw the light over black and check that it actually emits.
            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((800, 600));
            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("readback target");
            let mut fb = renderer.bind(&mut target).expect("bind readback target");
            let mut frame = renderer
                .render(&mut fb, Size::from((800, 600)), Transform::Normal)
                .expect("begin readback frame");
            let full = Rectangle::from_size(Size::from((800, 600)));
            frame
                .clear(Color32F::TRANSPARENT, &[full])
                .expect("clear readback target");
            let dst = light.geometry(Scale::from(1.));
            RenderElement::<VulkanRenderer>::draw(
                &light,
                &mut frame,
                light.src(),
                dst,
                &[dst],
                &[],
                None,
            )
            .expect("draw the decoration light");
            let _sync = frame.finish().expect("finish readback frame");

            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Argb8888)
                .expect("copy the readback framebuffer");
            let pixels = renderer.map_texture(&mapping).expect("map the framebuffer");
            let lit = pixels
                .as_chunks::<4>().0.iter()
                .filter(|p| p[2] > 0 || p[1] > 0 || p[0] > 0)
                .count();
            assert!(
                lit > 0,
                "the Vulkan decoration light produced no lit pixels"
            );
        });
    }
}
