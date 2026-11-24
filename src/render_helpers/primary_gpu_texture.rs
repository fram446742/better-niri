use smithay::backend::renderer::Frame;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::VulkanRenderer;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};
use tracing::warn;

use super::renderer::AsGlesFrame;
use super::texture::TextureRenderElement;
use crate::backend::tty::{TtyFrame, TtyOffscreen, TtyRenderer, TtyRendererError};

/// Wrapper for a texture from the primary GPU for rendering with the primary
/// GPU.
///
/// Despite the historical name, the inner texture is a [`TtyOffscreen`], so the
/// element draws with the GLES renderer (winit/headless/TTY GLES) and with the
/// Vulkan TTY renderer, matching the texture arm to the frame.
#[derive(Debug, Clone)]
pub struct PrimaryGpuTextureRenderElement(pub TextureRenderElement<TtyOffscreen>);

impl Element for PrimaryGpuTextureRenderElement {
    fn id(&self) -> &Id {
        self.0.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.0.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.0.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.0.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.0.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.0.alpha()
    }

    fn kind(&self) -> Kind {
        self.0.kind()
    }
}

impl RenderElement<GlesRenderer> for PrimaryGpuTextureRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let buffer = self.0.buffer();
        let TtyOffscreen::Gles(texture) = buffer.texture() else {
            warn!("trying to render a non-GLES texture with the GLES renderer");
            return Ok(());
        };

        frame.render_texture_from_to(
            texture,
            src,
            dst,
            damage,
            opaque_regions,
            buffer.texture_transform(),
            self.0.alpha(),
            None,
            &[],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // If scanout for things other than Wayland buffers is implemented, this will need to take
        // the target GPU into account.
        None
    }
}

#[cfg(feature = "vulkan")]
impl RenderElement<VulkanRenderer> for PrimaryGpuTextureRenderElement {
    fn draw(
        &self,
        frame: &mut smithay::backend::renderer::vulkan::VulkanFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), smithay::backend::renderer::vulkan::VulkanError> {
        let buffer = self.0.buffer();
        let TtyOffscreen::Vulkan(texture) = buffer.texture() else {
            warn!("trying to render a non-Vulkan texture with the Vulkan renderer");
            return Ok(());
        };

        frame.render_texture_from_to(
            texture,
            src,
            dst,
            damage,
            opaque_regions,
            buffer.texture_transform(),
            self.0.alpha(),
        )
    }

    fn underlying_storage(&self, _renderer: &mut VulkanRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for PrimaryGpuTextureRenderElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let buffer = self.0.buffer();
        let transform = buffer.texture_transform();
        let alpha = self.0.alpha();

        #[cfg(feature = "vulkan")]
        {
            match (&mut *frame, buffer.texture()) {
                (frame, TtyOffscreen::Multi(texture)) => frame.render_texture_from_to(
                    texture,
                    src,
                    dst,
                    damage,
                    opaque_regions,
                    transform,
                    alpha,
                ),
                (TtyFrame::Gles(_), TtyOffscreen::Gles(texture)) => {
                    let Some(gles_frame) = frame.as_gles_frame() else {
                        return Ok(());
                    };
                    gles_frame
                        .render_texture_from_to(
                            texture,
                            src,
                            dst,
                            damage,
                            opaque_regions,
                            transform,
                            alpha,
                            None,
                            &[],
                        )
                        .map_err(Into::into)
                }
                (TtyFrame::Vulkan(multi), TtyOffscreen::Vulkan(texture)) => {
                    let vk_frame: &mut smithay::backend::renderer::vulkan::VulkanFrame<'_, '_> =
                        multi.as_mut();
                    vk_frame
                        .render_texture_from_to(
                            texture,
                            src,
                            dst,
                            damage,
                            opaque_regions,
                            transform,
                            alpha,
                        )
                        .map_err(|err| {
                            TtyRendererError::Vulkan(
                                smithay::backend::renderer::multigpu::Error::Render(err),
                            )
                        })
                }
                _ => {
                    warn!("texture arm does not match the TTY renderer variant");
                    Ok(())
                }
            }
        }
        #[cfg(not(feature = "vulkan"))]
        {
            match buffer.texture() {
                TtyOffscreen::Multi(texture) => frame.render_texture_from_to(
                    texture,
                    src,
                    dst,
                    damage,
                    opaque_regions,
                    transform,
                    alpha,
                ),
                TtyOffscreen::Gles(_) => {
                    warn!("GLES offscreen texture on the multi-GPU TTY renderer");
                    Ok(())
                }
            }
        }
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
