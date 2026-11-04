//! The menu a widget window shows when the user right-clicks it: Resize, with the sizes the
//! widget allows, and Remove.
//!
//! Choosing a size resizes the window, which tells the service and Android the new size as a
//! drag of its edges does. Choosing Remove asks the service to remove the widget; its window
//! closes when the service says the widget is gone, so the settings app and every other
//! watcher learn of it the same way.
//!
//! The menu speaks the user's display language when DSA has it (Korean), else English.

use dsa_host_core::widgets::{clamp_size, Placement, WidgetProvider};

/// A size the Resize menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeName {
    /// The smallest the widget allows.
    Small,
    /// The size the widget is placed at.
    Default,
    /// One and a half times its default.
    Large,
    /// Twice its default.
    ExtraLarge,
}

/// One size of the Resize menu, in dp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeChoice {
    pub name: SizeName,
    pub width_dp: u32,
    pub height_dp: u32,
}

/// What the user chose in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    /// Resize the window to this size, in dp.
    Resize { width_dp: u32, height_dp: u32 },
    /// Remove the widget.
    Remove,
}

/// The menu command of Remove. A size is [`FIRST_SIZE`] plus its place in the list.
pub const REMOVE: usize = 1;
/// The menu command of the first size.
pub const FIRST_SIZE: usize = 100;

/// The sizes the Resize menu offers for `provider`, each within what the widget allows. A
/// size equal to one before it is left out, so a widget that cannot be resized offers one.
pub fn size_choices(provider: &WidgetProvider) -> Vec<SizeChoice> {
    let (width, height) = clamp_size(
        provider,
        provider.default_width_dp,
        provider.default_height_dp,
    );
    let asked = [
        (SizeName::Small, 0, 0),
        (SizeName::Default, width, height),
        (SizeName::Large, width * 3 / 2, height * 3 / 2),
        (SizeName::ExtraLarge, width * 2, height * 2),
    ];
    let mut out: Vec<SizeChoice> = Vec::with_capacity(asked.len());
    for (name, w, h) in asked {
        let (width_dp, height_dp) = clamp_size(provider, w, h);
        if out
            .iter()
            .all(|c| (c.width_dp, c.height_dp) != (width_dp, height_dp))
        {
            out.push(SizeChoice {
                name,
                width_dp,
                height_dp,
            });
        }
    }
    out
}

/// What the menu command `command` chose among `sizes`; None for no choice (0, as when the
/// menu was dismissed) or an unknown one.
pub fn choice_of(command: usize, sizes: &[SizeChoice]) -> Option<MenuChoice> {
    if command == REMOVE {
        return Some(MenuChoice::Remove);
    }
    let size = sizes.get(command.checked_sub(FIRST_SIZE)?)?;
    Some(MenuChoice::Resize {
        width_dp: size.width_dp,
        height_dp: size.height_dp,
    })
}

/// Whether `placement` is at the size of `choice`, which the menu marks.
pub fn is_current(choice: &SizeChoice, placement: &Placement) -> bool {
    (choice.width_dp, choice.height_dp) == (placement.width_dp, placement.height_dp)
}

/// The words of the menu in one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuText {
    pub resize: &'static str,
    pub remove: &'static str,
    small: &'static str,
    default: &'static str,
    large: &'static str,
    extra_large: &'static str,
}

/// The menu's words: Korean for a Korean display language, else English.
pub fn menu_text(korean: bool) -> MenuText {
    if korean {
        MenuText {
            resize: "크기 조정",
            remove: "제거",
            small: "작게",
            default: "기본 크기",
            large: "크게",
            extra_large: "아주 크게",
        }
    } else {
        MenuText {
            resize: "Resize",
            remove: "Remove",
            small: "Small",
            default: "Default size",
            large: "Large",
            extra_large: "Extra large",
        }
    }
}

/// The label of a size in the menu, with the size itself, such as "Large (270 × 165)".
pub fn size_label(text: &MenuText, choice: &SizeChoice) -> String {
    let name = match choice.name {
        SizeName::Small => text.small,
        SizeName::Default => text.default,
        SizeName::Large => text.large,
        SizeName::ExtraLarge => text.extra_large,
    };
    format!("{name} ({} × {})", choice.width_dp, choice.height_dp)
}

/// Whether a Windows language ID is Korean (its primary language is 0x12).
pub fn is_korean(lang_id: u16) -> bool {
    lang_id & 0x3ff == 0x12
}

/// Shows the menu of a widget window at `x`, `y` on the screen and returns what the user
/// chose. `window` is the widget window's handle as a number. Called on the window's own
/// thread; no state is borrowed while the menu is open.
#[cfg(windows)]
pub fn show(
    window: isize,
    x: i32,
    y: i32,
    provider: &WidgetProvider,
    placement: &Placement,
) -> Option<MenuChoice> {
    use std::ptr::null;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, PostMessageW, SetForegroundWindow,
        TrackPopupMenu, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, WM_NULL,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    // SAFETY: reads the user's display language.
    let text = menu_text(is_korean(unsafe { GetUserDefaultUILanguage() }));
    let sizes = size_choices(provider);
    let hwnd = window as HWND;
    // SAFETY: the menus are made, shown and destroyed here, on the window's thread; every
    // string outlives the call that reads it.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        let sizes_menu = CreatePopupMenu();
        if !sizes_menu.is_null() {
            for (i, size) in sizes.iter().enumerate() {
                let label = wide(&size_label(&text, size));
                let flags = if is_current(size, placement) {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                AppendMenuW(sizes_menu, flags, FIRST_SIZE + i, label.as_ptr());
            }
            let resize = wide(text.resize);
            AppendMenuW(menu, MF_POPUP, sizes_menu as usize, resize.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
        }
        let remove = wide(text.remove);
        AppendMenuW(menu, MF_STRING, REMOVE, remove.as_ptr());
        // A menu closes when the user clicks elsewhere only if its window is in front.
        SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            x,
            y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        choice_of(command.max(0) as usize, &sizes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> WidgetProvider {
        WidgetProvider {
            provider: "com.example.clock/.ClockWidget".into(),
            min_width_dp: 110,
            min_height_dp: 40,
            default_width_dp: 180,
            default_height_dp: 110,
            resize_mode: 3,
            ..WidgetProvider::default()
        }
    }

    #[test]
    fn fr12_the_widget_menu_offers_sizes_the_widget_allows() {
        let sizes = size_choices(&clock());
        let sizes: Vec<(SizeName, u32, u32)> = sizes
            .iter()
            .map(|c| (c.name, c.width_dp, c.height_dp))
            .collect();
        assert_eq!(sizes[1], (SizeName::Default, 180, 110));
        assert_eq!(sizes[2], (SizeName::Large, 270, 165));
        assert_eq!(sizes[3], (SizeName::ExtraLarge, 360, 220));
        assert_eq!(sizes[0].0, SizeName::Small);
        assert!(sizes[0].1 <= 180 && sizes[0].2 <= 110);
        // A widget that cannot be resized offers its one size.
        let fixed = WidgetProvider {
            resize_mode: 0,
            ..clock()
        };
        assert_eq!(size_choices(&fixed).len(), 1);
        // A widget with a maximum offers nothing past it.
        let capped = WidgetProvider {
            max_width_dp: 200,
            max_height_dp: 120,
            ..clock()
        };
        assert!(size_choices(&capped)
            .iter()
            .all(|c| c.width_dp <= 200 && c.height_dp <= 120));
    }

    #[test]
    fn fr12_the_widget_menu_choice_is_remove_or_a_size() {
        let sizes = size_choices(&clock());
        assert_eq!(choice_of(REMOVE, &sizes), Some(MenuChoice::Remove));
        assert_eq!(
            choice_of(FIRST_SIZE + 2, &sizes),
            Some(MenuChoice::Resize {
                width_dp: 270,
                height_dp: 165
            })
        );
        assert_eq!(choice_of(0, &sizes), None);
        assert_eq!(choice_of(FIRST_SIZE + 99, &sizes), None);
        assert_eq!(choice_of(50, &sizes), None);
        assert!(is_current(
            &sizes[1],
            &Placement {
                x: 0,
                y: 0,
                width_dp: 180,
                height_dp: 110
            }
        ));
    }

    #[test]
    fn fr12_the_widget_menu_speaks_korean_and_english() {
        assert!(is_korean(0x0412));
        assert!(!is_korean(0x0409));
        let large = SizeChoice {
            name: SizeName::Large,
            width_dp: 270,
            height_dp: 165,
        };
        assert_eq!(size_label(&menu_text(false), &large), "Large (270 × 165)");
        assert_eq!(size_label(&menu_text(true), &large), "크게 (270 × 165)");
        assert_eq!(menu_text(false).remove, "Remove");
        assert_eq!(menu_text(true).resize, "크기 조정");
    }
}
