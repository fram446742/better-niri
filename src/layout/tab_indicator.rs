use std::cell::RefCell;
use std::cmp::min;
use std::iter::zip;
use std::mem;

use anyhow::ensure;
use niri_config::{CornerRadius, Gradient, GradientRelativeTo, TabIndicatorPosition};
use pango::FontDescription;
use pango::glib::property::PropertySet;
use pangocairo::cairo::{self, ImageSurface};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

use super::LayoutElement;
use super::tile::Tile;
use crate::animation::{Animation, Clock};
use crate::niri_render_elements;
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::{
    floor_logical_in_physical_max1, round_logical_in_physical, round_logical_in_physical_max1,
    to_physical_precise_round,
};

/// Smallest horizontal distance a tab title keeps from the tab's edges.
const MIN_DIST_TO_EDGES: f64 = 20.;

/// Distance between a tab title and the tab bar.
const GAP_TO_BAR: f64 = 2.;

#[derive(Debug)]
pub struct TabIndicator {
    shader_locs: Vec<Point<f64, Logical>>,
    shaders: Vec<BorderRenderElement>,
    open_anim: Option<Animation>,
    /// The tabs as of the last update, for drawing titles.
    tabs: Vec<TabInfo>,
    /// A rendered title texture per tab, rebuilt when its title or geometry changes.
    title_textures: Vec<TitleTexture>,
    config: niri_config::TabIndicator,
    /// Opacity of the whole indicator.
    alpha: f32,
    /// The rectangle the tabs cover, as of the last update, in the coordinates of the area.
    bounds: Option<Rectangle<f64, Logical>>,
    /// Where the tabs were drawn on the last update, relative to the area.
    shown_rects: Vec<Rectangle<f64, Logical>>,
    /// The tabs moving to their new places after the tab count changed, from where they were.
    count_anim: Option<(Vec<Rectangle<f64, Logical>>, Animation)>,
    /// Clock and timing for `count_anim`, if the tab count changes animate.
    count_anim_config: Option<(Clock, niri_config::Animation)>,
    /// Whether `open_anim` runs backwards, folding the indicator away.
    closing: bool,
}

#[derive(Debug, Clone)]
pub struct TabInfo {
    /// Gradient for the tab indicator.
    pub gradient: Gradient,
    /// Tab geometry in the same coordinate system as the area.
    pub geometry: Rectangle<f64, Logical>,
    /// Title of the window the tab stands for.
    pub title: String,
}

niri_render_elements! {
    TabIndicatorRenderElement => {
        Gradient = BorderRenderElement,
        Title = PrimaryGpuTextureRenderElement,
    }
}

/// A tab title rendered into a texture, cached between frames.
#[derive(Debug, Default)]
struct TitleTexture {
    title: String,
    scale: f64,
    max_size: Size<f64, Logical>,
    texture: RefCell<
        Option<crate::render_helpers::texture::TextureBuffer<crate::backend::tty::TtyOffscreen>>,
    >,
    /// The size the text wants when given unlimited space.
    wanted_size: RefCell<Option<Size<i32, Physical>>>,
    font_size: u32,
}

impl TabIndicator {
    pub fn new(config: niri_config::TabIndicator) -> Self {
        Self {
            shader_locs: Vec::new(),
            shaders: Vec::new(),
            open_anim: None,
            tabs: Vec::new(),
            title_textures: Vec::new(),
            config,
            alpha: 1.,
            bounds: None,
            shown_rects: Vec::new(),
            count_anim: None,
            count_anim_config: None,
            closing: false,
        }
    }

    /// Animates the tabs to their new places when the tab count changes, with this timing.
    pub fn set_count_animation(&mut self, clock: Clock, config: niri_config::Animation) {
        self.count_anim_config = Some((clock, config));
    }

    /// The rectangle the tabs cover, as of the last update.
    pub fn bounds(&self) -> Option<Rectangle<f64, Logical>> {
        self.bounds
    }

    pub fn update_config(&mut self, config: niri_config::TabIndicator) {
        self.config = config;
    }

    /// Sets the opacity of the whole indicator, applied on the next update.
    pub fn set_alpha(&mut self, alpha: f32) {
        self.alpha = alpha;
    }

    pub fn update_shaders(&mut self) {
        for elem in &mut self.shaders {
            elem.damage_all();
        }
    }

    pub fn advance_animations(&mut self) {
        if let Some(anim) = &mut self.open_anim {
            // A folded-away indicator stays folded.
            if anim.is_done() && !self.closing {
                self.open_anim = None;
            }
        }
        if self
            .count_anim
            .as_ref()
            .is_some_and(|(_, anim)| anim.is_done())
        {
            self.count_anim = None;
        }
    }

    pub fn are_animations_ongoing(&self) -> bool {
        self.open_anim
            .as_ref()
            .is_some_and(|anim| !self.closing || !anim.is_done())
            || self.count_anim.is_some()
    }

    pub fn start_open_animation(&mut self, clock: Clock, config: niri_config::Animation) {
        // Unfolding one that was folding away starts from where it got to.
        let from = if self.closing {
            self.open_anim.as_ref().map_or(0., |anim| anim.value())
        } else {
            0.
        };
        self.closing = false;
        self.open_anim = Some(Animation::new(clock, from, 1., 0., config));
    }

    /// Folds the indicator away, the opening animation backwards.
    pub fn start_close_animation(&mut self, clock: Clock, config: niri_config::Animation) {
        if self.closing {
            return;
        }
        let from = self.open_anim.as_ref().map_or(1., |anim| anim.value());
        self.closing = true;
        self.open_anim = Some(Animation::new(clock, from, 0., 0., config));
    }

    /// Whether the indicator finished folding away.
    pub fn is_closed(&self) -> bool {
        self.closing && self.open_anim.as_ref().is_none_or(|anim| anim.is_done())
    }

    fn tab_rects(
        &self,
        area: Rectangle<f64, Logical>,
        count: usize,
        scale: f64,
    ) -> impl Iterator<Item = Rectangle<f64, Logical>> {
        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let round_max1 = |logical: f64| round_logical_in_physical_max1(scale, logical);

        let progress = self.open_anim.as_ref().map_or(1., |a| a.value().max(0.));

        let width = round_max1(self.config.width);
        let gap = self.config.gap;
        let gap = round_max1(gap.abs()).copysign(gap);
        let gaps_between = round_max1(self.config.gaps_between_tabs);

        let position = self.config.position;
        let side = match position {
            TabIndicatorPosition::Left | TabIndicatorPosition::Right => area.size.h,
            TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => area.size.w,
        };
        let total_prop = self.config.length.total_proportion.unwrap_or(0.5);
        let min_length = round(side * total_prop.clamp(0., 2.));

        // Compute px_per_tab before applying the animation to gaps_between in order to avoid it
        // growing and shrinking over the duration of the animation.
        let pixel = 1. / scale;
        let shortest_length = count as f64 * (pixel + gaps_between) - gaps_between;
        let length = f64::max(min_length, shortest_length);
        let px_per_tab = (length + gaps_between) / count as f64 - gaps_between;

        let px_per_tab = px_per_tab * progress;
        let gaps_between = round(self.config.gaps_between_tabs * progress);

        let length = count as f64 * (px_per_tab + gaps_between) - gaps_between;
        let px_per_tab = floor_logical_in_physical_max1(scale, px_per_tab);
        let floored_length = count as f64 * (px_per_tab + gaps_between) - gaps_between;
        let mut ones_left = ((length - floored_length) / pixel).round() as usize;

        let mut shader_loc = Point::from((-gap - width, round((side - length) / 2.)));
        match position {
            TabIndicatorPosition::Left => (),
            TabIndicatorPosition::Right => shader_loc.x = area.size.w + gap,
            TabIndicatorPosition::Top => mem::swap(&mut shader_loc.x, &mut shader_loc.y),
            TabIndicatorPosition::Bottom => {
                shader_loc.x = shader_loc.y;
                shader_loc.y = area.size.h + gap;
            }
        }
        shader_loc += area.loc;

        (0..count).map(move |_| {
            let mut px_per_tab = px_per_tab;
            if ones_left > 0 {
                ones_left -= 1;
                px_per_tab += pixel;
            }

            let loc = shader_loc;

            match position {
                TabIndicatorPosition::Left | TabIndicatorPosition::Right => {
                    shader_loc.y += px_per_tab + gaps_between
                }
                TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => {
                    shader_loc.x += px_per_tab + gaps_between
                }
            }

            let size = match position {
                TabIndicatorPosition::Left | TabIndicatorPosition::Right => {
                    Size::from((width, px_per_tab))
                }
                TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => {
                    Size::from((px_per_tab, width))
                }
            };

            Rectangle::new(loc, size)
        })
    }

    pub fn update_render_elements(
        &mut self,
        enabled: bool,
        // Geometry of the tabs area.
        area: Rectangle<f64, Logical>,
        // View rect relative to the tabs area.
        area_view_rect: Rectangle<f64, Logical>,
        tabs: impl Iterator<Item = TabInfo>,
        is_active: bool,
        scale: f64,
    ) {
        self.tabs = tabs.collect();
        self.bounds = None;
        if !enabled || self.config.off {
            self.tabs.clear();
            self.title_textures.clear();
            self.shader_locs.clear();
            self.shaders.clear();
            self.shown_rects.clear();
            self.count_anim = None;
            return;
        }

        let count = self.tabs.len();
        if self.config.hide_when_single_tab && count == 1 {
            self.title_textures.clear();
            self.shader_locs.clear();
            self.shaders.clear();
            self.shown_rects.clear();
            self.count_anim = None;
            return;
        }

        self.shaders.resize_with(count, Default::default);
        self.shader_locs.resize_with(count, Default::default);

        let position = self.config.position;
        let radius = self.config.corner_radius as f32;
        let shared_rounded_corners = self.config.gaps_between_tabs == 0.;
        let mut tabs_left = count;

        let rects = self.animated_tab_rects(area, count, scale);
        self.bounds = rects.iter().copied().reduce(|a, b| a.merge(b));

        // Keep one title texture per tab, re-rendering only the ones whose text or space changed.
        self.title_textures
            .resize_with(count, TitleTexture::default);
        for ((tex, tab), rect) in zip(zip(&mut self.title_textures, &self.tabs), rects.iter()) {
            tex.update_config(
                Some(tab.title.clone()),
                Some(scale),
                Some(Size::new((rect.size.w - MIN_DIST_TO_EDGES).max(0.), 16384.)),
                Some(self.config.title_font_size),
            );
        }

        for ((shader, loc), (tab, rect)) in zip(
            zip(&mut self.shaders, &mut self.shader_locs),
            zip(&self.tabs, rects),
        ) {
            *loc = rect.loc;

            let mut gradient_area = match tab.gradient.relative_to {
                GradientRelativeTo::Window => tab.geometry,
                GradientRelativeTo::WorkspaceView => area_view_rect,
            };
            gradient_area.loc -= *loc;

            let mut color_from = tab.gradient.from;
            let mut color_to = tab.gradient.to;
            if !is_active {
                color_from *= 0.5;
                color_to *= 0.5;
            }

            let radius = if shared_rounded_corners && count > 1 {
                if tabs_left == count {
                    // First tab.
                    match position {
                        TabIndicatorPosition::Left | TabIndicatorPosition::Right => CornerRadius {
                            top_left: radius,
                            top_right: radius,
                            bottom_right: 0.,
                            bottom_left: 0.,
                        },
                        TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => CornerRadius {
                            top_left: radius,
                            top_right: 0.,
                            bottom_right: 0.,
                            bottom_left: radius,
                        },
                    }
                } else if tabs_left == 1 {
                    // Last tab.
                    match position {
                        TabIndicatorPosition::Left | TabIndicatorPosition::Right => CornerRadius {
                            top_left: 0.,
                            top_right: 0.,
                            bottom_right: radius,
                            bottom_left: radius,
                        },
                        TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => CornerRadius {
                            top_left: 0.,
                            top_right: radius,
                            bottom_right: radius,
                            bottom_left: 0.,
                        },
                    }
                } else {
                    // Tab in the middle.
                    CornerRadius::default()
                }
            } else {
                // Separate tabs, or the only tab.
                CornerRadius::from(radius)
            };
            let radius = radius.fit_to(rect.size.w as f32, rect.size.h as f32);
            tabs_left -= 1;

            shader.update(
                rect.size,
                gradient_area,
                tab.gradient.in_,
                color_from,
                color_to,
                ((tab.gradient.angle as f32) - 90.).to_radians(),
                Rectangle::from_size(rect.size),
                0.,
                radius,
                scale as f32,
                self.alpha,
            );
        }
    }

    /// The tab rectangles, moving from where they were when the tab count changed.
    ///
    /// The indicator only knows the count, not which tab came or went: a new tab grows in at the
    /// end, and when one goes, the rest stretch to fill its place.
    fn animated_tab_rects(
        &mut self,
        area: Rectangle<f64, Logical>,
        count: usize,
        scale: f64,
    ) -> Vec<Rectangle<f64, Logical>> {
        let target: Vec<_> = self
            .tab_rects(area, count, scale)
            .map(|rect| Rectangle::new(rect.loc - area.loc, rect.size))
            .collect();

        let prev = &self.shown_rects;
        if !prev.is_empty()
            && prev.len() != count
            && let Some((clock, config)) = &self.count_anim_config
        {
            let mut from = prev.clone();
            if count > from.len() {
                // New tabs start out empty at the end of the last one.
                let last = *from.last().unwrap();
                let (loc, size) = match self.config.position {
                    TabIndicatorPosition::Left | TabIndicatorPosition::Right => (
                        Point::from((last.loc.x, last.loc.y + last.size.h)),
                        Size::from((last.size.w, 0.)),
                    ),
                    TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => (
                        Point::from((last.loc.x + last.size.w, last.loc.y)),
                        Size::from((0., last.size.h)),
                    ),
                };
                from.resize(count, Rectangle::new(loc, size));
            } else {
                from.truncate(count);
            }
            let anim = Animation::new(clock.clone(), 0., 1., 0., *config);
            self.count_anim = Some((from, anim));
        }

        let rects = match &self.count_anim {
            Some((from, anim)) if from.len() == count => {
                let t = anim.value();
                let lerp = |a: f64, b: f64| a + (b - a) * t;
                zip(from, &target)
                    .map(|(a, b)| {
                        Rectangle::new(
                            Point::from((lerp(a.loc.x, b.loc.x), lerp(a.loc.y, b.loc.y))),
                            Size::from((
                                lerp(a.size.w, b.size.w).max(0.),
                                lerp(a.size.h, b.size.h).max(0.),
                            )),
                        )
                    })
                    .collect()
            }
            _ => {
                self.count_anim = None;
                target
            }
        };

        self.shown_rects = rects.clone();
        rects
            .into_iter()
            .map(|rect| Rectangle::new(rect.loc + area.loc, rect.size))
            .collect()
    }

    pub fn hit(
        &self,
        area: Rectangle<f64, Logical>,
        tab_count: usize,
        scale: f64,
        point: Point<f64, Logical>,
    ) -> Option<usize> {
        if self.config.off {
            return None;
        }

        let count = tab_count;
        if self.config.hide_when_single_tab && count == 1 {
            return None;
        }

        let font_height = self.font_height();

        self.tab_rects(area, count, scale)
            .map(|mut rect| {
                // The title above (or below) the bar is part of the tab.
                if font_height > 0. {
                    match self.config.position {
                        TabIndicatorPosition::Top => {
                            rect.size.h += font_height + GAP_TO_BAR;
                        }
                        TabIndicatorPosition::Bottom => {
                            rect.loc.y -= font_height + GAP_TO_BAR;
                            rect.size.h += font_height + GAP_TO_BAR;
                        }
                        TabIndicatorPosition::Left | TabIndicatorPosition::Right => (),
                    }
                }
                rect
            })
            .enumerate()
            .find_map(|(idx, rect)| rect.contains(point).then_some(idx))
    }

    /// Height taken by the tab titles, zero when they are off or not shown for this position.
    fn font_height(&self) -> f64 {
        if self.config.hide_titles
            || matches!(
                self.config.position,
                TabIndicatorPosition::Left | TabIndicatorPosition::Right
            )
        {
            return 0.;
        }

        // Before the first textures are rendered, estimate with the configured font size, since
        // the tile resize animation starts right away.
        self.title_textures
            .iter()
            .fold(self.config.title_font_size as f64, |acc, tex| {
                if let Some(texture) = tex.texture.borrow().as_ref() {
                    texture.logical_size().h.max(acc)
                } else {
                    acc
                }
            })
    }

    pub fn render(
        &self,
        renderer: &mut impl NiriRenderer,
        pos: Point<f64, Logical>,
        push: &mut dyn FnMut(TabIndicatorRenderElement),
    ) {
        let has_border_shader = BorderRenderElement::has_shader(renderer);
        if !has_border_shader {
            return;
        }

        let font_height = self.font_height();

        if font_height > 0. {
            for (tex, loc) in zip(&self.title_textures, &self.shader_locs) {
                let Ok(texture) = tex.get(renderer) else {
                    // A missing title is not worth failing the frame over.
                    continue;
                };
                let title_pos = Point::new(
                    (tex.max_size.w + MIN_DIST_TO_EDGES) / 2. - texture.logical_size().w / 2.,
                    match self.config.position {
                        TabIndicatorPosition::Top => self.config.width + GAP_TO_BAR,
                        _ => -texture.logical_size().h - GAP_TO_BAR,
                    },
                );
                push(
                    PrimaryGpuTextureRenderElement(TextureRenderElement::from_texture_buffer(
                        texture,
                        pos + *loc + title_pos,
                        1.,
                        None,
                        None,
                        Kind::Unspecified,
                    ))
                    .into(),
                );
            }
        }

        for (shader, loc) in zip(&self.shaders, &self.shader_locs) {
            let elem = shader.clone().with_location(pos + *loc);
            push(TabIndicatorRenderElement::from(elem));
        }
    }

    /// Extra size occupied by the tab indicator when it is placed within a column.
    pub fn extra_size(&self, tab_count: usize, scale: f64) -> Size<f64, Logical> {
        if self.config.off
            || !self.config.place_within_column
            || (self.config.hide_when_single_tab && tab_count == 1)
        {
            return Size::from((0., 0.));
        }

        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let width = round(self.config.width);
        let gap = round(self.config.gap);

        let size = f64::max(0., width + gap);

        match self.config.position {
            TabIndicatorPosition::Left | TabIndicatorPosition::Right => Size::from((size, 0.)),
            TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => Size::from((0., size)),
        }
    }

    /// Offset of the tabbed content due to space occupied by the tab indicator.
    pub fn content_offset(&self, tab_count: usize, scale: f64) -> Point<f64, Logical> {
        match self.config.position {
            TabIndicatorPosition::Left | TabIndicatorPosition::Top => {
                self.extra_size(tab_count, scale).to_point()
            }
            TabIndicatorPosition::Right | TabIndicatorPosition::Bottom => Point::from((0., 0.)),
        }
    }

    /// Extra size the tab indicator takes inside a tile: the bar plus the title band.
    ///
    /// Unlike [`TabIndicator::extra_size`], this never depends on `place_within_column`: a grouped
    /// tile always makes room for its tabs.
    pub fn extra_size_in_tile(&self, tab_count: usize, scale: f64) -> Size<f64, Logical> {
        if self.config.off || (self.config.hide_when_single_tab && tab_count == 1) {
            return Size::from((0., 0.));
        }

        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let width = round(self.config.width);
        let gap = round(self.config.gap);
        let title = if self.font_height() > 0. {
            round(self.font_height() + GAP_TO_BAR)
        } else {
            0.
        };

        // No, I am *not* falling into the rabbit hole of "what if the tab indicator is wide enough
        // that it peeks from the other side of the window".
        let size = f64::max(0., width + gap + title);

        match self.config.position {
            TabIndicatorPosition::Left | TabIndicatorPosition::Right => Size::from((size, 0.)),
            TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => Size::from((0., size)),
        }
    }

    /// Offset of a grouped tile's window due to the space its tab indicator takes.
    pub fn content_offset_in_tile(&self, tab_count: usize, scale: f64) -> Point<f64, Logical> {
        match self.config.position {
            TabIndicatorPosition::Top => self.extra_size_in_tile(tab_count, scale).to_point(),
            TabIndicatorPosition::Bottom
            | TabIndicatorPosition::Left
            | TabIndicatorPosition::Right => Point::from((0., 0.)),
        }
    }

    /// Area to lay the tabs out in for a grouped tile, given the tile's own size.
    ///
    /// The tab bars sit just outside this area, so this drops it into the band the tile reserves
    /// for the indicator.
    pub fn tile_tab_area(
        &self,
        tile_size: Size<f64, Logical>,
        tab_count: usize,
        scale: f64,
    ) -> Rectangle<f64, Logical> {
        let extra = self.extra_size_in_tile(tab_count, scale);
        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let width = round(self.config.width);
        let gap = round(self.config.gap);

        let loc = match self.config.position {
            TabIndicatorPosition::Top => Point::new(0., width + gap),
            TabIndicatorPosition::Bottom => Point::new(0., extra.h - width - gap),
            TabIndicatorPosition::Left => Point::new(width + gap, 0.),
            TabIndicatorPosition::Right => Point::new(extra.w - width - gap, 0.),
        };
        Rectangle::new(loc, tile_size)
    }

    pub fn config(&self) -> niri_config::TabIndicator {
        self.config
    }
}

impl TabInfo {
    pub fn from_tile<W: LayoutElement>(
        tile: &Tile<W>,
        position: Point<f64, Logical>,
        is_active: bool,
        is_urgent: bool,
        config: &niri_config::TabIndicator,
    ) -> Self {
        Self::from_window(
            tile.window(),
            tile.focus_ring().config(),
            tile.border().config(),
            position,
            tile.animated_tile_size(),
            is_active,
            is_urgent,
            config,
        )
    }

    /// Builds tab info for one window of a group, in the given tab area coordinate system.
    #[allow(clippy::too_many_arguments)]
    pub fn from_window<W: LayoutElement>(
        window: &W,
        focus_ring_config: &niri_config::FocusRing,
        border_config: &niri_config::FocusRing,
        position: Point<f64, Logical>,
        tile_size: Size<f64, Logical>,
        is_active: bool,
        is_urgent: bool,
        config: &niri_config::TabIndicator,
    ) -> Self {
        let rules = window.rules();
        let rule = rules.tab_indicator;

        let gradient_from_rule = || {
            let (color, gradient) = if is_urgent {
                (rule.urgent_color, rule.urgent_gradient)
            } else if is_active {
                (rule.active_color, rule.active_gradient)
            } else {
                (rule.inactive_color, rule.inactive_gradient)
            };
            let color = color.map(Gradient::from);
            gradient.or(color)
        };

        let gradient_from_config = || {
            let (color, gradient) = if is_urgent {
                (config.urgent_color, config.urgent_gradient)
            } else if is_active {
                (config.active_color, config.active_gradient)
            } else {
                (config.inactive_color, config.inactive_gradient)
            };
            let color = color.map(Gradient::from);
            gradient.or(color)
        };

        let gradient_from_border = || {
            // Come up with tab indicator gradient matching the focus ring or the border, whichever
            // one is enabled.
            let config = if focus_ring_config.off {
                border_config
            } else {
                focus_ring_config
            };

            let (color, gradient) = if is_urgent {
                (config.urgent_color, config.urgent_gradient)
            } else if is_active {
                (config.active_color, config.active_gradient)
            } else {
                (config.inactive_color, config.inactive_gradient)
            };
            gradient.unwrap_or_else(|| Gradient::from(color))
        };

        let gradient = gradient_from_rule()
            .or_else(gradient_from_config)
            .unwrap_or_else(gradient_from_border);

        let geometry = Rectangle::new(position, tile_size);

        TabInfo {
            gradient,
            geometry,
            title: window.title().unwrap_or_default(),
        }
    }
}

impl TitleTexture {
    fn update_config(
        &mut self,
        new_title: Option<String>,
        new_scale: Option<f64>,
        new_max_size: Option<Size<f64, Logical>>,
        new_font_size: Option<u32>,
    ) {
        if let Some(new_font_size) = new_font_size {
            if new_font_size != self.font_size {
                self.texture.set(None);
                self.wanted_size.set(None);
            }
            self.font_size = new_font_size;
        }
        if let Some(new_title) = new_title {
            if new_title != self.title {
                self.texture.set(None);
                self.wanted_size.set(None);
            }
            self.title = new_title;
        }

        if let Some(new_scale) = new_scale {
            if new_scale != self.scale {
                self.texture.set(None);
                self.wanted_size.set(None);
            }
            self.scale = new_scale;
        }

        if let Some(new_max_size) = new_max_size {
            if new_max_size != self.max_size {
                // Only re-render when the current texture no longer fits or could grow into the
                // new space; otherwise the existing one is still correct.
                let should_dirty = if let (Some(texture), Some(wanted_size)) = (
                    self.texture.borrow().as_ref(),
                    self.wanted_size.borrow().as_ref(),
                ) {
                    let wanted_size = wanted_size.to_f64().to_logical(self.scale);
                    let tex_size = texture.logical_size();
                    (wanted_size.w >= self.max_size.w && new_max_size.w > self.max_size.w)
                        || (wanted_size.h >= self.max_size.h && new_max_size.h > self.max_size.h)
                        || (tex_size.w > new_max_size.w)
                        || (tex_size.h > new_max_size.h)
                } else {
                    true
                };

                if should_dirty {
                    self.texture.set(None);
                    self.wanted_size.set(None);
                }
            }
            self.max_size = new_max_size;
        }
    }

    fn get<R: NiriRenderer>(
        &self,
        renderer: &mut R,
    ) -> anyhow::Result<
        crate::render_helpers::texture::TextureBuffer<crate::backend::tty::TtyOffscreen>,
    > {
        let mut tex = self.texture.borrow_mut();

        if self.title.is_empty() {
            return Err(anyhow::anyhow!(
                "cannot render title texture if title is empty"
            ));
        }

        match &*tex {
            Some(texture) => Ok(texture.clone()),
            None => {
                let (new_tex, wanted_size) = render_title_texture(
                    renderer,
                    &self.title,
                    self.scale,
                    self.max_size,
                    self.font_size,
                )?;
                *tex = Some(new_tex.clone());
                self.wanted_size.set(Some(wanted_size));
                Ok(new_tex)
            }
        }
    }
}

fn render_title_texture<R: NiriRenderer>(
    renderer: &mut R,
    title: &str,
    scale: f64,
    max_size: Size<f64, Logical>,
    font_size: u32,
) -> anyhow::Result<(
    crate::render_helpers::texture::TextureBuffer<crate::backend::tty::TtyOffscreen>,
    Size<i32, Physical>,
)> {
    let _span = tracy_client::span!("tab_indicator::render_title_texture");

    let mut font = FontDescription::from_string(&format!("sans {font_size}px"));
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);

    layout.set_single_paragraph_mode(true);
    layout.set_font_description(Some(&font));
    layout.set_text(title);

    let (width, height) = layout.pixel_size();
    let wanted_size = Size::new(width, height);

    // Guard against overly long window titles.
    let max_size = max_size.to_physical_precise_round(scale);
    let width = min(width, max_size.w);
    let height = min(height, max_size.h);

    ensure!(width > 0 && height > 0);

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    cr.set_source_rgb(1., 1., 1.);
    pangocairo::functions::show_layout(&cr, &layout);

    drop(cr);

    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;

    Ok((buffer.map_texture(R::wrap_texture), wanted_size))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use niri_config::Color;

    use super::*;

    fn tabs(count: usize) -> impl Iterator<Item = TabInfo> {
        (0..count).map(|_| TabInfo {
            gradient: Gradient::from(Color::new_unpremul(1., 1., 1., 1.)),
            geometry: Rectangle::from_size(Size::from((100., 100.))),
            title: String::new(),
        })
    }

    fn update(indicator: &mut TabIndicator, count: usize) {
        let area = Rectangle::from_size(Size::from((100., 400.)));
        indicator.update_render_elements(true, area, area, tabs(count), true, 1.);
    }

    #[test]
    fn tab_count_change_grows_new_tab_in() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let mut indicator = TabIndicator::new(niri_config::TabIndicator::default());
        let config = niri_config::Animation {
            off: false,
            kind: niri_config::animations::Kind::Easing(niri_config::animations::EasingParams {
                duration_ms: 100,
                curve: niri_config::animations::Curve::Linear,
            }),
        };
        indicator.set_count_animation(clock.clone(), config);

        update(&mut indicator, 3);
        assert!(!indicator.are_animations_ongoing());
        let settled = indicator.bounds().unwrap();

        // A fourth tab starts out empty at the end, so the tabs still cover the same length.
        update(&mut indicator, 4);
        assert!(indicator.are_animations_ongoing());
        assert_eq!(indicator.shown_rects.len(), 4);
        assert_eq!(indicator.shown_rects[3].size.h, 0.);
        assert_eq!(indicator.bounds().unwrap(), settled);

        clock.set_unadjusted(Duration::from_millis(200));
        indicator.advance_animations();
        update(&mut indicator, 4);
        assert!(!indicator.are_animations_ongoing());
        assert!(indicator.shown_rects[3].size.h > 0.);
    }

    #[test]
    fn tab_count_change_is_instant_without_animation() {
        let mut indicator = TabIndicator::new(niri_config::TabIndicator::default());
        update(&mut indicator, 3);
        update(&mut indicator, 2);
        assert!(!indicator.are_animations_ongoing());
        assert_eq!(indicator.shown_rects.len(), 2);
    }

    #[test]
    fn group_tabs_reserve_a_title_band_above_the_window() {
        let config = niri_config::TabIndicator {
            position: TabIndicatorPosition::Top,
            ..Default::default()
        };
        let indicator = TabIndicator::new(config);

        let tile_size = Size::from((200., 100.));
        let extra = indicator.extra_size_in_tile(2, 1.);

        // Bar width, gap on both sides of it, and the font band (configured 12px + bar gap).
        assert_eq!(extra, Size::from((0., 4. + 5. + 12. + 2.)));
        assert_eq!(
            indicator.content_offset_in_tile(2, 1.),
            Point::from((0., extra.h)),
            "the window starts below the bar and its title"
        );

        // The bar itself sits at the very top of the tile.
        let area = indicator.tile_tab_area(tile_size, 2, 1.);
        assert_eq!(area.loc, Point::from((0., 4. + 5.)));
        assert_eq!(area.loc.y - 5. - 4., 0.);

        // Clicking the bar or the title selects the tab; the gap below it does not.
        // The bar length is half the tile (100 px) and is centered, so tabs span x 50..150.
        assert_eq!(indicator.hit(area, 2, 1., Point::from((60., 2.))), Some(0));
        assert_eq!(
            indicator.hit(area, 2, 1., Point::from((140., 10.))),
            Some(1)
        );
        assert_eq!(indicator.hit(area, 2, 1., Point::from((60., 22.))), None);
    }

    #[test]
    fn single_tab_group_can_hide_its_indicator() {
        let config = niri_config::TabIndicator {
            position: TabIndicatorPosition::Top,
            hide_when_single_tab: true,
            ..Default::default()
        };
        let indicator = TabIndicator::new(config);

        assert_eq!(
            indicator.extra_size_in_tile(1, 1.),
            Size::from((0., 0.)),
            "a group of one with hide-when-single-tab takes no room"
        );
        assert!(indicator.extra_size_in_tile(2, 1.).h > 0.);
    }
}
