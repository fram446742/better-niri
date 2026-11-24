use std::time::Duration;

use smithay::backend::renderer::element::Kind;
use smithay::utils::{Scale, Transform};

use crate::animation::Clock;
use crate::backend::tty::TtyOffscreen;
use crate::render_helpers::RenderTarget;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};

pub const DELAY: Duration = Duration::from_millis(250);
pub const DURATION: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub struct ScreenTransition {
    /// Texture to crossfade from for each render target.
    from_texture: [TextureBuffer<TtyOffscreen>; 3],
    /// Monotonic time when to start the crossfade.
    start_at: Duration,
    /// Clock to drive animations.
    clock: Clock,
    /// Whether the captured textures are stored unrotated (`Transform::Normal`).
    ///
    /// The GLES capture applies the output transform and compensates with the buffer transform;
    /// the generic `OffscreenBuffer` capture (Vulkan) bakes an unrotated full-output image, so it
    /// must keep `Transform::Normal` across scale/transform updates.
    normal_transform: bool,
}

impl ScreenTransition {
    pub fn new(
        from_texture: [TextureBuffer<TtyOffscreen>; 3],
        delay: Duration,
        clock: Clock,
    ) -> Self {
        let normal_transform = from_texture
            .first()
            .is_some_and(|buffer| buffer.texture_transform() == Transform::Normal);
        Self {
            from_texture,
            start_at: clock.now_unadjusted() + delay,
            clock,
            normal_transform,
        }
    }

    pub fn is_done(&self) -> bool {
        self.start_at + DURATION <= self.clock.now_unadjusted()
    }

    pub fn update_render_elements(&mut self, scale: Scale<f64>, transform: Transform) {
        // These textures should remain full-screen, even if scale or transform changes.
        let transform = if self.normal_transform {
            Transform::Normal
        } else {
            transform
        };
        for buffer in &mut self.from_texture {
            buffer.set_texture_scale(scale);
            buffer.set_texture_transform(transform);
        }
    }

    pub fn render(&self, target: RenderTarget) -> PrimaryGpuTextureRenderElement {
        // Screen transition ignores animation slowdown.
        let now = self.clock.now_unadjusted();

        let alpha = if self.start_at + DURATION <= now {
            0.
        } else if self.start_at <= now {
            1. - (now - self.start_at).as_secs_f32() / DURATION.as_secs_f32()
        } else {
            1.
        };

        let idx = match target {
            RenderTarget::Output => 0,
            RenderTarget::Screencast => 1,
            RenderTarget::ScreenCapture => 2,
        };

        PrimaryGpuTextureRenderElement(TextureRenderElement::from_texture_buffer(
            self.from_texture[idx].clone(),
            (0., 0.),
            alpha,
            None,
            None,
            Kind::Unspecified,
        ))
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::{Element as _, Kind, RenderElement};
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{
        Bind, Color32F, ExportMem, Frame as _, Offscreen as _, Renderer as _, Texture as _,
    };
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Buffer, Point, Rectangle, Size, Transform};

    use super::*;
    use crate::render_helpers::offscreen::OffscreenBuffer;
    use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};

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
        target: &mut smithay::backend::renderer::vulkan::VulkanTexture,
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

    /// The transition crossfade must composite the captured (Vulkan) texture with its alpha.
    #[test]
    fn vulkan_screen_transition_composites_captured_texture() {
        for_each_vulkan_renderer(|renderer| {
            let size = Size::<i32, Buffer>::from((4, 4));
            let buffer = OffscreenBuffer::default();
            let solid = SolidColorBuffer::new((4., 4.), [1., 0., 0., 1.]);
            let elem = SolidColorRenderElement::from_buffer(
                &solid,
                Point::from((0., 0.)),
                1.,
                Kind::Unspecified,
            );
            let (baked, _sync, _data) = buffer
                .render(renderer, Scale::from(1.), &[elem])
                .expect("bake transition texture");

            let tex = TextureBuffer::from_tty_texture(
                baked.texture().clone(),
                baked.context_id(),
                Scale::from(1.),
                Transform::Normal,
                Vec::new(),
            );

            let start = std::time::Duration::from_secs(1);
            let mut clock = Clock::with_time(start);
            let transition = ScreenTransition::new(
                [tex.clone(), tex.clone(), tex.clone()],
                std::time::Duration::ZERO,
                clock.clone(),
            );
            assert!(!transition.is_done());

            let elem = transition.render(RenderTarget::Output);

            let mut target = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .expect("target");
            {
                let mut fb = renderer.bind(&mut target).expect("bind target");
                let mut frame = renderer
                    .render(&mut fb, Size::from((4, 4)), Transform::Normal)
                    .expect("begin frame");
                let dst = Rectangle::from_size(Size::from((4, 4)));
                frame
                    .clear(Color32F::TRANSPARENT, &[dst])
                    .expect("clear frame");
                RenderElement::<VulkanRenderer>::draw(
                    &elem,
                    &mut frame,
                    elem.src(),
                    elem.geometry(Scale::from(1.)),
                    &[dst],
                    &[],
                    None,
                )
                .expect("draw transition element");
                let _sync = frame.finish().expect("finish frame");
            }

            let pixels = read_back(renderer, &mut target, size);
            assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);

            clock.set_unadjusted(start + DURATION);
            assert!(
                transition.is_done(),
                "transition must finish after DURATION"
            );
        });
    }
}
