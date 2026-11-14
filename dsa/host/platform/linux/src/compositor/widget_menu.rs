//! The menu of a widget window, with no Wayland or X11 in it: what the platform-neutral menu
//! offers (`dsa_host_core::widget_windows::menu`), laid out and drawn as the task windows'
//! menu is ([`super::window_menu`]).
//!
//! The desktop's own menus cannot be opened for a surface of DSA's, so the menu is a popup
//! DSA draws, in the look of the window menu. GNOME's menus show a submenu as a section with
//! its name over its items rather than as a menu beside the menu, and so does this one:
//! Widget settings (for a widget that can be set up again), then Resize with its sizes under
//! it, the current size checked, a separator, and Remove. The arrow keys move through the
//! items, Enter or Space chooses, and Escape or a click elsewhere closes it.

use dsa_host_core::compositor::caption::scaled;
use dsa_host_core::widget_windows::menu::Entry;

use super::title_bar::Coverage;
use super::window_menu::{
    blend, premultiplied, BACKGROUND, BORDER, HIGHLIGHT, ITEM_AT_100, LABEL_PADDING_AT_100,
    MIN_WIDTH_AT_100, PADDING_AT_100, SEPARATOR, SEPARATOR_AT_100, TEXT,
};

/// The color of a section's name: the labels' color, dimmer.
const HEADER: u32 = 0x8C_000000;

/// How far the sizes sit in from the labels, at 100%: past the check mark's column.
const INDENT_AT_100: u32 = 20;

/// The height of a section's name at 100%.
const HEADER_AT_100: u32 = 26;

/// One row of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// An item the user chooses, by its command; a size is `checked` when the widget is at it,
    /// and `indented` under its section's name.
    Item {
        command: u32,
        label: String,
        checked: bool,
        indented: bool,
    },
    /// A section's name, which is not chosen: Resize.
    Header(String),
    Separator,
}

impl Row {
    fn chosen(&self) -> bool {
        matches!(self, Row::Item { .. })
    }

    /// The text the row shows; none for a separator.
    pub fn label(&self) -> Option<&str> {
        match self {
            Row::Item { label, .. } | Row::Header(label) => Some(label),
            Row::Separator => None,
        }
    }
}

/// The rows of the menu's `entries`: a submenu becomes a section, its name over its items.
pub fn rows(entries: &[Entry]) -> Vec<Row> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            Entry::Item {
                command,
                label,
                checked,
            } => out.push(Row::Item {
                command: *command,
                label: label.clone(),
                checked: *checked,
                indented: false,
            }),
            Entry::Sizes { label, sizes } => {
                out.push(Row::Header(label.clone()));
                for size in sizes {
                    if let Entry::Item {
                        command,
                        label,
                        checked,
                    } = size
                    {
                        out.push(Row::Item {
                            command: *command,
                            label: label.clone(),
                            checked: *checked,
                            indented: true,
                        });
                    }
                }
            }
            Entry::Separator => out.push(Row::Separator),
        }
    }
    out
}

/// Where each row of a widget's menu is, in its buffer's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetMenuLayout {
    pub rows: Vec<Row>,
    pub width: u32,
    pub height: u32,
    /// Each row's top and height, in order.
    pub tops: Vec<(u32, u32)>,
    /// Where a label starts from the left, and an indented one.
    pub label_left: u32,
    pub indent_left: u32,
}

impl WidgetMenuLayout {
    /// The layout of `rows` at `dpi`, wide enough for the widest of `label_widths` (one per
    /// row, in pixels at that DPI).
    pub fn new(rows: Vec<Row>, label_widths: &[u32], dpi: u32) -> WidgetMenuLayout {
        let padding = scaled(PADDING_AT_100, dpi);
        let label_padding = scaled(LABEL_PADDING_AT_100, dpi);
        let indent = scaled(INDENT_AT_100, dpi);
        let widest = rows
            .iter()
            .zip(label_widths.iter().copied().chain(std::iter::repeat(0)))
            .map(|(row, width)| match row {
                Row::Item { indented: true, .. } => width + indent,
                _ => width,
            })
            .max()
            .unwrap_or(0);
        // A whole number of logical units wide, as a buffer of a scaled surface must be.
        let unit = (dpi / 96).max(1);
        let width = (widest + 2 * (padding + label_padding))
            .max(scaled(MIN_WIDTH_AT_100, dpi))
            .div_ceil(unit)
            * unit;
        let mut tops = Vec::with_capacity(rows.len());
        let mut top = padding;
        for row in &rows {
            let height = match row {
                Row::Separator => scaled(SEPARATOR_AT_100, dpi),
                Row::Header(_) => scaled(HEADER_AT_100, dpi),
                Row::Item { .. } => scaled(ITEM_AT_100, dpi),
            };
            tops.push((top, height));
            top += height;
        }
        let height = (top + padding).div_ceil(unit) * unit;
        WidgetMenuLayout {
            rows,
            width,
            height,
            tops,
            label_left: padding + label_padding,
            indent_left: padding + label_padding + indent,
        }
    }

    /// The item under the point (`x`, `y`) that can be chosen; `None` off the items, on a
    /// section's name and on the separator.
    pub fn item_at(&self, x: f64, y: f64) -> Option<usize> {
        if x < 0.0 || x >= f64::from(self.width) {
            return None;
        }
        self.tops
            .iter()
            .position(|(top, height)| y >= f64::from(*top) && y < f64::from(top + height))
            .filter(|&index| self.rows[index].chosen())
    }

    /// The item the keyboard moves to from `from`, down or up, past the names and the
    /// separator, wrapping round at the ends.
    pub fn step(&self, from: Option<usize>, down: bool) -> Option<usize> {
        let count = self.rows.len();
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
            if self.rows[at].chosen() {
                return Some(at);
            }
        }
        None
    }

    /// The command of the item at `index`.
    pub fn command(&self, index: usize) -> Option<u32> {
        match self.rows.get(index)? {
            Row::Item { command, .. } => Some(*command),
            _ => None,
        }
    }
}

/// Lays `text` (its coverage) in `color` into the row at `top` of `row_height`, from `left`,
/// cut before `right`.
fn draw_text(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    text: &Coverage,
    (left, right): (usize, usize),
    (top, row_height): (usize, usize),
    color: u32,
) {
    let text_top = top as i64 + (row_height as i64 - i64::from(text.height)) / 2;
    for row in 0..text.height as usize {
        let y = text_top + row as i64;
        if y < 0 || y as usize >= height {
            continue;
        }
        for column in 0..text.width as usize {
            let x = left + column;
            if x >= right {
                break;
            }
            let coverage = text.alpha[row * text.width as usize + column];
            if coverage == 0 {
                continue;
            }
            let at = (y as usize * width + x) * 4;
            blend(
                &mut pixels[at..at + 4],
                premultiplied(color, f64::from(coverage) / 255.0),
            );
        }
    }
}

/// The menu as premultiplied BGRA rows (`ARGB8888`), `layout.width` x `layout.height`: the
/// background with a border, the highlighted item, the separator, each label (one per row,
/// `None` for the separator), the sections' names dimmer, and `check` (the check mark's
/// coverage) before the checked size.
pub fn paint_widget_menu(
    layout: &WidgetMenuLayout,
    labels: &[Option<Coverage>],
    check: Option<&Coverage>,
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
    let right = width.saturating_sub(padding);
    for (index, (row, (top, row_height))) in layout.rows.iter().zip(&layout.tops).enumerate() {
        let (top, row_height) = (*top as usize, *row_height as usize);
        match row {
            Row::Separator => {
                let y = top + row_height / 2;
                for x in padding..right {
                    let at = (y * width + x) * 4;
                    blend(&mut pixels[at..at + 4], premultiplied(SEPARATOR, 1.0));
                }
                continue;
            }
            Row::Header(_) => {
                if let Some(Some(text)) = labels.get(index) {
                    let left = layout.label_left as usize;
                    draw_text(
                        &mut pixels,
                        width,
                        height,
                        text,
                        (left, right),
                        (top, row_height),
                        HEADER,
                    );
                }
                continue;
            }
            Row::Item {
                checked, indented, ..
            } => {
                if highlighted == Some(index) {
                    for y in top..(top + row_height).min(height) {
                        for x in padding..right {
                            let at = (y * width + x) * 4;
                            blend(&mut pixels[at..at + 4], premultiplied(HIGHLIGHT, 1.0));
                        }
                    }
                }
                let left = if *indented {
                    layout.indent_left as usize
                } else {
                    layout.label_left as usize
                };
                if let (true, Some(check)) = (*checked, check) {
                    draw_text(
                        &mut pixels,
                        width,
                        height,
                        check,
                        (layout.label_left as usize, left),
                        (top, row_height),
                        TEXT,
                    );
                }
                if let Some(Some(text)) = labels.get(index) {
                    draw_text(
                        &mut pixels,
                        width,
                        height,
                        text,
                        (left, right),
                        (top, row_height),
                        TEXT,
                    );
                }
            }
        }
    }
    pixels
}

/// The check mark before the current size.
pub const CHECK: &str = "✓";

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::widget_windows::menu::{entries, menu_text, REMOVE, SETTINGS};
    use dsa_host_core::widgets::{Placement, WidgetProvider};

    fn clock(reconfigurable: bool) -> WidgetProvider {
        WidgetProvider {
            provider: "com.example.clock/.ClockWidget".into(),
            min_width_dp: 110,
            min_height_dp: 40,
            default_width_dp: 180,
            default_height_dp: 110,
            resize_mode: 3,
            reconfigurable,
            ..WidgetProvider::default()
        }
    }

    fn at_default() -> Placement {
        Placement {
            x: 0,
            y: 0,
            width_dp: 180,
            height_dp: 110,
            resized: false,
        }
    }

    #[test]
    fn fr12_right_clicking_a_widget_offers_resize_and_remove() {
        let menu = rows(&entries(&menu_text(false), &clock(false), &at_default()));
        assert_eq!(menu[0], Row::Header("Resize".into()));
        let sizes: Vec<&Row> = menu
            .iter()
            .filter(|row| matches!(row, Row::Item { indented: true, .. }))
            .collect();
        assert!(sizes.len() >= 3);
        assert!(sizes.iter().any(|row| matches!(
            row,
            Row::Item { label, checked: true, .. } if label == "Default size (180 × 110)"
        )));
        assert_eq!(menu[menu.len() - 2], Row::Separator);
        assert!(matches!(
            menu.last(),
            Some(Row::Item { command: REMOVE, label, .. }) if label == "Remove"
        ));
    }

    #[test]
    fn fr12_the_widget_menu_offers_settings_only_for_a_widget_that_can_be_set_up_again() {
        let plain = rows(&entries(&menu_text(true), &clock(false), &at_default()));
        assert!(!plain.iter().any(|row| matches!(
            row,
            Row::Item {
                command: SETTINGS,
                ..
            }
        )));
        let settable = rows(&entries(&menu_text(true), &clock(true), &at_default()));
        assert!(matches!(
            &settable[0],
            Row::Item { command: SETTINGS, label, indented: false, .. } if label == "위젯 설정"
        ));
        assert_eq!(settable[1], Row::Header("크기 조정".into()));
    }

    #[test]
    fn fr12_the_widget_menu_skips_its_section_names_and_separator() {
        let menu = rows(&entries(&menu_text(false), &clock(true), &at_default()));
        let widths = vec![80; menu.len()];
        let layout = WidgetMenuLayout::new(menu, &widths, 96);
        // Down from nothing wraps to the last item (Remove); up from the first item wraps too.
        let last = layout.step(None, true).unwrap();
        assert_eq!(layout.command(last), Some(REMOVE));
        let first = layout.step(None, false).unwrap();
        assert_eq!(layout.command(first), Some(SETTINGS));
        // The next item down from Widget settings is the first size, past Resize's name.
        let next = layout.step(Some(first), true).unwrap();
        assert!(matches!(
            layout.rows[next],
            Row::Item { indented: true, .. }
        ));
        // A point on Resize's name chooses nothing; one on a size chooses it.
        let (header_top, _) = layout.tops[1];
        assert_eq!(layout.item_at(20.0, f64::from(header_top) + 2.0), None);
        let (size_top, _) = layout.tops[next];
        assert_eq!(layout.item_at(20.0, f64::from(size_top) + 2.0), Some(next));
        // Sizes sit in from the other labels.
        assert!(layout.indent_left > layout.label_left);
        // At 200% the menu is a whole number of logical pixels.
        let menu = rows(&entries(&menu_text(false), &clock(true), &at_default()));
        let layout = WidgetMenuLayout::new(menu, &[81; 8], 192);
        assert_eq!(layout.width % 2, 0);
        assert_eq!(layout.height % 2, 0);
    }

    #[test]
    fn fr12_the_widget_menu_is_drawn_with_its_highlight_and_check_mark() {
        let menu = rows(&entries(&menu_text(false), &clock(false), &at_default()));
        let count = menu.len();
        let layout = WidgetMenuLayout::new(menu, &vec![10; count], 96);
        let label = Coverage {
            width: 10,
            height: 10,
            alpha: vec![255; 100],
        };
        let labels: Vec<Option<Coverage>> = layout
            .rows
            .iter()
            .map(|row| row.label().map(|_| label.clone()))
            .collect();
        let checked = layout
            .rows
            .iter()
            .position(|row| matches!(row, Row::Item { checked: true, .. }))
            .unwrap();
        let pixels = paint_widget_menu(&layout, &labels, Some(&label), Some(checked), 96);
        let width = layout.width as usize;
        let (top, row_height) = layout.tops[checked];
        let middle = (top + row_height / 2) as usize;
        // The check mark is dark, at the labels' place; the size's label is further in.
        let check_at = (middle * width + layout.label_left as usize + 2) * 4;
        assert!(pixels[check_at] < 0x40);
        let label_at = (middle * width + layout.indent_left as usize + 2) * 4;
        assert!(pixels[label_at] < 0x40);
        // The highlight darkens the row beside its label.
        let beside = (middle * width + width - 8) * 4;
        let plain_row = layout.tops[checked + 1].0 as usize + 2;
        let plain = (plain_row * width + width - 8) * 4;
        assert!(pixels[beside] < pixels[plain]);
    }
}
