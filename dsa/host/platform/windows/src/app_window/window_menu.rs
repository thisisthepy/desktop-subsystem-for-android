//! A task window's window menu, drawn as a Windows 11 menu.
//!
//! Windows would show its classic system menu for a click on the app's icon, a right click on
//! the title bar, Alt+Space and a Shift+right click on the taskbar button. The task window
//! shows the Windows 11 menu instead ([`crate::compositor::fluent_menu`]), the one the tray
//! and the widgets use, with UI Automation: Windows' own items (Restore, Move, Size, Minimize,
//! Maximize, and Close with Alt+F4), enabled as Windows enables them, then, after a separator,
//! the Android window menu ([`super::system_menu`]). Choosing an item posts the
//! `WM_SYSCOMMAND` Windows' menu would have posted, so Move and Size run Windows' own move and
//! size loops and the Android items go where they always went.
//!
//! A single click on the icon opens the menu, as in Windows 11's own apps, and sends nothing
//! to Android. A double click on the icon closes the window, as it does in every Win32 app:
//! the first click has opened the menu, which takes the second press, so a second press on
//! the icon within the double click time closes the window as Windows' own menu does
//! ([`second_click_closes`]).

use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HTCAPTION, HTCLOSE, HTHELP, HTMAXBUTTON, HTMINBUTTON, HTSYSMENU, SC_CLOSE, SC_KEYMENU,
    SC_MAXIMIZE, SC_MINIMIZE, SC_MOVE, SC_RESTORE, SC_SIZE, WM_CONTEXTMENU, WM_NCLBUTTONDBLCLK,
    WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_SYSCOMMAND,
};

use super::system_menu::{self, MenuText, SystemMenuItem};
use crate::compositor::fluent_menu::{MenuEntry, MenuItem};

/// `SC_MOUSEMENU`: the system menu opened by the mouse, which windows-sys does not name.
const SC_MOUSEMENU: u32 = 0xF090;

/// The message the taskbar sends for a Shift+right click on a window's button, with the
/// point on the screen; Windows answers it with the system menu.
const WM_POPUPSYSTEMMENU: u32 = 0x0313;

/// How the window menu was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    /// A click on the app's icon: the menu opens under the icon's cell.
    Icon,
    /// A right click, at this point on the screen.
    Pointer { x: i32, y: i32 },
    /// Alt+Space: under the icon's cell, on its first item, with its access keys underlined.
    Keyboard,
}

/// What a task window does with a message, for its window menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuMessage {
    /// Shows the window menu.
    Open(Opening),
    /// Takes the message and does nothing, so Windows does not act on it either.
    Swallow,
    /// Closes the window (`SC_CLOSE`), as a double click on a window's icon does.
    Close,
}

/// Whether the press that closed the menu a click on the icon opened is the second click of
/// a double click on the icon, which closes the window: a press of the left button on the
/// icon's cell (`on_icon`, `left_down`) at `press_ms` within `double_click_ms` of the press
/// at `opened_ms` that opened the menu, both on the message clock.
pub fn second_click_closes(
    opened_ms: u32,
    press_ms: u32,
    double_click_ms: u32,
    on_icon: bool,
    left_down: bool,
) -> bool {
    on_icon && left_down && press_ms.wrapping_sub(opened_ms) <= double_click_ms
}

fn point_of(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam & 0xffff) as u16 as i16 as i32,
        ((lparam >> 16) & 0xffff) as u16 as i16 as i32,
    )
}

/// What a message means for the window menu; `None` leaves it to the window's other handlers.
/// `hit` is the window's hit test code at a point on the screen, for a right click.
///
/// A press on the icon (`HTSYSMENU`) opens the menu, and its release is taken; a double click
/// on it closes the window, as Windows' default does. A right click on the title bar,
/// the icon's cell or a caption button, Alt+Space, the system menu Windows would open itself
/// (`SC_MOUSEMENU`), and the taskbar's Shift+right click open the menu. A context menu asked
/// for from the keyboard (Shift+F10, the menu key) is the app's.
pub fn menu_message(
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    hit: &dyn Fn(i32, i32) -> u32,
) -> Option<MenuMessage> {
    match message {
        WM_NCLBUTTONDOWN if wparam as u32 == HTSYSMENU => Some(MenuMessage::Open(Opening::Icon)),
        WM_NCLBUTTONDBLCLK if wparam as u32 == HTSYSMENU => Some(MenuMessage::Close),
        WM_NCLBUTTONUP if wparam as u32 == HTSYSMENU => Some(MenuMessage::Swallow),
        WM_CONTEXTMENU => {
            if lparam == -1 {
                return None;
            }
            let (x, y) = point_of(lparam);
            let on_title_bar = matches!(
                hit(x, y),
                HTCAPTION | HTSYSMENU | HTHELP | HTMINBUTTON | HTMAXBUTTON | HTCLOSE
            );
            on_title_bar.then_some(MenuMessage::Open(Opening::Pointer { x, y }))
        }
        WM_SYSCOMMAND => match (wparam & 0xFFF0) as u32 {
            SC_KEYMENU if lparam == LPARAM::from(b' ') => {
                Some(MenuMessage::Open(Opening::Keyboard))
            }
            SC_MOUSEMENU => {
                let (x, y) = point_of(lparam);
                Some(MenuMessage::Open(Opening::Pointer { x, y }))
            }
            _ => None,
        },
        WM_POPUPSYSTEMMENU => {
            let (x, y) = point_of(lparam);
            Some(MenuMessage::Open(Opening::Pointer { x, y }))
        }
        _ => None,
    }
}

/// What the window is like, for which items are enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowState {
    pub maximized: bool,
    pub minimized: bool,
    /// Whether the user may resize the window: the user's settings for the app may fix it.
    pub resizable: bool,
    /// Whether the app supports several windows, for New window.
    pub new_window: bool,
}

/// The words of Windows' own items in one language, each with its access key marked by `&`,
/// as Windows' system menu writes them, and what UI Automation calls the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowText {
    pub restore: &'static str,
    pub move_window: &'static str,
    pub size: &'static str,
    pub minimize: &'static str,
    pub maximize: &'static str,
    pub close: &'static str,
    pub close_shortcut: &'static str,
    pub name: &'static str,
}

/// Windows' own words: Korean for a Korean display language, else English.
pub fn window_text(korean: bool) -> WindowText {
    if korean {
        WindowText {
            restore: "복원(&R)",
            move_window: "이동(&M)",
            size: "크기 조정(&S)",
            minimize: "최소화(&N)",
            maximize: "최대화(&X)",
            close: "닫기(&C)",
            close_shortcut: "Alt+F4",
            name: "시스템",
        }
    } else {
        WindowText {
            restore: "&Restore",
            move_window: "&Move",
            size: "&Size",
            minimize: "Mi&nimize",
            maximize: "Ma&ximize",
            close: "&Close",
            close_shortcut: "Alt+F4",
            name: "System",
        }
    }
}

/// A label with its access key marked by `&`, as the label drawn and the key that chooses it.
pub fn plain_label(marked: &str) -> (String, Option<char>) {
    let mut label = String::with_capacity(marked.len());
    let mut key = None;
    let mut chars = marked.chars();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.next() {
                Some('&') => label.push('&'),
                Some(next) => {
                    if key.is_none() {
                        key = Some(next.to_ascii_uppercase());
                    }
                    label.push(next);
                }
                None => {}
            }
        } else {
            label.push(c);
        }
    }
    (label, key)
}

/// Segoe Fluent Icons' caption glyphs, which Windows 11's system menu draws before Restore,
/// Minimize, Maximize and Close.
const RESTORE_GLYPH: char = '\u{E923}';
const MINIMIZE_GLYPH: char = '\u{E921}';
const MAXIMIZE_GLYPH: char = '\u{E922}';
const CLOSE_GLYPH: char = '\u{E8BB}';

fn item(id: u32, marked: &str) -> MenuItem {
    let (label, key) = plain_label(marked);
    let item = MenuItem::new(id, label);
    match key {
        Some(key) => item.with_access_key(key),
        None => item,
    }
}

/// The window menu of a window in `state`: Windows' items, enabled as Windows enables them
/// (Restore only for a maximized or minimized window, Move only for one that is not maximized,
/// Size and Maximize only for one the user may resize and that is not maximized or minimized,
/// Minimize only for one that is not minimized), Close with its shortcut after a separator,
/// then the Android window menu after another. Each item's id is the `WM_SYSCOMMAND` it posts.
pub fn entries(state: &WindowState, text: &WindowText, android: &MenuText) -> Vec<MenuEntry> {
    let sizable = state.resizable && !state.maximized && !state.minimized;
    let mut entries = vec![
        MenuEntry::Item(
            item(SC_RESTORE, text.restore)
                .with_glyph(RESTORE_GLYPH)
                .with_enabled(state.maximized || state.minimized),
        ),
        MenuEntry::Item(item(SC_MOVE, text.move_window).with_enabled(!state.maximized)),
        MenuEntry::Item(item(SC_SIZE, text.size).with_enabled(sizable)),
        MenuEntry::Item(
            item(SC_MINIMIZE, text.minimize)
                .with_glyph(MINIMIZE_GLYPH)
                .with_enabled(!state.minimized),
        ),
        MenuEntry::Item(
            item(SC_MAXIMIZE, text.maximize)
                .with_glyph(MAXIMIZE_GLYPH)
                .with_enabled(state.resizable && !state.maximized),
        ),
        MenuEntry::Separator,
        MenuEntry::Item(
            item(SC_CLOSE, text.close)
                .with_glyph(CLOSE_GLYPH)
                .with_shortcut(text.close_shortcut),
        ),
    ];
    for android_item in system_menu::items(state.new_window) {
        match system_menu::label(android, android_item) {
            None => entries.push(MenuEntry::Separator),
            Some(marked) => entries.push(MenuEntry::Item(item(
                system_menu::command(android_item),
                marked,
            ))),
        }
    }
    entries
}

/// Whether the Android part of `hwnd`'s system menu has New window: the app supports several
/// windows ([`system_menu::apply`] keeps it so).
///
/// # Safety
///
/// `hwnd` is a live window of the calling thread.
unsafe fn has_new_window(hwnd: HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetMenuState, GetSystemMenu, MF_BYCOMMAND};
    let menu = GetSystemMenu(hwnd, 0);
    !menu.is_null()
        && GetMenuState(
            menu,
            system_menu::command(SystemMenuItem::NewWindow),
            MF_BYCOMMAND,
        ) != u32::MAX
}

/// Where the menu goes on the screen for `opening`: at the pointer, or under the icon's cell
/// (`cell_bottom_left`, client pixels) as Windows places its system menu under its icon.
///
/// # Safety
///
/// `hwnd` is a live window of the calling thread.
unsafe fn anchor_point(hwnd: HWND, opening: Opening, cell_bottom_left: (i32, i32)) -> (i32, i32) {
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    match opening {
        Opening::Pointer { x, y } => (x, y),
        Opening::Icon | Opening::Keyboard => {
            let mut point = POINT {
                x: cell_bottom_left.0,
                y: cell_bottom_left.1,
            };
            ClientToScreen(hwnd, &mut point);
            (point.x, point.y)
        }
    }
}

/// Shows `hwnd`'s window menu for `opening` and posts the `WM_SYSCOMMAND` of the item chosen.
/// `cell` is the icon's cell (left, top, width, height, client pixels), which the menu opens
/// under; `resizable` says whether the user may resize the window. A menu a click on the icon
/// opened and a second press on the icon closed within the double click time closes the
/// window ([`second_click_closes`]). A window without a system menu (one that fills its
/// monitor) shows none, as Windows shows none for it. Runs the menu's messages until it closes.
///
/// # Safety
///
/// `hwnd` is a live window of the calling thread, and no state of its window procedure is
/// borrowed while the menu runs.
pub unsafe fn show(hwnd: HWND, opening: Opening, cell: (i32, i32, u32, u32), resizable: bool) {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetDoubleClickTime, GetKeyState, VK_LBUTTON,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetMessagePos, GetMessageTime, GetWindowLongPtrW, IsIconic, IsZoomed, PostMessageW,
        GWL_STYLE, WS_SYSMENU,
    };
    let (cell_left, cell_top, cell_width, cell_height) = cell;
    let cell_bottom_left = (cell_left, cell_top + cell_height as i32);
    let opened_ms = GetMessageTime() as u32;
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
    if style & WS_SYSMENU == 0 {
        return;
    }
    let state = WindowState {
        maximized: IsZoomed(hwnd) != 0,
        minimized: IsIconic(hwnd) != 0,
        resizable,
        new_window: has_new_window(hwnd),
    };
    let korean = crate::compositor::widget_menu::is_korean(GetUserDefaultUILanguage());
    let text = window_text(korean);
    let menu = entries(&state, &text, &system_menu::menu_text(korean));
    let (x, y) = anchor_point(hwnd, opening, cell_bottom_left);
    let chosen = crate::compositor::fluent_menu::show(crate::compositor::fluent_menu::Request {
        owner: hwnd as isize,
        anchor: crate::compositor::fluent_menu::Anchor::Point { x, y },
        entries: &menu,
        name: text.name,
        keyboard: opening == Opening::Keyboard,
    });
    let command = match chosen {
        Some(command) => Some(command),
        None if opening == Opening::Icon => {
            // The press that closed the menu is the last message its loop took.
            let mut press = POINT {
                x: (GetMessagePos() & 0xffff) as u16 as i16 as i32,
                y: ((GetMessagePos() >> 16) & 0xffff) as u16 as i16 as i32,
            };
            ScreenToClient(hwnd, &mut press);
            let on_icon = press.x >= cell_left
                && press.x < cell_left + cell_width as i32
                && press.y >= cell_top
                && press.y < cell_top + cell_height as i32;
            let left_down = GetKeyState(i32::from(VK_LBUTTON)) < 0;
            let press_ms = GetMessageTime() as u32;
            second_click_closes(
                opened_ms,
                press_ms,
                GetDoubleClickTime(),
                on_icon,
                left_down,
            )
            .then_some(SC_CLOSE)
        }
        None => None,
    };
    if let Some(command) = command {
        // Posted, as Windows' menu posts it, so the window finishes this message first.
        PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caption_everywhere(_: i32, _: i32) -> u32 {
        HTCAPTION
    }

    fn labels(entries: &[MenuEntry]) -> Vec<Option<String>> {
        entries
            .iter()
            .map(|entry| entry.item().map(|item| item.label.clone()))
            .collect()
    }

    #[test]
    fn fr22_a_click_on_the_icon_opens_the_window_menu_and_a_double_click_closes() {
        let down = menu_message(
            WM_NCLBUTTONDOWN,
            HTSYSMENU as WPARAM,
            0,
            &caption_everywhere,
        );
        assert_eq!(down, Some(MenuMessage::Open(Opening::Icon)));
        // Its release is taken, so a single click does nothing more.
        let up = menu_message(WM_NCLBUTTONUP, HTSYSMENU as WPARAM, 0, &caption_everywhere);
        assert_eq!(up, Some(MenuMessage::Swallow));
        // A double click on the icon closes the window, as in every Win32 app.
        let double = menu_message(
            WM_NCLBUTTONDBLCLK,
            HTSYSMENU as WPARAM,
            0,
            &caption_everywhere,
        );
        assert_eq!(double, Some(MenuMessage::Close));
        // The first click opened the menu, which takes the second press: a second press on the
        // icon within the double click time closes the window too, and nothing else does.
        assert!(second_click_closes(1000, 1300, 500, true, true));
        assert!(second_click_closes(u32::MAX - 100, 200, 500, true, true));
        assert!(
            !second_click_closes(1000, 1600, 500, true, true),
            "too late"
        );
        assert!(
            !second_click_closes(1000, 1300, 500, false, true),
            "a press beside the icon only closes the menu"
        );
        assert!(
            !second_click_closes(1000, 1300, 500, true, false),
            "Escape over the icon only closes the menu"
        );
        // A press elsewhere in the band (the drag area, the back arrow) is not the menu's.
        for code in [HTCAPTION, HTHELP] {
            let other = menu_message(WM_NCLBUTTONDOWN, code as WPARAM, 0, &caption_everywhere);
            assert_eq!(other, None);
        }
        // Nothing the menu does closes the window but its Close item.
        let state = WindowState {
            resizable: true,
            ..WindowState::default()
        };
        let menu = entries(&state, &window_text(false), &system_menu::menu_text(false));
        let closing: Vec<u32> = menu
            .iter()
            .filter_map(MenuEntry::item)
            .map(|item| item.id)
            .filter(|id| *id == SC_CLOSE)
            .collect();
        assert_eq!(closing, vec![SC_CLOSE]);
    }

    #[test]
    fn fr22_alt_space_and_a_right_click_on_the_title_bar_open_the_window_menu() {
        let space = menu_message(
            WM_SYSCOMMAND,
            SC_KEYMENU as WPARAM,
            0x20,
            &caption_everywhere,
        );
        assert_eq!(space, Some(MenuMessage::Open(Opening::Keyboard)));
        // Alt alone (the menu bar) is not the window menu.
        let alt = menu_message(WM_SYSCOMMAND, SC_KEYMENU as WPARAM, 0, &caption_everywhere);
        assert_eq!(alt, None);
        let at = (300 & 0xffff) | (40 << 16);
        let right = menu_message(WM_CONTEXTMENU, 0, at, &caption_everywhere);
        assert_eq!(
            right,
            Some(MenuMessage::Open(Opening::Pointer { x: 300, y: 40 }))
        );
        // On a monitor left of the main one, the point is negative.
        let left = ((-20i32 as u16) as isize) | (40 << 16);
        let right = menu_message(WM_CONTEXTMENU, 0, left, &caption_everywhere);
        assert_eq!(
            right,
            Some(MenuMessage::Open(Opening::Pointer { x: -20, y: 40 }))
        );
        // A right click in the app, and a context menu from the keyboard, are the app's.
        let client = |_: i32, _: i32| windows_sys::Win32::UI::WindowsAndMessaging::HTCLIENT;
        assert_eq!(menu_message(WM_CONTEXTMENU, 0, at, &client), None);
        assert_eq!(
            menu_message(WM_CONTEXTMENU, 0, -1, &caption_everywhere),
            None
        );
        // The taskbar's Shift+right click, and the system menu Windows would open itself.
        let taskbar = menu_message(WM_POPUPSYSTEMMENU, 0, at, &caption_everywhere);
        assert!(matches!(
            taskbar,
            Some(MenuMessage::Open(Opening::Pointer { .. }))
        ));
        let mouse = menu_message(
            WM_SYSCOMMAND,
            SC_MOUSEMENU as WPARAM,
            at,
            &caption_everywhere,
        );
        assert!(matches!(
            mouse,
            Some(MenuMessage::Open(Opening::Pointer { .. }))
        ));
    }

    #[test]
    fn fr22_the_window_menu_is_the_windows_11_menu_with_the_standard_items() {
        let normal = WindowState {
            resizable: true,
            new_window: true,
            ..WindowState::default()
        };
        let menu = entries(&normal, &window_text(false), &system_menu::menu_text(false));
        let some = |label: &str| Some(label.to_string());
        assert_eq!(
            labels(&menu),
            vec![
                some("Restore"),
                some("Move"),
                some("Size"),
                some("Minimize"),
                some("Maximize"),
                None,
                some("Close"),
                None,
                some("New window"),
                some("Screenshot"),
                some("Aspect ratio"),
                some("Open by default"),
                some("App info"),
            ]
        );
        let items: Vec<&MenuItem> = menu.iter().filter_map(MenuEntry::item).collect();
        // Each item posts the system command Windows' menu posts, the Android ones theirs.
        let ids: Vec<u32> = items.iter().map(|item| item.id).collect();
        assert_eq!(
            ids[..6],
            [
                SC_RESTORE,
                SC_MOVE,
                SC_SIZE,
                SC_MINIMIZE,
                SC_MAXIMIZE,
                SC_CLOSE
            ]
        );
        assert_eq!(
            system_menu::chosen(ids[6] as usize),
            Some(SystemMenuItem::NewWindow)
        );
        // Windows' access keys, and Close's shortcut.
        let keys: Vec<Option<char>> = items[..6].iter().map(|item| item.access_key).collect();
        let expected = ['R', 'M', 'S', 'N', 'X', 'C'].map(Some);
        assert_eq!(keys, expected);
        assert_eq!(items[5].shortcut.as_deref(), Some("Alt+F4"));
        assert!(items[6..].iter().all(|item| item.shortcut.is_none()));
        // A window as it opens: Restore is off, the rest on.
        let enabled: Vec<bool> = items[..6].iter().map(|item| item.enabled).collect();
        assert_eq!(enabled, [false, true, true, true, true, true]);
        // Maximized: Restore on, Move, Size and Maximize off.
        let maximized = WindowState {
            maximized: true,
            ..normal
        };
        let menu = entries(
            &maximized,
            &window_text(false),
            &system_menu::menu_text(false),
        );
        let enabled: Vec<bool> = menu
            .iter()
            .filter_map(MenuEntry::item)
            .take(6)
            .map(|item| item.enabled)
            .collect();
        assert_eq!(enabled, [true, false, false, true, false, true]);
        // Fixed in size by the user's settings: no Size, no Maximize.
        let fixed = WindowState {
            resizable: false,
            new_window: false,
            ..WindowState::default()
        };
        let menu = entries(&fixed, &window_text(false), &system_menu::menu_text(false));
        let enabled: Vec<bool> = menu
            .iter()
            .filter_map(MenuEntry::item)
            .take(6)
            .map(|item| item.enabled)
            .collect();
        assert_eq!(enabled, [false, true, false, true, false, true]);
        assert!(!labels(&menu).contains(&some("New window")));
        // In Korean, Windows' own words with the same keys.
        let korean = entries(&normal, &window_text(true), &system_menu::menu_text(true));
        assert_eq!(korean[0].item().unwrap().label, "복원(R)");
        assert_eq!(korean[0].item().unwrap().access_key, Some('R'));
        assert_eq!(korean[6].item().unwrap().label, "닫기(C)");
    }

    #[test]
    fn fr22_a_marked_label_gives_its_access_key() {
        assert_eq!(plain_label("Mi&nimize"), ("Minimize".into(), Some('N')));
        assert_eq!(plain_label("앱 정보(&I)"), ("앱 정보(I)".into(), Some('I')));
        assert_eq!(plain_label("A && B"), ("A & B".into(), None));
        assert_eq!(plain_label("Plain"), ("Plain".into(), None));
    }
}
