use std::cell::RefCell;

use anyhow::Context as _;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{
    Element, Id, Kind, RenderElement, RenderElementStates, UnderlyingStorage,
};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::utils::{
    CommitCounter, DamageBag, DamageSet, DamageSnapshot, OpaqueRegions,
};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{VulkanError, VulkanFrame, VulkanRenderer, VulkanTexture};
use smithay::backend::renderer::{Color32F, ContextId, ErasedContextId, Frame as _, Texture as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use super::encompassing_geo;
use super::renderer::{AsGlesFrame as _, NiriRenderer};
use crate::backend::tty::{TtyFrame, TtyOffscreen, TtyRenderer, TtyRendererError};

/// Buffer for offscreen rendering.
#[derive(Debug)]
pub struct OffscreenBuffer {
    id: Id,

    /// The cached texture buffer.
    ///
    /// Lazily created when `render` is called. Recreated when necessary.
    inner: RefCell<Option<Inner>>,
}

#[derive(Debug)]
struct Inner {
    /// The texture with offscreened contents.
    ///
    /// Backend-agnostic: GLES renderers wrap their `GlesTexture` as
    /// [`TtyOffscreen::Gles`], the TTY renderer uses whatever its active API produced
    /// (`Gles`/`Vulkan`), and the Vulkan renderer wraps a `VulkanTexture`.
    texture: TtyOffscreen,
    /// Id of the renderer context that the texture comes from (type-erased, so the same
    /// offscreen element can be compared against GLES and Vulkan renderer contexts).
    renderer_context_id: ErasedContextId,
    /// Scale of the texture.
    scale: Scale<f64>,
    /// Damage tracker for drawing to the texture.
    damage: OutputDamageTracker,
    /// Damage of this offscreen element itself facing outside.
    outer_damage: DamageBag<i32, Buffer>,
}

#[derive(Debug, Clone)]
pub struct OffscreenRenderElement {
    id: Id,
    texture: TtyOffscreen,
    renderer_context_id: ErasedContextId,
    scale: Scale<f64>,
    damage: DamageSnapshot<i32, Buffer>,
    offset: Point<f64, Logical>,
    src_size: Size<i32, Buffer>,
    alpha: f32,
    kind: Kind,
}

#[derive(Debug)]
pub struct OffscreenData {
    /// Id of the offscreen element.
    pub id: Id,
    /// States for the render into the offscreen buffer.
    pub states: RenderElementStates,
    /// Whether this render actually changed the texture contents, i.e. the inner
    /// `OutputDamageTracker` reported damage. `false` means the render was a pure diff no-op and
    /// the texture pixels are bit-identical to the previous `render` call on this buffer. Buffer
    /// (re)creation (first render, size growth, lost uniqueness, renderer context change) resets
    /// the tracker and thus always reports `true` via full damage. Callers that retain the
    /// returned element across frames (e.g. `PanelSource`'s publish-on-damage) key off this.
    pub damaged: bool,
}

impl OffscreenBuffer {
    pub fn clear(&self) {
        self.inner.borrow_mut().take();
    }

    /// Renders `elements` into this buffer's cached texture.
    ///
    /// Generic over the renderer: GLES callers pass a `GlesRenderer` (the buffer stores a
    /// [`TtyOffscreen::Gles`] and behaves exactly as before), while the TTY backend can pass its
    /// `TtyRenderer` to bake on Vulkan as well.
    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        scale: Scale<f64>,
        elements: &[impl RenderElement<R>],
    ) -> anyhow::Result<(OffscreenRenderElement, SyncPoint, OffscreenData)> {
        let geo = encompassing_geo(scale, elements.iter());
        self.render_with_geometry(renderer, scale, geo, false, elements)
    }

    pub fn render_with_bounds<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        scale: Scale<f64>,
        bounds: Rectangle<i32, Physical>,
        elements: &[impl RenderElement<R>],
    ) -> anyhow::Result<(OffscreenRenderElement, SyncPoint, OffscreenData)> {
        self.render_with_geometry(renderer, scale, bounds, true, elements)
    }

    fn render_with_geometry<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        scale: Scale<f64>,
        geo: Rectangle<i32, Physical>,
        exact_size: bool,
        elements: &[impl RenderElement<R>],
    ) -> anyhow::Result<(OffscreenRenderElement, SyncPoint, OffscreenData)> {
        let _span = tracy_client::span!("OffscreenBuffer::render");

        let elements = Vec::from_iter(elements.iter().map(|ele| {
            RelocateRenderElement::from_element(ele, geo.loc.upscale(-1), Relocate::Relative)
        }));

        // Guard against empty elements producing a zero size.
        let mut src_size = geo.size;
        if src_size.w == 0 || src_size.h == 0 {
            src_size = Size::new(1, 1);
        }

        let src_size = src_size.to_logical(1).to_buffer(1, Transform::Normal);
        let offset = geo.loc.to_f64().to_logical(scale);

        let mut inner = self.inner.borrow_mut();

        // Check if we need to create or recreate the texture.
        let size_string;
        let mut reason = "";
        if let Some(Inner {
            texture,
            renderer_context_id,
            ..
        }) = inner.as_mut()
        {
            let old_size = texture.size();
            if (exact_size && old_size != src_size)
                || old_size.w < src_size.w
                || old_size.h < src_size.h
            {
                size_string = format!(
                    "size changed from {} × {} to {} × {}",
                    old_size.w, old_size.h, src_size.w, src_size.h
                );
                reason = &size_string;

                *inner = None;
            } else if !texture.is_unique_reference() {
                reason = "not unique";

                *inner = None;
            } else if *renderer_context_id != ContextId::erased(&renderer.context_id()) {
                reason = "renderer id changed";

                *inner = None;
            }
        } else {
            reason = "first render";
        }

        let inner = if let Some(inner) = inner.as_mut() {
            inner
        } else {
            trace!("creating new texture: {reason}");
            let span = tracy_client::span!("creating offscreen buffer");
            span.emit_text(reason);

            let texture = renderer
                .create_buffer(Fourcc::Abgr8888, src_size)
                .context("error creating texture")?;
            let texture = R::wrap_offscreen(texture);

            let buffer_size = src_size.to_logical(1, Transform::Normal).to_physical(1);
            let damage = OutputDamageTracker::new(buffer_size, scale, Transform::Normal);

            inner.insert(Inner {
                texture,
                renderer_context_id: ContextId::erased(&renderer.context_id()),
                scale,
                damage,
                outer_damage: DamageBag::default(),
            })
        };

        // When leaving the old texture as is, its size might be bigger than src_size.
        let texture_size = inner.texture.size();
        let buffer_size = texture_size.to_logical(1, Transform::Normal).to_physical(1);

        // Recreate the damage tracker if the scale changes. We already recreate it for buffer size
        // changes, and transform is always Normal.
        if inner.scale != scale {
            inner.scale = scale;

            trace!("recreating damage tracker due to scale change");
            inner.damage = OutputDamageTracker::new(buffer_size, scale, Transform::Normal);
            inner.outer_damage = DamageBag::default();
        }

        let res = {
            let target = R::unwrap_offscreen(&mut inner.texture)
                .context("offscreen texture variant does not match the active renderer")?;
            let mut target = renderer.bind(target)?;
            inner
                .damage
                .render_output(renderer, &mut target, 1, &elements, Color32F::TRANSPARENT)
                .context("error rendering")?
        };

        // Add the resulting damage to the outer tracker. `OutputDamageTracker::render_output`
        // returns `damage: None` exactly when there was no damage (a `Some` is never empty), so
        // `is_some()` is a reliable "contents changed" signal.
        let damaged = res.damage.is_some();
        if let Some(damage) = res.damage {
            // OutputDamageTracker gives us Physical coordinate space, but it's actually the Buffer
            // space because we were rendering to a texture.
            let size = buffer_size.to_logical(1);
            let damage = damage
                .iter()
                .map(|rect| rect.to_logical(1).to_buffer(1, Transform::Normal, &size));
            inner.outer_damage.add(damage);
        }

        let elem = OffscreenRenderElement {
            id: self.id.clone(),
            texture: inner.texture.clone(),
            renderer_context_id: inner.renderer_context_id.clone(),
            scale,
            damage: inner.outer_damage.snapshot(),
            offset,
            src_size,
            alpha: 1.,
            kind: Kind::Unspecified,
        };

        let data = OffscreenData {
            id: self.id.clone(),
            states: res.states,
            damaged,
        };

        Ok((elem, res.sync, data))
    }

    /// Whether this buffer currently holds no cached texture, i.e. either never rendered or
    /// already [`Self::clear`]-ed.
    pub fn is_empty(&self) -> bool {
        self.inner.borrow().is_none()
    }
}

impl Default for OffscreenBuffer {
    fn default() -> Self {
        OffscreenBuffer {
            inner: RefCell::new(None),
            id: Id::new(),
        }
    }
}

impl OffscreenRenderElement {
    /// The backend-agnostic offscreen texture.
    ///
    /// GLES producers get a [`TtyOffscreen::Gles`], a Vulkan (or Vulkan-capable TTY) producer a
    /// [`TtyOffscreen::Vulkan`]. Consumers that require a raw GLES texture (the GLES-only blur and
    /// shader chains) use [`Self::texture_gles`] instead.
    pub fn texture(&self) -> &TtyOffscreen {
        &self.texture
    }

    /// The offscreen texture as a raw GLES texture, if it was produced by a GLES renderer.
    pub fn texture_gles(&self) -> Option<&GlesTexture> {
        match &self.texture {
            TtyOffscreen::Gles(texture) => Some(texture),
            _ => None,
        }
    }

    /// The offscreen texture as a raw Vulkan texture, if it was produced by a Vulkan renderer.
    #[cfg(feature = "vulkan")]
    pub fn texture_vulkan(&self) -> Option<&VulkanTexture> {
        match &self.texture {
            TtyOffscreen::Vulkan(texture) => Some(texture),
            _ => None,
        }
    }

    /// Renderer context this element's texture was created against (type-erased from the
    /// creating renderer's `TextureId`). Callers building their own render elements from this
    /// texture (e.g. `PanelRenderElement`) need this to guard their own `draw` against
    /// cross-context rendering, the same way `Self::draw` guards itself below.
    pub fn context_id(&self) -> ErasedContextId {
        self.renderer_context_id.clone()
    }

    pub fn offset(&self) -> Point<f64, Logical> {
        self.offset
    }

    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha;
        self
    }

    pub fn with_offset(mut self, offset: Point<f64, Logical>) -> Self {
        self.offset = offset;
        self
    }

    pub fn logical_size(&self) -> Size<f64, Logical> {
        self.src_size
            .to_f64()
            .to_logical(self.scale, Transform::Normal)
    }

    /// Builds a texture buffer from this element's texture, carrying the producing renderer's
    /// context id. Useful for consumers that need the baked texture without the element wrapper
    /// (screen transitions, screenshot UI).
    pub fn texture_buffer(
        &self,
        transform: Transform,
        opaque_regions: Vec<Rectangle<i32, Buffer>>,
    ) -> super::texture::TextureBuffer<TtyOffscreen> {
        super::texture::TextureBuffer::from_tty_texture(
            self.texture.clone(),
            self.renderer_context_id.clone(),
            self.scale,
            transform,
            opaque_regions,
        )
    }

    fn damage_since(&self, commit: Option<CommitCounter>) -> DamageSet<i32, Buffer> {
        self.damage
            .damage_since(commit)
            .unwrap_or_else(|| DamageSet::from_slice(&[Rectangle::from_size(self.texture.size())]))
    }
}

impl Element for OffscreenRenderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.damage.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let logical_geo = Rectangle::new(self.offset, self.logical_size());
        logical_geo.to_physical_precise_round(scale)
    }

    fn transform(&self) -> Transform {
        Transform::Normal
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.src_size).to_f64()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        let texture_size = self.texture.size().to_f64();
        let src = self.src();

        self.damage_since(commit)
            .into_iter()
            .filter_map(|region| {
                let mut region = region.to_f64().intersection(src)?;

                region.loc -= src.loc;
                region = region.upscale(texture_size / src.size);

                let logical = region.to_logical(self.scale, Transform::Normal, &src.size);
                Some(logical.to_physical_precise_up(scale))
            })
            .collect()
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        self.kind
    }
}

impl OffscreenRenderElement {
    /// Draws the offscreen texture into a GLES frame without checking the renderer context.
    ///
    /// Skips silently for a non-GLES texture (e.g. a Vulkan-baked offscreen drawn through a GLES
    /// frame, which cannot sample it).
    fn draw_gles(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dest: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let Some(texture) = self.texture_gles() else {
            warn!("trying to render a non-GLES offscreen texture through GLES");
            return Ok(());
        };

        frame.render_texture_from_to(
            texture,
            src,
            dest,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
            None,
            &[],
        )
    }

    /// Draws the offscreen texture into a Vulkan frame without checking the renderer context.
    #[cfg(feature = "vulkan")]
    fn draw_vulkan_raw(
        &self,
        frame: &mut VulkanFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dest: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), VulkanError> {
        let Some(texture) = self.texture_vulkan() else {
            warn!("trying to render a non-Vulkan offscreen texture through Vulkan");
            return Ok(());
        };

        frame.render_texture_from_to(
            texture,
            src,
            dest,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
        )
    }
}

impl RenderElement<GlesRenderer> for OffscreenRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dest: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        if ContextId::erased(&frame.context_id()) != self.renderer_context_id {
            warn!("trying to render texture from different renderer");
            return Ok(());
        }

        self.draw_gles(frame, src, dest, damage, opaque_regions)
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // If scanout for things other than Wayland buffers is implemented, this will need to take
        // the target GPU into account.
        None
    }
}

#[cfg(feature = "vulkan")]
impl RenderElement<VulkanRenderer> for OffscreenRenderElement {
    fn draw(
        &self,
        frame: &mut VulkanFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dest: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), VulkanError> {
        if ContextId::erased(&frame.context_id()) != self.renderer_context_id {
            warn!("trying to render texture from different renderer");
            return Ok(());
        }

        self.draw_vulkan_raw(frame, src, dest, damage, opaque_regions)
    }

    fn underlying_storage(&self, _renderer: &mut VulkanRenderer) -> Option<UnderlyingStorage<'_>> {
        // If scanout for things other than Wayland buffers is implemented, this will need to take
        // the target GPU into account.
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for OffscreenRenderElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        // Offscreens baked through `TtyRenderer` itself carry the TTY renderer's own
        // (MultiTexture-typed) context id; the inner frames below report their concrete API's
        // texture type instead, so match the outer frame first.
        let created_by_tty = ContextId::erased(&frame.context_id()) == self.renderer_context_id;

        #[cfg(feature = "vulkan")]
        if let TtyFrame::Vulkan(multi) = frame {
            if !created_by_tty {
                warn!("trying to render texture from different renderer");
                return Ok(());
            }
            let vk_frame: &mut VulkanFrame<'_, '_> = multi.as_mut();
            return self
                .draw_vulkan_raw(vk_frame, src, dst, damage, opaque_regions)
                .map_err(|err| {
                    TtyRendererError::Vulkan(smithay::backend::renderer::multigpu::Error::Render(
                        err,
                    ))
                });
        }

        let Some(gles_frame) = frame.as_gles_frame() else {
            return Ok(());
        };
        if created_by_tty {
            self.draw_gles(gles_frame, src, dst, damage, opaque_regions)?;
        } else {
            // Produced by a raw `GlesRenderer` (e.g. `RenderCtx::as_gles`): delegate so the
            // typed impl still performs its own context check.
            RenderElement::<GlesRenderer>::draw(
                self,
                gles_frame,
                src,
                dst,
                damage,
                opaque_regions,
                cache,
            )?;
        }
        Ok(())
    }

    fn underlying_storage(
        &self,
        _renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        // If scanout for things other than Wayland buffers is implemented, this will need to take
        // the target GPU into account.
        None
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen, Renderer, Texture};
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Point, Rectangle, Scale, Size, Transform};

    use super::*;
    use crate::render_helpers::panel::PanelRenderElement;
    use crate::render_helpers::panel_quad::tilted_panel_corners;
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

    /// Reads back a frame region drawn into `target` and returns the raw bytes.
    fn read_back(
        renderer: &mut VulkanRenderer,
        target: &mut smithay::backend::renderer::vulkan::VulkanTexture,
        size: Size<i32, smithay::utils::Buffer>,
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

    /// A Vulkan `OffscreenBuffer` render must allocate a real Vulkan texture, report damage on
    /// the first render, and the element must be drawable with its own texture contents. A second
    /// identical render (after dropping the returned element) must be a damage no-op, proving the
    /// generic damage/`is_unique_reference` path still works on Vulkan.
    #[test]
    fn vulkan_offscreen_buffer_renders_and_reads_back() {
        for_each_vulkan_renderer(|renderer| {
            let size = Size::<i32, smithay::utils::Buffer>::from((4, 4));
            let buffer = OffscreenBuffer::default();
            let color = SolidColorBuffer::new((4., 4.), [1., 0., 0., 1.]);
            let element = SolidColorRenderElement::from_buffer(
                &color,
                Point::from((0., 0.)),
                1.,
                Kind::Unspecified,
            );

            let (elem, _sync, data) = buffer
                .render(renderer, Scale::from(1.), &[element.clone()])
                .expect("render into the generic offscreen buffer");
            assert!(elem.texture_vulkan().is_some(), "expected a Vulkan texture");
            assert!(data.damaged, "first offscreen render must report damage");

            let mut target = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .expect("readback target");
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
                    dst,
                    &[dst],
                    &[],
                    None,
                )
                .expect("draw the offscreen element");
                let _sync = frame.finish().expect("finish frame");
            }

            let pixels = read_back(renderer, &mut target, size);
            assert_eq!(
                &pixels[0..4],
                &[255, 0, 0, 255],
                "the offscreen texture did not reach the readback target"
            );

            drop(elem);
            let (_elem, _sync, data) = buffer
                .render(renderer, Scale::from(1.), &[element])
                .expect("second render into the generic offscreen buffer");
            assert!(
                !data.damaged,
                "an unchanged second offscreen render must report no damage"
            );
        });
    }

    /// End-to-end carousel-panel shape on Vulkan: bake a source through the generic
    /// `OffscreenBuffer` (the panel prepass), then build a `PanelRenderElement` from that
    /// `TtyOffscreen::Vulkan` texture (the host-side retained element) and draw it. The centre of
    /// the quad must sample the baked red source.
    #[test]
    fn vulkan_panel_bake_then_draw_samples_baked_texture() {
        for_each_vulkan_renderer(|renderer| {
            init_vulkan(renderer, &niri_config::Config::default());

            let source = SolidColorBuffer::new((4., 4.), [1., 0., 0., 1.]);
            let source_elem = SolidColorRenderElement::from_buffer(
                &source,
                Point::from((0., 0.)),
                1.,
                Kind::Unspecified,
            );
            let buffer = OffscreenBuffer::default();
            let (baked, _sync, data) = buffer
                .render(renderer, Scale::from(1.), &[source_elem])
                .expect("bake the panel source");
            assert!(data.damaged);

            let corners = tilted_panel_corners((4., 4.), (8., 8.), 0., 1000.);
            let panel = PanelRenderElement::new(
                corners,
                baked.texture().clone(),
                baked.id().clone(),
                1,
                baked.context_id(),
                1.,
                1.,
                1.,
            )
            .expect("build the panel element");

            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((8, 8));
            let mut target = renderer
                .create_buffer(Fourcc::Abgr8888, buffer_size)
                .expect("panel readback target");
            {
                let mut fb = renderer.bind(&mut target).expect("bind target");
                let mut frame = renderer
                    .render(&mut fb, Size::from((8, 8)), Transform::Normal)
                    .expect("begin frame");
                let dst = Rectangle::from_size(Size::from((8, 8)));
                frame
                    .clear(Color32F::TRANSPARENT, &[dst])
                    .expect("clear frame");
                RenderElement::<VulkanRenderer>::draw(
                    &panel,
                    &mut frame,
                    Rectangle::from_size(Size::from((1., 1.))),
                    dst,
                    &[dst],
                    &[],
                    None,
                )
                .expect("draw the panel element");
                let _sync = frame.finish().expect("finish frame");
            }

            let pixels = read_back(renderer, &mut target, buffer_size);
            let center = (4 * 8 + 4) * 4;
            assert_eq!(
                &pixels[center..center + 4],
                &[255, 0, 0, 255],
                "the panel did not sample the baked offscreen texture"
            );
        });
    }
}
