//! The items DSA adds to a task window's menu, whichever desktop shows the menu.
//!
//! Every host offers the same Android items in the same order, after the items the desktop
//! names itself: Full screen (where the desktop has no full screen of its own to offer), New
//! window (only for an app that supports several windows, since its second task would only
//! bring the first to the front), Screenshot, Aspect ratio, Open by default and App info.
//! Which of them the window manager carries out, and which the window host takes itself, is
//! decided here too. How a desktop draws them (an access key in the label, an ellipsis, a
//! command ID) stays with its host.

use crate::compositor::window_hosts::MenuAction;
use crate::ui_text::window_menu::DsaMenuText;

/// An item DSA adds to a task window's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AndroidItem {
    /// Fills the monitor, or leaves full screen, as F11 does.
    FullScreen,
    NewWindow,
    Screenshot,
    AspectRatio,
    OpenByDefault,
    AppInfo,
}

impl AndroidItem {
    /// What the window manager does for the item; `None` for Full screen and Screenshot,
    /// which the window host takes itself, since it has the window and its frames.
    pub fn action(self) -> Option<MenuAction> {
        match self {
            AndroidItem::NewWindow => Some(MenuAction::NewWindow),
            AndroidItem::AspectRatio => Some(MenuAction::AspectRatio),
            AndroidItem::OpenByDefault => Some(MenuAction::OpenByDefault),
            AndroidItem::AppInfo => Some(MenuAction::AppInfo),
            AndroidItem::FullScreen | AndroidItem::Screenshot => None,
        }
    }

    /// The item's words as written, before a desktop's own marks.
    pub fn label(self, text: &DsaMenuText) -> &'static str {
        match self {
            AndroidItem::FullScreen => text.full_screen,
            AndroidItem::NewWindow => text.new_window,
            AndroidItem::Screenshot => text.screenshot,
            AndroidItem::AspectRatio => text.aspect_ratio,
            AndroidItem::OpenByDefault => text.open_by_default,
            AndroidItem::AppInfo => text.app_info,
        }
    }
}

/// The Android items in order: Full screen when the desktop has none of its own (`full_screen`),
/// New window when the app supports several windows (`new_window`), then Screenshot, Aspect
/// ratio, Open by default and App info.
pub fn android_items(full_screen: bool, new_window: bool) -> Vec<AndroidItem> {
    let mut items = Vec::with_capacity(6);
    if full_screen {
        items.push(AndroidItem::FullScreen);
    }
    if new_window {
        items.push(AndroidItem::NewWindow);
    }
    items.extend([
        AndroidItem::Screenshot,
        AndroidItem::AspectRatio,
        AndroidItem::OpenByDefault,
        AndroidItem::AppInfo,
    ]);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr22_the_android_items_come_in_the_same_order_on_every_desktop() {
        use AndroidItem::*;
        assert_eq!(
            android_items(true, true),
            [
                FullScreen,
                NewWindow,
                Screenshot,
                AspectRatio,
                OpenByDefault,
                AppInfo
            ]
        );
        assert_eq!(
            android_items(true, false),
            [FullScreen, Screenshot, AspectRatio, OpenByDefault, AppInfo]
        );
        assert_eq!(
            android_items(false, true),
            [NewWindow, Screenshot, AspectRatio, OpenByDefault, AppInfo]
        );
    }

    #[test]
    fn fr22_the_window_host_takes_full_screen_and_screenshot_itself() {
        assert_eq!(AndroidItem::FullScreen.action(), None);
        assert_eq!(AndroidItem::Screenshot.action(), None);
        assert_eq!(AndroidItem::NewWindow.action(), Some(MenuAction::NewWindow));
        assert_eq!(
            AndroidItem::AspectRatio.action(),
            Some(MenuAction::AspectRatio)
        );
        assert_eq!(
            AndroidItem::OpenByDefault.action(),
            Some(MenuAction::OpenByDefault)
        );
        assert_eq!(AndroidItem::AppInfo.action(), Some(MenuAction::AppInfo));
    }
}
