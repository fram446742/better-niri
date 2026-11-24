//! Backend-agnostic effect buffer.
//!
//! Bakes a list of render elements into an offscreen texture through any
//! [`NiriRenderer`] (GLES on the winit/GLES-TTY paths, Vulkan on the TTY Vulkan path), then
//! serves that texture back — optionally blurred — for the shared xray backdrop composite.
//!
//! The texture is a [`TtyOffscreen`] carrying the producing renderer's type-erased context id,
//! the blur is either the GLES [`Blur`] (run through the frame's raw `GlesRenderer`) or the
//! Vulkan [`VulkanBlur`] ladder (run inside the frame), matching the per-window background
//! effect's arrangement.

use anyhow::{Context as _, ensure};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::{Id, RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::{GlesFrame, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{VulkanFrame, VulkanTexture};
use smithay::backend::renderer::{
    Color32F, ContextId, ErasedContextId, FrameContext as _, Offscreen as _, Renderer as _, Texture,
};
use smithay::utils::{Buffer, Logical, Physical, Scale, Size, Transform};

use crate::backend::tty::TtyOffscreen;
use crate::render_helpers::blur::{Blur, BlurOptions};
#[cfg(feature = "vulkan")]
use crate::render_helpers::blur_vulkan::VulkanBlur;
use crate::render_helpers::renderer::NiriRenderer;

#[derive(Debug)]
pub struct EffectBuffer {
    /// Id to be used for this effect buffer's elements.
    id: Id,

    /// Size of the effect buffer.
    size: Size<i32, Buffer>,
    /// Scale of the effect buffer.
    scale: Scale<f64>,
    /// Options for blurring.
    blur_options: BlurOptions,

    /// Offscreen buffer where elements get rendered.
    offscreen: Option<Offscreen>,
    /// GLES blurring program, if available.
    blur: Option<Blur>,
    /// Vulkan blur ladder, if available.
    #[cfg(feature = "vulkan")]
    blur_vulkan: Option<VulkanBlur>,

    /// Commit counter that takes into account both original and blurred texture changes.
    commit_counter: CommitCounter,
}

#[derive(Debug)]
struct Offscreen {
    /// The texture with the offscreen contents.
    ///
    /// Backend-agnostic: GLES producers store a [`TtyOffscreen::Gles`], Vulkan producers a
    /// [`TtyOffscreen::Vulkan`].
    texture: TtyOffscreen,
    /// Id of the renderer context that the texture comes from (type-erased).
    renderer_context_id: ErasedContextId,
    /// Scale of the texture.
    scale: Scale<f64>,
    /// Damage tracker for drawing to the texture.
    damage: OutputDamageTracker,
    /// Render element states from the last render into the offscreen.
    states: RenderElementStates,
    /// Rendered blurred version of the texture.
    ///
    /// When texture needs to be reblurred, this field must be reset to `None`.
    blurred: Option<TtyOffscreen>,
}

impl EffectBuffer {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            size: Size::default(),
            scale: Scale::from(1.),
            blur_options: BlurOptions::default(),
            offscreen: None,
            blur: None,
            #[cfg(feature = "vulkan")]
            blur_vulkan: None,
            commit_counter: CommitCounter::default(),
        }
    }

    pub fn id(&self) -> &Id {
        &self.id
    }

    pub fn commit(&self) -> CommitCounter {
        self.commit_counter
    }

    pub fn logical_size(&self) -> Size<f64, Logical> {
        self.size.to_f64().to_logical(self.scale, Transform::Normal)
    }

    pub fn scale(&self) -> Scale<f64> {
        self.scale
    }

    pub fn render_element_states(&self) -> Option<&RenderElementStates> {
        self.offscreen.as_ref().map(|o| &o.states)
    }

    pub fn update_size(&mut self, size: Size<i32, Physical>, scale: Scale<f64>) {
        self.size = size.to_logical(1).to_buffer(1, Transform::Normal);
        self.scale = scale;
    }

    pub fn update_blur_options(&mut self, options: BlurOptions) {
        if self.blur_options == options {
            return;
        }

        self.blur_options = options;

        if let Some(offscreen) = &mut self.offscreen
            && offscreen.blurred.is_some()
        {
            offscreen.blurred = None;
            self.commit_counter.increment();
        }
    }

    /// Bakes `elements` into this buffer's cached offscreen texture.
    ///
    /// Generic over the renderer: GLES callers pass a `GlesRenderer` (the buffer stores a
    /// [`TtyOffscreen::Gles`] and behaves exactly as before), while the TTY backend can pass a
    /// `TtyRenderer`/`VulkanRenderer` to bake on Vulkan as well.
    pub fn render_elements<R>(
        &mut self,
        renderer: &mut R,
        elements: &[impl RenderElement<R>],
    ) -> anyhow::Result<()>
    where
        R: NiriRenderer,
    {
        let _span = tracy_client::span!("EffectBuffer::render_elements");

        if self.size.w <= 0 || self.size.h <= 0 {
            return Ok(());
        }

        // Check if we need to create or recreate the texture.
        let size_string;
        let mut reason = "";
        if let Some(Offscreen {
            texture,
            renderer_context_id,
            ..
        }) = &mut self.offscreen
        {
            let old_size = texture.size();
            if old_size != self.size {
                size_string = format!(
                    "size changed from {} × {} to {} × {}",
                    old_size.w, old_size.h, self.size.w, self.size.h
                );
                reason = &size_string;

                self.offscreen = None;
            } else if !texture.is_unique_reference() {
                reason = "not unique";

                self.offscreen = None;
            } else if *renderer_context_id != ContextId::erased(&renderer.context_id()) {
                reason = "renderer id changed";

                self.offscreen = None;
            }
        } else {
            reason = "first render";
        }

        let offscreen = if let Some(offscreen) = &mut self.offscreen {
            offscreen
        } else {
            trace!("creating new offscreen texture: {reason}");
            let span = tracy_client::span!("creating effect offscreen texture");
            span.emit_text(reason);

            let texture = renderer
                .create_buffer(Fourcc::Abgr8888, self.size)
                .context("error creating texture")?;
            let texture = R::wrap_offscreen(texture);

            let buffer_size = self.size.to_logical(1, Transform::Normal).to_physical(1);
            let damage = OutputDamageTracker::new(buffer_size, self.scale, Transform::Normal);

            self.offscreen.insert(Offscreen {
                texture,
                renderer_context_id: ContextId::erased(&renderer.context_id()),
                scale: self.scale,
                damage,
                states: RenderElementStates::default(),
                blurred: None,
            })
        };

        // Recreate the damage tracker if the scale changes. We already recreate it for buffer size
        // changes, and transform is always Normal.
        if offscreen.scale != self.scale {
            offscreen.scale = self.scale;

            trace!("recreating damage tracker due to scale change");
            let buffer_size = self.size.to_logical(1, Transform::Normal).to_physical(1);
            offscreen.damage = OutputDamageTracker::new(buffer_size, self.scale, Transform::Normal);

            self.commit_counter.increment();
            offscreen.blurred = None;
        }

        let res = {
            let target = R::unwrap_offscreen(&mut offscreen.texture)
                .context("offscreen texture variant does not match the active renderer")?;
            let mut target = renderer.bind(target).context("error binding texture")?;
            offscreen
                .damage
                .render_output(renderer, &mut target, 1, elements, Color32F::TRANSPARENT)
                .context("error rendering")?
        };

        offscreen.states = res.states;

        if res.damage.is_some() {
            self.commit_counter.increment();

            // Original texture changed; reset the blurred texture.
            offscreen.blurred = None;
        }

        Ok(())
    }

    /// Returns the baked texture, running the GLES blur lazily if requested.
    pub fn render(
        &mut self,
        frame: &mut GlesFrame<'_, '_>,
        blur: bool,
    ) -> anyhow::Result<GlesTexture> {
        let offscreen = self.offscreen.as_mut().context("offscreen is missing")?;

        let texture = match &offscreen.texture {
            TtyOffscreen::Gles(texture) => texture.clone(),
            _ => anyhow::bail!("offscreen texture is not a GLES texture"),
        };

        if !blur {
            return Ok(texture);
        }

        let blurred = match &offscreen.blurred {
            Some(TtyOffscreen::Gles(texture)) => Some(texture.clone()),
            _ => None,
        };
        if let Some(texture) = blurred {
            return Ok(texture);
        }

        let offscreen_context_id = offscreen.renderer_context_id.clone();
        let blur_options = self.blur_options;

        let mut guard = frame.renderer();
        let renderer = guard.as_mut();

        if let Some(blur) = &self.blur
            && blur.context_id() != renderer.context_id()
        {
            debug!("recreating blur: renderer changed");
            self.blur = None;
        }

        ensure!(
            offscreen_context_id == ContextId::erased(&renderer.context_id()),
            "wrong renderer context id"
        );

        let blur = if let Some(blur) = &mut self.blur {
            blur
        } else {
            let Some(blur) = Blur::new(renderer) else {
                // Missing blur shader.
                return Ok(texture);
            };
            self.blur.insert(blur)
        };

        blur.prepare_textures(
            |fourcc, size| renderer.create_buffer(fourcc, size),
            &texture,
            blur_options,
        )
        .context("error preparing blur textures")?;

        let blurred = blur
            .render(renderer, &texture, blur_options)
            .context("error rendering blur")?;

        let offscreen = self.offscreen.as_mut().context("offscreen is missing")?;
        offscreen.blurred = Some(TtyOffscreen::Gles(blurred.clone()));

        Ok(blurred)
    }

    /// Returns the baked texture for the Vulkan renderer, running the Vulkan blur ladder lazily
    /// inside `frame` if requested.
    #[cfg(feature = "vulkan")]
    pub fn render_vulkan(
        &mut self,
        frame: &mut VulkanFrame<'_, '_>,
        blur: bool,
    ) -> anyhow::Result<VulkanTexture> {
        let offscreen = self.offscreen.as_mut().context("offscreen is missing")?;

        let texture = match &offscreen.texture {
            TtyOffscreen::Vulkan(texture) => texture.clone(),
            _ => anyhow::bail!("offscreen texture is not a Vulkan texture"),
        };

        if !blur {
            return Ok(texture);
        }

        let blurred = match &offscreen.blurred {
            Some(TtyOffscreen::Vulkan(texture)) => Some(texture.clone()),
            _ => None,
        };
        if let Some(texture) = blurred {
            return Ok(texture);
        }

        let blur_options = self.blur_options;
        let blur = self.blur_vulkan.get_or_insert_with(VulkanBlur::default);

        let Some(blurred) = blur
            .render_in_frame(frame, &texture, blur_options)
            .context("error rendering Vulkan blur")?
        else {
            // Blur programs unavailable; fall back to the unblurred texture.
            return Ok(texture);
        };

        let offscreen = self.offscreen.as_mut().context("offscreen is missing")?;
        offscreen.blurred = Some(TtyOffscreen::Vulkan(blurred.clone()));

        Ok(blurred)
    }
}

impl Default for EffectBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{
        Bind, Color32F, ExportMem, Frame as _, Offscreen, Renderer, Texture,
    };
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Point, Rectangle, Size, Transform};

    use super::*;
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

    /// Reads back a frame region drawn into `target` and returns the raw bytes.
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

    fn solid(
        color: [f32; 4],
        loc: Point<f64, smithay::utils::Logical>,
        size: (f64, f64),
    ) -> SolidColorRenderElement {
        let buffer = SolidColorBuffer::new(size, color);
        SolidColorRenderElement::from_buffer(&buffer, loc, 1., Kind::Unspecified)
    }

    /// `EffectBuffer` is backend-agnostic: baking solid elements through a Vulkan renderer and
    /// sampling the result must reproduce the baked pixels.
    #[test]
    fn vulkan_effect_buffer_bakes_and_reads_back() {
        for_each_vulkan_renderer(|renderer| {
            let size = Size::<i32, Buffer>::from((4, 4));
            let mut buffer = EffectBuffer::new();
            buffer.update_size(
                Size::<i32, smithay::utils::Physical>::from((4, 4)),
                Scale::from(1.),
            );

            let element = solid([1., 0., 0., 1.], Point::from((0., 0.)), (4., 4.));
            buffer
                .render_elements(renderer, &[element])
                .expect("bake the element into the effect buffer");

            let mut target = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .expect("readback target");
            {
                let mut fb = renderer.bind(&mut target).expect("bind target");
                let mut frame = renderer
                    .render(&mut fb, Size::from((4, 4)), Transform::Normal)
                    .expect("begin frame");
                let dst = Rectangle::from_size(Size::from((4, 4)));
                frame.clear(Color32F::TRANSPARENT, &[dst]).expect("clear");
                let texture = buffer
                    .render_vulkan(&mut frame, false)
                    .expect("sample the baked texture");
                frame
                    .render_texture_from_to(
                        &texture,
                        Rectangle::from_size(Size::from((4., 4.))),
                        dst,
                        &[dst],
                        &[],
                        Transform::Normal,
                        1.0,
                    )
                    .expect("draw the baked texture");
                frame.finish().expect("finish frame");
            }

            let pixels = read_back(renderer, &mut target, size);
            assert_eq!(
                &pixels[0..4],
                &[255, 0, 0, 255],
                "the baked texture did not reach the readback target"
            );
        });
    }

    /// The optional blur path of the generic buffer must run the Vulkan ladder over the baked
    /// texture and mix a hard red/blue edge.
    #[test]
    fn vulkan_effect_buffer_blur_mixes_a_hard_edge() {
        for_each_vulkan_renderer(|renderer| {
            crate::render_helpers::shaders::init_vulkan(renderer, &niri_config::Config::default());

            let size = Size::<i32, Buffer>::from((16, 16));
            let mut buffer = EffectBuffer::new();
            buffer.update_size(
                Size::<i32, smithay::utils::Physical>::from((16, 16)),
                Scale::from(1.),
            );
            buffer.update_blur_options(BlurOptions {
                passes: 3,
                offset: 1.0,
            });

            let left = solid([1., 0., 0., 1.], Point::from((0., 0.)), (8., 16.));
            let right = solid([0., 0., 1., 1.], Point::from((8., 0.)), (8., 16.));
            buffer
                .render_elements(renderer, &[left, right])
                .expect("bake the red/blue edge");

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
                let texture = buffer
                    .render_vulkan(&mut frame, true)
                    .expect("blurred baked texture");
                frame
                    .render_texture_from_to(
                        &texture,
                        Rectangle::from_size(Size::from((16., 16.))),
                        dst,
                        &[dst],
                        &[],
                        Transform::Normal,
                        1.0,
                    )
                    .expect("draw the blurred texture");
                frame.finish().expect("finish frame");
            }

            let pixels = read_back(renderer, &mut target, size);
            // Read back as R, G, B, A (see the offscreen readback tests).
            let center = (8 * 16 + 8) * 4;
            let (r, g, b, a) = (
                pixels[center],
                pixels[center + 1],
                pixels[center + 2],
                pixels[center + 3],
            );
            assert_eq!(g, 0, "unexpected green channel: {g}");
            assert_eq!(a, 255, "blurred output should be opaque, got alpha {a}");
            assert!(
                (24..=231).contains(&b) && (24..=231).contains(&r),
                "blur did not mix the edge: R={r} B={b}"
            );
        });
    }
}
