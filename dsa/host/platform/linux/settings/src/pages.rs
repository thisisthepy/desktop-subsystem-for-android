//! The pages of the app, in the order the sidebar lists them. They are the pages of the
//! Windows settings app, so each platform's app shows the same items.

/// One page: what the sidebar calls it and its symbolic icon from the Adwaita icon theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    General,
    Memory,
    Apps,
    Background,
    Widgets,
    Display,
    Android,
    Updates,
    Identity,
    Network,
    Advanced,
    Logs,
    About,
}

impl Page {
    pub const ALL: [Page; 13] = [
        Page::General,
        Page::Memory,
        Page::Apps,
        Page::Background,
        Page::Widgets,
        Page::Display,
        Page::Android,
        Page::Updates,
        Page::Identity,
        Page::Network,
        Page::Advanced,
        Page::Logs,
        Page::About,
    ];

    /// The page's title, in English; the sidebar shows it through gettext.
    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Memory => "Memory",
            Page::Apps => "Apps",
            Page::Background => "Running in the background",
            Page::Widgets => "Widgets",
            Page::Display => "Display and GPU",
            Page::Android => "Android version",
            Page::Updates => "Updates",
            Page::Identity => "Device identity",
            Page::Network => "Network",
            Page::Advanced => "Advanced",
            Page::Logs => "Android log",
            Page::About => "About",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Page::General => "go-home-symbolic",
            Page::Memory => "drive-harddisk-solidstate-symbolic",
            Page::Apps => "view-app-grid-symbolic",
            Page::Background => "media-playback-start-symbolic",
            Page::Widgets => "view-grid-symbolic",
            Page::Display => "video-display-symbolic",
            Page::Android => "phone-symbolic",
            Page::Updates => "software-update-available-symbolic",
            Page::Identity => "contact-new-symbolic",
            Page::Network => "network-wired-symbolic",
            Page::Advanced => "preferences-system-symbolic",
            Page::Logs => "text-x-generic-symbolic",
            Page::About => "help-about-symbolic",
        }
    }

    /// The name `--page` takes, to open the app on this page.
    pub fn id(self) -> &'static str {
        match self {
            Page::General => "general",
            Page::Memory => "memory",
            Page::Apps => "apps",
            Page::Background => "background",
            Page::Widgets => "widgets",
            Page::Display => "display",
            Page::Android => "android",
            Page::Updates => "updates",
            Page::Identity => "identity",
            Page::Network => "network",
            Page::Advanced => "advanced",
            Page::Logs => "logs",
            Page::About => "about",
        }
    }

    pub fn from_id(id: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|page| page.id() == id)
    }
}
