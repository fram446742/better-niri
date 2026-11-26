use niri_config::BlockOutFrom;
use smithay::wayland::shell::wlr_layer::Layer;

use super::Fade;
use crate::render_helpers::RenderTarget;
use crate::render_helpers::offscreen::OffscreenRenderElement;

/// A layer surface that was unmapped and is now fading out.
///
/// The whole layer (surface, shadow, background effect) is baked into an offscreen texture, so the
/// fade covers effects that don't take an alpha multiplier themselves.
#[derive(Debug)]
pub struct ClosingLayer {
    /// The baked layer contents.
    element: OffscreenRenderElement,

    /// The layer-shell layer the surface was on, used to keep the fade at the right z-order.
    layer: Layer,

    /// Whether the surface was rendered in the overview backdrop pass.
    for_backdrop: bool,

    /// Where the surface was blocked out from, to keep it out of the same targets while fading.
    block_out_from: Option<BlockOutFrom>,

    /// Fade from 1 to 0.
    fade: Fade,
}

impl ClosingLayer {
    pub(crate) fn new(
        element: OffscreenRenderElement,
        layer: Layer,
        for_backdrop: bool,
        block_out_from: Option<BlockOutFrom>,
        fade: Fade,
    ) -> Self {
        Self {
            element,
            layer,
            for_backdrop,
            block_out_from,
            fade,
        }
    }

    pub fn layer(&self) -> Layer {
        self.layer
    }

    pub fn for_backdrop(&self) -> bool {
        self.for_backdrop
    }

    pub fn is_ongoing(&self) -> bool {
        self.fade.is_ongoing()
    }

    /// The current frame's element, or `None` when this target must not show the layer.
    pub fn render_element(&self, target: RenderTarget) -> Option<OffscreenRenderElement> {
        // The snapshot holds normal contents only. Drawing it in a target the layer was blocked
        // out from would leak the layer there, so skip those targets for the whole fade.
        if target.should_block_out(self.block_out_from) {
            return None;
        }

        Some(self.element.clone().with_alpha(self.fade.alpha()))
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use std::time::Duration;

    use niri_config::animations::{Curve, EasingParams, Kind};
    use niri_config::{BlockOutFrom, BlockOutKind, BlockOutTarget};
    use smithay::backend::renderer::element::{Element as _, Kind as ElementKind};
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Point, Scale};

    use super::*;
    use crate::animation::Clock;
    use crate::render_helpers::RenderTarget;
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

    /// A closing layer must bake into a real texture, fade with the clock, and stay out of targets
    /// the layer was blocked out from.
    #[test]
    fn vulkan_closing_layer_fades_and_respects_block_out() {
        for_each_vulkan_renderer(|renderer| {
            let buffer = OffscreenBuffer::default();
            let color = SolidColorBuffer::new((4., 4.), [1., 0., 0., 1.]);
            let elem = SolidColorRenderElement::from_buffer(
                &color,
                Point::from((0., 0.)),
                1.,
                ElementKind::Unspecified,
            );
            let (baked, _sync, _data) = buffer
                .render(renderer, Scale::from(1.), &[elem])
                .expect("bake the closing layer snapshot");
            assert!(baked.texture_vulkan().is_some());

            let mut clock = Clock::with_time(Duration::ZERO);
            let config = niri_config::Animation {
                off: false,
                kind: Kind::Easing(EasingParams {
                    duration_ms: 100,
                    curve: Curve::Linear,
                }),
            };
            let fade = Fade::new(clock.clone(), 1., 0., config).unwrap();
            let closing = ClosingLayer::new(
                baked,
                Layer::Top,
                false,
                Some(BlockOutFrom {
                    target: BlockOutTarget::Screencast,
                    kind: BlockOutKind::Fill,
                }),
                fade,
            );

            let elem = closing.render_element(RenderTarget::Output).unwrap();
            assert_eq!(elem.alpha(), 1.);
            assert!(
                closing.render_element(RenderTarget::Screencast).is_none(),
                "a layer blocked out from screencasts must not fade into them"
            );

            clock.set_unadjusted(Duration::from_millis(50));
            let mid = closing
                .render_element(RenderTarget::Output)
                .unwrap()
                .alpha();
            assert!(mid > 0. && mid < 1., "unexpected mid-fade alpha {mid}");

            clock.set_unadjusted(Duration::from_millis(100));
            assert_eq!(
                closing
                    .render_element(RenderTarget::Output)
                    .unwrap()
                    .alpha(),
                0.
            );
            assert!(!closing.is_ongoing());
        });
    }
}
