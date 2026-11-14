//! The menu of a widget window, which the user opens by long-pressing the widget, by
//! right-clicking it, or with the menu key or Shift+F10: Widget settings, for a widget whose
//! app lets it be set up again, Resize, with the sizes the widget allows, and Remove, as
//! Android's home screen offers them. It is a Windows 11 menu
//! ([`crate::compositor::fluent_menu`]), with an icon for each item and a check mark on the
//! widget's current size.
//!
//! Choosing Widget settings asks the service to open the widget's setup again, which shows as
//! the app's window; once the user finishes it, the widget shows what they chose. Choosing a
//! size resizes the window, which tells the service and Android the new size as a drag of its
//! edges does. Choosing Remove asks the service to remove the widget; its window closes when
//! the service says the widget is gone, so the settings app and every other watcher learn of
//! it the same way.
//!
//! The menu speaks the user's display language when DSA has it (Korean), else English.

//!
//! What it offers is the platform-neutral menu every host shares
//! (`dsa_host_core::widget_windows::menu`); what is here is its Windows 11 look.

pub use dsa_host_core::widget_windows::menu::{
    choice_of, is_current, menu_name, menu_text, size_choices, size_label, MenuChoice, MenuText,
    SizeChoice, SizeName, FIRST_SIZE, REMOVE, RESIZE, SETTINGS,
};
use dsa_host_core::widgets::{Placement, WidgetProvider};

use super::fluent::glyph;
use super::fluent_menu::{MenuEntry, MenuItem};

/// The key that chooses a size while the menu is open: the first letter of its English
/// name, which Windows' Korean menus use as well.
fn size_key(name: SizeName) -> char {
    match name {
        SizeName::Small => 'S',
        SizeName::Default => 'D',
        SizeName::Large => 'L',
        SizeName::ExtraLarge => 'X',
    }
}

/// The menu's entries: Widget settings when `settings` (the widget can be set up again),
/// Resize, which opens the sizes with the current one checked, a separator, and Remove.
/// Widget settings is chosen with W, Resize opens with R and Remove is chosen with M (Resize
/// has R).
pub fn entries(
    text: &MenuText,
    sizes: &[SizeChoice],
    placement: &Placement,
    settings: bool,
) -> Vec<MenuEntry> {
    let sizes: Vec<MenuEntry> = sizes
        .iter()
        .zip(FIRST_SIZE..)
        .map(|(size, id)| {
            let item = MenuItem::new(id, size_label(text, size));
            let item = item.with_access_key(size_key(size.name));
            MenuEntry::Item(item.with_check(is_current(size, placement)))
        })
        .collect();
    let remove = MenuItem::new(REMOVE, text.remove).with_glyph(glyph::UNPIN);
    let mut out = Vec::with_capacity(4);
    if settings {
        let item = MenuItem::new(SETTINGS, text.settings).with_glyph(glyph::SETTINGS);
        out.push(MenuEntry::Item(item.with_access_key('W')));
    }
    if !sizes.is_empty() {
        let resize = MenuItem::new(RESIZE, text.resize).with_glyph(glyph::RESIZE);
        out.push(MenuEntry::Item(
            resize.with_access_key('R').with_submenu(sizes),
        ));
        out.push(MenuEntry::Separator);
    }
    out.push(MenuEntry::Item(remove.with_access_key('M')));
    out
}

/// Whether a Windows language ID is Korean (its primary language is 0x12).
pub fn is_korean(lang_id: u16) -> bool {
    lang_id & 0x3ff == 0x12
}

/// The menu's words in the user's display language.
#[cfg(windows)]
pub fn display_text() -> MenuText {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    // SAFETY: reads the user's display language.
    menu_text(is_korean(unsafe { GetUserDefaultUILanguage() }))
}

/// Shows the menu of a widget window where `anchor` says and returns what the user chose.
/// `window` is the widget window's handle as a number; `keyboard` says the keyboard opened
/// the menu. Called on the window's own thread; no state is borrowed while the menu is open.
#[cfg(windows)]
pub fn show(
    window: isize,
    anchor: super::fluent_menu::Anchor,
    keyboard: bool,
    provider: &WidgetProvider,
    placement: &Placement,
) -> Option<MenuChoice> {
    let text = display_text();
    let sizes = size_choices(provider);
    let entries = entries(&text, &sizes, placement, provider.reconfigurable);
    let name = menu_name(&text, provider);
    let chosen = super::fluent_menu::show(super::fluent_menu::Request {
        owner: window,
        anchor,
        entries: &entries,
        name: &name,
        keyboard,
    })?;
    choice_of(chosen, &sizes)
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

    fn at(width_dp: u32, height_dp: u32) -> Placement {
        Placement {
            x: 0,
            y: 0,
            width_dp,
            height_dp,
            resized: false,
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
        assert_eq!(
            choice_of(RESIZE, &sizes),
            None,
            "Resize only opens the sizes"
        );
        assert_eq!(choice_of(FIRST_SIZE + 99, &sizes), None);
        assert_eq!(choice_of(50, &sizes), None);
        assert!(is_current(&sizes[1], &at(180, 110)));
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
        assert_eq!(menu_text(false).settings, "Widget settings");
        assert_eq!(menu_text(true).settings, "위젯 설정");
    }

    /// A widget whose app lets it be set up again has Widget settings first, in the long
    /// press's menu and the right click's alike; any other widget has no such item.
    #[test]
    fn fr12_the_widget_menu_offers_settings_only_for_a_widget_that_can_be_set_up_again() {
        let sizes = size_choices(&clock());
        for korean in [false, true] {
            let text = menu_text(korean);
            let menu = entries(&text, &sizes, &at(180, 110), true);
            assert_eq!(menu.len(), 4);
            let settings = menu[0].item().unwrap();
            assert_eq!(
                (settings.id, settings.label.as_str()),
                (SETTINGS, text.settings)
            );
            assert_eq!(settings.glyph, Some(glyph::SETTINGS));
            assert_eq!(settings.access_key, Some('W'));
            assert!(settings.submenu.is_empty());
            assert_eq!(menu[1].item().unwrap().id, RESIZE);
            assert_eq!(menu[3].item().unwrap().id, REMOVE);
            let without = entries(&text, &sizes, &at(180, 110), false);
            assert!(without
                .iter()
                .filter_map(MenuEntry::item)
                .all(|item| item.id != SETTINGS));
        }
        assert_eq!(choice_of(SETTINGS, &sizes), Some(MenuChoice::Settings));
    }

    #[test]
    fn fr12_right_clicking_a_widget_offers_resize_and_remove() {
        let sizes = size_choices(&clock());
        for korean in [false, true] {
            let text = menu_text(korean);
            let menu = entries(&text, &sizes, &at(270, 165), false);
            assert_eq!(menu.len(), 3);
            let resize = menu[0].item().unwrap();
            assert_eq!(resize.label, text.resize);
            assert_eq!(resize.glyph, Some(glyph::RESIZE));
            assert_eq!(resize.access_key, Some('R'));
            assert_eq!(menu[1], MenuEntry::Separator);
            let remove = menu[2].item().unwrap();
            assert_eq!((remove.id, remove.label.as_str()), (REMOVE, text.remove));
            assert_eq!(remove.glyph, Some(glyph::UNPIN));
            assert_eq!(remove.access_key, Some('M'));
            // The sizes, each with its key, and a check mark on the current one only.
            let offered: Vec<(u32, &str, Option<bool>)> = resize
                .submenu
                .iter()
                .filter_map(MenuEntry::item)
                .map(|item| (item.id, item.label.as_str(), item.checked))
                .collect();
            assert_eq!(offered.len(), sizes.len());
            assert_eq!(offered[2].0, FIRST_SIZE + 2);
            assert_eq!(offered[2].1, size_label(&text, &sizes[2]));
            let checked: Vec<bool> = offered.iter().map(|o| o.2 == Some(true)).collect();
            assert_eq!(checked, [false, false, true, false]);
            let keys: Vec<Option<char>> = resize
                .submenu
                .iter()
                .filter_map(MenuEntry::item)
                .map(|item| item.access_key)
                .collect();
            assert_eq!(keys, [Some('S'), Some('D'), Some('L'), Some('X')]);
        }
        // A widget with no name is called "Widget".
        assert_eq!(menu_name(&menu_text(false), &clock()), "Widget");
        let named = WidgetProvider {
            label: "Clock".into(),
            ..clock()
        };
        assert_eq!(menu_name(&menu_text(true), &named), "Clock");
    }

    /// The widget windows never fall back to a plain Win32 popup menu: neither the menu nor
    /// the windows that open it call the Win32 menu functions.
    #[test]
    fn fr12_no_widget_menu_is_a_plain_win32_menu() {
        let calls = [
            ["Track", "PopupMenu"].concat(),
            ["Create", "PopupMenu"].concat(),
            ["Append", "MenuW"].concat(),
            ["Insert", "MenuItemW"].concat(),
        ];
        let files = [
            "widget_menu.rs",
            "widgets.rs",
            "fluent_menu/mod.rs",
            "fluent_menu/window.rs",
        ];
        let sources = [
            include_str!("widget_menu.rs"),
            include_str!("widgets.rs"),
            include_str!("fluent_menu/mod.rs"),
            include_str!("fluent_menu/window.rs"),
        ];
        for (file, source) in files.into_iter().zip(sources) {
            for call in &calls {
                assert!(!source.contains(call.as_str()), "{file} calls {call}");
            }
        }
    }
}
