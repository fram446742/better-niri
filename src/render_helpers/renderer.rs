use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer, GlesTexture};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{VulkanRenderer, VulkanTexture};
use smithay::backend::renderer::{
    Bind, ExportMem, ImportAll, ImportMem, Offscreen, Renderer, Texture,
};

use crate::backend::tty::{TtyFrame, TtyOffscreen, TtyRenderer};

/// Trait with our main renderer requirements to save on the typing.
pub trait NiriRenderer:
    ImportAll
    + ImportMem
    + ExportMem
    + Bind<Dmabuf>
    + Renderer<TextureId = Self::NiriTextureId, Error = Self::NiriError>
    + AsGlesRenderer
    + AsVulkanRenderer
    + HasOffscreen
    + Offscreen<<Self as HasOffscreen>::Offscreen>
    + Bind<<Self as HasOffscreen>::Offscreen>
{
    // Associated types to work around the instability of associated type bounds.
    type NiriTextureId: Texture + Clone + Send + 'static;
    type NiriError: std::error::Error + Send + Sync + 'static;
}

impl<R> NiriRenderer for R
where
    R: ImportAll
        + ImportMem
        + ExportMem
        + Bind<Dmabuf>
        + AsGlesRenderer
        + AsVulkanRenderer
        + HasOffscreen
        + Offscreen<<R as HasOffscreen>::Offscreen>
        + Bind<<R as HasOffscreen>::Offscreen>,
    R::TextureId: Texture + Clone + Send + 'static,
    R::Error: std::error::Error + Send + Sync + 'static,
{
    type NiriTextureId = R::TextureId;
    type NiriError = R::Error;
}

/// Trait for getting the underlying `GlesRenderer`, if any.
///
/// Returns `None` for the Vulkan renderer; GLES-specific functionality (custom
/// shaders, offscreen effects, elite visual modules) degrades gracefully in that
/// case.
pub trait AsGlesRenderer {
    fn as_gles_renderer(&mut self) -> Option<&mut GlesRenderer>;
}

impl AsGlesRenderer for GlesRenderer {
    fn as_gles_renderer(&mut self) -> Option<&mut GlesRenderer> {
        Some(self)
    }
}

#[cfg(feature = "vulkan")]
impl AsGlesRenderer for TtyRenderer<'_> {
    fn as_gles_renderer(&mut self) -> Option<&mut GlesRenderer> {
        match self {
            TtyRenderer::Gles(renderer) => Some(renderer.as_mut()),
            TtyRenderer::Vulkan(_) => None,
        }
    }
}

#[cfg(not(feature = "vulkan"))]
impl AsGlesRenderer for TtyRenderer<'_> {
    fn as_gles_renderer(&mut self) -> Option<&mut GlesRenderer> {
        Some(self.as_mut())
    }
}

#[cfg(feature = "vulkan")]
impl AsGlesRenderer for VulkanRenderer {
    fn as_gles_renderer(&mut self) -> Option<&mut GlesRenderer> {
        None
    }
}

/// Trait for getting the underlying `GlesFrame`, if any.
pub trait AsGlesFrame<'frame, 'buffer>
where
    Self: 'frame,
{
    fn as_gles_frame(&mut self) -> Option<&mut GlesFrame<'frame, 'buffer>>;
}

impl<'frame, 'buffer> AsGlesFrame<'frame, 'buffer> for GlesFrame<'frame, 'buffer> {
    fn as_gles_frame(&mut self) -> Option<&mut GlesFrame<'frame, 'buffer>> {
        Some(self)
    }
}

#[cfg(feature = "vulkan")]
impl<'frame, 'buffer> AsGlesFrame<'frame, 'buffer> for TtyFrame<'_, 'frame, 'buffer> {
    fn as_gles_frame(&mut self) -> Option<&mut GlesFrame<'frame, 'buffer>> {
        match self {
            TtyFrame::Gles(frame) => Some(frame.as_mut()),
            TtyFrame::Vulkan(_) => None,
        }
    }
}

#[cfg(not(feature = "vulkan"))]
impl<'frame, 'buffer> AsGlesFrame<'frame, 'buffer> for TtyFrame<'_, 'frame, 'buffer> {
    fn as_gles_frame(&mut self) -> Option<&mut GlesFrame<'frame, 'buffer>> {
        Some(self.as_mut())
    }
}

/// Trait for getting the underlying `VulkanRenderer`, if any.
#[cfg(feature = "vulkan")]
pub trait AsVulkanRenderer {
    fn as_vulkan_renderer(&mut self) -> Option<&mut VulkanRenderer>;
}

/// Marker for builds without the Vulkan feature, so shared bounds still compile.
#[cfg(not(feature = "vulkan"))]
pub trait AsVulkanRenderer {}

#[cfg(not(feature = "vulkan"))]
impl AsVulkanRenderer for GlesRenderer {}

#[cfg(not(feature = "vulkan"))]
impl AsVulkanRenderer for TtyRenderer<'_> {}

#[cfg(feature = "vulkan")]
impl AsVulkanRenderer for GlesRenderer {
    fn as_vulkan_renderer(&mut self) -> Option<&mut VulkanRenderer> {
        None
    }
}

#[cfg(feature = "vulkan")]
impl AsVulkanRenderer for TtyRenderer<'_> {
    fn as_vulkan_renderer(&mut self) -> Option<&mut VulkanRenderer> {
        match self {
            TtyRenderer::Gles(_) => None,
            TtyRenderer::Vulkan(renderer) => Some(renderer.as_mut()),
        }
    }
}

#[cfg(feature = "vulkan")]
impl AsVulkanRenderer for VulkanRenderer {
    fn as_vulkan_renderer(&mut self) -> Option<&mut VulkanRenderer> {
        Some(self)
    }
}

/// The offscreen render target texture type of a renderer.
pub trait HasOffscreen: Renderer {
    type Offscreen: Texture + Clone + Send + 'static;

    /// Wraps the renderer's offscreen texture into the renderer-agnostic enum.
    fn wrap_offscreen(texture: Self::Offscreen) -> TtyOffscreen;
    /// Unwraps the renderer-agnostic enum back into this renderer's texture type.
    fn unwrap_offscreen(texture: &mut TtyOffscreen) -> Option<&mut Self::Offscreen>;
    /// Wraps the renderer's surface texture into the renderer-agnostic enum.
    fn wrap_texture(texture: Self::TextureId) -> TtyOffscreen;
}

impl HasOffscreen for GlesRenderer {
    type Offscreen = GlesTexture;

    fn wrap_offscreen(texture: GlesTexture) -> TtyOffscreen {
        TtyOffscreen::Gles(texture)
    }

    fn unwrap_offscreen(texture: &mut TtyOffscreen) -> Option<&mut GlesTexture> {
        match texture {
            TtyOffscreen::Gles(texture) => Some(texture),
            _ => None,
        }
    }

    fn wrap_texture(texture: GlesTexture) -> TtyOffscreen {
        TtyOffscreen::Gles(texture)
    }
}

#[cfg(feature = "vulkan")]
impl HasOffscreen for TtyRenderer<'_> {
    type Offscreen = TtyOffscreen;

    fn wrap_offscreen(texture: TtyOffscreen) -> TtyOffscreen {
        texture
    }

    fn unwrap_offscreen(texture: &mut TtyOffscreen) -> Option<&mut TtyOffscreen> {
        Some(texture)
    }

    fn wrap_texture(texture: Self::TextureId) -> TtyOffscreen {
        TtyOffscreen::Multi(texture)
    }
}

#[cfg(not(feature = "vulkan"))]
impl HasOffscreen for TtyRenderer<'_> {
    type Offscreen = GlesTexture;

    fn wrap_offscreen(texture: GlesTexture) -> TtyOffscreen {
        TtyOffscreen::Gles(texture)
    }

    fn unwrap_offscreen(texture: &mut TtyOffscreen) -> Option<&mut GlesTexture> {
        match texture {
            TtyOffscreen::Gles(texture) => Some(texture),
            TtyOffscreen::Multi(_) => None,
        }
    }

    fn wrap_texture(texture: Self::TextureId) -> TtyOffscreen {
        TtyOffscreen::Multi(texture)
    }
}

#[cfg(feature = "vulkan")]
impl HasOffscreen for VulkanRenderer {
    type Offscreen = VulkanTexture;

    fn wrap_offscreen(texture: VulkanTexture) -> TtyOffscreen {
        TtyOffscreen::Vulkan(texture)
    }

    fn unwrap_offscreen(texture: &mut TtyOffscreen) -> Option<&mut VulkanTexture> {
        match texture {
            TtyOffscreen::Vulkan(texture) => Some(texture),
            _ => None,
        }
    }

    fn wrap_texture(texture: VulkanTexture) -> TtyOffscreen {
        TtyOffscreen::Vulkan(texture)
    }
}
