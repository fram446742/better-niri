use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Context as _;
use glam::{Mat3, Vec2};
use niri_config::BlockOutFrom;
use smithay::backend::renderer::Texture;
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::{Kind, RenderElement};
use smithay::backend::renderer::gles::Uniform;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::{Blocker, BlockerState};

use crate::animation::{Animation, Clock};
use crate::backend::tty::TtyOffscreen;
use crate::niri_render_elements;
use crate::render_helpers::offscreen::OffscreenBuffer;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::shader_element::{ShaderRenderElement, gles_texture_map};
use crate::render_helpers::shaders::{ProgramType, Shaders, mat3_uniform};
use crate::render_helpers::snapshot::RenderSnapshot;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::{RenderCtx, RenderTarget, ToRenderElement};
use crate::utils::transaction::TransactionBlocker;

#[derive(Debug)]
pub struct ClosingWindow {
    /// Contents of the window.
    ///
    /// Backend-agnostic: `TtyOffscreen::Gles` when baked by the GLES (winit/headless/TTY GLES)
    /// renderer, `TtyOffscreen::Vulkan` when baked on the Vulkan TTY renderer.
    buffer: TextureBuffer<TtyOffscreen>,

    /// Contents that are not blocked out, but the background is blocked out.
    ///
    /// If `None` then the background doesn't have any blocked-out surfaces, and normal `buffer`
    /// can be used instead.
    buffer_with_blocked_out_bg: Option<TextureBuffer<TtyOffscreen>>,

    /// Blocked-out contents and their buffer offset, or `None` when hidden.
    blocked_out_buffer: Option<(TextureBuffer<TtyOffscreen>, Point<f64, Logical>)>,

    /// Where the window should be blocked out from.
    block_out_from: Option<BlockOutFrom>,

    /// Size of the window geometry.
    geo_size: Size<f64, Logical>,

    /// Position in the workspace.
    pos: Point<f64, Logical>,

    /// Horizontal movement synchronized with a layout change.
    move_x: Option<Animation>,

    /// Whether this window should compensate for scrolling view movements.
    compensate_view_movements: bool,

    /// How much the texture should be offset.
    buffer_offset: Point<f64, Logical>,

    /// How much the texture with blocked-out bg should be offset.
    buffer_with_blocked_out_bg_offset: Point<f64, Logical>,

    /// The closing animation.
    anim_state: AnimationState,

    /// Random seed for the shader.
    random_seed: f32,
}

niri_render_elements! {
    ClosingWindowRenderElement => {
        Texture = RelocateRenderElement<RescaleRenderElement<PrimaryGpuTextureRenderElement>>,
        Shader = ShaderRenderElement,
    }
}

#[derive(Debug)]
enum AnimationState {
    Waiting {
        /// Blocker for a transaction before starting the animation.
        blocker: TransactionBlocker,
        anim: Animation,
    },
    Animating(Animation),
}

impl AnimationState {
    pub fn new(blocker: TransactionBlocker, anim: Animation) -> Self {
        if blocker.state() == BlockerState::Pending {
            Self::Waiting { blocker, anim }
        } else {
            // This actually doesn't normally happen because the window is removed only after the
            // closing animation is created. Though, it does happen with disable-transactions debug
            // flag.
            Self::Animating(anim)
        }
    }
}

impl ClosingWindow {
    pub fn position(&self) -> Point<f64, Logical> {
        self.pos
    }

    pub fn new<R, E>(
        renderer: &mut R,
        snapshot: RenderSnapshot<E, E>,
        scale: Scale<f64>,
        geo_size: Size<f64, Logical>,
        pos: Point<f64, Logical>,
        blocker: TransactionBlocker,
        anim: Animation,
    ) -> anyhow::Result<Self>
    where
        R: NiriRenderer,
        E: ToRenderElement,
        E::RenderElement: RenderElement<R>,
    {
        let _span = tracy_client::span!("ClosingWindow::new");
        let snapshot_size: Size<f64, Logical> =
            Size::from((snapshot.size.w.max(1.), snapshot.size.h.max(1.)));
        let snapshot_scale = (geo_size.w / snapshot_size.w).max(0.);

        let mut render_to_texture = |elements: Vec<E>| -> anyhow::Result<_> {
            let elements: Vec<_> = elements
                .into_iter()
                .map(|baked| {
                    let elem = baked.to_render_element(
                        Point::from((0., 0.)),
                        scale,
                        1.,
                        Kind::Unspecified,
                    );
                    RescaleRenderElement::from_element(elem, Point::from((0, 0)), snapshot_scale)
                })
                .collect();

            // Bakes through the generic offscreen buffer, so the same code runs on GLES and on
            // the Vulkan TTY renderer (where no GLES renderer exists).
            let offscreen = OffscreenBuffer::default();
            let (elem, _sync_point, _data) = offscreen
                .render(renderer, scale, &elements)
                .context("error rendering to texture")?;

            let offset = elem.offset();
            let buffer = elem.texture_buffer(Transform::Normal, Vec::new());

            Ok((buffer, offset))
        };

        let (buffer, buffer_offset) =
            render_to_texture(snapshot.contents).context("error rendering contents")?;
        let (buffer_with_blocked_out_bg, buffer_with_blocked_out_bg_offset) =
            if let Some(contents) = snapshot.contents_with_blocked_out_bg {
                let (buffer, offset) = render_to_texture(contents)
                    .context("error rendering contents with blocked-out bg")?;
                (Some(buffer), offset)
            } else {
                (None, Point::default())
            };
        // Both block-out targets include screencasts.
        let blocked_out_buffer = (!RenderTarget::Screencast.should_hide(snapshot.block_out_from))
            .then(|| render_to_texture(snapshot.blocked_out_contents))
            .transpose()
            .context("error rendering blocked-out contents")?;

        Ok(Self {
            buffer,
            buffer_with_blocked_out_bg,
            blocked_out_buffer,
            block_out_from: snapshot.block_out_from,
            geo_size,
            pos,
            move_x: None,
            compensate_view_movements: false,
            buffer_offset,
            buffer_with_blocked_out_bg_offset,
            anim_state: AnimationState::new(blocker, anim),
            random_seed: fastrand::f32(),
        })
    }

    pub fn advance_animations(&mut self) {
        match &mut self.anim_state {
            AnimationState::Waiting { blocker, anim } => {
                if blocker.state() != BlockerState::Pending {
                    let anim = anim.restarted(0., 1., 0.);
                    self.anim_state = AnimationState::Animating(anim);
                }
            }
            AnimationState::Animating(_anim) => (),
        }
    }

    pub fn are_animations_ongoing(&self) -> bool {
        match &self.anim_state {
            AnimationState::Waiting { .. } => true,
            AnimationState::Animating(anim) => !anim.is_done(),
        }
    }

    pub(super) fn enable_view_movement_compensation(&mut self) {
        self.compensate_view_movements = true;
    }

    pub(super) fn compensate_view_movement(
        &mut self,
        delta: f64,
        clock: &Clock,
        config: niri_config::Animation,
    ) {
        if !self.compensate_view_movements || delta == 0. {
            return;
        }

        let current = self.move_x.as_ref().map_or(0., Animation::value);
        self.move_x = Some(Animation::new(
            clock.clone(),
            current,
            current + delta,
            0.,
            config,
        ));
    }

    pub(super) fn position_in_view(&self, view_loc: Point<f64, Logical>) -> Point<f64, Logical> {
        let mut pos = self.pos;
        pos.x += self.move_x.as_ref().map_or(0., Animation::value);
        pos -= view_loc;
        pos
    }

    pub fn render<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        view_rect: Rectangle<f64, Logical>,
        scale: Scale<f64>,
    ) -> Option<ClosingWindowRenderElement> {
        let (buffer, offset) = if ctx.target.should_block_out(self.block_out_from) {
            let (buffer, offset) = self.blocked_out_buffer.as_ref()?;
            (buffer, *offset)
        } else if ctx.target != RenderTarget::Output && self.buffer_with_blocked_out_bg.is_some() {
            (
                self.buffer_with_blocked_out_bg.as_ref().unwrap(),
                self.buffer_with_blocked_out_bg_offset,
            )
        } else {
            (&self.buffer, self.buffer_offset)
        };

        let anim = match &self.anim_state {
            AnimationState::Waiting { .. } => {
                let elem = TextureRenderElement::from_texture_buffer(
                    buffer.clone(),
                    Point::from((0., 0.)),
                    1.,
                    None,
                    None,
                    Kind::Unspecified,
                );

                let elem = PrimaryGpuTextureRenderElement(elem);
                let elem = RescaleRenderElement::from_element(elem, Point::from((0, 0)), 1.);

                let location = self.position_in_view(view_rect.loc) + offset;
                let elem = RelocateRenderElement::from_element(
                    elem,
                    location.to_physical_precise_round(scale),
                    Relocate::Relative,
                );

                return Some(elem.into());
            }
            AnimationState::Animating(anim) => anim,
        };

        let progress = anim.value();
        let clamped_progress = anim.clamped_value().clamp(0., 1.);

        // The custom close shader only exists on GLES, so on the Vulkan TTY renderer we fall
        // back to the plain texture path below (the snapshot texture is backend-agnostic).
        let close_shader = ctx.as_gles().is_some_and(|gles| {
            Shaders::get(gles.renderer)
                .program(ProgramType::Close)
                .is_some()
        }) && matches!(self.buffer.texture(), TtyOffscreen::Gles(_));

        if close_shader {
            let area_loc = Vec2::new(view_rect.loc.x as f32, view_rect.loc.y as f32);
            let area_size = Vec2::new(view_rect.size.w as f32, view_rect.size.h as f32);

            // Round to physical pixels relative to the view position. This is similar to what
            // happens when rendering normal windows.
            let relative = self.position_in_view(view_rect.loc);
            let pos = view_rect.loc + relative.to_physical_precise_round(scale).to_logical(scale);

            let geo_loc = Vec2::new(pos.x as f32, pos.y as f32);
            let geo_size = Vec2::new(self.geo_size.w as f32, self.geo_size.h as f32);

            let input_to_geo = Mat3::from_scale(area_size / geo_size)
                * Mat3::from_translation((area_loc - geo_loc) / area_size);

            let tex_scale = self.buffer.texture_scale();
            let tex_scale = Vec2::new(tex_scale.x as f32, tex_scale.y as f32);
            let tex_loc = Vec2::new(offset.x as f32, offset.y as f32);
            let tex_size = self.buffer.texture().size();
            let tex_size = Vec2::new(tex_size.w as f32, tex_size.h as f32) / tex_scale;

            let geo_to_tex =
                Mat3::from_translation(-tex_loc / tex_size) * Mat3::from_scale(geo_size / tex_size);

            let elem = ShaderRenderElement::new(
                ProgramType::Close,
                view_rect.size,
                None,
                scale.x as f32,
                1.,
                Rc::new([
                    mat3_uniform("niri_input_to_geo", input_to_geo),
                    Uniform::new("niri_geo_size", geo_size.to_array()),
                    mat3_uniform("niri_geo_to_tex", geo_to_tex),
                    Uniform::new("niri_progress", progress as f32),
                    Uniform::new("niri_clamped_progress", clamped_progress as f32),
                    Uniform::new("niri_random_seed", self.random_seed),
                ]),
                gles_texture_map(HashMap::from([(
                    String::from("niri_tex"),
                    match self.buffer.texture() {
                        TtyOffscreen::Gles(texture) => texture.clone(),
                        _ => unreachable!("close shader requires a GLES texture"),
                    },
                )])),
                Kind::Unspecified,
            )
            .with_location(Point::from((0., 0.)));
            return Some(elem.into());
        }

        let elem = TextureRenderElement::from_texture_buffer(
            buffer.clone(),
            Point::from((0., 0.)),
            1. - clamped_progress as f32,
            None,
            None,
            Kind::Unspecified,
        );

        let elem = PrimaryGpuTextureRenderElement(elem);

        let center = self.geo_size.to_point().downscale(2.);
        let elem = RescaleRenderElement::from_element(
            elem,
            (center - offset).to_physical_precise_round(scale),
            ((1. - clamped_progress) / 5. + 0.8).max(0.),
        );

        let location = self.position_in_view(view_rect.loc) + offset;
        let elem = RelocateRenderElement::from_element(
            elem,
            location.to_physical_precise_round(scale),
            Relocate::Relative,
        );

        Some(elem.into())
    }
}

#[cfg(feature = "vulkan")]
impl
    smithay::backend::renderer::element::RenderElement<
        smithay::backend::renderer::vulkan::VulkanRenderer,
    > for ClosingWindowRenderElement
{
    fn draw(
        &self,
        frame: &mut smithay::backend::renderer::vulkan::VulkanFrame<'_, '_>,
        src: smithay::utils::Rectangle<f64, smithay::utils::Buffer>,
        dst: smithay::utils::Rectangle<i32, smithay::utils::Physical>,
        damage: &[smithay::utils::Rectangle<i32, smithay::utils::Physical>],
        opaque_regions: &[smithay::utils::Rectangle<i32, smithay::utils::Physical>],
        cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), smithay::backend::renderer::vulkan::VulkanError> {
        match self {
            ClosingWindowRenderElement::Texture(elem) => {
                smithay::backend::renderer::element::RenderElement::<
                    smithay::backend::renderer::vulkan::VulkanRenderer,
                >::draw(elem, frame, src, dst, damage, opaque_regions, cache)
            }
            ClosingWindowRenderElement::Shader(elem) => {
                smithay::backend::renderer::element::RenderElement::<
                    smithay::backend::renderer::vulkan::VulkanRenderer,
                >::draw(elem, frame, src, dst, damage, opaque_regions, cache)
            }
        }
    }

    fn underlying_storage(
        &self,
        renderer: &mut smithay::backend::renderer::vulkan::VulkanRenderer,
    ) -> Option<smithay::backend::renderer::element::UnderlyingStorage<'_>> {
        match self {
            ClosingWindowRenderElement::Texture(elem) => elem.underlying_storage(renderer),
            ClosingWindowRenderElement::Shader(elem) => elem.underlying_storage(renderer),
        }
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use std::time::Duration;

    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::{Element as _, Kind};
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{
        Bind, Color32F, ExportMem, Frame as _, Offscreen as _, Renderer as _, Texture as _,
    };
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Buffer, Point, Rectangle, Scale, Size, Transform};

    use super::*;
    use crate::layout::tile::{TileRenderSnapshot, bake_snapshot_contents};
    use crate::render_helpers::BakedBuffer;
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

    fn baked(
        renderer: &mut VulkanRenderer,
        color: [f32; 4],
        size: Size<i32, Buffer>,
    ) -> BakedBuffer<TextureBuffer<TtyOffscreen>> {
        let solid = SolidColorBuffer::new((size.w as f64, size.h as f64), color);
        let elem = SolidColorRenderElement::from_buffer(
            &solid,
            Point::from((0., 0.)),
            1.,
            Kind::Unspecified,
        );
        bake_snapshot_contents(renderer, Scale::from(1.), vec![elem])
            .expect("bake snapshot contents")
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

    /// Builds a `TileRenderSnapshot` the same way `Tile::render_snapshot` does (baked textures),
    /// creates a `ClosingWindow` from it on Vulkan, renders the closing-window element and checks
    /// that the snapshot texture is sampled inside the closing region.
    #[test]
    fn vulkan_closing_window_draws_snapshot_texture() {
        for_each_vulkan_renderer(|renderer| {
            let size = Size::<i32, Buffer>::from((4, 4));
            let snapshot: TileRenderSnapshot = RenderSnapshot {
                contents: vec![baked(renderer, [1., 0., 0., 1.], size)],
                contents_with_blocked_out_bg: None,
                blocked_out_contents: vec![baked(renderer, [0., 0., 1., 1.], size)],
                block_out_from: None,
                size: Size::from((4., 4.)),
                texture: Default::default(),
                texture_with_blocked_out_bg: Default::default(),
                blocked_out_texture: Default::default(),
            };

            let clock = Clock::with_time(Duration::ZERO);
            let anim = Animation::new(
                clock,
                0.,
                1.,
                0.,
                niri_config::Animation {
                    off: false,
                    kind: niri_config::animations::Kind::Easing(
                        niri_config::animations::EasingParams {
                            duration_ms: 1000,
                            curve: niri_config::animations::Curve::Linear,
                        },
                    ),
                },
            );

            let closing = ClosingWindow::new(
                renderer,
                snapshot,
                Scale::from(1.),
                Size::from((4., 4.)),
                Point::from((0., 0.)),
                TransactionBlocker::completed(),
                anim,
            )
            .expect("create a closing window from a Vulkan snapshot");

            assert!(
                matches!(closing.buffer.texture(), TtyOffscreen::Vulkan(_)),
                "the closing window must draw a Vulkan texture"
            );

            let view_rect = Rectangle::new(Point::from((0., 0.)), Size::from((4., 4.)));
            let element = {
                let ctx = RenderCtx {
                    renderer,
                    target: RenderTarget::Output,
                    intent: crate::render_helpers::RenderIntent::Normal,
                    xray: None,
                    shader_time: 0.,
                };
                closing
                    .render(ctx, view_rect, Scale::from(1.))
                    .expect("render the closing window element")
            };

            let physical = Size::<i32, smithay::utils::Physical>::from((4, 4));
            let mut target = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .expect("readback target");
            {
                let mut fb = renderer.bind(&mut target).expect("bind target");
                let mut frame = renderer
                    .render(&mut fb, physical, Transform::Normal)
                    .expect("begin frame");
                let dst = Rectangle::from_size(physical);
                frame
                    .clear(Color32F::TRANSPARENT, &[dst])
                    .expect("clear frame");
                RenderElement::<VulkanRenderer>::draw(
                    &element,
                    &mut frame,
                    element.src(),
                    dst,
                    &[dst],
                    &[],
                    None,
                )
                .expect("draw the closing window");
                let _sync = frame.finish().expect("finish frame");
            }

            let pixels = read_back(renderer, &mut target, size);
            let center = (2 * 4 + 2) * 4;
            assert_eq!(
                &pixels[center..center + 4],
                &[255, 0, 0, 255],
                "the closing window did not sample its snapshot texture"
            );
        });
    }
}
