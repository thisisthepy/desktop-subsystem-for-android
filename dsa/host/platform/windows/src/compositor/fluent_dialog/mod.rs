//! A Windows 11 dialog: the look of a WinUI 3 `ContentDialog`, for the window manager's own
//! questions (the one asked before an app is stopped), which have no XAML to show one.
//!
//! The dialog is drawn by the window manager, as its menus are ([`super::fluent_menu`]): a
//! window with rounded corners and DWM's shadow on Mica where DWM draws it (the theme's solid
//! fill on Windows 10, with transparency effects off and under high contrast), the user's
//! light or dark theme, a Segoe UI Variable Display title and Segoe UI Variable Text body at
//! `ContentDialog`'s metrics ([`metrics`]), a check box, and a footer with the accent primary
//! button and a standard secondary one. The keyboard works as in WinUI: Tab and Shift+Tab move
//! the focus, which a focus rectangle shows once a key was pressed, Space toggles the check box
//! or presses the focused button, Enter presses the focused button (the primary one while the
//! check box has the focus), Escape cancels, and each control has an access key. Under a high
//! contrast theme it takes the theme's system colors.
//!
//! What is platform neutral is here and tested on its own: the text ([`DialogText`]), the
//! layout at a DPI ([`layout`]) with the body wrapped to the dialog's width ([`wrap`]), what a
//! point hits, what the keyboard and the pointer do ([`DialogState`]), the colors
//! ([`palette`]) and the drawing ([`render`]). The window that shows it is in `window`, and
//! its UI Automation tree (a dialog with its message, check box and buttons, so Narrator reads
//! and operates it as it does a Win32 dialog box) in `automation`.

#[cfg(windows)]
mod automation;
#[cfg(windows)]
mod window;

#[cfg(windows)]
pub use window::{show, Request};

use super::fluent::{Argb, Canvas, Theme};
use super::fluent_menu::{Mask, Rect};
use super::widgets::{px_of_dip, ScreenRect};

/// The words of a dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogText {
    pub title: String,
    /// Paragraphs apart by an empty line (`\n\n`); a single `\n` breaks a line.
    pub body: String,
    /// The check box's label; none when the dialog has no check box.
    pub check: Option<String>,
    pub primary: String,
    pub secondary: String,
    /// The access keys of the check box, the primary and the secondary button.
    pub keys: [Option<char>; 3],
}

/// WinUI 3's `ContentDialog` metrics, in device-independent pixels.
pub mod metrics {
    /// The dialog's width: between `ContentDialogMinWidth` (320) and `ContentDialogMaxWidth`
    /// (548), as WinUI sizes a dialog of a short question.
    pub const WIDTH: u32 = 448;
    /// `ContentDialogPadding`, around the content and the footer's buttons.
    pub const PADDING: u32 = 24;
    /// The title (`SubtitleTextBlockStyle`): 20 px semibold on a 28 px line.
    pub const TITLE_SIZE: u32 = 20;
    pub const TITLE_LINE: u32 = 28;
    /// `ContentDialogTitleMargin`: between the title and the body.
    pub const TITLE_GAP: u32 = 12;
    /// The body (`BodyTextBlockStyle`): 14 px on a 20 px line.
    pub const BODY_SIZE: u32 = 14;
    pub const BODY_LINE: u32 = 20;
    /// Between the body and the check box.
    pub const CHECK_GAP: u32 = 16;
    /// A check box: its 32 px row, its 20 px box and the label 8 px after the box.
    pub const CHECK_ROW: u32 = 32;
    pub const CHECK_BOX: u32 = 20;
    pub const CHECK_LABEL_GAP: u32 = 8;
    /// The check mark in the box.
    pub const CHECK_GLYPH: u32 = 12;
    /// The footer's buttons: 32 px high, 8 px apart (`ContentDialogButtonSpacing`).
    pub const BUTTON_HEIGHT: u32 = 32;
    pub const BUTTON_GAP: u32 = 8;
    /// `ControlCornerRadius`: buttons and the check box.
    pub const CONTROL_RADIUS: u32 = 4;
    /// `OverlayCornerRadius`: the dialog's own corners, which DWM rounds.
    pub const CORNER_RADIUS: u32 = 8;
    /// The keyboard's focus rectangle: 2 px wide, 3 px outside the control.
    pub const FOCUS_STROKE: u32 = 2;
    pub const FOCUS_MARGIN: u32 = 3;
}

/// What the text is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Title,
    Body,
}

/// A dialog laid out at a DPI, in its own pixels from its top left corner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogLayout {
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    pub title: Rect,
    /// The body's lines, as wrapped, and where each goes; an empty line is a paragraph break.
    pub lines: Vec<(String, Rect)>,
    /// The check box's whole row (what the pointer hits), its box and its label.
    pub check_row: Option<Rect>,
    pub check_box: Option<Rect>,
    pub check_label: Option<Rect>,
    /// The footer band and its buttons.
    pub footer: Rect,
    pub primary: Rect,
    pub secondary: Rect,
}

/// A control of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Check,
    Primary,
    Secondary,
}

impl DialogLayout {
    /// The control under `x`, `y`; None elsewhere, where the dialog is dragged.
    pub fn hit(&self, x: i32, y: i32) -> Option<Control> {
        if self.primary.contains(x, y) {
            Some(Control::Primary)
        } else if self.secondary.contains(x, y) {
            Some(Control::Secondary)
        } else if self.check_row.is_some_and(|row| row.contains(x, y)) {
            Some(Control::Check)
        } else {
            None
        }
    }

    /// Where the control is.
    pub fn rect_of(&self, control: Control) -> Option<Rect> {
        match control {
            Control::Primary => Some(self.primary),
            Control::Secondary => Some(self.secondary),
            Control::Check => self.check_box,
        }
    }
}

/// `text` broken into lines no wider than `width`, as `measure` measures them: at spaces
/// where it can, and inside a word only when the word alone is too wide. A `\n` breaks a
/// line, and an empty line stays one.
pub fn wrap(text: &str, width: i32, measure: &dyn Fn(&str) -> i32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if measure(&candidate) <= width {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            // A word wider than the line is cut where it no longer fits.
            for c in word.chars() {
                let longer = format!("{line}{c}");
                if !line.is_empty() && measure(&longer) > width {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(c);
            }
        }
        lines.push(line);
    }
    lines
}

/// `text` laid out at `dpi`, its lines measured by `measure` in the font they are set in.
pub fn layout(text: &DialogText, dpi: u32, measure: &dyn Fn(&str, Font) -> i32) -> DialogLayout {
    use metrics::*;
    let px = |dip: u32| px_of_dip(dip, dpi);
    let width = px(WIDTH);
    let inner = width - 2 * px(PADDING);
    let mut top = px(PADDING);
    let title = Rect {
        x: px(PADDING),
        y: top,
        w: inner,
        h: px(TITLE_LINE),
    };
    top += px(TITLE_LINE) + px(TITLE_GAP);
    let body_measure = |line: &str| measure(line, Font::Body);
    let lines: Vec<(String, Rect)> = wrap(&text.body, inner, &body_measure)
        .into_iter()
        .map(|line| {
            let rect = Rect {
                x: px(PADDING),
                y: top,
                w: inner,
                h: px(BODY_LINE),
            };
            top += px(BODY_LINE);
            (line, rect)
        })
        .collect();
    let (mut check_row, mut check_box, mut check_label) = (None, None, None);
    if text.check.is_some() {
        top += px(CHECK_GAP);
        let row = Rect {
            x: px(PADDING),
            y: top,
            w: inner,
            h: px(CHECK_ROW),
        };
        let side = px(CHECK_BOX);
        let boxed = Rect {
            x: row.x,
            y: row.y + (row.h - side) / 2,
            w: side,
            h: side,
        };
        let label_x = boxed.x + side + px(CHECK_LABEL_GAP);
        check_label = Some(Rect {
            x: label_x,
            y: row.y,
            w: row.x + row.w - label_x,
            h: row.h,
        });
        check_row = Some(row);
        check_box = Some(boxed);
        top += px(CHECK_ROW);
    }
    top += px(PADDING);
    let footer_height = 2 * px(PADDING) + px(BUTTON_HEIGHT);
    let footer = Rect {
        x: 0,
        y: top,
        w: width,
        h: footer_height,
    };
    // The two buttons share the footer's width, primary first, as WinUI places them.
    let gap = px(BUTTON_GAP);
    let button_width = (inner - gap) / 2;
    let primary = Rect {
        x: px(PADDING),
        y: top + px(PADDING),
        w: button_width,
        h: px(BUTTON_HEIGHT),
    };
    let secondary = Rect {
        x: primary.x + button_width + gap,
        w: inner - button_width - gap,
        ..primary
    };
    DialogLayout {
        width,
        height: top + footer_height,
        dpi,
        title,
        lines,
        check_row,
        check_box,
        check_label,
        footer,
        primary,
        secondary,
    }
}

/// DWM's window attributes this needs, by number: windows-sys names some only on newer SDKs.
pub const DWMWA_TRANSITIONS_FORCEDISABLED: u32 = 3;
pub const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
pub const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
pub const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
/// `DWMWCP_ROUND`: the larger of Windows 11's two corner radii, as a dialog has.
pub const DWMWCP_ROUND: i32 = 2;
/// `DWMSBT_MAINWINDOW`: Mica, the backdrop of a window that stays.
pub const DWMSBT_MAINWINDOW: i32 = 2;

/// DWM's window attributes the dialog's window takes for `theme`, as (attribute, value): dark
/// or light, no open animation, rounded corners from Windows 11, and Mica where DWM draws it
/// and the user wants it (never under high contrast).
pub fn frame_attributes(theme: &Theme) -> Vec<(u32, i32)> {
    use super::fluent::Backdrop;
    let mut attributes = vec![
        (DWMWA_USE_IMMERSIVE_DARK_MODE, i32::from(theme.dark)),
        (DWMWA_TRANSITIONS_FORCEDISABLED, 1),
    ];
    if theme.rounded() {
        attributes.push((DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND));
    }
    if theme.backdrop() == Backdrop::Acrylic {
        attributes.push((DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW));
    }
    attributes
}

/// Where a dialog of `size` goes on the work area `area`: in its middle.
pub fn centred(size: (i32, i32), area: ScreenRect) -> (i32, i32) {
    let (left, top, right, bottom) = area;
    (
        left + ((right - left - size.0) / 2).max(0),
        top + ((bottom - top - size.1) / 2).max(0),
    )
}

/// A key the dialog acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Tab {
        shift: bool,
    },
    Left,
    Right,
    Enter,
    Space,
    Escape,
    /// A letter or a digit, as an access key.
    Char(char),
}

/// The key a virtual-key code is, if the dialog acts on it.
pub fn key_of(vk: u16, shift: bool) -> Option<Key> {
    Some(match vk {
        0x09 => Key::Tab { shift },
        0x25 => Key::Left,
        0x27 => Key::Right,
        0x0D => Key::Enter,
        0x20 => Key::Space,
        0x1B => Key::Escape,
        0x30..=0x39 | 0x41..=0x5A => Key::Char(char::from(vk as u8)),
        _ => return None,
    })
}

/// How the dialog ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The primary button, with whether the check box was ticked.
    Confirmed { checked: bool },
    /// The secondary button, Escape, or the dialog closed.
    Cancelled,
}

/// What an input did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    /// The dialog must be drawn again.
    Changed,
    Done(Answer),
}

/// The dialog's state while it is open: which control has the focus, whether the check box
/// is ticked, what the pointer is on and presses, and whether the focus rectangle shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogState {
    pub focus: Control,
    pub checked: bool,
    pub hover: Option<Control>,
    pub pressed: Option<Control>,
    /// Whether a key was pressed, so the focus rectangle shows, as in WinUI.
    pub cues: bool,
    has_check: bool,
}

impl DialogState {
    /// The dialog as it opens: the primary (default) button has the focus, as WinUI gives it
    /// a `ContentDialog`'s default button, and the box is not ticked.
    pub fn new(has_check: bool) -> DialogState {
        DialogState {
            focus: Control::Primary,
            checked: false,
            hover: None,
            pressed: None,
            cues: false,
            has_check,
        }
    }

    /// Whether the dialog has a check box.
    pub fn has_check(&self) -> bool {
        self.has_check
    }

    /// What a UI Automation client asked of `control` (Invoke on a button, Toggle on the
    /// check box), done as a click on it is.
    pub fn activate(&mut self, control: Control) -> Outcome {
        if control == Control::Check && !self.has_check {
            return Outcome::Nothing;
        }
        self.invoke(control)
    }

    /// A UI Automation client moved the focus to `control`.
    pub fn focus_on(&mut self, control: Control) -> Outcome {
        if (control == Control::Check && !self.has_check) || self.focus == control {
            return Outcome::Nothing;
        }
        self.focus = control;
        Outcome::Changed
    }

    /// The controls in tab order.
    fn order(&self) -> Vec<Control> {
        let mut order = Vec::with_capacity(3);
        if self.has_check {
            order.push(Control::Check);
        }
        order.extend([Control::Primary, Control::Secondary]);
        order
    }

    fn invoke(&mut self, control: Control) -> Outcome {
        match control {
            Control::Check => {
                self.checked = !self.checked;
                self.focus = Control::Check;
                Outcome::Changed
            }
            Control::Primary => Outcome::Done(Answer::Confirmed {
                checked: self.checked,
            }),
            Control::Secondary => Outcome::Done(Answer::Cancelled),
        }
    }

    pub fn key(&mut self, key: Key, keys: &[Option<char>; 3]) -> Outcome {
        let cued = !std::mem::replace(&mut self.cues, true);
        let outcome = match key {
            Key::Tab { shift } => {
                let order = self.order();
                let at = order.iter().position(|c| *c == self.focus).unwrap_or(0);
                let n = order.len();
                self.focus = order[if shift {
                    (at + n - 1) % n
                } else {
                    (at + 1) % n
                }];
                Outcome::Changed
            }
            Key::Left | Key::Right if self.focus != Control::Check => {
                self.focus = if key == Key::Left {
                    Control::Primary
                } else {
                    Control::Secondary
                };
                Outcome::Changed
            }
            Key::Left | Key::Right => Outcome::Nothing,
            Key::Space => self.invoke(self.focus),
            Key::Enter => match self.focus {
                Control::Secondary => self.invoke(Control::Secondary),
                _ => self.invoke(Control::Primary),
            },
            Key::Escape => Outcome::Done(Answer::Cancelled),
            Key::Char(c) => {
                let controls = [Control::Check, Control::Primary, Control::Secondary];
                let hit = keys.iter().zip(controls).find(|(key, control)| {
                    key.is_some_and(|k| k.eq_ignore_ascii_case(&c))
                        && (*control != Control::Check || self.has_check)
                });
                match hit {
                    Some((_, control)) => self.invoke(control),
                    None => Outcome::Nothing,
                }
            }
        };
        match outcome {
            Outcome::Nothing if cued => Outcome::Changed,
            outcome => outcome,
        }
    }

    /// The pointer moved onto `over` (None: off every control).
    pub fn hover(&mut self, over: Option<Control>) -> Outcome {
        if self.hover == over {
            return Outcome::Nothing;
        }
        self.hover = over;
        Outcome::Changed
    }

    /// A press began on `on`.
    pub fn press(&mut self, on: Option<Control>) -> Outcome {
        self.pressed = on;
        if let Some(control) = on {
            self.focus = control;
        }
        Outcome::Changed
    }

    /// The press ended on `on`: a control is invoked when the press began on it too.
    pub fn release(&mut self, on: Option<Control>) -> Outcome {
        let pressed = self.pressed.take();
        match (pressed, on) {
            (Some(a), Some(b)) if a == b => self.invoke(a),
            (None, _) => Outcome::Nothing,
            _ => Outcome::Changed,
        }
    }
}

/// The colors a dialog is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogPalette {
    /// Behind the title, the body and the check box: see-through over Mica, else the
    /// theme's fill.
    pub background: Argb,
    /// The footer band behind the buttons, and the line above it.
    pub footer: Argb,
    pub divider: Argb,
    pub text: Argb,
    /// The primary button: the accent, lighter or dimmer under the pointer and when pressed.
    pub accent: Argb,
    pub accent_hover: Argb,
    pub accent_pressed: Argb,
    pub text_on_accent: Argb,
    /// The secondary button.
    pub button: Argb,
    pub button_hover: Argb,
    pub button_pressed: Argb,
    pub button_stroke: Argb,
    /// An unticked check box.
    pub check_fill: Argb,
    pub check_stroke: Argb,
    pub focus: Argb,
}

/// The dialog's colors for `theme`: WinUI's `ContentDialog`, `Button`, `AccentButton` and
/// `CheckBox` theme resources, or the high contrast theme's system colors.
pub fn palette(theme: &Theme) -> DialogPalette {
    use super::fluent::Backdrop;
    if let Some(hc) = theme.high_contrast {
        let opaque = |c: Argb| c | 0xFF00_0000;
        return DialogPalette {
            background: opaque(hc.window),
            footer: opaque(hc.window),
            divider: opaque(hc.text),
            text: opaque(hc.text),
            accent: opaque(hc.highlight),
            accent_hover: opaque(hc.highlight),
            accent_pressed: opaque(hc.highlight),
            text_on_accent: opaque(hc.highlight_text),
            button: opaque(hc.window),
            button_hover: opaque(hc.highlight),
            button_pressed: opaque(hc.highlight),
            button_stroke: opaque(hc.text),
            check_fill: opaque(hc.window),
            check_stroke: opaque(hc.text),
            focus: opaque(hc.text),
        };
    }
    let mica = theme.backdrop() == Backdrop::Acrylic;
    if theme.dark {
        let accent = theme.accent.light2;
        DialogPalette {
            // LayerFillColorAlt over Mica; SolidBackgroundFillColorBase without it.
            background: if mica { 0x0DFF_FFFF } else { 0xFF20_2020 },
            footer: if mica { 0x4C3A_3A3A } else { 0xFF2B_2B2B },
            divider: 0x15FF_FFFF,
            text: 0xFFFF_FFFF,
            accent,
            accent_hover: (accent & 0x00FF_FFFF) | 0xE600_0000,
            accent_pressed: (accent & 0x00FF_FFFF) | 0xCC00_0000,
            text_on_accent: 0xFF00_0000,
            button: 0xFF2D_2D2D,
            button_hover: 0xFF32_3232,
            button_pressed: 0xFF27_2727,
            button_stroke: 0x18FF_FFFF,
            check_fill: 0x0FFF_FFFF,
            check_stroke: 0x8BFF_FFFF,
            focus: 0xFFFF_FFFF,
        }
    } else {
        let accent = theme.accent.dark1;
        DialogPalette {
            background: if mica { 0x80FF_FFFF } else { 0xFFFF_FFFF },
            footer: if mica { 0x4CF3_F3F3 } else { 0xFFF3_F3F3 },
            divider: 0x0F00_0000,
            text: 0xE400_0000,
            accent,
            accent_hover: (accent & 0x00FF_FFFF) | 0xE600_0000,
            accent_pressed: (accent & 0x00FF_FFFF) | 0xCC00_0000,
            text_on_accent: 0xFFFF_FFFF,
            button: 0xFFFB_FBFB,
            button_hover: 0xFFF6_F6F6,
            button_pressed: 0xFFF5_F5F5,
            button_stroke: 0x29_000000,
            check_fill: 0x05_000000,
            check_stroke: 0x9C00_0000,
            focus: 0xE400_0000,
        }
    }
}

/// The text of a laid out dialog, rasterized at its DPI: one mask per box of the layout.
#[derive(Debug, Clone, Default)]
pub struct DialogArt {
    pub title: Option<Mask>,
    pub lines: Vec<Option<Mask>>,
    pub check_label: Option<Mask>,
    pub check_mark: Option<Mask>,
    pub primary: Option<Mask>,
    pub secondary: Option<Mask>,
}

/// The dialog, drawn: premultiplied BGRA pixels of the layout's size.
pub fn render(
    laid: &DialogLayout,
    art: &DialogArt,
    state: &DialogState,
    palette: &DialogPalette,
) -> Vec<u8> {
    use metrics::*;
    let (w, h) = (laid.width.max(0) as usize, laid.height.max(0) as usize);
    let mut pixels = vec![0u8; w * h * 4];
    let mut canvas = Canvas {
        width: w,
        height: h,
        pixels: &mut pixels,
    };
    let px = |dip: u32| px_of_dip(dip, laid.dpi) as f32;
    canvas.clear(palette.background);
    let f = laid.footer;
    canvas.fill_rect(f.x, f.y, f.w, f.h, palette.footer);
    canvas.fill_rect(
        f.x,
        f.y,
        f.w,
        px_of_dip(1, laid.dpi).max(1),
        palette.divider,
    );
    fn text(canvas: &mut Canvas<'_>, mask: &Option<Mask>, at: Rect, color: Argb) {
        if let Some(mask) = mask {
            canvas.draw_mask(at.x, at.y, mask.width, mask.height, &mask.cover, color, 1.0);
        }
    }
    text(&mut canvas, &art.title, laid.title, palette.text);
    for ((_, at), mask) in laid.lines.iter().zip(&art.lines) {
        text(&mut canvas, mask, *at, palette.text);
    }
    let radius = px(CONTROL_RADIUS);
    let shade = |control: Control, normal: Argb, hover: Argb, pressed: Argb| {
        if state.pressed == Some(control) && state.hover == Some(control) {
            pressed
        } else if state.hover == Some(control) {
            hover
        } else {
            normal
        }
    };
    if let (Some(boxed), Some(label)) = (laid.check_box, laid.check_label) {
        let (x, y, s) = (boxed.x as f32, boxed.y as f32, boxed.w as f32);
        if state.checked {
            let fill = shade(
                Control::Check,
                palette.accent,
                palette.accent_hover,
                palette.accent_pressed,
            );
            canvas.fill_rounded(x, y, s, s, radius, fill);
            if let Some(mark) = &art.check_mark {
                let mx = boxed.x + (boxed.w - mark.width as i32) / 2;
                let my = boxed.y + (boxed.h - mark.height as i32) / 2;
                let at = Rect {
                    x: mx,
                    y: my,
                    w: mark.width as i32,
                    h: mark.height as i32,
                };
                text(&mut canvas, &art.check_mark, at, palette.text_on_accent);
            }
        } else {
            canvas.fill_rounded(x, y, s, s, radius, palette.check_fill);
            canvas.stroke_rounded(x, y, s, s, radius, 1.0, palette.check_stroke);
        }
        text(&mut canvas, &art.check_label, label, palette.text);
    }
    let p = laid.primary;
    let fill = shade(
        Control::Primary,
        palette.accent,
        palette.accent_hover,
        palette.accent_pressed,
    );
    canvas.fill_rounded(p.x as f32, p.y as f32, p.w as f32, p.h as f32, radius, fill);
    text(&mut canvas, &art.primary, p, palette.text_on_accent);
    let s = laid.secondary;
    let fill = shade(
        Control::Secondary,
        palette.button,
        palette.button_hover,
        palette.button_pressed,
    );
    let (sx, sy, sw, sh) = (s.x as f32, s.y as f32, s.w as f32, s.h as f32);
    canvas.fill_rounded(sx, sy, sw, sh, radius, fill);
    canvas.stroke_rounded(sx, sy, sw, sh, radius, 1.0, palette.button_stroke);
    text(&mut canvas, &art.secondary, s, palette.text);
    if state.cues {
        if let Some(at) = laid.rect_of(state.focus) {
            let margin = px(FOCUS_MARGIN);
            canvas.stroke_rounded(
                at.x as f32 - margin,
                at.y as f32 - margin,
                at.w as f32 + 2.0 * margin,
                at.h as f32 + 2.0 * margin,
                radius + margin,
                px(FOCUS_STROKE),
                palette.focus,
            );
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::fluent::HighContrast;

    /// Every character 8 px wide, at any font.
    fn fixed(text: &str, _: Font) -> i32 {
        8 * text.chars().count() as i32
    }

    fn stop_music() -> DialogText {
        DialogText {
            title: "Stop Music?".into(),
            body: "It stops everything it does in the background, such as playing music, and \
                   the memory it used is freed."
                .into(),
            check: Some("Do not ask again".into()),
            primary: "Stop".into(),
            secondary: "Cancel".into(),
            keys: [Some('D'), Some('S'), Some('C')],
        }
    }

    #[test]
    fn fr24_the_stop_dialog_has_content_dialog_metrics() {
        for dpi in [96, 144] {
            let laid = layout(&stop_music(), dpi, &fixed);
            let px = |dip: u32| px_of_dip(dip, dpi);
            assert_eq!(laid.width, px(metrics::WIDTH));
            assert_eq!(laid.title.x, px(metrics::PADDING));
            assert_eq!(laid.title.y, px(metrics::PADDING));
            // The body is wrapped to the content's width.
            assert!(laid.lines.len() > 1);
            let inner = laid.width - 2 * px(metrics::PADDING);
            assert!(laid
                .lines
                .iter()
                .all(|(line, _)| fixed(line, Font::Body) <= inner));
            // The check box under the body, its label after the box.
            let row = laid.check_row.unwrap();
            let boxed = laid.check_box.unwrap();
            assert!(row.y >= laid.lines.last().unwrap().1.y + px(metrics::BODY_LINE));
            assert_eq!(boxed.w, px(metrics::CHECK_BOX));
            assert_eq!(
                laid.check_label.unwrap().x,
                boxed.x + boxed.w + px(metrics::CHECK_LABEL_GAP)
            );
            // The footer at the bottom, the buttons sharing its width, primary first.
            assert_eq!(laid.footer.y + laid.footer.h, laid.height);
            assert_eq!(laid.primary.h, px(metrics::BUTTON_HEIGHT));
            assert_eq!(laid.primary.y, laid.footer.y + px(metrics::PADDING));
            assert!(laid.primary.x < laid.secondary.x);
            assert_eq!(
                laid.secondary.x,
                laid.primary.x + laid.primary.w + px(metrics::BUTTON_GAP)
            );
            assert_eq!(
                laid.secondary.x + laid.secondary.w,
                laid.width - px(metrics::PADDING)
            );
            // What a point hits; elsewhere the dialog is dragged.
            assert_eq!(
                laid.hit(laid.primary.x + 1, laid.primary.y + 1),
                Some(Control::Primary)
            );
            assert_eq!(
                laid.hit(laid.secondary.x + 1, laid.secondary.y + 1),
                Some(Control::Secondary)
            );
            assert_eq!(laid.hit(row.x + row.w - 1, row.y + 1), Some(Control::Check));
            assert_eq!(laid.hit(laid.title.x, laid.title.y), None);
        }
        // Centred on the work area.
        assert_eq!(centred((400, 200), (0, 0, 1920, 1040)), (760, 420));
        assert_eq!(centred((400, 200), (1920, 0, 3840, 1080)), (2680, 440));
    }

    #[test]
    fn fr24_the_stop_dialog_wraps_its_question_at_spaces_and_breaks() {
        let measure = |text: &str| 8 * text.chars().count() as i32;
        assert_eq!(wrap("aa bb cc", 40, &measure), ["aa bb", "cc"]);
        assert_eq!(wrap("one\n\ntwo", 400, &measure), ["one", "", "two"]);
        // A word wider than the line is cut inside it.
        assert_eq!(wrap("abcdefghij", 32, &measure), ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn fr24_the_stop_dialog_works_from_the_keyboard() {
        let keys = stop_music().keys;
        // It opens on the primary button, with no focus rectangle until a key is pressed.
        let mut state = DialogState::new(true);
        assert_eq!(state.focus, Control::Primary);
        assert!(!state.cues);
        assert_eq!(
            state.key(Key::Enter, &keys),
            Outcome::Done(Answer::Confirmed { checked: false })
        );
        // Tab goes round the check box and the buttons; Space ticks the box.
        let mut state = DialogState::new(true);
        assert_eq!(
            state.key(Key::Tab { shift: false }, &keys),
            Outcome::Changed
        );
        assert!(state.cues);
        assert_eq!(state.focus, Control::Secondary);
        state.key(Key::Tab { shift: false }, &keys);
        assert_eq!(state.focus, Control::Check);
        state.key(Key::Space, &keys);
        assert!(state.checked);
        // Enter on the check box presses the primary button, with the box as it is.
        assert_eq!(
            state.key(Key::Enter, &keys),
            Outcome::Done(Answer::Confirmed { checked: true })
        );
        // Shift+Tab goes back; Enter on the secondary button and Escape cancel.
        let mut state = DialogState::new(true);
        state.key(Key::Tab { shift: true }, &keys);
        assert_eq!(state.focus, Control::Check);
        state.key(Key::Right, &keys);
        assert_eq!(state.focus, Control::Check);
        let mut state = DialogState::new(true);
        state.key(Key::Right, &keys);
        assert_eq!(state.focus, Control::Secondary);
        assert_eq!(
            state.key(Key::Enter, &keys),
            Outcome::Done(Answer::Cancelled)
        );
        let mut state = DialogState::new(true);
        assert_eq!(
            state.key(Key::Escape, &keys),
            Outcome::Done(Answer::Cancelled)
        );
        // The access keys.
        let mut state = DialogState::new(true);
        state.key(Key::Char('d'), &keys);
        assert!(state.checked);
        assert_eq!(
            state.key(Key::Char('S'), &keys),
            Outcome::Done(Answer::Confirmed { checked: true })
        );
        assert_eq!(key_of(0x09, true), Some(Key::Tab { shift: true }));
        assert_eq!(key_of(0x44, false), Some(Key::Char('D')));
        assert_eq!(key_of(0x70, false), None);
        // A dialog with no check box skips it.
        let mut state = DialogState::new(false);
        state.key(Key::Tab { shift: true }, &keys);
        assert_eq!(state.focus, Control::Secondary);
        state.key(Key::Char('D'), &keys);
        assert!(!state.checked);
    }

    #[test]
    fn fr24_the_stop_dialog_answers_the_pointer() {
        let mut state = DialogState::new(true);
        assert_eq!(state.hover(Some(Control::Check)), Outcome::Changed);
        assert_eq!(state.hover(Some(Control::Check)), Outcome::Nothing);
        state.press(Some(Control::Check));
        assert_eq!(state.release(Some(Control::Check)), Outcome::Changed);
        assert!(state.checked);
        // A press that ends elsewhere does nothing.
        state.press(Some(Control::Primary));
        assert_eq!(state.release(None), Outcome::Changed);
        state.press(Some(Control::Primary));
        assert_eq!(
            state.release(Some(Control::Primary)),
            Outcome::Done(Answer::Confirmed { checked: true })
        );
        state.press(Some(Control::Secondary));
        assert_eq!(
            state.release(Some(Control::Secondary)),
            Outcome::Done(Answer::Cancelled)
        );
    }

    #[test]
    fn fr24_the_stop_dialog_follows_the_theme_and_high_contrast() {
        let light = Theme::light();
        let p = palette(&light);
        assert_eq!(p.accent, light.accent.dark1);
        assert_eq!(p.text_on_accent, 0xFFFF_FFFF);
        // Over Mica the content is see-through.
        assert!(p.background >> 24 < 0xFF);
        let dark = palette(&Theme {
            dark: true,
            ..light
        });
        assert_eq!(dark.accent, light.accent.light2);
        assert_eq!(dark.text, 0xFFFF_FFFF);
        assert_eq!(dark.text_on_accent, 0xFF00_0000);
        // Without the backdrop the fill is opaque.
        let solid = palette(&Theme {
            transparency: false,
            ..light
        });
        assert_eq!(solid.background >> 24, 0xFF);
        let hc = HighContrast {
            window: 0xFF00_0000,
            text: 0xFFFF_FFFF,
            highlight: 0xFF1A_EBFF,
            highlight_text: 0xFF00_0000,
            gray_text: 0xFF3F_F23F,
        };
        let contrast = palette(&Theme {
            high_contrast: Some(hc),
            ..light
        });
        assert_eq!(contrast.background, hc.window);
        assert_eq!(contrast.text, hc.text);
        assert_eq!(contrast.accent, hc.highlight);
        assert_eq!(contrast.text_on_accent, hc.highlight_text);
        assert_eq!(contrast.focus, hc.text);
        // The window: rounded and on Mica on Windows 11 22H2, dark or light; a solid fill
        // under high contrast and on Windows 10, which rounds nothing.
        let value = |theme: &Theme, attribute: u32| {
            frame_attributes(theme)
                .into_iter()
                .find(|(a, _)| *a == attribute)
                .map(|(_, v)| v)
        };
        assert_eq!(
            value(&light, DWMWA_WINDOW_CORNER_PREFERENCE),
            Some(DWMWCP_ROUND)
        );
        assert_eq!(
            value(&light, DWMWA_SYSTEMBACKDROP_TYPE),
            Some(DWMSBT_MAINWINDOW)
        );
        let dark_theme = Theme {
            dark: true,
            ..light
        };
        assert_eq!(value(&dark_theme, DWMWA_USE_IMMERSIVE_DARK_MODE), Some(1));
        let hc_theme = Theme {
            high_contrast: Some(hc),
            ..light
        };
        assert_eq!(value(&hc_theme, DWMWA_SYSTEMBACKDROP_TYPE), None);
        let ten = Theme {
            build: 19045,
            ..light
        };
        assert_eq!(value(&ten, DWMWA_WINDOW_CORNER_PREFERENCE), None);
    }

    #[test]
    fn fr24_the_stop_dialog_draws_the_accent_button_the_box_and_the_focus() {
        let laid = layout(&stop_music(), 96, &fixed);
        let colors = palette(&Theme {
            transparency: false,
            ..Theme::light()
        });
        let at = |pixels: &[u8], x: i32, y: i32| {
            let i = ((y * laid.width + x) * 4) as usize;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        };
        let bgra = |c: Argb| crate::compositor::fluent::premultiplied(c, 1.0);
        let state = DialogState::new(true);
        let pixels = render(&laid, &DialogArt::default(), &state, &colors);
        assert_eq!(pixels.len(), (laid.width * laid.height * 4) as usize);
        let middle = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);
        // The primary button in the accent, the footer and the content in their fills.
        let (px_, py_) = middle(laid.primary);
        assert_eq!(at(&pixels, px_, py_), bgra(colors.accent));
        assert_eq!(at(&pixels, 1, laid.footer.y + 4), bgra(colors.footer));
        assert_eq!(at(&pixels, 1, 1), bgra(colors.background));
        // The button's corner is rounded: its very corner pixel is not the accent.
        assert_ne!(
            at(&pixels, laid.primary.x, laid.primary.y),
            bgra(colors.accent)
        );
        // A ticked box is filled with the accent.
        let ticked = DialogState {
            checked: true,
            ..state
        };
        let pixels = render(&laid, &DialogArt::default(), &ticked, &colors);
        let (bx, by) = middle(laid.check_box.unwrap());
        assert_eq!(at(&pixels, bx, by), bgra(colors.accent));
        // The focus rectangle shows only after a key, around the focused control.
        let cued = DialogState {
            cues: true,
            ..state
        };
        let margin = px_of_dip(metrics::FOCUS_MARGIN, 96);
        let probe = (laid.primary.x + laid.primary.w / 2, laid.primary.y - margin);
        let without = render(&laid, &DialogArt::default(), &state, &colors);
        let with = render(&laid, &DialogArt::default(), &cued, &colors);
        assert_ne!(at(&without, probe.0, probe.1), at(&with, probe.0, probe.1));
    }
}
