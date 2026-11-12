//! The Android items of a task window's system menu.
//!
//! The system menu (Alt+Space, a right click on the title bar, a click on the app's icon in
//! it) keeps Windows' own items: Restore, Move, Size, Minimize, Maximize and Close. Under them,
//! after a separator, come the items an Android desktop window offers in its window menu:
//! New window (only for an app that supports several windows), Screenshot, Aspect ratio, Open
//! by default and App info. They sit where a Windows user looks for what a window can do, and
//! they take no room in the title bar. The default title bar and one the app draws itself
//! share the same menu, since Windows opens it from the same window. The window shows it as a
//! Windows 11 menu ([`super::window_menu`]), whose Android part is these items.
//!
//! The window host takes the screenshot itself ([`super::screenshot`]), since it has the
//! window's frames. Every other item goes to the window manager as `FromHost::MenuChosen`.
//!
//! The items speak the user's display language when DSA has it (Korean), else English.

use dsa_host_core::compositor::window_hosts::MenuAction;

/// One entry of the Android part of the system menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemMenuItem {
    /// The line between Windows' items and these.
    Separator,
    NewWindow,
    Screenshot,
    AspectRatio,
    OpenByDefault,
    AppInfo,
}

/// The entries under Windows' own items, in order. New window is left out for an app that
/// does not support several windows, since its second task would only bring the first to the
/// front.
pub fn items(new_window: bool) -> Vec<SystemMenuItem> {
    let mut items = vec![SystemMenuItem::Separator];
    if new_window {
        items.push(SystemMenuItem::NewWindow);
    }
    items.extend([
        SystemMenuItem::Screenshot,
        SystemMenuItem::AspectRatio,
        SystemMenuItem::OpenByDefault,
        SystemMenuItem::AppInfo,
    ]);
    items
}

/// Every entry this menu may have, for removing them all before the menu is made again.
pub const ALL: [SystemMenuItem; 6] = [
    SystemMenuItem::Separator,
    SystemMenuItem::NewWindow,
    SystemMenuItem::Screenshot,
    SystemMenuItem::AspectRatio,
    SystemMenuItem::OpenByDefault,
    SystemMenuItem::AppInfo,
];

/// The command ID of an entry. `WM_SYSCOMMAND` carries an item's ID with its low four bits
/// used by Windows, and Windows' own commands are 0xF000 and above, so each ID has its low
/// four bits zero and stays below them.
pub fn command(item: SystemMenuItem) -> u32 {
    match item {
        SystemMenuItem::Separator => 0xE000,
        SystemMenuItem::NewWindow => 0xE010,
        SystemMenuItem::Screenshot => 0xE020,
        SystemMenuItem::AspectRatio => 0xE030,
        SystemMenuItem::OpenByDefault => 0xE040,
        SystemMenuItem::AppInfo => 0xE050,
    }
}

/// The entry a `WM_SYSCOMMAND` chose, from its `wparam`; `None` for Windows' own commands and
/// for the separator, which cannot be chosen.
pub fn chosen(wparam: usize) -> Option<SystemMenuItem> {
    let id = (wparam & 0xFFF0) as u32;
    ALL.into_iter()
        .filter(|item| *item != SystemMenuItem::Separator)
        .find(|item| command(*item) == id)
}

/// What the window manager does for an entry; `None` for the separator and for Screenshot,
/// which the window host takes.
pub fn action(item: SystemMenuItem) -> Option<MenuAction> {
    match item {
        SystemMenuItem::NewWindow => Some(MenuAction::NewWindow),
        SystemMenuItem::AspectRatio => Some(MenuAction::AspectRatio),
        SystemMenuItem::OpenByDefault => Some(MenuAction::OpenByDefault),
        SystemMenuItem::AppInfo => Some(MenuAction::AppInfo),
        SystemMenuItem::Separator | SystemMenuItem::Screenshot => None,
    }
}

/// The menu's words in one language. Each label marks its access key with `&`; the keys
/// differ from Windows' own items (R, M, S, N, X, C), so each key still picks one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuText {
    pub new_window: &'static str,
    pub screenshot: &'static str,
    pub aspect_ratio: &'static str,
    pub open_by_default: &'static str,
    pub app_info: &'static str,
    /// What a screenshot's file name starts with, as Windows' Snipping Tool names its own.
    pub screenshot_file: &'static str,
}

/// The menu's words: Korean for a Korean display language, else English.
pub fn menu_text(korean: bool) -> MenuText {
    if korean {
        MenuText {
            new_window: "새 창(&E)",
            screenshot: "스크린샷(&H)",
            aspect_ratio: "화면 비율(&A)",
            open_by_default: "링크 열기 설정(&O)",
            app_info: "앱 정보(&I)",
            screenshot_file: "스크린샷",
        }
    } else {
        MenuText {
            new_window: "N&ew window",
            screenshot: "Screens&hot",
            aspect_ratio: "&Aspect ratio",
            open_by_default: "&Open by default",
            app_info: "App &info",
            screenshot_file: "Screenshot",
        }
    }
}

/// The label of an entry; `None` for the separator.
pub fn label(text: &MenuText, item: SystemMenuItem) -> Option<&'static str> {
    Some(match item {
        SystemMenuItem::Separator => return None,
        SystemMenuItem::NewWindow => text.new_window,
        SystemMenuItem::Screenshot => text.screenshot,
        SystemMenuItem::AspectRatio => text.aspect_ratio,
        SystemMenuItem::OpenByDefault => text.open_by_default,
        SystemMenuItem::AppInfo => text.app_info,
    })
}

/// The menu's words in the user's display language.
#[cfg(windows)]
pub fn display_text() -> MenuText {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    // SAFETY: reads the user's display language.
    let language = unsafe { GetUserDefaultUILanguage() };
    menu_text(crate::compositor::widget_menu::is_korean(language))
}

/// Makes the Android part of `hwnd`'s system menu again: the entries [`items`] gives for
/// `new_window`, after Windows' own. Entries made before are removed first, so the menu
/// follows the app when it starts or stops supporting several windows.
///
/// # Safety
///
/// `hwnd` is a live window of the calling thread.
#[cfg(windows)]
pub unsafe fn apply(hwnd: windows_sys::Win32::Foundation::HWND, new_window: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DeleteMenu, GetMenuItemCount, GetSystemMenu, InsertMenuItemW, MENUITEMINFOW, MFT_SEPARATOR,
        MFT_STRING, MF_BYCOMMAND, MIIM_FTYPE, MIIM_ID, MIIM_STRING,
    };
    let menu = GetSystemMenu(hwnd, 0);
    if menu.is_null() {
        return;
    }
    for item in ALL {
        // An entry the menu does not have is simply not found.
        DeleteMenu(menu, command(item), MF_BYCOMMAND);
    }
    let text = display_text();
    for item in items(new_window) {
        let mut label: Vec<u16> = label(&text, item)
            .unwrap_or("")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut info: MENUITEMINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MENUITEMINFOW>() as u32;
        info.wID = command(item);
        if item == SystemMenuItem::Separator {
            info.fMask = MIIM_FTYPE | MIIM_ID;
            info.fType = MFT_SEPARATOR;
        } else {
            info.fMask = MIIM_FTYPE | MIIM_ID | MIIM_STRING;
            info.fType = MFT_STRING;
            info.dwTypeData = label.as_mut_ptr();
            info.cch = (label.len() - 1) as u32;
        }
        let at = GetMenuItemCount(menu).max(0) as u32;
        // The menu copies the label; `label` only has to live through the call.
        InsertMenuItemW(menu, at, 1, &info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr22_the_system_menu_carries_the_android_window_menu() {
        use SystemMenuItem::*;
        // After Windows' own items: a separator, then the Android window menu in its order.
        assert_eq!(
            items(true),
            vec![
                Separator,
                NewWindow,
                Screenshot,
                AspectRatio,
                OpenByDefault,
                AppInfo
            ]
        );
        // New window only for an app that supports several windows.
        assert_eq!(
            items(false),
            vec![Separator, Screenshot, AspectRatio, OpenByDefault, AppInfo]
        );
        // Each ID is one WM_SYSCOMMAND carries back whole, and none is one of Windows' own.
        for item in ALL {
            let id = command(item);
            assert_eq!(id & 0xF, 0, "{item:?}");
            assert!(id < 0xF000, "{item:?}");
            assert_eq!(ALL.iter().filter(|other| command(**other) == id).count(), 1);
        }
        // Windows sets the low four bits of a system command; the item is still found.
        assert_eq!(chosen(command(Screenshot) as usize | 0x2), Some(Screenshot));
        assert_eq!(chosen(command(Separator) as usize), None);
        // SC_CLOSE stays Windows'.
        assert_eq!(chosen(0xF060), None);
        // The window manager carries out every item but the screenshot.
        assert_eq!(action(NewWindow), Some(MenuAction::NewWindow));
        assert_eq!(action(AspectRatio), Some(MenuAction::AspectRatio));
        assert_eq!(action(OpenByDefault), Some(MenuAction::OpenByDefault));
        assert_eq!(action(AppInfo), Some(MenuAction::AppInfo));
        assert_eq!(action(Screenshot), None);
    }

    #[test]
    fn fr22_the_system_menu_speaks_korean_and_english() {
        for korean in [false, true] {
            let text = menu_text(korean);
            let labels: Vec<&str> = items(true)
                .into_iter()
                .filter_map(|item| label(&text, item))
                .collect();
            assert_eq!(labels.len(), 5);
            // One access key each, none of Windows' own (R, M, S, N, X, C).
            let mut keys = Vec::new();
            for label in labels {
                let at = label.find('&').expect(label);
                let key = label[at + 1..].chars().next().unwrap().to_ascii_uppercase();
                assert!(!"RMSNXC".contains(key), "{label}");
                assert!(!keys.contains(&key), "{label}");
                keys.push(key);
            }
        }
        assert_eq!(menu_text(false).new_window, "N&ew window");
        assert_eq!(menu_text(true).app_info, "앱 정보(&I)");
        assert_eq!(menu_text(true).screenshot_file, "스크린샷");
    }
}
