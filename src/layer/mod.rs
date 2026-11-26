use niri_config::layer_rule::{LayerRule, Match};
use niri_config::utils::MergeWith as _;
use niri_config::{BackgroundEffect, BlockOutFrom, CornerRadius, ResolvedPopupsRules, ShadowRule};
use smithay::desktop::LayerSurface;
use smithay::wayland::shell::wlr_layer::Layer;

use crate::animation::{Animation, Clock};

pub mod closing;
pub mod mapped;
pub use closing::ClosingLayer;
pub use mapped::MappedLayer;

/// A time-driven alpha fade shared by layer open and close animations.
#[derive(Debug)]
pub(crate) struct Fade {
    anim: Animation,
}

impl Fade {
    /// Creates a fade from `from` to `to`, or `None` when it is disabled and would do nothing.
    pub fn new(clock: Clock, from: f64, to: f64, config: niri_config::Animation) -> Option<Self> {
        if config.off || clock.should_complete_instantly() {
            return None;
        }

        Some(Self {
            anim: Animation::new(clock, from, to, 0., config),
        })
    }

    pub fn alpha(&self) -> f32 {
        self.anim.clamped_value() as f32
    }

    pub fn is_ongoing(&self) -> bool {
        !self.anim.is_done()
    }
}

/// Rules fully resolved for a layer-shell surface.
#[derive(Debug, Default, PartialEq)]
pub struct ResolvedLayerRules {
    /// Extra opacity to draw this layer surface with.
    pub opacity: Option<f32>,

    /// Whether to block out this layer surface from certain render targets.
    pub block_out_from: Option<BlockOutFrom>,

    /// Shadow overrides.
    pub shadow: ShadowRule,

    /// Corner radius to assume this layer surface has.
    pub geometry_corner_radius: Option<CornerRadius>,

    /// Whether to place this layer surface within the overview backdrop.
    pub place_within_backdrop: bool,

    /// Whether to focus this layer surface when typing in the overview.
    pub focus_on_type_in_overview: bool,

    /// Whether to bob this window up and down.
    pub baba_is_float: bool,

    /// Background effect configuration.
    pub background_effect: BackgroundEffect,

    /// Rules for this layer surface's popups.
    pub popups: ResolvedPopupsRules,
}

impl ResolvedLayerRules {
    pub fn compute(rules: &[LayerRule], surface: &LayerSurface, is_at_startup: bool) -> Self {
        let _span = tracy_client::span!("ResolvedLayerRules::compute");

        let mut resolved = ResolvedLayerRules::default();

        for rule in rules {
            let matches = |m: &Match| {
                if let Some(at_startup) = m.at_startup
                    && at_startup != is_at_startup
                {
                    return false;
                }

                surface_matches(surface, m)
            };

            if !(rule.matches.is_empty() || rule.matches.iter().any(matches)) {
                continue;
            }

            if rule.excludes.iter().any(matches) {
                continue;
            }

            if let Some(x) = rule.opacity {
                resolved.opacity = Some(x);
            }
            if let Some(x) = rule.block_out_from {
                resolved.block_out_from = Some(x);
            }
            if let Some(x) = rule.geometry_corner_radius {
                resolved.geometry_corner_radius = Some(x);
            }
            if let Some(x) = rule.place_within_backdrop {
                resolved.place_within_backdrop = x;
            }
            if let Some(x) = rule.focus_on_type_in_overview {
                resolved.focus_on_type_in_overview = x;
            }
            if let Some(x) = rule.baba_is_float {
                resolved.baba_is_float = x;
            }

            resolved.shadow.merge_with(&rule.shadow);

            resolved
                .background_effect
                .merge_with(&rule.background_effect);

            resolved.popups.merge_with(&rule.popups);
        }

        resolved
    }
}

fn surface_matches(surface: &LayerSurface, m: &Match) -> bool {
    if let Some(namespace_re) = &m.namespace
        && !namespace_re.0.is_match(surface.namespace())
    {
        return false;
    }

    if let Some(layer) = m.layer {
        let surface_layer = match surface.layer() {
            Layer::Background => niri_ipc::Layer::Background,
            Layer::Bottom => niri_ipc::Layer::Bottom,
            Layer::Top => niri_ipc::Layer::Top,
            Layer::Overlay => niri_ipc::Layer::Overlay,
        };
        if layer != surface_layer {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use niri_config::animations::{Curve, EasingParams, Kind};

    use super::*;

    fn fade_config(duration_ms: u32) -> niri_config::Animation {
        niri_config::Animation {
            off: false,
            kind: Kind::Easing(EasingParams {
                duration_ms,
                curve: Curve::Linear,
            }),
        }
    }

    #[test]
    fn fade_progresses_and_finishes() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let fade = Fade::new(clock.clone(), 0., 1., fade_config(100)).unwrap();

        assert_eq!(fade.alpha(), 0.);
        assert!(fade.is_ongoing());

        clock.set_unadjusted(Duration::from_millis(50));
        let mid = fade.alpha();
        assert!(mid > 0. && mid < 1., "unexpected mid-fade alpha {mid}");

        clock.set_unadjusted(Duration::from_millis(100));
        assert_eq!(fade.alpha(), 1.);
        assert!(!fade.is_ongoing());
    }

    #[test]
    fn fade_is_skipped_when_disabled() {
        let clock = Clock::with_time(Duration::ZERO);
        let mut disabled = fade_config(100);
        disabled.off = true;
        assert!(Fade::new(clock.clone(), 1., 0., disabled).is_none());

        let mut instant = Clock::with_time(Duration::ZERO);
        instant.set_complete_instantly(true);
        assert!(Fade::new(instant, 1., 0., fade_config(100)).is_none());
    }

    #[test]
    fn finished_fades_are_removed_from_the_closing_list() {
        let mut clock = Clock::with_time(Duration::ZERO);
        // Mirrors the per-output retain in Niri::advance_animations.
        let mut closing = vec![
            Fade::new(clock.clone(), 1., 0., fade_config(100)).unwrap(),
            Fade::new(clock.clone(), 1., 0., fade_config(10)).unwrap(),
        ];

        clock.set_unadjusted(Duration::from_millis(50));
        closing.retain(Fade::is_ongoing);
        assert_eq!(closing.len(), 1);

        clock.set_unadjusted(Duration::from_millis(101));
        closing.retain(Fade::is_ongoing);
        assert!(closing.is_empty());
    }
}
