//! A Windows 11 menu: the flyout a WinUI 3 `MenuFlyout` and the Windows 11 shell's context
//! menu draw, for the window manager's own windows, which have no XAML to show one.
//!
//! The menu is drawn by the window manager: rounded corners, the transient acrylic backdrop
//! (a solid fill on Windows 10, with transparency effects off and under high contrast), the
//! user's light or dark theme with the accent for the chosen item's check mark, Segoe UI
//! Variable text at WinUI's `MenuFlyoutItem` metrics ([`metrics`]), a rounded highlight
//! under the item the pointer or the keyboard is on, a Segoe Fluent Icons glyph for each item
//! that has one, and WinUI's full-width separators. An item may open a submenu, which
//! cascades beside it.
//!
//! What is platform neutral is here and tested on its own: the items ([`MenuEntry`]), their
//! layout at a DPI ([`layout`]) and what a point hits ([`MenuLayout::hit`]), where the menu
//! goes on its monitor ([`place`]), and what the keyboard and the pointer do
//! ([`MenuState`]). The windows that show it, one per open level, and their UI Automation
//! tree (a Menu of MenuItems, so Narrator reads it) are in `window` and `automation`.

#[cfg(windows)]
mod automation;
#[cfg(windows)]
mod window;

#[cfg(windows)]
pub(crate) use window::{blit, work_area};
#[cfg(windows)]
pub use window::{row_art, show, Request};

use super::fluent::{Argb, Canvas, MenuPalette};
use super::widgets::{px_of_dip, ScreenRect};

/// One item of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// What choosing the item returns.
    pub id: u32,
    pub label: String,
    /// A Segoe Fluent Icons glyph drawn before the label.
    pub glyph: Option<char>,
    /// The key (a letter or a digit) that chooses the item while the menu is open.
    pub access_key: Option<char>,
    /// Some for an item that can be checked, with whether it is.
    pub checked: Option<bool>,
    pub enabled: bool,
    /// The items of the submenu the item opens; empty for an item that is chosen.
    pub submenu: Vec<MenuEntry>,
    /// The keyboard shortcut that does the same outside the menu ("Alt+F4"), drawn at the
    /// item's right end in the secondary text color, as WinUI draws a keyboard accelerator.
    pub shortcut: Option<String>,
}

impl MenuItem {
    pub fn new(id: u32, label: impl Into<String>) -> MenuItem {
        MenuItem {
            id,
            label: label.into(),
            glyph: None,
            access_key: None,
            checked: None,
            enabled: true,
            submenu: Vec::new(),
            shortcut: None,
        }
    }

    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> MenuItem {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> MenuItem {
        self.enabled = enabled;
        self
    }

    pub fn with_glyph(mut self, glyph: char) -> MenuItem {
        self.glyph = Some(glyph);
        self
    }

    pub fn with_access_key(mut self, key: char) -> MenuItem {
        self.access_key = Some(key);
        self
    }

    pub fn with_check(mut self, checked: bool) -> MenuItem {
        self.checked = Some(checked);
        self
    }

    pub fn with_submenu(mut self, entries: Vec<MenuEntry>) -> MenuItem {
        self.submenu = entries;
        self
    }

    pub fn opens_submenu(&self) -> bool {
        !self.submenu.is_empty()
    }
}

/// An entry of a menu: an item or a separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item(MenuItem),
    Separator,
}

impl MenuEntry {
    pub fn item(&self) -> Option<&MenuItem> {
        match self {
            MenuEntry::Item(item) => Some(item),
            MenuEntry::Separator => None,
        }
    }

    /// Whether the pointer and the keyboard can land on the entry: an enabled item.
    pub fn selectable(&self) -> bool {
        self.item().is_some_and(|item| item.enabled)
    }
}

/// WinUI 3's `MenuFlyout` metrics, in device-independent pixels (pixels at a scale of 100%).
pub mod metrics {
    /// `MenuFlyoutPresenterThemePadding`: above the first item and below the last.
    pub const PRESENTER_PADDING_Y: u32 = 2;
    /// `MenuFlyoutItemMargin`: around each item's highlight, left and right.
    pub const ITEM_MARGIN_X: u32 = 4;
    /// `MenuFlyoutItemMargin`: around each item's highlight, above and below.
    pub const ITEM_MARGIN_Y: u32 = 2;
    /// `MenuFlyoutItemThemePaddingNarrow`, left and right, inside the highlight.
    pub const ITEM_PADDING_X: u32 = 11;
    /// An item's highlight: the narrow padding (4 above, 7 below) around a 19 px line.
    pub const ITEM_HEIGHT: u32 = 30;
    /// `ControlContentThemeFontSize`: the label's size.
    pub const TEXT_SIZE: u32 = 14;
    /// An item's icon and check mark.
    pub const GLYPH_SIZE: u32 = 16;
    /// The room an icon or a check mark takes before the label: the glyph and 12 px.
    pub const GLYPH_COLUMN: u32 = 28;
    /// The chevron of an item that opens a submenu.
    pub const CHEVRON_SIZE: u32 = 12;
    /// The least room between a label and its chevron.
    pub const CHEVRON_GAP: u32 = 24;
    /// `MenuFlyoutSeparatorThemePadding`, above and below the line.
    pub const SEPARATOR_PADDING_Y: u32 = 1;
    /// The separator's line, which spans the whole menu.
    pub const SEPARATOR_HEIGHT: u32 = 1;
    /// `ControlCornerRadius`: the item highlight's corners.
    pub const HIGHLIGHT_RADIUS: u32 = 4;
    /// `OverlayCornerRadius`: the menu's own corners, which DWM rounds.
    pub const CORNER_RADIUS: u32 = 8;
    /// `FlyoutThemeMinWidth` and `FlyoutThemeMaxWidth`.
    pub const MIN_WIDTH: u32 = 96;
    pub const MAX_WIDTH: u32 = 456;
    /// How far below a button a menu it opens sits.
    pub const ANCHOR_GAP: u32 = 4;
    /// The keyboard accelerator's text size (`CaptionTextBlockStyle`).
    pub const SHORTCUT_TEXT_SIZE: u32 = 12;
    /// The least room between a label and its keyboard accelerator.
    pub const SHORTCUT_GAP: u32 = 24;
}

/// A rectangle in a menu's pixels, or on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Where one entry of a menu is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The entry's place in its menu.
    pub entry: usize,
    /// The whole row, margins included, as wide as the menu: what the pointer hits.
    pub slot: Rect,
    /// An item's rounded highlight; a separator's line.
    pub highlight: Rect,
    /// Where the check mark goes, when the menu has an item that can be checked.
    pub check: Option<Rect>,
    /// Where the icon goes, when the menu has an item with one.
    pub glyph: Option<Rect>,
    /// The label's box.
    pub text: Rect,
    /// The chevron of an item that opens a submenu.
    pub chevron: Option<Rect>,
    /// The keyboard shortcut's box, right-aligned in the item.
    pub shortcut: Option<Rect>,
    pub separator: bool,
}

/// A menu laid out at a DPI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuLayout {
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    pub rows: Vec<Row>,
}

impl MenuLayout {
    /// The entry under `x`, `y` (from the menu's top left corner), when it is an item that
    /// can be chosen; never a separator or the padding at the ends.
    pub fn hit(&self, entries: &[MenuEntry], x: i32, y: i32) -> Option<usize> {
        let row = self.rows.iter().find(|row| row.slot.contains(x, y))?;
        selectable(entries, row.entry).then_some(row.entry)
    }

    /// The row of entry `entry`.
    pub fn row_of(&self, entry: usize) -> Option<&Row> {
        self.rows.iter().find(|row| row.entry == entry)
    }
}

/// Whether entry `entry` of `entries` is an item the pointer and the keyboard can land on.
fn selectable(entries: &[MenuEntry], entry: usize) -> bool {
    entries.get(entry).is_some_and(MenuEntry::selectable)
}

/// `entries` laid out at `dpi`, their labels `text_widths` pixels wide (in the same order;
/// a separator's width is not read). Every item gets a check mark column when one of them
/// can be checked, and an icon column when one has an icon, so the labels line up, as WinUI
/// lines them up.
pub fn layout(entries: &[MenuEntry], text_widths: &[i32], dpi: u32) -> MenuLayout {
    layout_with_shortcuts(entries, text_widths, &[], dpi)
}

/// [`layout`], with the keyboard shortcuts of the items that have one `shortcut_widths` pixels
/// wide (in the same order as the entries; missing widths count as 0). The shortcuts line up
/// at the right end of the menu, after the widest label and a gap, as WinUI lines up its
/// keyboard accelerators.
pub fn layout_with_shortcuts(
    entries: &[MenuEntry],
    text_widths: &[i32],
    shortcut_widths: &[i32],
    dpi: u32,
) -> MenuLayout {
    use metrics::*;
    let px = |dip: u32| px_of_dip(dip, dpi);
    let items = || entries.iter().filter_map(MenuEntry::item);
    let checks = items().any(|item| item.checked.is_some());
    let glyphs = items().any(|item| item.glyph.is_some());
    let submenus = items().any(MenuItem::opens_submenu);
    let check_x = px(ITEM_MARGIN_X) + px(ITEM_PADDING_X);
    let glyph_x = check_x + if checks { px(GLYPH_COLUMN) } else { 0 };
    let text_x = glyph_x + if glyphs { px(GLYPH_COLUMN) } else { 0 };
    let chevron = if submenus {
        px(CHEVRON_GAP) + px(CHEVRON_SIZE)
    } else {
        0
    };
    let trailing = chevron + px(ITEM_PADDING_X) + px(ITEM_MARGIN_X);
    let widest = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.item().is_some())
        .map(|(index, _)| text_widths.get(index).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
    let shortcut_of = |index: usize| {
        let has = entries
            .get(index)
            .and_then(MenuEntry::item)
            .is_some_and(|item| item.shortcut.is_some());
        has.then(|| shortcut_widths.get(index).copied().unwrap_or(0).max(0))
    };
    let widest_shortcut = (0..entries.len()).filter_map(shortcut_of).max();
    let shortcut_room = widest_shortcut.map_or(0, |width| px(SHORTCUT_GAP) + width);
    let width = (text_x + widest + shortcut_room + trailing).clamp(px(MIN_WIDTH), px(MAX_WIDTH));
    let line = px(SEPARATOR_HEIGHT).max(1);
    let item_height = px(ITEM_HEIGHT);
    let mut rows = Vec::with_capacity(entries.len());
    let mut top = px(PRESENTER_PADDING_Y);
    for (entry, each) in entries.iter().enumerate() {
        let Some(item) = each.item() else {
            let height = 2 * px(SEPARATOR_PADDING_Y) + line;
            rows.push(Row {
                entry,
                slot: Rect {
                    x: 0,
                    y: top,
                    w: width,
                    h: height,
                },
                highlight: Rect {
                    x: 0,
                    y: top + px(SEPARATOR_PADDING_Y),
                    w: width,
                    h: line,
                },
                check: None,
                glyph: None,
                text: Rect::default(),
                chevron: None,
                shortcut: None,
                separator: true,
            });
            top += height;
            continue;
        };
        let height = item_height + 2 * px(ITEM_MARGIN_Y);
        let highlight = Rect {
            x: px(ITEM_MARGIN_X),
            y: top + px(ITEM_MARGIN_Y),
            w: width - 2 * px(ITEM_MARGIN_X),
            h: item_height,
        };
        let middle = highlight.y + item_height / 2;
        let square = |x: i32, size: u32| Rect {
            x,
            y: middle - px(size) / 2,
            w: px(size),
            h: px(size),
        };
        let arrow = square(width - trailing + px(CHEVRON_GAP), CHEVRON_SIZE);
        let shortcut = shortcut_of(entry).map(|shortcut_width| Rect {
            x: (width - trailing - shortcut_width).max(text_x),
            y: highlight.y,
            w: shortcut_width,
            h: item_height,
        });
        let text_end = match shortcut {
            Some(at) => at.x - px(SHORTCUT_GAP),
            None => width - trailing,
        };
        rows.push(Row {
            entry,
            slot: Rect {
                x: 0,
                y: top,
                w: width,
                h: height,
            },
            highlight,
            check: checks.then(|| square(check_x, GLYPH_SIZE)),
            glyph: glyphs.then(|| square(glyph_x, GLYPH_SIZE)),
            text: Rect {
                x: text_x,
                y: highlight.y,
                w: (text_end - text_x).max(0),
                h: item_height,
            },
            chevron: item.opens_submenu().then_some(arrow),
            shortcut,
            separator: false,
        });
        top += height;
    }
    MenuLayout {
        width,
        height: top + px(PRESENTER_PADDING_Y),
        dpi,
        rows,
    }
}

/// Where a menu opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// At a point, as a context menu opens at the pointer: its top left corner there, or
    /// its right or bottom edge there where the screen would cut it.
    Point { x: i32, y: i32 },
    /// Under a button (left, top, right, bottom on the screen), its right edge on the
    /// button's, or above the button where the screen would cut it.
    Below(ScreenRect),
    /// Beside an item, as a submenu opens: to its right, or to its left where the screen
    /// would cut it, its top on the item's.
    Beside(ScreenRect),
}

impl Anchor {
    /// A point on the screen the menu opens near, which picks its monitor.
    pub fn point(&self) -> (i32, i32) {
        match *self {
            Anchor::Point { x, y } => (x, y),
            Anchor::Below((_, _, right, bottom)) => (right - 1, bottom - 1),
            Anchor::Beside((_, top, right, _)) => (right - 1, top),
        }
    }
}

/// Where the top left corner of a menu of `size` pixels goes for `anchor`, so all of it is
/// on the work area `area` of its monitor. `gap` is how far below a button it sits.
pub fn place(anchor: Anchor, size: (i32, i32), area: ScreenRect, gap: i32) -> (i32, i32) {
    let (w, h) = size;
    let (left, top, right, bottom) = area;
    let (x, y) = match anchor {
        Anchor::Point { x, y } => (
            if x + w > right { x - w } else { x },
            if y + h > bottom { y - h } else { y },
        ),
        Anchor::Below((button_left, button_top, button_right, button_bottom)) => {
            let x = button_right - w;
            let x = if x < left { button_left } else { x };
            let y = if button_bottom + gap + h > bottom {
                button_top - gap - h
            } else {
                button_bottom + gap
            };
            (x, y)
        }
        Anchor::Beside((item_left, item_top, item_right, _)) => {
            let x = if item_right + w > right {
                item_left - w
            } else {
                item_right
            };
            (x, item_top)
        }
    };
    (
        x.clamp(left, (right - w).max(left)),
        y.clamp(top, (bottom - h).max(top)),
    )
}

/// One open level of a menu: the menu itself, or a submenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// The entry of the level above whose submenu this is; None for the menu itself.
    pub parent: Option<usize>,
    /// The entry the pointer or the keyboard is on.
    pub focus: Option<usize>,
}

/// A key the open menu acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    /// Enter or Space.
    Enter,
    Escape,
    Tab {
        back: bool,
    },
    /// Alt, F10 or Alt+F4, which close the menu as they close a Windows menu.
    Alt,
    /// A letter or a digit, which chooses the item it is the access key of.
    Char(char),
}

/// The key a virtual-key code is in the menu, with Shift and Alt as they are. Letters and
/// digits are taken as their key, not as the character the keyboard layout types, so the
/// access keys work in every layout and with an IME on.
pub fn key_of(vk: u16, shift: bool, alt: bool) -> Option<Key> {
    Some(match vk {
        0x26 => Key::Up,
        0x28 => Key::Down,
        0x25 => Key::Left,
        0x27 => Key::Right,
        0x24 => Key::Home,
        0x23 => Key::End,
        // Enter and Space.
        0x0D | 0x20 => Key::Enter,
        0x1B => Key::Escape,
        0x09 => Key::Tab { back: shift },
        // Alt and F10.
        0x12 | 0x79 => Key::Alt,
        // F4, with Alt.
        0x73 if alt => Key::Alt,
        0x30..=0x39 | 0x41..=0x5A => Key::Char(char::from(vk as u8)),
        _ => return None,
    })
}

/// What a key or a pointer action did to the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    /// What is focused or which levels are open changed: the menu is drawn again.
    Changed,
    /// The item of this id was chosen; the menu closes.
    Invoke(u32),
    /// The menu closes with nothing chosen.
    Dismiss,
}

/// The state of an open menu: which levels are open and which entry each is on. The
/// keyboard acts on the deepest level, as in a Windows menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    levels: Vec<Level>,
    /// The level the pointer was last over, which a pause settles ([`MenuState::settle`]).
    hovered: Option<usize>,
}

impl MenuState {
    /// A menu just opened. One opened from the keyboard starts on its first item, as a
    /// Windows menu does; one opened by the pointer starts on none.
    pub fn new(entries: &[MenuEntry], keyboard: bool) -> MenuState {
        let focus = if keyboard {
            step(entries, None, true)
        } else {
            None
        };
        MenuState {
            levels: vec![Level {
                parent: None,
                focus,
            }],
            hovered: None,
        }
    }

    /// How many levels are open: 1 for the menu alone.
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    /// The entry level `level` is on.
    pub fn focus(&self, level: usize) -> Option<usize> {
        self.levels.get(level)?.focus
    }

    /// The entries of level `level` of `menu`.
    pub fn entries<'a>(&self, menu: &'a [MenuEntry], level: usize) -> &'a [MenuEntry] {
        let mut entries = menu;
        for below in self.levels.iter().take(level + 1).skip(1) {
            let parent = below.parent.and_then(|parent| entries.get(parent));
            entries = match parent.and_then(MenuEntry::item) {
                Some(item) => &item.submenu,
                None => return &[],
            };
        }
        entries
    }

    fn item<'a>(&self, menu: &'a [MenuEntry], level: usize, entry: usize) -> Option<&'a MenuItem> {
        self.entries(menu, level).get(entry)?.item()
    }

    fn focus_to(&mut self, level: usize, entry: Option<usize>) -> Outcome {
        if entry.is_none() || self.levels[level].focus == entry {
            return Outcome::Nothing;
        }
        self.levels[level].focus = entry;
        Outcome::Changed
    }

    /// Opens the submenu of `entry` at `level`, closing any deeper level, on its first item
    /// when `focus_first`.
    fn open(
        &mut self,
        menu: &[MenuEntry],
        level: usize,
        entry: usize,
        focus_first: bool,
    ) -> Outcome {
        let Some(item) = self.item(menu, level, entry) else {
            return Outcome::Nothing;
        };
        if !item.enabled || !item.opens_submenu() {
            return Outcome::Nothing;
        }
        let focus = if focus_first {
            step(&item.submenu, None, true)
        } else {
            None
        };
        self.levels.truncate(level + 1);
        self.levels[level].focus = Some(entry);
        self.levels.push(Level {
            parent: Some(entry),
            focus,
        });
        Outcome::Changed
    }

    /// Chooses `entry` at `level`: opens its submenu, or picks it.
    fn choose(
        &mut self,
        menu: &[MenuEntry],
        level: usize,
        entry: usize,
        keyboard: bool,
    ) -> Outcome {
        match self.item(menu, level, entry) {
            Some(item) if !item.enabled => Outcome::Nothing,
            Some(item) if item.opens_submenu() => {
                let open = self.levels.get(level + 1).map(|below| below.parent);
                if open == Some(Some(entry)) && !keyboard {
                    return Outcome::Nothing;
                }
                self.open(menu, level, entry, keyboard)
            }
            Some(item) => Outcome::Invoke(item.id),
            None => Outcome::Nothing,
        }
    }

    /// Closes the deepest level, when it is a submenu.
    fn close_deepest(&mut self) -> Outcome {
        if self.levels.len() > 1 {
            self.levels.pop();
            Outcome::Changed
        } else {
            Outcome::Nothing
        }
    }

    /// What `key` does: the arrows move through the items of the deepest level, skipping
    /// separators and wrapping at the ends, Right and Enter open a submenu on its first
    /// item, Left closes it, Escape closes it or the menu, Enter chooses an item, and an
    /// access key chooses its item (or moves between the items that share it).
    pub fn key(&mut self, menu: &[MenuEntry], key: Key) -> Outcome {
        let level = self.levels.len() - 1;
        let entries = self.entries(menu, level);
        let focus = self.levels[level].focus;
        match key {
            Key::Down | Key::Tab { back: false } => {
                self.focus_to(level, step(entries, focus, true))
            }
            Key::Up | Key::Tab { back: true } => self.focus_to(level, step(entries, focus, false)),
            Key::Home => self.focus_to(level, step(entries, None, true)),
            Key::End => self.focus_to(level, step(entries, None, false)),
            Key::Right => match focus {
                Some(entry) => self.open(menu, level, entry, true),
                None => Outcome::Nothing,
            },
            Key::Left => self.close_deepest(),
            Key::Enter => match focus {
                Some(entry) => self.choose(menu, level, entry, true),
                None => Outcome::Nothing,
            },
            Key::Escape => match self.close_deepest() {
                Outcome::Nothing => Outcome::Dismiss,
                outcome => outcome,
            },
            Key::Alt => Outcome::Dismiss,
            Key::Char(key) => {
                let matching: Vec<usize> = entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry.selectable())
                    .filter(|(_, entry)| {
                        let access = entry.item().and_then(|item| item.access_key);
                        access.is_some_and(|access| access.eq_ignore_ascii_case(&key))
                    })
                    .map(|(index, _)| index)
                    .collect();
                match matching.as_slice() {
                    [] => Outcome::Nothing,
                    [only] => {
                        self.levels[level].focus = Some(*only);
                        match self.choose(menu, level, *only, true) {
                            Outcome::Nothing => Outcome::Changed,
                            outcome => outcome,
                        }
                    }
                    several => {
                        let after = several
                            .iter()
                            .copied()
                            .find(|&index| focus.is_some_and(|focus| index > focus));
                        self.focus_to(level, Some(after.unwrap_or(several[0])))
                    }
                }
            }
        }
    }

    /// The pointer is over `entry` of `level` (None: over no item of it). The item takes
    /// the highlight; a submenu opens or closes only once the pointer rests
    /// ([`MenuState::settle`]), so it can cross other items on its way to a submenu.
    pub fn hover(&mut self, menu: &[MenuEntry], level: usize, entry: Option<usize>) -> Outcome {
        if level >= self.levels.len() {
            return Outcome::Nothing;
        }
        self.hovered = Some(level);
        let entries = self.entries(menu, level);
        let entry = entry.filter(|&entry| selectable(entries, entry));
        // The item whose submenu is open keeps its highlight when the pointer leaves it.
        let keep = entry.is_none() && level + 1 < self.levels.len();
        if keep || self.levels[level].focus == entry {
            return Outcome::Nothing;
        }
        self.levels[level].focus = entry;
        Outcome::Changed
    }

    /// Whether the pointer's last hover waits to open or close a submenu.
    pub fn unsettled(&self, menu: &[MenuEntry]) -> bool {
        let Some(level) = self.hovered else {
            return false;
        };
        let Some(entry) = self.focus(level) else {
            return false;
        };
        let open = self.levels.get(level + 1).map(|below| below.parent);
        let opens = self
            .item(menu, level, entry)
            .is_some_and(MenuItem::opens_submenu);
        open != Some(Some(entry)) && (opens || open.is_some())
    }

    /// The pointer rested: the submenu of the item it is on opens, and any other closes.
    pub fn settle(&mut self, menu: &[MenuEntry]) -> Outcome {
        if !self.unsettled(menu) {
            return Outcome::Nothing;
        }
        let Some(level) = self.hovered else {
            return Outcome::Nothing;
        };
        let Some(entry) = self.focus(level) else {
            return Outcome::Nothing;
        };
        self.levels.truncate(level + 1);
        self.open(menu, level, entry, false);
        Outcome::Changed
    }

    /// A click (a button released) on `entry` of `level`: opens its submenu at once, or
    /// chooses it.
    pub fn click(&mut self, menu: &[MenuEntry], level: usize, entry: usize) -> Outcome {
        if level >= self.levels.len() {
            return Outcome::Nothing;
        }
        self.levels[level].focus = Some(entry);
        self.choose(menu, level, entry, false)
    }

    /// Opens the submenu of `entry` at `level`, as UI Automation's Expand asks.
    pub fn expand(&mut self, menu: &[MenuEntry], level: usize, entry: usize) -> Outcome {
        if level >= self.levels.len() {
            return Outcome::Nothing;
        }
        self.open(menu, level, entry, false)
    }

    /// Closes the levels below `level`, as UI Automation's Collapse asks.
    pub fn collapse(&mut self, level: usize) -> Outcome {
        if self.levels.len() <= level + 1 {
            return Outcome::Nothing;
        }
        self.levels.truncate(level + 1);
        Outcome::Changed
    }

    /// Puts `level` on `entry` and closes the levels below it, as UI Automation's SetFocus
    /// asks.
    pub fn focus_on(&mut self, menu: &[MenuEntry], level: usize, entry: usize) -> Outcome {
        if level >= self.levels.len() || !selectable(self.entries(menu, level), entry) {
            return Outcome::Nothing;
        }
        self.levels.truncate(level + 1);
        self.levels[level].focus = Some(entry);
        Outcome::Changed
    }
}

/// The next entry of `entries` after `from` (before it when not `forward`) that can be
/// landed on, wrapping at the ends; from None, the first (or the last).
fn step(entries: &[MenuEntry], from: Option<usize>, forward: bool) -> Option<usize> {
    let count = entries.len();
    if count == 0 {
        return None;
    }
    let start = match (from, forward) {
        (Some(from), _) => from.min(count - 1),
        (None, true) => count - 1,
        (None, false) => 0,
    };
    (1..=count)
        .map(|n| {
            if forward {
                (start + n) % count
            } else {
                (start + count - n % count) % count
            }
        })
        .find(|&index| entries[index].selectable())
}

/// A coverage mask: one byte per pixel, 255 where the shape covers the pixel fully.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub cover: Vec<u8>,
}

/// The rasterized text and glyphs of one row, each the size of its box in the row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RowArt {
    pub label: Option<Mask>,
    pub glyph: Option<Mask>,
    pub check: Option<Mask>,
    pub chevron: Option<Mask>,
    /// The keyboard shortcut's text.
    pub shortcut: Option<Mask>,
    /// The line under the label's access key, in the menu's pixels.
    pub underline: Option<Rect>,
}

/// What one level of a menu looks like now.
pub struct Look<'a> {
    pub layout: &'a MenuLayout,
    /// The art of each row of the layout, in its order.
    pub art: &'a [RowArt],
    pub entries: &'a [MenuEntry],
    /// The entry the pointer or the keyboard is on.
    pub focus: Option<usize>,
    /// The entry being pressed.
    pub pressed: Option<usize>,
    /// Whether access keys are underlined: once the keyboard is used in the menu, as Windows
    /// shows them.
    pub cues: bool,
    pub palette: &'a MenuPalette,
}

/// The level drawn as premultiplied BGRA pixels of its layout's size.
pub fn render(look: &Look<'_>) -> Vec<u8> {
    let width = look.layout.width.max(1) as usize;
    let height = look.layout.height.max(1) as usize;
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = Canvas {
        width,
        height,
        pixels: &mut pixels,
    };
    let palette = look.palette;
    canvas.clear(palette.background);
    let radius = px_of_dip(metrics::HIGHLIGHT_RADIUS, look.layout.dpi) as f32;
    // Under high contrast the highlight is opaque, and what is on it takes its text color.
    let solid_highlight = palette.hover >> 24 == 0xff;
    for (row, art) in look.layout.rows.iter().zip(look.art) {
        let at = row.highlight;
        if row.separator {
            canvas.fill_rect(at.x, at.y, at.w, at.h, palette.separator);
            continue;
        }
        let Some(item) = look.entries.get(row.entry).and_then(MenuEntry::item) else {
            continue;
        };
        let pressed = look.pressed == Some(row.entry) && item.enabled;
        let highlighted = pressed || look.focus == Some(row.entry);
        if highlighted {
            let fill = if pressed {
                palette.pressed
            } else {
                palette.hover
            };
            let (x, y, w, h) = (at.x as f32, at.y as f32, at.w as f32, at.h as f32);
            canvas.fill_rounded(x, y, w, h, radius, fill);
        }
        let text = if !item.enabled {
            palette.text_disabled
        } else if pressed {
            palette.text_pressed
        } else if highlighted {
            palette.text_highlighted
        } else {
            palette.text
        };
        let on_highlight = |color: Argb| {
            if highlighted && solid_highlight {
                palette.text_highlighted
            } else {
                color
            }
        };
        if item.checked == Some(true) {
            let check = on_highlight(palette.check);
            draw(&mut canvas, row.check, art.check.as_ref(), check);
        }
        draw(&mut canvas, row.glyph, art.glyph.as_ref(), text);
        draw(&mut canvas, Some(row.text), art.label.as_ref(), text);
        if let (true, Some(line)) = (look.cues, art.underline) {
            canvas.fill_rect(line.x, line.y, line.w, line.h, text);
        }
        let chevron = on_highlight(palette.chevron);
        draw(&mut canvas, row.chevron, art.chevron.as_ref(), chevron);
        // The shortcut in the secondary text color, as WinUI draws a keyboard accelerator.
        let shortcut = if item.enabled {
            on_highlight(palette.chevron)
        } else {
            palette.text_disabled
        };
        draw(&mut canvas, row.shortcut, art.shortcut.as_ref(), shortcut);
    }
    pixels
}

fn draw(canvas: &mut Canvas<'_>, at: Option<Rect>, mask: Option<&Mask>, color: Argb) {
    if let (Some(at), Some(mask)) = (at, mask) {
        let Mask {
            width,
            height,
            cover,
        } = mask;
        canvas.draw_mask(at.x, at.y, *width, *height, cover, color, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::fluent::{menu_palette, Theme};

    fn size(id: u32, label: &str, key: char, on: bool) -> MenuEntry {
        let item = MenuItem::new(id, label).with_access_key(key);
        MenuEntry::Item(item.with_check(on))
    }

    /// The widget menu's shape: an item with a glyph that opens a submenu of items that can
    /// be checked, a separator, and an item with a glyph.
    fn menu() -> Vec<MenuEntry> {
        let sizes = vec![
            size(100, "Small", 'S', false),
            size(101, "Default size", 'D', true),
            size(102, "Large", 'L', false),
        ];
        let resize = MenuItem::new(0, "Resize").with_glyph('\u{E740}');
        let remove = MenuItem::new(1, "Remove").with_glyph('\u{E77A}');
        vec![
            MenuEntry::Item(resize.with_access_key('R').with_submenu(sizes)),
            MenuEntry::Separator,
            MenuEntry::Item(remove.with_access_key('M')),
        ]
    }

    fn point(x: i32, y: i32) -> Anchor {
        Anchor::Point { x, y }
    }

    #[test]
    fn fr12_the_widget_menu_has_winui_menu_flyout_metrics() {
        let entries = menu();
        let at_100 = layout(&entries, &[44, 0, 52], 96);
        let resize = at_100.rows[0];
        // The highlight is inset 4 px from the sides and 2 px from its row, 30 px high.
        let highlight = Rect {
            x: 4,
            y: 4,
            w: at_100.width - 8,
            h: 30,
        };
        assert_eq!(resize.highlight, highlight);
        assert_eq!(resize.slot.h, 34);
        // The icon sits 11 px into the highlight and the label 28 px after it.
        let glyph = resize.glyph.unwrap();
        assert_eq!((glyph.x, glyph.w, glyph.h), (15, 16, 16));
        assert_eq!(resize.text.x, 43);
        assert_eq!(resize.check, None, "no item of the menu can be checked");
        assert!(resize.chevron.is_some());
        assert_eq!(at_100.rows[2].chevron, None);
        // The separator is a 1 px line across the whole menu, 1 px from its row's ends.
        let separator = at_100.rows[1];
        assert!(separator.separator);
        assert_eq!(separator.highlight.x, 0);
        assert_eq!(separator.highlight.w, at_100.width);
        assert_eq!(separator.highlight.h, 1);
        assert_eq!(separator.slot.h, 3);
        assert_eq!(at_100.height, 2 + 34 + 3 + 34 + 2);
        assert!(at_100.width >= 96);
        // Everything grows with the scale.
        let at_150 = layout(&entries, &[66, 0, 78], 144);
        assert_eq!(at_150.rows[0].highlight.h, 45);
        assert_eq!(at_150.rows[0].highlight.x, 6);
        // The submenu has a check mark column before its labels.
        let sizes = &entries[0].item().unwrap().submenu;
        let submenu = layout(sizes, &[40; 3], 96);
        assert_eq!(submenu.rows[0].check.map(|check| check.x), Some(15));
        assert_eq!(submenu.rows[0].text.x, 43);
        // A long label is cut at the widest a flyout gets.
        let long = layout(&entries, &[2000, 0, 52], 96);
        assert_eq!(long.width, 456);
    }

    #[test]
    fn fr12_a_point_hits_the_item_under_it_and_never_a_separator() {
        let entries = menu();
        let laid = layout(&entries, &[44, 0, 52], 96);
        assert_eq!(laid.hit(&entries, 20, 20), Some(0));
        // The rows' margins count, so the pointer never falls between two items.
        assert_eq!(laid.hit(&entries, 1, 3), Some(0));
        assert_eq!(laid.hit(&entries, 20, 37), None, "the separator");
        assert_eq!(laid.hit(&entries, 20, 50), Some(2));
        assert_eq!(laid.hit(&entries, 20, 1), None, "the padding at the top");
        assert_eq!(laid.hit(&entries, laid.width, 20), None);
        let mut disabled = menu();
        if let MenuEntry::Item(item) = &mut disabled[2] {
            item.enabled = false;
        }
        assert_eq!(laid.hit(&disabled, 20, 50), None);
    }

    #[test]
    fn fr12_the_menu_opens_at_the_pointer_and_stays_on_its_monitor() {
        let area = (0, 0, 1920, 1040);
        let at = |anchor: Anchor, size: (i32, i32)| place(anchor, size, area, 4);
        // Room to the right and below: the top left corner is at the pointer.
        assert_eq!(at(point(100, 200), (180, 120)), (100, 200));
        // Near the right and bottom edges it opens to the left and above.
        assert_eq!(at(point(1900, 1000), (180, 120)), (1720, 880));
        // On a second monitor to the left.
        let left = (-1920, 0, 0, 1040);
        assert_eq!(place(point(-10, 5), (180, 120), left, 4), (-190, 5));
        // Under a button, right edges lined up, or above it at the bottom of the screen.
        let button = (1000, 100, 1028, 128);
        assert_eq!(at(Anchor::Below(button), (180, 120)), (848, 132));
        let low = (1000, 1000, 1028, 1028);
        assert_eq!(at(Anchor::Below(low), (180, 120)), (848, 876));
        // A submenu opens to the right of its item, or to the left at the screen's edge.
        let item = (500, 300, 680, 334);
        assert_eq!(at(Anchor::Beside(item), (150, 100)), (680, 300));
        let edge = (1700, 300, 1880, 334);
        assert_eq!(at(Anchor::Beside(edge), (150, 100)), (1550, 300));
        // Nothing larger than the screen goes past its top left.
        assert_eq!(at(point(5, 5), (4000, 3000)), (0, 0));
    }

    #[test]
    fn fr12_the_arrow_keys_move_through_the_items_and_skip_separators() {
        let entries = menu();
        // Opened by the pointer, nothing is focused; by the keyboard, the first item.
        assert_eq!(MenuState::new(&entries, false).focus(0), None);
        let mut state = MenuState::new(&entries, true);
        assert_eq!(state.focus(0), Some(0));
        assert_eq!(state.key(&entries, Key::Down), Outcome::Changed);
        assert_eq!(state.focus(0), Some(2), "the separator is skipped");
        assert_eq!(state.key(&entries, Key::Down), Outcome::Changed);
        assert_eq!(state.focus(0), Some(0), "it wraps past the last item");
        assert_eq!(state.key(&entries, Key::Up), Outcome::Changed);
        assert_eq!(state.focus(0), Some(2), "and before the first");
        state.key(&entries, Key::Home);
        assert_eq!(state.focus(0), Some(0));
        state.key(&entries, Key::End);
        assert_eq!(state.focus(0), Some(2));
        state.key(&entries, Key::Tab { back: true });
        assert_eq!(state.focus(0), Some(0));
        // Up from nothing lands on the last item.
        let mut state = MenuState::new(&entries, false);
        state.key(&entries, Key::Up);
        assert_eq!(state.focus(0), Some(2));
    }

    #[test]
    fn fr12_right_and_enter_open_a_submenu_and_left_and_escape_close_it() {
        let entries = menu();
        let mut state = MenuState::new(&entries, true);
        assert_eq!(state.key(&entries, Key::Right), Outcome::Changed);
        assert_eq!(state.depth(), 2);
        assert_eq!(
            state.focus(1),
            Some(0),
            "the submenu opens on its first item"
        );
        assert_eq!(state.entries(&entries, 1).len(), 3);
        state.key(&entries, Key::Down);
        assert_eq!(state.focus(1), Some(1));
        assert_eq!(state.focus(0), Some(0), "the parent keeps its highlight");
        assert_eq!(state.key(&entries, Key::Left), Outcome::Changed);
        assert_eq!(state.depth(), 1);
        assert_eq!(state.key(&entries, Key::Left), Outcome::Nothing);
        assert_eq!(state.key(&entries, Key::Enter), Outcome::Changed);
        assert_eq!(state.depth(), 2);
        assert_eq!(state.key(&entries, Key::Escape), Outcome::Changed);
        assert_eq!(state.depth(), 1);
        assert_eq!(state.key(&entries, Key::Escape), Outcome::Dismiss);
        // Right on an item without a submenu does nothing.
        let mut state = MenuState::new(&entries, false);
        state.key(&entries, Key::End);
        assert_eq!(state.key(&entries, Key::Right), Outcome::Nothing);
        assert_eq!(state.key(&entries, Key::Alt), Outcome::Dismiss);
    }

    #[test]
    fn fr12_enter_and_the_access_keys_choose_an_item() {
        let entries = menu();
        let mut state = MenuState::new(&entries, true);
        state.key(&entries, Key::Down);
        assert_eq!(state.key(&entries, Key::Enter), Outcome::Invoke(1));
        // An access key chooses its item, in either case, from anywhere in the level.
        let mut state = MenuState::new(&entries, false);
        assert_eq!(state.key(&entries, Key::Char('m')), Outcome::Invoke(1));
        // The access key of an item with a submenu opens it; then the submenu's keys work.
        let mut state = MenuState::new(&entries, false);
        assert_eq!(state.key(&entries, Key::Char('R')), Outcome::Changed);
        assert_eq!(state.depth(), 2);
        assert_eq!(state.key(&entries, Key::Char('L')), Outcome::Invoke(102));
        assert_eq!(state.key(&entries, Key::Char('Q')), Outcome::Nothing);
        // Enter with nothing focused does nothing.
        let mut state = MenuState::new(&entries, false);
        assert_eq!(state.key(&entries, Key::Enter), Outcome::Nothing);
    }

    #[test]
    fn fr12_the_keys_of_the_menu_are_read_from_virtual_key_codes() {
        assert_eq!(key_of(0x28, false, false), Some(Key::Down));
        assert_eq!(key_of(0x0D, false, false), Some(Key::Enter));
        assert_eq!(key_of(0x20, false, false), Some(Key::Enter));
        assert_eq!(key_of(0x1B, false, false), Some(Key::Escape));
        assert_eq!(key_of(0x09, true, false), Some(Key::Tab { back: true }));
        assert_eq!(key_of(0x52, false, false), Some(Key::Char('R')));
        assert_eq!(key_of(0x52, false, true), Some(Key::Char('R')));
        assert_eq!(key_of(0x73, false, true), Some(Key::Alt));
        assert_eq!(key_of(0x73, false, false), None);
        assert_eq!(key_of(0x12, false, true), Some(Key::Alt));
        assert_eq!(key_of(0x10, true, false), None, "Shift alone");
    }

    #[test]
    fn fr12_hovering_an_item_with_a_submenu_opens_it_once_the_pointer_rests() {
        let entries = menu();
        let mut state = MenuState::new(&entries, false);
        assert_eq!(state.hover(&entries, 0, Some(0)), Outcome::Changed);
        assert_eq!(state.depth(), 1, "not while the pointer moves");
        assert!(state.unsettled(&entries));
        assert_eq!(state.settle(&entries), Outcome::Changed);
        assert_eq!(state.depth(), 2);
        assert_eq!(state.focus(1), None);
        assert!(!state.unsettled(&entries));
        // Into the submenu: the parent stays highlighted.
        state.hover(&entries, 1, Some(2));
        assert_eq!(state.focus(1), Some(2));
        state.hover(&entries, 1, None);
        assert_eq!(state.focus(1), None);
        assert_eq!(state.focus(0), Some(0));
        // Back over the menu's other item: the submenu closes once the pointer rests.
        state.hover(&entries, 0, Some(2));
        assert_eq!(state.depth(), 2);
        assert_eq!(state.settle(&entries), Outcome::Changed);
        assert_eq!(state.depth(), 1);
        // Over the separator, nothing is highlighted.
        state.hover(&entries, 0, Some(1));
        assert_eq!(state.focus(0), None);
    }

    #[test]
    fn fr12_a_click_on_an_item_chooses_it_and_on_a_submenu_opens_it_at_once() {
        let entries = menu();
        let mut state = MenuState::new(&entries, false);
        assert_eq!(state.click(&entries, 0, 2), Outcome::Invoke(1));
        let mut state = MenuState::new(&entries, false);
        assert_eq!(state.click(&entries, 0, 0), Outcome::Changed);
        assert_eq!(state.depth(), 2);
        assert_eq!(
            state.click(&entries, 0, 0),
            Outcome::Nothing,
            "it stays open"
        );
        assert_eq!(state.click(&entries, 1, 1), Outcome::Invoke(101));
        // UI Automation's Expand, Collapse and SetFocus.
        let mut state = MenuState::new(&entries, false);
        assert_eq!(state.expand(&entries, 0, 0), Outcome::Changed);
        assert_eq!(state.focus_on(&entries, 1, 2), Outcome::Changed);
        assert_eq!(state.focus(1), Some(2));
        assert_eq!(state.collapse(0), Outcome::Changed);
        assert_eq!(state.depth(), 1);
        assert_eq!(
            state.focus_on(&entries, 0, 1),
            Outcome::Nothing,
            "a separator"
        );
    }

    #[test]
    fn fr12_the_highlight_is_drawn_rounded_under_the_focused_item_only() {
        let entries = menu();
        let laid = layout(&entries, &[44, 0, 52], 96);
        let art = vec![RowArt::default(); laid.rows.len()];
        let solid = Theme {
            transparency: false,
            ..Theme::light()
        };
        let palette = menu_palette(&solid);
        let look = |focus, pressed| Look {
            layout: &laid,
            art: &art,
            entries: &entries,
            focus,
            pressed,
            cues: false,
            palette: &palette,
        };
        let pixel = |pixels: &[u8], x: i32, y: i32| {
            let index = ((y * laid.width + x) * 4) as usize;
            [
                pixels[index],
                pixels[index + 1],
                pixels[index + 2],
                pixels[index + 3],
            ]
        };
        let plain = render(&look(None, None));
        let focused = render(&look(Some(2), None));
        let pressed = render(&look(Some(2), Some(2)));
        let remove = laid.rows[2].highlight;
        let (x, y) = (laid.width / 2, remove.y + 15);
        assert_eq!(pixel(&plain, x, y), [0xF9, 0xF9, 0xF9, 0xFF]);
        assert_ne!(pixel(&focused, x, y), pixel(&plain, x, y));
        assert_ne!(pixel(&pressed, x, y), pixel(&focused, x, y));
        // The highlight's corner is rounded off, and the other item is untouched.
        let (cx, cy) = (remove.x, remove.y);
        assert_eq!(pixel(&focused, cx, cy), pixel(&plain, cx, cy));
        let other = laid.rows[0].highlight.y + 15;
        assert_eq!(pixel(&focused, x, other), pixel(&plain, x, other));
        // The separator's line is drawn.
        let line = laid.rows[1].highlight;
        assert_ne!(pixel(&plain, line.x + 10, line.y), [0xF9, 0xF9, 0xF9, 0xFF]);
    }

    #[test]
    fn fr22_a_keyboard_shortcut_stands_at_the_right_end_of_its_item() {
        let entries = vec![
            MenuEntry::Item(MenuItem::new(1, "Minimize")),
            MenuEntry::Separator,
            MenuEntry::Item(MenuItem::new(2, "Close").with_shortcut("Alt+F4")),
        ];
        let laid = layout_with_shortcuts(&entries, &[60, 0, 36], &[0, 0, 40], 96);
        let close = laid.rows[2];
        let shortcut = close.shortcut.expect("Close has its shortcut");
        // Right-aligned before the item's padding, a gap after the widest label.
        assert_eq!(shortcut.x + shortcut.w, laid.width - 4 - 11);
        assert_eq!(shortcut.w, 40);
        assert!(close.text.x + close.text.w <= shortcut.x - 24);
        assert!(laid.width >= close.text.x + 60 + 24 + 40 + 15);
        assert_eq!(laid.rows[0].shortcut, None);
        // A menu without shortcuts is laid out as before.
        let plain = vec![
            MenuEntry::Item(MenuItem::new(1, "Minimize")),
            MenuEntry::Item(MenuItem::new(2, "Close")),
        ];
        let laid = layout(&plain, &[60, 36], 96);
        assert!(laid.rows.iter().all(|row| row.shortcut.is_none()));
        assert_eq!(laid.rows[0].text.w, laid.width - 15 - 15);
    }
}
