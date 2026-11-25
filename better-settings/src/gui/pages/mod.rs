//! The control-center pages.

use std::rc::Rc;

use gtk4 as gtk;

use crate::gui::App;

pub mod about;
pub mod audio;
pub mod automation;
pub mod compositor;
pub mod displays;
pub mod overview;
pub mod services;
pub mod sessions;
pub mod sidepanels;
pub mod wallpaper;

/// Every page in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Compositor,
    Displays,
    Sessions,
    Wallpaper,
    Audio,
    Sidepanels,
    Automation,
    Services,
    About,
}

impl Page {
    pub const ALL: [Page; 10] = [
        Page::Overview,
        Page::Compositor,
        Page::Displays,
        Page::Sessions,
        Page::Wallpaper,
        Page::Audio,
        Page::Sidepanels,
        Page::Automation,
        Page::Services,
        Page::About,
    ];

    /// Stable stack/sidebar name.
    pub fn name(self) -> &'static str {
        match self {
            Page::Overview => "overview",
            Page::Compositor => "compositor",
            Page::Displays => "displays",
            Page::Sessions => "sessions",
            Page::Wallpaper => "wallpaper",
            Page::Audio => "audio",
            Page::Sidepanels => "sidepanels",
            Page::Automation => "automation",
            Page::Services => "services",
            Page::About => "about",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|page| page.name() == name)
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Compositor => "Compositor",
            Page::Displays => "Displays",
            Page::Sessions => "Sessions",
            Page::Wallpaper => "Wallpaper",
            Page::Audio => "Audio",
            Page::Sidepanels => "Side panels",
            Page::Automation => "Automation",
            Page::Services => "Services",
            Page::About => "About",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Page::Overview => "go-home-symbolic",
            Page::Compositor => "preferences-system-symbolic",
            Page::Displays => "video-display-symbolic",
            Page::Sessions => "view-restore-symbolic",
            Page::Wallpaper => "preferences-desktop-wallpaper-symbolic",
            Page::Audio => "audio-volume-high-symbolic",
            Page::Sidepanels => "sidebar-show-symbolic",
            Page::Automation => "emblem-system-symbolic",
            Page::Services => "emblem-shared-symbolic",
            Page::About => "help-about-symbolic",
        }
    }

    pub fn search_terms(self) -> &'static str {
        match self {
            Page::Overview => "overview compositor status summary quick actions validate",
            Page::Compositor => {
                "compositor layout input outputs binds rules startup animations cursor"
            }
            Page::Displays => "displays monitors profiles arrangement hdr outputs",
            Page::Sessions => "sessions contexts restore capture history",
            Page::Wallpaper => "wallpaper scenes apply workspace",
            Page::Audio => "audio sinks sources mixer volume default rules",
            Page::Sidepanels => "side panels layers layouts config validate",
            Page::Automation => "automation companion idle rotate rules marks history",
            Page::Services => "services tools portal noctalia diagnostics install",
            Page::About => "about versions docs links help",
        }
    }
}

/// Builds one page; the caller owns the returned widget.
pub fn build(app: &Rc<App>, page: Page) -> gtk::Widget {
    match page {
        Page::Overview => overview::build(app),
        Page::Compositor => compositor::build(app),
        Page::Displays => displays::build(app),
        Page::Sessions => sessions::build(app),
        Page::Wallpaper => wallpaper::build(app),
        Page::Audio => audio::build(app),
        Page::Sidepanels => sidepanels::build(app),
        Page::Automation => automation::build(app),
        Page::Services => services::build(app),
        Page::About => about::build(app),
    }
}
