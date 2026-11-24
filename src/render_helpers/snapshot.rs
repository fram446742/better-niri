use std::cell::OnceCell;

use niri_config::BlockOutFrom;
use smithay::backend::renderer::ErasedContextId;
use smithay::backend::renderer::element::{Kind, RenderElement};
use smithay::backend::renderer::gles::GlesTexture;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale, Size};

use super::{ToRenderElement, encompassing_geo};
use crate::backend::tty::TtyOffscreen;
use crate::render_helpers::offscreen::OffscreenBuffer;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::{RenderCtx, RenderTarget};

/// A snapshot baked into a texture, together with the renderer context that produced it.
#[derive(Debug, Clone)]
pub struct SnapshotTexture {
    /// Backend-agnostic texture contents.
    pub texture: TtyOffscreen,
    /// Id of the renderer context that created `texture`.
    pub context_id: ErasedContextId,
    /// Physical geometry of the baked contents.
    pub geo: Rectangle<i32, Physical>,
}

impl SnapshotTexture {
    /// The backend-agnostic texture.
    pub fn texture(&self) -> &TtyOffscreen {
        &self.texture
    }

    /// The texture as a raw GLES texture, if it was baked by a GLES renderer.
    pub fn texture_gles(&self) -> Option<&GlesTexture> {
        match &self.texture {
            TtyOffscreen::Gles(texture) => Some(texture),
            _ => None,
        }
    }

    /// The texture as a raw Vulkan texture, if it was baked by a Vulkan renderer.
    #[cfg(feature = "vulkan")]
    pub fn texture_vulkan(&self) -> Option<&smithay::backend::renderer::vulkan::VulkanTexture> {
        match &self.texture {
            TtyOffscreen::Vulkan(texture) => Some(texture),
            _ => None,
        }
    }
}

/// Snapshot of a render.
#[derive(Debug)]
pub struct RenderSnapshot<C, B> {
    /// Contents for a normal render.
    ///
    /// Relative to the geometry.
    pub contents: Vec<C>,

    /// Contents that are not blocked out, but the background is blocked out.
    ///
    /// If `None` then the background doesn't have any blocked-out surfaces, and normal `contents`
    /// can be used instead.
    pub contents_with_blocked_out_bg: Option<Vec<C>>,

    /// Blocked-out contents.
    ///
    /// Relative to the geometry.
    pub blocked_out_contents: Vec<B>,

    /// Where the contents were blocked out from at the time of the snapshot.
    pub block_out_from: Option<BlockOutFrom>,

    /// Visual size of the element at the point of the snapshot.
    pub size: Size<f64, Logical>,

    /// Contents rendered into a texture (lazily).
    ///
    /// Stored as a backend-agnostic [`TtyOffscreen`] plus the context id of the renderer that
    /// produced it, so the same snapshot can be baked on GLES or Vulkan and drawn on the
    /// matching backend.
    pub texture: OnceCell<Option<SnapshotTexture>>,

    /// Contents with blocked-out bg rendered into a texture (lazily).
    pub texture_with_blocked_out_bg: OnceCell<Option<SnapshotTexture>>,

    /// Blocked-out contents rendered into a texture (lazily).
    pub blocked_out_texture: OnceCell<Option<SnapshotTexture>>,
}

impl<C, B, EC, EB> RenderSnapshot<C, B>
where
    C: ToRenderElement<RenderElement = EC>,
    B: ToRenderElement<RenderElement = EB>,
{
    pub fn texture<R>(&self, ctx: RenderCtx<R>, scale: Scale<f64>) -> Option<&SnapshotTexture>
    where
        R: NiriRenderer,
        EC: RenderElement<R>,
        EB: RenderElement<R>,
    {
        // We don't check for should_hide() here. The only way for that to be true here is when
        // rendering a resizing window that went from blocked-out-hidden to currently visible.
        // It's ok to render the black rectangle in this case, and avoids having to deal with edge
        // cases like animated border size not matching window size.
        if ctx.target.should_block_out(self.block_out_from) {
            self.blocked_out_texture.get_or_init(|| {
                let _span = tracy_client::span!("RenderSnapshot::texture");
                bake(
                    ctx,
                    scale,
                    &self.blocked_out_contents,
                    "blocked-out contents",
                )
            })
        } else if ctx.target != RenderTarget::Output && self.contents_with_blocked_out_bg.is_some()
        {
            let contents = self.contents_with_blocked_out_bg.as_ref().unwrap();
            self.texture_with_blocked_out_bg.get_or_init(|| {
                let _span = tracy_client::span!("RenderSnapshot::texture");
                bake(ctx, scale, contents, "contents with blocked-out bg")
            })
        } else {
            self.texture.get_or_init(|| {
                let _span = tracy_client::span!("RenderSnapshot::texture");
                bake(ctx, scale, &self.contents, "contents")
            })
        }
        .as_ref()
    }
}

fn bake<R, C, EC>(
    ctx: RenderCtx<R>,
    scale: Scale<f64>,
    contents: &[C],
    what: &str,
) -> Option<SnapshotTexture>
where
    R: NiriRenderer,
    C: ToRenderElement<RenderElement = EC>,
    EC: RenderElement<R>,
{
    let elements: Vec<_> = contents
        .iter()
        .map(|baked| baked.to_render_element(Point::from((0., 0.)), scale, 1., Kind::Unspecified))
        .collect();

    // Compute the geometry exactly like the old GLES `render_to_encompassing_texture` did, rather
    // than recovering it from the rounded element geometry.
    let geo = encompassing_geo(scale, elements.iter());

    let buffer = OffscreenBuffer::default();
    match buffer.render(ctx.renderer, scale, &elements) {
        Ok((elem, _sync_point, _data)) => Some(SnapshotTexture {
            texture: elem.texture().clone(),
            context_id: elem.context_id(),
            geo,
        }),
        Err(err) => {
            warn!("error rendering {what} to texture: {err:?}");
            None
        }
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{
        Bind, Color32F, ExportMem, Frame as _, Offscreen as _, Renderer as _, Texture as _,
    };
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Buffer, Point, Rectangle, Scale, Size, Transform};

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

    fn baked_red_buffer(
        renderer: &mut VulkanRenderer,
        color: [f32; 4],
        size: Size<i32, Buffer>,
    ) -> crate::render_helpers::BakedBuffer<
        crate::render_helpers::texture::TextureBuffer<TtyOffscreen>,
    > {
        let buffer = OffscreenBuffer::default();
        let solid = SolidColorBuffer::new((size.w as f64, size.h as f64), color);
        let elem = SolidColorRenderElement::from_buffer(
            &solid,
            Point::from((0., 0.)),
            1.,
            Kind::Unspecified,
        );
        let (baked, _sync, _data) = buffer
            .render(renderer, Scale::from(1.), &[elem])
            .expect("bake a solid color");
        crate::render_helpers::BakedBuffer {
            buffer: baked.texture_buffer(Transform::Normal, Vec::new()),
            location: Point::from((0., 0.)),
            src: None,
            dst: None,
        }
    }

    /// The generic snapshot bake must produce a Vulkan texture on the TTY renderer path, and the
    /// normal and blocked-out caches must sample their own contents.
    #[test]
    fn vulkan_snapshot_bake_reads_back_contents() {
        for_each_vulkan_renderer(|renderer| {
            let size = Size::<i32, Buffer>::from((4, 4));
            let contents = baked_red_buffer(renderer, [1., 0., 0., 1.], size);

            let blocked_out = SolidColorBuffer::new((4., 4.), [0., 0., 1., 1.]);
            let blocked_out = crate::render_helpers::BakedBuffer {
                buffer: blocked_out,
                location: Point::from((0., 0.)),
                src: None,
                dst: None,
            };

            let snapshot: RenderSnapshot<
                crate::render_helpers::BakedBuffer<
                    crate::render_helpers::texture::TextureBuffer<TtyOffscreen>,
                >,
                crate::render_helpers::BakedBuffer<SolidColorBuffer>,
            > = RenderSnapshot {
                contents: vec![contents],
                contents_with_blocked_out_bg: None,
                blocked_out_contents: vec![blocked_out],
                block_out_from: Some(BlockOutFrom {
                    target: niri_config::BlockOutTarget::ScreenCapture,
                    kind: niri_config::BlockOutKind::Fill,
                }),
                size: Size::from((4., 4.)),
                texture: Default::default(),
                texture_with_blocked_out_bg: Default::default(),
                blocked_out_texture: Default::default(),
            };

            let mut target = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .expect("target");

            for (target_kind, expected) in [
                (RenderTarget::Output, [255, 0, 0, 255]),
                (RenderTarget::ScreenCapture, [0, 0, 255, 255]),
            ] {
                let ctx = RenderCtx {
                    renderer,
                    target: target_kind,
                    intent: crate::render_helpers::RenderIntent::Normal,
                    xray: None,
                    shader_time: 0.,
                };
                let baked = snapshot
                    .texture(ctx, Scale::from(1.))
                    .expect("bake snapshot");
                assert!(
                    baked.texture_vulkan().is_some(),
                    "{} snapshot must be a Vulkan texture",
                    target_kind as u8
                );

                let texture = baked.texture_vulkan().unwrap();
                let src = Rectangle::from_size(texture.size().to_f64());
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
                        &crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement(
                            crate::render_helpers::texture::TextureRenderElement::from_texture_buffer(
                                crate::render_helpers::texture::TextureBuffer::from_tty_texture(
                                    baked.texture().clone(),
                                    baked.context_id.clone(),
                                    Scale::from(1.),
                                    Transform::Normal,
                                    Vec::new(),
                                ),
                                (0., 0.),
                                1.,
                                None,
                                None,
                                Kind::Unspecified,
                            ),
                        ),
                        &mut frame,
                        src,
                        dst,
                        &[dst],
                        &[],
                        None,
                    )
                    .expect("draw snapshot");
                    let _sync = frame.finish().expect("finish frame");
                }

                let pixels = read_back(renderer, &mut target, size);
                assert_eq!(
                    &pixels[0..4],
                    &expected,
                    "unexpected snapshot content for {}",
                    target_kind as u8
                );
            }
        });
    }
}
