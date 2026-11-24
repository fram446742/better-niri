use smithay::backend::renderer::Frame as _;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{VulkanError, VulkanFrame, VulkanRenderer};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};
use tracing::warn;

use super::blend::FrameBlendState;
use super::texture::TextureRenderElement;
use crate::backend::tty::{TtyFrame, TtyOffscreen, TtyRenderer, TtyRendererError};
#[cfg(feature = "vulkan")]
use crate::render_helpers::renderer::AsGlesFrame as _;
use crate::render_helpers::shaders::Shaders;

/// A texture drawn with the gradient-fade texture program.
///
/// The texture itself is backend-agnostic: on GLES it is drawn with the `gradient_fade` texture
/// program override (unchanged pixels), on Vulkan it falls back to a plain texture draw because
/// the Vulkan shader set has no equivalent program.
#[derive(Debug, Clone)]
pub struct GradientFadeTextureRenderElement {
    inner: TextureRenderElement<TtyOffscreen>,
    program: GradientFadeShader,
    cutoff: (f32, f32),
}

#[derive(Debug, Clone)]
pub struct GradientFadeShader(GlesTexProgram);

impl GradientFadeTextureRenderElement {
    pub fn new(texture: TextureRenderElement<TtyOffscreen>, program: GradientFadeShader) -> Self {
        let logical_w = texture.buffer().logical_size().w;
        let logical_src_w = texture.logical_src().size.w;
        let cutoff = if logical_src_w < logical_w {
            // Texture is clipped, add a fade.
            let cutoff = 1. - f64::min(18. / logical_src_w, 1.);
            let full = logical_src_w / logical_w;
            ((cutoff * full) as f32, full as f32)
        } else {
            // Texture is displayed full-size, no cutoff necessary.
            (1., 1.)
        };
        Self {
            inner: texture,
            program,
            cutoff,
        }
    }

    pub fn shader(renderer: &mut GlesRenderer) -> Option<GradientFadeShader> {
        let program = Shaders::get(renderer).gradient_fade.clone();
        program.map(GradientFadeShader)
    }
}

impl Element for GradientFadeTextureRenderElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl GradientFadeTextureRenderElement {
    fn draw_gles(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let buffer = self.inner.buffer();
        let TtyOffscreen::Gles(texture) = buffer.texture() else {
            warn!("trying to draw a non-GLES gradient-fade texture with the GLES renderer");
            return Ok(());
        };
        let mut uniforms = vec![Uniform::new("cutoff", self.cutoff)];
        uniforms.extend(FrameBlendState::uniforms(frame));
        let saved = frame.take_tex_program_override();
        frame.override_default_tex_program(self.program.0.clone(), uniforms);
        let res = frame.render_texture_from_to(
            texture,
            src,
            dst,
            damage,
            opaque_regions,
            buffer.texture_transform(),
            self.inner.alpha(),
            None,
            &[],
        );
        frame.set_tex_program_override(saved);
        res
    }
}

impl RenderElement<GlesRenderer> for GradientFadeTextureRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        self.draw_gles(frame, src, dst, damage, opaque_regions)
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // If scanout for things other than Wayland buffers is implemented, this will need to take
        // the target GPU into account.
        None
    }
}

#[cfg(feature = "vulkan")]
impl RenderElement<VulkanRenderer> for GradientFadeTextureRenderElement {
    fn draw(
        &self,
        frame: &mut VulkanFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), VulkanError> {
        let buffer = self.inner.buffer();
        let TtyOffscreen::Vulkan(texture) = buffer.texture() else {
            warn!("trying to draw a non-Vulkan gradient-fade texture with the Vulkan renderer");
            return Ok(());
        };

        frame.render_texture_from_to(
            texture,
            src,
            dst,
            damage,
            opaque_regions,
            buffer.texture_transform(),
            self.inner.alpha(),
        )
    }

    fn underlying_storage(&self, _renderer: &mut VulkanRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for GradientFadeTextureRenderElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let buffer = self.inner.buffer();
        let transform = buffer.texture_transform();
        let alpha = self.inner.alpha();

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
                (TtyFrame::Gles(_), TtyOffscreen::Gles(_)) => {
                    let Some(gles_frame) = frame.as_gles_frame() else {
                        return Ok(());
                    };
                    self.draw_gles(gles_frame, src, dst, damage, opaque_regions)
                        .map_err(Into::into)
                }
                (TtyFrame::Vulkan(multi), TtyOffscreen::Vulkan(texture)) => {
                    let vk_frame: &mut VulkanFrame<'_, '_> = multi.as_mut();
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
                    warn!("gradient-fade texture arm does not match the TTY renderer variant");
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
                    warn!("GLES gradient-fade texture on the multi-GPU TTY renderer");
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
