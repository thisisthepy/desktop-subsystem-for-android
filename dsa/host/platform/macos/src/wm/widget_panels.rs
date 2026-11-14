//! What a widget window is on a Mac, with no AppKit in it: its level and its behaviour on the
//! desktop, how screen points come to the widget desk's pixels and back, its buttons and keys,
//! and its menu as an `NSMenu` shows it.
//!
//! Each widget is a borderless, non-activating panel that belongs to the desktop: right above
//! the desktop icons' window (`kCGDesktopIconWindowLevel + 1`) and below every app's window,
//! on every Space, unmoved by Mission Control and left out of the window cycle, as the desktop
//! icons are ([`COLLECTION_BEHAVIOR`]). It sits above the icons' window rather than right
//! above the wallpaper (`kCGDesktopWindowLevel + 1`) because Finder's desktop window takes
//! every click on the empty desktop: a widget under it would show but never be pressed.
//!
//! The desk measures in pixels: a Mac's points times the screen's backing scale, from the top
//! left corner of the main screen, so a widget of 180 dp is 108 points wide on any screen
//! (96 x scale dpi: a dp is 0.6 of a point).

use dsa_host_core::widget_windows::desk::{Button, Key};
use dsa_host_core::widget_windows::menu::Entry;
use dsa_host_core::widget_windows::paint::Look;

/// `NSWindowCollectionBehaviorCanJoinAllSpaces`: on every Space.
pub const BEHAVIOR_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
/// `NSWindowCollectionBehaviorStationary`: Mission Control leaves it where it is, as it does
/// the desktop.
pub const BEHAVIOR_STATIONARY: usize = 1 << 4;
/// `NSWindowCollectionBehaviorIgnoresCycle`: not in the window cycle (Command-`).
pub const BEHAVIOR_IGNORES_CYCLE: usize = 1 << 6;

/// How a widget panel behaves among the Spaces, Mission Control and the window cycle.
pub const COLLECTION_BEHAVIOR: usize =
    BEHAVIOR_CAN_JOIN_ALL_SPACES | BEHAVIOR_STATIONARY | BEHAVIOR_IGNORES_CYCLE;

/// `NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel`: no frame, and a click
/// on it leaves the app the user works in active.
pub const STYLE: usize = 1 << 7;

/// `kCGDesktopWindowLevelKey` and `kCGDesktopIconWindowLevelKey`, for `CGWindowLevelForKey`.
pub const DESKTOP_LEVEL_KEY: i32 = 2;
pub const DESKTOP_ICON_LEVEL_KEY: i32 = 18;

/// The level of a widget panel, given the desktop icons' level: right above it.
pub fn widget_level(desktop_icon_level: i32) -> isize {
    desktop_icon_level as isize + 1
}

/// The density of a screen of backing scale `scale`: 96 at a scale of 1, 192 on a Retina
/// screen.
pub fn dpi_of_backing(scale: f64) -> u32 {
    (96.0 * scale.max(1.0)).round() as u32
}

/// A point on the screen (AppKit's points, from the bottom left corner of the main screen,
/// which is `main_height` points high) in the desk's pixels at `scale`.
pub fn desk_point(point: (f64, f64), main_height: f64, scale: f64) -> (i32, i32) {
    let scale = scale.max(1.0);
    (
        (point.0 * scale).floor() as i32,
        ((main_height - point.1) * scale).floor() as i32,
    )
}

/// The frame of a panel (its bottom left corner and size, in points, as AppKit takes it) for a
/// window at `origin` of `size`, in the desk's pixels at `scale`.
pub fn panel_frame(
    origin: (i32, i32),
    size: (i32, i32),
    main_height: f64,
    scale: f64,
) -> (f64, f64, f64, f64) {
    let scale = scale.max(1.0);
    let (width, height) = (
        f64::from(size.0.max(1)) / scale,
        f64::from(size.1.max(1)) / scale,
    );
    let x = f64::from(origin.0) / scale;
    let top = f64::from(origin.1) / scale;
    (x, main_height - top - height, width, height)
}

/// `NSEventModifierFlagControl`: a click with Control held is a secondary click on a Mac.
pub const FLAG_CONTROL: u64 = 1 << 18;

/// The button of a mouse event: its button number (0 for the left), with Control held or not.
pub fn button(number: isize, modifiers: u64) -> Option<Button> {
    match number {
        0 if modifiers & FLAG_CONTROL != 0 => Some(Button::Secondary),
        0 => Some(Button::Primary),
        1 => Some(Button::Secondary),
        _ => None,
    }
}

/// The key a Mac virtual key code is to a widget panel: Escape ends an edit; Return, Space and
/// the keypad's Enter open its menu.
pub fn key(code: u16) -> Option<Key> {
    const ESCAPE: u16 = 0x35;
    const RETURN: u16 = 0x24;
    const SPACE: u16 = 0x31;
    const KEYPAD_ENTER: u16 = 0x4C;
    match code {
        ESCAPE => Some(Key::Escape),
        RETURN | SPACE | KEYPAD_ENTER => Some(Key::Menu),
        _ => None,
    }
}

/// How firm a trackpad press is for a force click, as `NSEvent.stage` says: 2 and more is a
/// force click, which edits a widget at once, as a long press does.
pub fn force_click(stage: isize) -> bool {
    stage >= 2
}

/// The accent macOS has when it says none: the system blue.
pub const DEFAULT_ACCENT: u32 = 0xFF00_7AFF;

/// The widget panels' look on a dark or light appearance with `accent` (0 for none).
pub fn look(dark: bool, accent: u32) -> Look {
    Look::new(
        dark,
        if accent >> 24 == 0 {
            DEFAULT_ACCENT
        } else {
            accent
        },
    )
}

/// The color of three components from 0 to 1 as `0xFFRRGGBB`.
pub fn argb_of(red: f64, green: f64, blue: f64) -> u32 {
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    0xFF00_0000 | (channel(red) << 16) | (channel(green) << 8) | channel(blue)
}

/// One item of a widget's menu, as an `NSMenu` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuItem {
    /// An item the user chooses; its tag is the entry's command, and a size is checked when
    /// the widget is at it.
    Item {
        title: String,
        tag: isize,
        checked: bool,
    },
    /// Resize, with the sizes as a submenu.
    Submenu {
        title: String,
        items: Vec<MenuItem>,
    },
    Separator,
}

/// The `NSMenu` items of the menu's `entries`.
pub fn menu_items(entries: &[Entry]) -> Vec<MenuItem> {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::Item {
                command,
                label,
                checked,
            } => MenuItem::Item {
                title: label.clone(),
                tag: *command as isize,
                checked: *checked,
            },
            Entry::Sizes { label, sizes } => MenuItem::Submenu {
                title: label.clone(),
                items: menu_items(sizes),
            },
            Entry::Separator => MenuItem::Separator,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::widget_windows::menu::{entries, menu_text, REMOVE, SETTINGS};
    use dsa_host_core::widgets::{Placement, WidgetProvider};

    #[test]
    fn fr12_a_widget_panel_sits_on_the_desktop_on_every_space_and_out_of_mission_control() {
        // kCGDesktopIconWindowLevel is the desktop's level plus 20.
        let desktop = i32::MIN + 20;
        assert_eq!(widget_level(desktop + 20), desktop as isize + 21);
        assert!(
            widget_level(desktop + 20) < 0,
            "below every app's window, level 0"
        );
        assert_ne!(COLLECTION_BEHAVIOR & BEHAVIOR_CAN_JOIN_ALL_SPACES, 0);
        assert_ne!(COLLECTION_BEHAVIOR & BEHAVIOR_STATIONARY, 0);
        assert_ne!(COLLECTION_BEHAVIOR & BEHAVIOR_IGNORES_CYCLE, 0);
        assert_eq!(STYLE, 0x80, "borderless and non-activating");
    }

    #[test]
    fn fr12_a_widgets_size_follows_the_screens_scale_as_96_dpi_times_the_scale() {
        assert_eq!(dpi_of_backing(1.0), 96);
        assert_eq!(dpi_of_backing(2.0), 192);
        let px = dsa_host_core::widget_windows::window_size(
            &Placement {
                x: 0,
                y: 0,
                width_dp: 180,
                height_dp: 110,
                resized: false,
            },
            dpi_of_backing(2.0),
        );
        // Android draws its desktop at 131 dpi times the scale, rounded to an even density:
        // 262 on a Retina screen, so 180 x 110 dp is 295 x 180 pixels.
        assert_eq!(dsa_host_core::widget_windows::android_density(192), 262);
        assert_eq!(px, (295, 180));
        // 147.5 x 90 points on a Retina screen, at its place from the top of the main screen.
        let (x, y, width, height) = panel_frame((128, 128), px, 900.0, 2.0);
        assert_eq!((x, width, height), (64.0, 147.5, 90.0));
        assert_eq!(y, 900.0 - 64.0 - 90.0);
        // And a point on the screen comes back to the desk's pixels.
        assert_eq!(desk_point((64.0, 900.0 - 64.0), 900.0, 2.0), (128, 128));
        assert_eq!(desk_point((10.25, 890.0), 900.0, 2.0), (20, 20));
    }

    #[test]
    fn fr12_buttons_keys_and_force_clicks_of_a_widget_panel() {
        assert_eq!(button(0, 0), Some(Button::Primary));
        assert_eq!(button(0, FLAG_CONTROL), Some(Button::Secondary));
        assert_eq!(button(1, 0), Some(Button::Secondary));
        assert_eq!(button(2, 0), None);
        assert_eq!(key(0x35), Some(Key::Escape));
        assert_eq!(key(0x24), Some(Key::Menu));
        assert_eq!(key(0x31), Some(Key::Menu));
        assert_eq!(key(0x00), None);
        assert!(!force_click(1));
        assert!(force_click(2));
        assert_eq!(look(true, 0).accent, DEFAULT_ACCENT);
        assert_eq!(argb_of(1.0, 0.5, 0.0), 0xFFFF_8000);
    }

    #[test]
    fn fr12_the_widget_menu_is_a_native_menu_with_resize_as_a_submenu() {
        let provider = WidgetProvider {
            provider: "com.example.clock/.ClockWidget".into(),
            default_width_dp: 180,
            default_height_dp: 110,
            min_width_dp: 110,
            min_height_dp: 40,
            resize_mode: 3,
            reconfigurable: true,
            ..WidgetProvider::default()
        };
        let placement = Placement {
            x: 0,
            y: 0,
            width_dp: 180,
            height_dp: 110,
            resized: false,
        };
        let items = menu_items(&entries(&menu_text(false), &provider, &placement));
        assert_eq!(
            items[0],
            MenuItem::Item {
                title: "Widget settings".into(),
                tag: SETTINGS as isize,
                checked: false
            }
        );
        let MenuItem::Submenu {
            title,
            items: sizes,
        } = &items[1]
        else {
            panic!("Resize is a submenu: {items:?}");
        };
        assert_eq!(title, "Resize");
        assert!(sizes
            .iter()
            .any(|size| matches!(size, MenuItem::Item { checked: true, .. })));
        assert_eq!(items[2], MenuItem::Separator);
        assert!(matches!(&items[3], MenuItem::Item { tag, .. } if *tag == REMOVE as isize));
        // Without a setup to open again, the menu starts with Resize.
        let plain = WidgetProvider {
            reconfigurable: false,
            ..provider
        };
        let items = menu_items(&entries(&menu_text(true), &plain, &placement));
        assert!(matches!(&items[0], MenuItem::Submenu { title, .. } if title == "크기 조정"));
    }
}
