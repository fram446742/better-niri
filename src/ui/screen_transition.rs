use std::time::Duration;

use smithay::backend::renderer::element::{Kind, RenderElement};
use smithay::utils::{Physical, Rectangle, Scale, Size, Transform};

use crate::animation::Clock;
use crate::backend::tty::TtyOffscreen;
use crate::render_helpers::RenderTarget;
use crate::render_helpers::offscreen::OffscreenBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};

pub const DELAY: Duration = Duration::from_millis(250);
pub const DURATION: Duration = Duration::from_millis(500);

/// Bakes the current output contents for a screen transition on any renderer.
///
/// The output's elements are laid out in logical (post-transform) space, so the bake uses the
/// transformed mode size: a rotated output captures an upright `logical_size` image rather than
/// a mode-sized one. The returned buffer keeps `Transform::Normal`; the output frame applies the
/// output transform when compositing, which gives the same visible orientation as the GLES
/// capture (where the transform is baked into the texture and stored on the buffer instead).
pub fn bake_transition_texture<R: NiriRenderer>(
    renderer: &mut R,
    mode_size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    elements: &[impl RenderElement<R>],
) -> anyhow::Result<TextureBuffer<TtyOffscreen>> {
    let logical_size = transform.transform_size(mode_size);
    let buffer = OffscreenBuffer::default();
    let (elem, _sync, _data) = buffer.render_with_bounds(
        renderer,
        scale,
        Rectangle::from_size(logical_size),
        elements,
    )?;
    Ok(elem.texture_buffer(Transform::Normal, Vec::new()))
}

#[derive(Debug)]
pub struct ScreenTransition {
    /// Texture to crossfade from for each render target.
    from_texture: [TextureBuffer<TtyOffscreen>; 3],
    /// Monotonic time when to start the crossfade.
    start_at: Duration,
    /// Clock to drive animations.
    clock: Clock,
    /// Whether the captured textures are stored in logical space (`Transform::Normal`).
    ///
    /// The GLES capture bakes the output transform into the texture and stores the matching
    /// buffer transform; the generic [`bake_transition_texture`] path (Vulkan) bakes an upright
    /// logical-space image and lets the output frame apply the transform when compositing, so it
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
    use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};

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

    /// Bakes a four-colour quadrant pattern, composites the transition capture into a frame with
    /// the output transform, and checks both the orientation and the pixels against a direct
    /// render of the same logical scene.
    ///
    /// This is the regression test for rotated outputs: the capture must use the transformed
    /// (logical) size, not the raw mode size, for 90/270 and flipped variants too.
    #[test]
    fn vulkan_screen_transition_respects_output_transform() {
        const RED: [u8; 4] = [255, 0, 0, 255];
        const GREEN: [u8; 4] = [0, 255, 0, 255];
        const BLUE: [u8; 4] = [0, 0, 255, 255];
        const WHITE: [u8; 4] = [255, 255, 255, 255];

        let transforms = [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ];

        for_each_vulkan_renderer(|renderer| {
            let mode = Size::<i32, Physical>::from((8, 4));
            let buffer_size = Size::<i32, Buffer>::from((mode.w, mode.h));
            let scale = Scale::from(1.);
            let start = std::time::Duration::from_secs(1);

            for transform in transforms {
                let logical = transform.transform_size(mode).to_f64();
                let logical_size =
                    Size::<i32, Physical>::from((logical.w as i32, logical.h as i32));
                let dst = Rectangle::from_size(logical_size);
                let half_size = Size::new(logical.w / 2., logical.h / 2.);

                // Four distinct logical quadrants: top-left red, top-right green,
                // bottom-left blue, bottom-right white.
                let tl = SolidColorBuffer::new(half_size, [1., 0., 0., 1.]);
                let tr = SolidColorBuffer::new(half_size, [0., 1., 0., 1.]);
                let bl = SolidColorBuffer::new(half_size, [0., 0., 1., 1.]);
                let br = SolidColorBuffer::new(half_size, [1., 1., 1., 1.]);
                let elements = [
                    SolidColorRenderElement::from_buffer(
                        &tl,
                        Point::new(0., 0.),
                        1.,
                        Kind::Unspecified,
                    ),
                    SolidColorRenderElement::from_buffer(
                        &tr,
                        Point::new(half_size.w, 0.),
                        1.,
                        Kind::Unspecified,
                    ),
                    SolidColorRenderElement::from_buffer(
                        &bl,
                        Point::new(0., half_size.h),
                        1.,
                        Kind::Unspecified,
                    ),
                    SolidColorRenderElement::from_buffer(
                        &br,
                        Point::new(half_size.w, half_size.h),
                        1.,
                        Kind::Unspecified,
                    ),
                ];

                let direct = {
                    let mut target = renderer
                        .create_buffer(Fourcc::Abgr8888, buffer_size)
                        .expect("target");
                    {
                        let mut fb = renderer.bind(&mut target).expect("bind target");
                        let mut frame = renderer
                            .render(&mut fb, mode, transform)
                            .expect("begin frame");
                        frame
                            .clear(Color32F::TRANSPARENT, &[dst])
                            .expect("clear frame");
                        let quadrants = [
                            (
                                Rectangle::new(
                                    Point::from((0, 0)),
                                    Size::from((half_size.w as i32, half_size.h as i32)),
                                ),
                                Color32F::new(1., 0., 0., 1.),
                            ),
                            (
                                Rectangle::new(
                                    Point::from((half_size.w as i32, 0)),
                                    Size::from((half_size.w as i32, half_size.h as i32)),
                                ),
                                Color32F::new(0., 1., 0., 1.),
                            ),
                            (
                                Rectangle::new(
                                    Point::from((0, half_size.h as i32)),
                                    Size::from((half_size.w as i32, half_size.h as i32)),
                                ),
                                Color32F::new(0., 0., 1., 1.),
                            ),
                            (
                                Rectangle::new(
                                    Point::from((half_size.w as i32, half_size.h as i32)),
                                    Size::from((half_size.w as i32, half_size.h as i32)),
                                ),
                                Color32F::new(1., 1., 1., 1.),
                            ),
                        ];
                        for (rect, color) in quadrants {
                            frame.clear(color, &[rect]).expect("draw quadrant");
                        }
                        let _sync = frame.finish().expect("finish frame");
                    }
                    read_back(renderer, &mut target, buffer_size)
                };

                let texture = bake_transition_texture(renderer, mode, scale, transform, &elements)
                    .expect("bake transform-aware transition texture");
                let transition = ScreenTransition::new(
                    [texture.clone(), texture.clone(), texture],
                    std::time::Duration::ZERO,
                    Clock::with_time(start),
                );
                let elem = transition.render(RenderTarget::Output);

                let composite = {
                    let mut target = renderer
                        .create_buffer(Fourcc::Abgr8888, buffer_size)
                        .expect("target");
                    {
                        let mut fb = renderer.bind(&mut target).expect("bind target");
                        let mut frame = renderer
                            .render(&mut fb, mode, transform)
                            .expect("begin frame");
                        frame
                            .clear(Color32F::TRANSPARENT, &[dst])
                            .expect("clear frame");
                        RenderElement::<VulkanRenderer>::draw(
                            &elem,
                            &mut frame,
                            elem.src(),
                            elem.geometry(scale),
                            &[dst],
                            &[],
                            None,
                        )
                        .expect("draw transition element");
                        let _sync = frame.finish().expect("finish frame");
                    }
                    read_back(renderer, &mut target, buffer_size)
                };

                assert_eq!(
                    composite, direct,
                    "transition composite must match the direct render for {transform:?}"
                );

                // Physical corners of the 8×4 mode buffer, after rotating/flipping the logical
                // scene. Order: top-left, top-right, bottom-left, bottom-right. These are the
                // orientations the live output shows, which is exactly what the transition must
                // freeze before the crossfade.
                let expected_corners = match transform {
                    Transform::Normal => [RED, GREEN, BLUE, WHITE],
                    Transform::_90 => [BLUE, RED, WHITE, GREEN],
                    Transform::_180 => [WHITE, BLUE, GREEN, RED],
                    Transform::_270 => [GREEN, WHITE, RED, BLUE],
                    Transform::Flipped => [GREEN, RED, WHITE, BLUE],
                    Transform::Flipped90 => [WHITE, GREEN, BLUE, RED],
                    Transform::Flipped180 => [BLUE, WHITE, RED, GREEN],
                    Transform::Flipped270 => [RED, BLUE, GREEN, WHITE],
                };
                let corners = [
                    (0, 0),
                    (mode.w - 1, 0),
                    (0, mode.h - 1),
                    (mode.w - 1, mode.h - 1),
                ];
                for ((x, y), expected) in corners.into_iter().zip(expected_corners) {
                    let idx = ((y * mode.w + x) as usize) * 4;
                    assert_eq!(
                        &composite[idx..idx + 4],
                        &expected,
                        "corner ({x}, {y}) has the wrong orientation for {transform:?}"
                    );
                    assert_eq!(
                        &direct[idx..idx + 4],
                        &expected,
                        "direct render corner ({x}, {y}) disagrees for {transform:?}"
                    );
                }
            }
        });
    }
}
