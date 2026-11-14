//! A task window's menu, with no Wayland or X11 in it.
//!
//! A Wayland window cannot add items to the desktop's own window menu, so the window draws a
//! menu of its own, in a popup (`xdg_popup`). It opens from the icon's cell, from a right click
//! on the title bar, and from Alt+Space, where the desktop's window menu would open and as the
//! system menu opens on Windows. It carries the window's own items in the order of the Windows
//! system menu (Restore, Move, Size, Minimize, Maximize, Close), then, after a separator, the
//! items an Android desktop window offers in its window menu: New window (only for an app that
//! supports several windows), Screenshot, Aspect ratio, Open by default and App info, as the
//! Windows host puts them in the system menu. Last, after another separator, More window
//! options opens the desktop's own window menu (`xdg_toplevel.show_window_menu`, or
//! `_GTK_SHOW_WINDOW_MENU` for an X11 window), for what only the desktop offers: always on top,
//! workspaces and the like.
//!
//! Move and Size work as the desktop's own do. On Wayland a window can only start a move or a
//! resize from a press that is still held (`xdg_toplevel.move` and `.resize` take its serial),
//! and a menu item is chosen by a release or a key, so choosing one arms the window: the next
//! press in it starts the move, or the resize from the edge nearest the press
//! ([`size_edge`]). On X11 the window manager moves or sizes the window from the keyboard and
//! the pointer at once (`_NET_WM_MOVERESIZE_MOVE_KEYBOARD` and `_SIZE_KEYBOARD`).
//!
//! The window takes the screenshot itself, since it has the window's frames. The other
//! Android items go to the window rules as `FromHost::MenuChosen` and become requests to the
//! guest ([`menu_work`]), or open DSA Settings at the app's page.
//!
//! The items speak Korean when the session reads Korean, else English.

use dsa_host_core::compositor::caption::scaled;
use dsa_host_core::compositor::window_hosts::{MenuAction, MenuWork};
use dsa_host_core::tasks::{AppSettingsPage, TaskRequest};

use super::title_bar::Coverage;

/// One entry of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuItem {
    Restore,
    Move,
    Size,
    Minimize,
    Maximize,
    Close,
    /// The line between groups of items.
    Separator,
    NewWindow,
    Screenshot,
    AspectRatio,
    OpenByDefault,
    AppInfo,
    /// The desktop's own window menu.
    MoreOptions,
}

/// The menu's entries in order: the window's own items as Windows orders them in the system
/// menu, a separator, Android's, a separator and More window options. Restore takes
/// Maximize's place while the window is maximized, and Size is left out then and for a window
/// the user may not resize, where Windows greys them out. New window is left out for an app
/// that does not support several windows, since its second task would only bring the first to
/// the front.
pub fn items(maximized: bool, resizable: bool, new_window: bool) -> Vec<MenuItem> {
    let mut items = Vec::with_capacity(14);
    if maximized {
        items.push(MenuItem::Restore);
    }
    items.push(MenuItem::Move);
    if resizable && !maximized {
        items.push(MenuItem::Size);
    }
    items.push(MenuItem::Minimize);
    if !maximized {
        items.push(MenuItem::Maximize);
    }
    items.extend([MenuItem::Close, MenuItem::Separator]);
    if new_window {
        items.push(MenuItem::NewWindow);
    }
    items.extend([
        MenuItem::Screenshot,
        MenuItem::AspectRatio,
        MenuItem::OpenByDefault,
        MenuItem::AppInfo,
        MenuItem::Separator,
        MenuItem::MoreOptions,
    ]);
    items
}

/// Moving or sizing a window, from the window menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveSize {
    Move,
    Size,
}

/// How an entry was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChosenBy {
    /// A button's release over it.
    Pointer,
    /// Enter or Space while it was highlighted.
    Keyboard,
    /// A touch on it.
    Touch,
}

/// What the window does for an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowWork {
    Minimize,
    Maximize,
    Restore,
    Close,
    /// Move or Size.
    MoveSize(MoveSize),
    /// The desktop's own window menu.
    DesktopMenu,
    Screenshot,
    /// An Android item, for the window rules.
    Android(MenuAction),
    Nothing,
}

/// What choosing `item` does.
pub fn work(item: MenuItem) -> WindowWork {
    match item {
        MenuItem::Restore => WindowWork::Restore,
        MenuItem::Move => WindowWork::MoveSize(MoveSize::Move),
        MenuItem::Size => WindowWork::MoveSize(MoveSize::Size),
        MenuItem::Minimize => WindowWork::Minimize,
        MenuItem::Maximize => WindowWork::Maximize,
        MenuItem::Close => WindowWork::Close,
        MenuItem::Screenshot => WindowWork::Screenshot,
        MenuItem::MoreOptions => WindowWork::DesktopMenu,
        MenuItem::Separator => WindowWork::Nothing,
        other => action(other).map_or(WindowWork::Nothing, WindowWork::Android),
    }
}

/// The serial a request made for a chosen entry carries (the desktop's window menu, the
/// clipboard): the serial of the input the user chose with. A key or a touch carries its own.
/// A pointer's release carries none a desktop takes for a grab, so the press before it in the
/// menu does, else the press or key that opened the menu.
pub fn choice_serial(by: ChosenBy, chosen: u32, last_press: Option<u32>, opened: u32) -> u32 {
    match by {
        ChosenBy::Keyboard | ChosenBy::Touch => chosen,
        ChosenBy::Pointer => last_press.unwrap_or(opened),
    }
}

/// The edge (`xdg_toplevel` resize edge numbering) a resize armed by Size starts from: the
/// edge or corner of the window, `size` logical units, nearest the press at `point`, by
/// thirds of each side. A press in the middle third both ways sizes from the bottom right
/// corner, which grows and shrinks the window without moving it, as Windows' keyboard Size
/// does first.
pub fn size_edge(point: (f64, f64), size: (f64, f64)) -> u32 {
    const TOP: u32 = 1;
    const BOTTOM: u32 = 2;
    const LEFT: u32 = 4;
    const RIGHT: u32 = 8;
    let third = |at: f64, side: f64, low: u32, high: u32| {
        if side <= 0.0 {
            0
        } else if at < side / 3.0 {
            low
        } else if at >= side * 2.0 / 3.0 {
            high
        } else {
            0
        }
    };
    let edge = third(point.1, size.1, TOP, BOTTOM) | third(point.0, size.0, LEFT, RIGHT);
    if edge == 0 {
        BOTTOM | RIGHT
    } else {
        edge
    }
}

/// `_NET_WM_MOVERESIZE`'s directions, as the Extended Window Manager Hints number them.
pub mod moveresize {
    pub const SIZE_BOTTOMRIGHT: u32 = 4;
    pub const MOVE: u32 = 8;
    pub const SIZE_KEYBOARD: u32 = 9;
    pub const MOVE_KEYBOARD: u32 = 10;
}

/// The `_NET_WM_MOVERESIZE` direction for Move or Size on an X11 window. With a button held
/// the window manager follows that button's drag. A menu entry is chosen by a release or a
/// key, when no button is held, so the keyboard variants are asked for: the window manager
/// then moves or sizes the window with the arrow keys and with the pointer, and a click or
/// Enter ends it, as Windows' Move and Size do.
pub fn moveresize_direction(what: MoveSize, button_held: bool) -> u32 {
    match (what, button_held) {
        (MoveSize::Move, true) => moveresize::MOVE,
        (MoveSize::Size, true) => moveresize::SIZE_BOTTOMRIGHT,
        (MoveSize::Move, false) => moveresize::MOVE_KEYBOARD,
        (MoveSize::Size, false) => moveresize::SIZE_KEYBOARD,
    }
}

/// The five values of a `_NET_WM_MOVERESIZE` client message: the pointer's place on the root
/// window, the direction, the button (0 for none) and the source (1, an application).
pub fn moveresize_message(root: (i32, i32), direction: u32, button: u32) -> [u32; 5] {
    [root.0 as u32, root.1 as u32, direction, button, 1]
}

/// The five values of a `_GTK_SHOW_WINDOW_MENU` client message, which asks the window manager
/// for its window menu at `root` (the root window's coordinates): the device (0, the core
/// pointer), then the place.
pub fn show_window_menu_message(root: (i32, i32)) -> [u32; 5] {
    [0, root.0 as u32, root.1 as u32, 0, 0]
}

/// Whether the key with evdev code `code`, pressed with these modifiers held, opens the
/// window menu: Alt+Space, as on Windows. Ctrl+Alt+Space is left to the app.
pub fn opens_window_menu(code: u32, alt: bool, ctrl: bool) -> bool {
    const SPACE: u32 = 57;
    code == SPACE && alt && !ctrl
}

/// Where the menu opens against its window, in logical units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPlace {
    /// Below this rectangle (the icon's cell): left, top, width, height.
    Below(i32, i32, i32, i32),
    /// At this point (where the title bar was right clicked).
    At(i32, i32),
}

/// Where the desktop's own window menu opens for More window options, in the window's
/// logical units: where DSA's menu opened, its top left corner.
pub fn desktop_menu_point(place: MenuPlace) -> (i32, i32) {
    match place {
        MenuPlace::Below(left, top, _, height) => (left, top + height),
        MenuPlace::At(x, y) => (x, y),
    }
}

/// What the window rules do for an entry; `None` for the window's own items, the separator
/// and Screenshot, which the window carries out itself.
pub fn action(item: MenuItem) -> Option<MenuAction> {
    match item {
        MenuItem::NewWindow => Some(MenuAction::NewWindow),
        MenuItem::AspectRatio => Some(MenuAction::AspectRatio),
        MenuItem::OpenByDefault => Some(MenuAction::OpenByDefault),
        MenuItem::AppInfo => Some(MenuAction::AppInfo),
        _ => None,
    }
}

/// What the window manager does for `action`, chosen in a window of the app `package`; `None`
/// for New window of an app that does not support several windows, which its menu never
/// offers.
pub fn menu_work(action: MenuAction, package: &str, multiple_windows: bool) -> Option<MenuWork> {
    let settings = |page| {
        MenuWork::Request(TaskRequest::OpenAppSettings {
            package_name: package.to_string(),
            page,
        })
    };
    Some(match action {
        MenuAction::NewWindow if multiple_windows => MenuWork::Request(TaskRequest::NewWindow {
            package_name: package.to_string(),
        }),
        MenuAction::NewWindow => return None,
        MenuAction::AspectRatio => settings(AppSettingsPage::AspectRatio),
        MenuAction::OpenByDefault => settings(AppSettingsPage::OpenByDefault),
        MenuAction::AppInfo => MenuWork::AppPage {
            package: package.to_string(),
        },
    })
}

/// The arguments that open DSA Settings on the Apps page with `package`'s row open.
pub fn app_page_arguments(package: &str) -> [String; 4] {
    [
        "--page".into(),
        "apps".into(),
        "--app".into(),
        package.into(),
    ]
}

/// The menu's words in one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuText {
    pub minimize: &'static str,
    pub maximize: &'static str,
    pub restore: &'static str,
    pub move_window: &'static str,
    pub size: &'static str,
    pub close: &'static str,
    pub new_window: &'static str,
    pub screenshot: &'static str,
    pub aspect_ratio: &'static str,
    pub open_by_default: &'static str,
    pub app_info: &'static str,
    pub more_options: &'static str,
    /// What a screenshot's file name starts with.
    pub screenshot_file: &'static str,
}

/// The menu's words: Korean for a session that reads Korean, else English.
pub fn menu_text(korean: bool) -> MenuText {
    if korean {
        MenuText {
            minimize: "최소화",
            maximize: "최대화",
            restore: "복원",
            move_window: "이동",
            size: "크기 조정",
            close: "닫기",
            new_window: "새 창",
            screenshot: "스크린샷",
            aspect_ratio: "화면 비율",
            open_by_default: "링크 열기 설정",
            app_info: "앱 정보",
            more_options: "창 메뉴 더 보기",
            screenshot_file: "스크린샷",
        }
    } else {
        MenuText {
            minimize: "Minimize",
            maximize: "Maximize",
            restore: "Restore",
            move_window: "Move",
            size: "Size",
            close: "Close",
            new_window: "New window",
            screenshot: "Screenshot",
            aspect_ratio: "Aspect ratio",
            open_by_default: "Open by default",
            app_info: "App info",
            more_options: "More window options",
            screenshot_file: "Screenshot",
        }
    }
}

/// The label of an entry; `None` for the separator.
pub fn label(text: &MenuText, item: MenuItem) -> Option<&'static str> {
    Some(match item {
        MenuItem::Separator => return None,
        MenuItem::Minimize => text.minimize,
        MenuItem::Maximize => text.maximize,
        MenuItem::Restore => text.restore,
        MenuItem::Move => text.move_window,
        MenuItem::Size => text.size,
        MenuItem::Close => text.close,
        MenuItem::NewWindow => text.new_window,
        MenuItem::Screenshot => text.screenshot,
        MenuItem::AspectRatio => text.aspect_ratio,
        MenuItem::OpenByDefault => text.open_by_default,
        MenuItem::AppInfo => text.app_info,
        MenuItem::MoreOptions => text.more_options,
    })
}

/// Whether a session reads Korean: the language gettext would choose its messages in, from
/// `LC_ALL`, `LC_MESSAGES` or `LANG`, the first that is set, as `var` reads them.
pub fn reads_korean_in(var: impl Fn(&str) -> Option<String>) -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| var(name).filter(|value| !value.is_empty()))
        .is_some_and(|locale| locale.starts_with("ko"))
}

/// The height of an item at 100%, in pixels.
pub(crate) const ITEM_AT_100: u32 = 32;
/// The height of the separator's row at 100%.
pub(crate) const SEPARATOR_AT_100: u32 = 9;
/// The space around the items, and before and after each label, at 100%.
pub(crate) const PADDING_AT_100: u32 = 4;
pub(crate) const LABEL_PADDING_AT_100: u32 = 12;
/// The narrowest a menu is at 100%.
pub(crate) const MIN_WIDTH_AT_100: u32 = 180;
/// The labels' text size at 100%, in pixels.
pub const TEXT_AT_100: u32 = 14;

/// The menu's colors, 0xAARRGGBB: a light menu, as the desktop's own menus are by default.
pub(crate) const BACKGROUND: u32 = 0xFFF9_F9F9;
pub(crate) const BORDER: u32 = 0x33_000000;
pub(crate) const TEXT: u32 = 0xE4_000000;
pub(crate) const HIGHLIGHT: u32 = 0x14_000000;
pub(crate) const SEPARATOR: u32 = 0x1F_000000;

/// Where each entry of a menu is, in its buffer's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuLayout {
    pub items: Vec<MenuItem>,
    pub width: u32,
    pub height: u32,
    /// Each entry's top and height, in order.
    pub rows: Vec<(u32, u32)>,
    /// Where the labels start from the left.
    pub label_left: u32,
}

impl MenuLayout {
    /// The layout of `items` at `dpi`, wide enough for the widest of `label_widths` (one per
    /// item, in pixels at that DPI).
    pub fn new(items: Vec<MenuItem>, label_widths: &[u32], dpi: u32) -> MenuLayout {
        let padding = scaled(PADDING_AT_100, dpi);
        let label_padding = scaled(LABEL_PADDING_AT_100, dpi);
        let widest = label_widths.iter().copied().max().unwrap_or(0);
        // A whole number of logical units wide, as a buffer of a scaled surface must be.
        let unit = (dpi / 96).max(1);
        let width = (widest + 2 * (padding + label_padding))
            .max(scaled(MIN_WIDTH_AT_100, dpi))
            .div_ceil(unit)
            * unit;
        let mut rows = Vec::with_capacity(items.len());
        let mut top = padding;
        for item in &items {
            let height = if *item == MenuItem::Separator {
                scaled(SEPARATOR_AT_100, dpi)
            } else {
                scaled(ITEM_AT_100, dpi)
            };
            rows.push((top, height));
            top += height;
        }
        MenuLayout {
            items,
            width,
            height: top + padding,
            rows,
            label_left: padding + label_padding,
        }
    }

    /// The entry under the point (`x`, `y`) that can be chosen; `None` off the entries and on
    /// the separator.
    pub fn item_at(&self, x: f64, y: f64) -> Option<usize> {
        if x < 0.0 || x >= f64::from(self.width) {
            return None;
        }
        self.rows
            .iter()
            .position(|(top, height)| y >= f64::from(*top) && y < f64::from(top + height))
            .filter(|&index| self.items[index] != MenuItem::Separator)
    }

    /// The entry the keyboard moves to from `from`, down or up, past the separator, wrapping
    /// round at the ends.
    pub fn step(&self, from: Option<usize>, down: bool) -> Option<usize> {
        let count = self.items.len();
        if count == 0 {
            return None;
        }
        let mut at = match (from, down) {
            (None, true) => count - 1,
            (None, false) => 0,
            (Some(at), _) => at,
        };
        for _ in 0..count {
            at = if down {
                (at + 1) % count
            } else {
                (at + count - 1) % count
            };
            if self.items[at] != MenuItem::Separator {
                return Some(at);
            }
        }
        None
    }
}

pub(crate) fn premultiplied(color: u32, coverage: f64) -> [f64; 4] {
    let alpha = f64::from(color >> 24) / 255.0 * coverage;
    [
        f64::from(color & 0xff) / 255.0 * alpha,
        f64::from((color >> 8) & 0xff) / 255.0 * alpha,
        f64::from((color >> 16) & 0xff) / 255.0 * alpha,
        alpha,
    ]
}

/// `top` over the premultiplied BGRA pixel `pixel`.
pub(crate) fn blend(pixel: &mut [u8], top: [f64; 4]) {
    let keep = 1.0 - top[3];
    for (channel, value) in pixel.iter_mut().zip(top) {
        *channel = (f64::from(*channel) * keep + value * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
}

/// The menu as premultiplied BGRA rows (`ARGB8888`), `layout.width` x `layout.height`: the
/// background with a border, the highlighted entry, the separator and each label (one per
/// item, `None` for the separator), centred top to bottom in its row.
pub fn paint_menu(
    layout: &MenuLayout,
    labels: &[Option<Coverage>],
    highlighted: Option<usize>,
    dpi: u32,
) -> Vec<u8> {
    let (width, height) = (layout.width as usize, layout.height as usize);
    let mut pixels = vec![0u8; width * height * 4];
    let border = scaled(1, dpi).max(1) as usize;
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 4;
            let edge = x < border || y < border || x >= width - border || y >= height - border;
            let color = if edge { BORDER } else { BACKGROUND };
            blend(&mut pixels[at..at + 4], premultiplied(color, 1.0));
        }
    }
    let padding = scaled(PADDING_AT_100, dpi) as usize;
    for (index, (item, (top, row_height))) in layout.items.iter().zip(&layout.rows).enumerate() {
        let (top, row_height) = (*top as usize, *row_height as usize);
        if *item == MenuItem::Separator {
            let y = top + row_height / 2;
            for x in padding..width.saturating_sub(padding) {
                let at = (y * width + x) * 4;
                blend(&mut pixels[at..at + 4], premultiplied(SEPARATOR, 1.0));
            }
            continue;
        }
        if highlighted == Some(index) {
            for y in top..(top + row_height).min(height) {
                for x in padding..width.saturating_sub(padding) {
                    let at = (y * width + x) * 4;
                    blend(&mut pixels[at..at + 4], premultiplied(HIGHLIGHT, 1.0));
                }
            }
        }
        let Some(Some(text)) = labels.get(index) else {
            continue;
        };
        let text_top = top as i64 + (row_height as i64 - i64::from(text.height)) / 2;
        let left = layout.label_left as usize;
        for row in 0..text.height as usize {
            let y = text_top + row as i64;
            if y < 0 || y as usize >= height {
                continue;
            }
            for column in 0..text.width as usize {
                let x = left + column;
                if x >= width.saturating_sub(padding) {
                    break;
                }
                let coverage = text.alpha[row * text.width as usize + column];
                if coverage == 0 {
                    continue;
                }
                let at = (y as usize * width + x) * 4;
                blend(
                    &mut pixels[at..at + 4],
                    premultiplied(TEXT, f64::from(coverage) / 255.0),
                );
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr22_the_window_menu_carries_the_android_window_menu() {
        use MenuItem::*;
        // The window's own items in the order of the Windows system menu, then Android's, then
        // the desktop's own window menu.
        assert_eq!(
            items(false, true, true),
            vec![
                Move,
                Size,
                Minimize,
                Maximize,
                Close,
                Separator,
                NewWindow,
                Screenshot,
                AspectRatio,
                OpenByDefault,
                AppInfo,
                Separator,
                MoreOptions
            ]
        );
        // Restore first while maximized, where Size has nothing to do; New window only for an
        // app that supports several windows.
        assert_eq!(
            items(true, true, false),
            vec![
                Restore,
                Move,
                Minimize,
                Close,
                Separator,
                Screenshot,
                AspectRatio,
                OpenByDefault,
                AppInfo,
                Separator,
                MoreOptions
            ]
        );
        // A window the user may not resize has no Size.
        assert!(!items(false, false, false).contains(&Size));
        assert!(items(false, false, false).contains(&Move));
        assert_eq!(action(NewWindow), Some(MenuAction::NewWindow));
        assert_eq!(action(AspectRatio), Some(MenuAction::AspectRatio));
        assert_eq!(action(OpenByDefault), Some(MenuAction::OpenByDefault));
        assert_eq!(action(AppInfo), Some(MenuAction::AppInfo));
        for item in [
            Screenshot,
            Minimize,
            Maximize,
            Restore,
            Close,
            Separator,
            Move,
            Size,
            MoreOptions,
        ] {
            assert_eq!(action(item), None, "{item:?}");
        }
    }

    #[test]
    fn fr22_each_window_menu_item_makes_its_request() {
        use MenuItem::*;
        assert_eq!(work(Move), WindowWork::MoveSize(MoveSize::Move));
        assert_eq!(work(Size), WindowWork::MoveSize(MoveSize::Size));
        assert_eq!(work(MoreOptions), WindowWork::DesktopMenu);
        assert_eq!(work(Restore), WindowWork::Restore);
        assert_eq!(work(Minimize), WindowWork::Minimize);
        assert_eq!(work(Maximize), WindowWork::Maximize);
        assert_eq!(work(Close), WindowWork::Close);
        assert_eq!(work(Screenshot), WindowWork::Screenshot);
        assert_eq!(work(Separator), WindowWork::Nothing);
        assert_eq!(work(AppInfo), WindowWork::Android(MenuAction::AppInfo));
        // On X11 the menu's choice comes with no button held, so the window manager is asked
        // for its keyboard move and size, which follow the pointer too.
        assert_eq!(
            moveresize_direction(MoveSize::Move, false),
            moveresize::MOVE_KEYBOARD
        );
        assert_eq!(
            moveresize_direction(MoveSize::Size, false),
            moveresize::SIZE_KEYBOARD
        );
        assert_eq!(moveresize_direction(MoveSize::Move, true), moveresize::MOVE);
        assert_eq!(
            moveresize_direction(MoveSize::Size, true),
            moveresize::SIZE_BOTTOMRIGHT
        );
        assert_eq!(moveresize_message((10, 20), 10, 0), [10, 20, 10, 0, 1]);
        assert_eq!(show_window_menu_message((5, 6)), [0, 5, 6, 0, 0]);
    }

    #[test]
    fn fr22_a_window_menu_request_carries_the_serial_of_the_input_that_chose_it() {
        // A key or a touch carries its own serial.
        assert_eq!(choice_serial(ChosenBy::Keyboard, 30, Some(20), 10), 30);
        assert_eq!(choice_serial(ChosenBy::Touch, 31, None, 10), 31);
        // A release's serial is no grab's: the press in the menu before it, else the press or
        // key that opened the menu.
        assert_eq!(choice_serial(ChosenBy::Pointer, 32, Some(20), 10), 20);
        assert_eq!(choice_serial(ChosenBy::Pointer, 32, None, 10), 10);
    }

    #[test]
    fn fr22_size_from_the_window_menu_starts_at_the_edge_nearest_the_press() {
        let size = (300.0, 300.0);
        // Thirds of each side: corners, edges, and the bottom right from the middle.
        assert_eq!(size_edge((10.0, 10.0), size), 5);
        assert_eq!(size_edge((290.0, 290.0), size), 10);
        assert_eq!(size_edge((150.0, 10.0), size), 1);
        assert_eq!(size_edge((10.0, 150.0), size), 4);
        assert_eq!(size_edge((290.0, 150.0), size), 8);
        assert_eq!(size_edge((150.0, 290.0), size), 2);
        assert_eq!(size_edge((150.0, 150.0), size), 10);
        assert_eq!(size_edge((0.0, 0.0), (0.0, 0.0)), 10);
    }

    #[test]
    fn fr22_more_window_options_opens_where_the_menu_opened() {
        assert_eq!(desktop_menu_point(MenuPlace::Below(8, 4, 32, 24)), (8, 28));
        assert_eq!(desktop_menu_point(MenuPlace::At(120, 10)), (120, 10));
    }

    #[test]
    fn fr22_alt_space_opens_the_window_menu() {
        assert!(opens_window_menu(57, true, false));
        assert!(!opens_window_menu(57, false, false));
        assert!(!opens_window_menu(57, true, true));
        assert!(!opens_window_menu(30, true, false));
    }

    #[test]
    fn fr22_each_window_menu_item_does_what_android_s_does() {
        let package = "com.example.notes";
        assert_eq!(
            menu_work(MenuAction::NewWindow, package, true),
            Some(MenuWork::Request(TaskRequest::NewWindow {
                package_name: package.into()
            }))
        );
        assert_eq!(menu_work(MenuAction::NewWindow, package, false), None);
        assert_eq!(
            menu_work(MenuAction::AspectRatio, package, false),
            Some(MenuWork::Request(TaskRequest::OpenAppSettings {
                package_name: package.into(),
                page: AppSettingsPage::AspectRatio
            }))
        );
        assert_eq!(
            menu_work(MenuAction::OpenByDefault, package, false),
            Some(MenuWork::Request(TaskRequest::OpenAppSettings {
                package_name: package.into(),
                page: AppSettingsPage::OpenByDefault
            }))
        );
        assert_eq!(
            menu_work(MenuAction::AppInfo, package, false),
            Some(MenuWork::AppPage {
                package: package.into()
            })
        );
        assert_eq!(
            app_page_arguments(package),
            ["--page", "apps", "--app", package].map(String::from)
        );
    }

    #[test]
    fn fr22_the_window_menu_speaks_korean_and_english() {
        for korean in [false, true] {
            let text = menu_text(korean);
            let labels: Vec<&str> = items(false, true, true)
                .into_iter()
                .filter_map(|item| label(&text, item))
                .collect();
            assert_eq!(labels.len(), 11);
            assert!(labels.iter().all(|label| !label.is_empty()));
        }
        assert_eq!(menu_text(false).new_window, "New window");
        assert_eq!(menu_text(true).app_info, "앱 정보");
        assert_eq!(menu_text(false).more_options, "More window options");
        assert_eq!(menu_text(true).more_options, "창 메뉴 더 보기");
        assert_eq!(menu_text(true).move_window, "이동");
        assert_eq!(menu_text(true).size, "크기 조정");
        assert_eq!(menu_text(true).screenshot_file, "스크린샷");
        let korean = |name: &str| (name == "LANG").then(|| "ko_KR.UTF-8".to_string());
        let english = |name: &str| (name == "LANG").then(|| "en_US.UTF-8".to_string());
        // LC_ALL comes first, as gettext reads it.
        let both = |name: &str| match name {
            "LC_ALL" => Some("en_GB.UTF-8".to_string()),
            "LANG" => Some("ko_KR.UTF-8".to_string()),
            _ => None,
        };
        assert!(reads_korean_in(korean));
        assert!(!reads_korean_in(english));
        assert!(!reads_korean_in(both));
    }

    #[test]
    fn fr22_the_window_menu_is_chosen_by_pointer_and_keyboard() {
        use MenuItem::*;
        let entries = || {
            vec![
                Minimize,
                Maximize,
                Close,
                Separator,
                Screenshot,
                AspectRatio,
                OpenByDefault,
                AppInfo,
            ]
        };
        let layout = MenuLayout::new(entries(), &[60; 8], 96);
        assert_eq!(layout.width, 180);
        // Padding 4, then three 32 px items, the 9 px separator, four items, padding 4.
        assert_eq!(layout.height, 4 + 3 * 32 + 9 + 4 * 32 + 4);
        assert_eq!(layout.item_at(10.0, 5.0), Some(0));
        assert_eq!(layout.item_at(10.0, 4.0 + 3.0 * 32.0 + 4.0), None);
        assert_eq!(layout.item_at(10.0, 4.0 + 3.0 * 32.0 + 9.0), Some(4));
        assert_eq!(layout.item_at(-1.0, 5.0), None);
        assert_eq!(layout.item_at(10.0, 1000.0), None);
        // The keyboard skips the separator and wraps round.
        assert_eq!(layout.step(None, true), Some(0));
        assert_eq!(layout.step(Some(2), true), Some(4));
        assert_eq!(layout.step(Some(4), false), Some(2));
        assert_eq!(layout.step(Some(0), false), Some(7));
        // A long label widens the menu, and the menu scales.
        assert_eq!(MenuLayout::new(entries(), &[300], 96).width, 300 + 32);
        assert_eq!(
            MenuLayout::new(entries(), &[60; 8], 192).height,
            2 * layout.height
        );
    }

    #[test]
    fn fr22_the_window_menu_draws_its_labels_and_highlight() {
        let layout = MenuLayout::new(vec![MenuItem::Screenshot, MenuItem::AppInfo], &[4], 96);
        let label = Coverage {
            width: 4,
            height: 2,
            alpha: vec![0xff; 8],
        };
        let labels = vec![Some(label.clone()), Some(label)];
        let plain = paint_menu(&layout, &labels, None, 96);
        let lit = paint_menu(&layout, &labels, Some(1), 96);
        let width = layout.width as usize;
        let pixel = |pixels: &[u8], x: usize, y: usize| {
            let at = (y * width + x) * 4;
            pixels[at..at + 4].to_vec()
        };
        // The background is opaque, and the highlight darkens the second row only.
        let (first_top, _) = layout.rows[0];
        let (second_top, _) = layout.rows[1];
        assert_eq!(pixel(&plain, 6, first_top as usize + 2)[3], 0xff);
        assert_eq!(
            pixel(&plain, 6, first_top as usize + 2),
            pixel(&lit, 6, first_top as usize + 2)
        );
        assert_ne!(
            pixel(&plain, 6, second_top as usize + 2),
            pixel(&lit, 6, second_top as usize + 2)
        );
        // The label is drawn dark at its place, centred in its row.
        let y = first_top as usize + (32 - 2) / 2;
        let text = pixel(&plain, layout.label_left as usize, y);
        assert!(text[0] < 0x40, "{text:?}");
    }
}
