//! Vulkan Dual-Kawase blur.
//!
//! The Vulkan counterpart of the GLES [`Blur`](super::blur::Blur): a downsample chain through
//! progressively halved renderer-owned textures, then an upsample chain back into the full-size
//! output texture. The passes run through loom-smithay's custom-pass pipeline using the
//! `blur_down`/`blur_up` programs compiled by [`VulkanShaders`](super::shaders::VulkanShaders).
//!
//! [`VulkanBlur::render_in_frame`] is used inside an existing frame (the per-window background
//! effect), while [`VulkanBlur::render`] builds a short-lived frame around the ladder for
//! producers that run before any output frame exists (the decoration light prepass).

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::vulkan::{
    CustomPass, CustomUniform, CustomUniformValue, VulkanError, VulkanFrame, VulkanRenderer,
    VulkanTexture,
};
use smithay::backend::renderer::{
    Bind as _, Frame as _, FrameContext as _, Offscreen as _, Renderer as _, Texture as _,
};
use smithay::utils::{Buffer, Size, Transform};

use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::shaders::get_vulkan_frame;

/// Cached blur ladder: `textures[0]` is the full-size output, each following one is
/// half-sized. Recreated when the source size or pass count changes.
#[derive(Debug, Default)]
pub struct VulkanBlur {
    textures: Vec<VulkanTexture>,
    /// 1×1 target used to start the transient frame in [`Self::render`]. Cached so animation
    /// updates do not allocate a texture per frame.
    scratch: Option<VulkanTexture>,
}

impl VulkanBlur {
    /// Drops the cached ladder, e.g. when the producer's renderer changes.
    pub fn clear(&mut self) {
        self.textures.clear();
        self.scratch = None;
    }

    /// Runs the ladder for a producer that has no frame of its own yet, e.g. the decoration
    /// light emission prepass.
    ///
    /// A transient 1×1 frame is started (custom passes write only into the ladder textures),
    /// the passes are recorded and submitted, and the frame is finished. The returned output is
    /// shader-readable for draws in later frames; submissions on the queue are ordered.
    ///
    /// Returns `None` when the blur programs are not available, in which case the caller should
    /// fall back to the unblurred source.
    pub fn render(
        &mut self,
        renderer: &mut VulkanRenderer,
        source: &VulkanTexture,
        options: BlurOptions,
    ) -> Result<Option<VulkanTexture>, VulkanError> {
        let _span = tracy_client::span!("VulkanBlur::render");

        // A target is required to start a frame. Custom passes write only to the ladder
        // textures, so the target's contents are never read or written.
        if self.scratch.is_none() {
            let scratch_size: Size<i32, Buffer> = Size::from((1, 1));
            self.scratch = Some(renderer.create_buffer(Fourcc::Abgr8888, scratch_size)?);
        }
        let mut scratch = self.scratch.clone().expect("created above");
        let mut target = renderer.bind(&mut scratch)?;
        let mut frame = renderer.render(&mut target, Size::from((1, 1)), Transform::Normal)?;
        let result = self.render_in_frame(&mut frame, source, options);
        let sync = frame.finish()?;
        // The ladder was submitted on its own; make the next submission (the output frame that
        // samples the result) wait for it. This only schedules the wait, it does not block.
        renderer.wait(&sync)?;
        result
    }

    /// Runs the ladder inside `frame`, suspending its rendering scope for the passes.
    ///
    /// Returns `None` when the blur programs are not available, in which case the caller
    /// should fall back to the unblurred source.
    pub fn render_in_frame(
        &mut self,
        frame: &mut VulkanFrame<'_, '_>,
        source: &VulkanTexture,
        options: BlurOptions,
    ) -> Result<Option<VulkanTexture>, VulkanError> {
        let _span = tracy_client::span!("VulkanBlur::render_in_frame");

        // Clone the programs out first: `get_vulkan_frame` borrows the frame and the passes
        // below need it mutably.
        let (down, up) = {
            let Some(shaders) = get_vulkan_frame(frame) else {
                return Ok(None);
            };
            match (shaders.blur_down.clone(), shaders.blur_up.clone()) {
                (Some(down), Some(up)) => (down, up),
                _ => return Ok(None),
            }
        };

        let passes = options.passes.clamp(1, 31) as usize;
        let size = source.size();

        let recreate = self.textures.len() != passes + 1
            || self
                .textures
                .first()
                .is_none_or(|texture| texture.size() != size)
            || !self
                .textures
                .iter_mut()
                .all(|texture| texture.is_unique_reference());
        if recreate {
            self.textures.clear();
            let mut w = size.w;
            let mut h = size.h;
            let mut guard = frame.renderer();
            let renderer = guard.as_mut();
            for _ in 0..=passes {
                let texture_size = Size::new(w, h);
                w = std::cmp::max(1, w / 2);
                h = std::cmp::max(1, h / 2);
                self.textures
                    .push(renderer.create_buffer(Fourcc::Abgr8888, texture_size)?);
            }
        }
        let textures = &self.textures;

        let offset = options.offset as f32;
        let mut uniform_sets: Vec<Vec<CustomUniform<'_>>> = Vec::with_capacity(passes * 2);
        let mut texture_sets: Vec<Vec<(&str, &VulkanTexture)>> = Vec::with_capacity(passes * 2);

        // Downsample: source -> textures[1] -> ... -> textures[passes].
        for i in 0..passes {
            let src = if i == 0 { source } else { &textures[i] };
            let dst = &textures[i + 1];
            let dst_size = dst.size();
            uniform_sets.push(vec![
                CustomUniform {
                    name: "half_pixel",
                    value: CustomUniformValue::Vec2([
                        0.5 / dst_size.w as f32,
                        0.5 / dst_size.h as f32,
                    ]),
                },
                CustomUniform {
                    name: "offset",
                    value: CustomUniformValue::Float(offset),
                },
            ]);
            texture_sets.push(vec![("tex", src)]);
        }

        // Upsample: textures[passes] -> ... -> textures[0].
        for i in 0..passes {
            let src = &textures[passes - i];
            let src_size = src.size();
            uniform_sets.push(vec![
                CustomUniform {
                    name: "half_pixel",
                    value: CustomUniformValue::Vec2([
                        0.5 / src_size.w as f32,
                        0.5 / src_size.h as f32,
                    ]),
                },
                CustomUniform {
                    name: "offset",
                    value: CustomUniformValue::Float(offset),
                },
            ]);
            texture_sets.push(vec![("tex", src)]);
        }

        let mut pass_list = Vec::with_capacity(passes * 2);
        for i in 0..passes {
            pass_list.push(CustomPass {
                dst: &textures[i + 1],
                program: &down,
                uniforms: &uniform_sets[i],
                textures: &texture_sets[i],
            });
        }
        for i in 0..passes {
            pass_list.push(CustomPass {
                dst: &textures[passes - i - 1],
                program: &up,
                uniforms: &uniform_sets[passes + i],
                textures: &texture_sets[passes + i],
            });
        }

        frame.render_custom_passes(&pass_list)?;
        Ok(Some(textures[0].clone()))
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{Color32F, ExportMem as _};
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Point, Rectangle, Transform};

    use super::*;
    use crate::render_helpers::capture::capture_framebuffer_region_vulkan;
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

    /// Blurs a hard red/blue edge with the extracted ladder and checks that the boundary comes
    /// back mixed. This is the direct evidence that the helper runs the real
    /// `blur_down`/`blur_up` passes on Vulkan.
    #[test]
    fn vulkan_blur_ladder_mixes_a_hard_edge() {
        for_each_vulkan_renderer(|renderer| {
            init_vulkan(renderer, &niri_config::Config::default());

            let buffer_size = Size::<i32, Buffer>::from((16, 16));
            let output_size = Size::<i32, smithay::utils::Physical>::from((16, 16));
            let full = Rectangle::from_size(output_size);

            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("blit target");
            let source = renderer
                .create_buffer(Fourcc::Abgr8888, buffer_size)
                .expect("blur source");

            {
                let mut fb = renderer.bind(&mut target).expect("bind target");
                let mut frame = renderer
                    .render(&mut fb, output_size, Transform::Normal)
                    .expect("begin frame");
                frame
                    .clear(
                        Color32F::new(1., 0., 0., 1.),
                        &[Rectangle::new(Point::from((0, 0)), Size::from((8, 16)))],
                    )
                    .expect("clear red half");
                frame
                    .clear(
                        Color32F::new(0., 0., 1., 1.),
                        &[Rectangle::new(Point::from((8, 0)), Size::from((8, 16)))],
                    )
                    .expect("clear blue half");
                capture_framebuffer_region_vulkan(&mut frame, full, &source)
                    .expect("capture the edge");
                let sync = frame.finish().expect("finish capture");
                renderer.wait(&sync).expect("wait for the capture");
            }

            let mut blur = VulkanBlur::default();
            let blurred = blur
                .render(
                    renderer,
                    &source,
                    BlurOptions {
                        passes: 3,
                        offset: 1.0,
                    },
                )
                .expect("run the blur ladder")
                .expect("blur programs should be available");

            let mut readback = renderer
                .create_buffer(Fourcc::Argb8888, buffer_size)
                .expect("readback target");
            {
                let mut fb = renderer.bind(&mut readback).expect("bind readback");
                let mut frame = renderer
                    .render(&mut fb, output_size, Transform::Normal)
                    .expect("begin readback frame");
                frame
                    .clear(Color32F::TRANSPARENT, &[full])
                    .expect("clear readback");
                frame
                    .render_texture_from_to(
                        &blurred,
                        Rectangle::from_size(Size::from((16., 16.))),
                        full,
                        &[full],
                        &[],
                        Transform::Normal,
                        1.0,
                    )
                    .expect("sample the blurred texture");
                let _sync = frame.finish().expect("finish readback");
            }

            let fb = renderer.bind(&mut readback).expect("bind readback result");
            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Argb8888)
                .expect("copy framebuffer");
            let pixels = renderer.map_texture(&mapping).expect("map framebuffer");
            // Argb8888 little-endian: B, G, R, A. The blur smashed the boundary together, so
            // the middle pixel must contain a substantial amount of BOTH colours.
            let center = (8 * 16 + 8) * 4;
            let (b, g, r, a) = (
                pixels[center],
                pixels[center + 1],
                pixels[center + 2],
                pixels[center + 3],
            );
            assert_eq!(g, 0, "unexpected green channel: {g}");
            assert_eq!(a, 255, "blurred output should be opaque, got alpha {a}");
            assert!(
                (24..=231).contains(&b) && (24..=231).contains(&r),
                "blur did not mix the boundary: B={b} R={r}"
            );
        });
    }
}
