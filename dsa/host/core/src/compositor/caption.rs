//! The title bar a task window draws over the top of its app.
//!
//! A task window has no title bar of its own above the app: its client area covers the whole
//! window, the app draws to the very top, and the window draws only its caption buttons
//! (minimize, maximize or restore, close) over the app's top right corner. The guest gives
//! the task a top inset of the title bar's height, so the app keeps its own toolbar below
//! the buttons and draws its status bar background behind them.
//!
//! This module holds the rules the platform's window carries out:
//!
//! - [`CaptionLayout::hit`]: what a point in the window belongs to. The whole band of the
//!   title bar belongs to the window (its buttons, its top resize edge, and the rest a drag
//!   area), and only the points below it reach the app. An app receives the band as a system
//!   bar inset and keeps nothing it can be touched on there; sharing the band would make the
//!   same point sometimes drag the window and sometimes press the app.
//! - [`caption_tone`]: whether the buttons' glyphs are dark or light, from what the app told
//!   the system about its status bar, and [`shown_tone`]: the tone actually drawn, from the
//!   pixels of the app's frame under the band whenever they can be read ([`band_sample`]),
//!   so the glyphs stay readable over what the app really draws there.
//! - [`paint_buttons`]: the buttons as an image, in the tone and state they are in.
//! - [`paint_title_bar`]: the whole band as an image: the app's icon and name at the left
//!   (the platform lays the name out), the buttons at the right, and a fill when
//!   [`band_fill`] finds that the app leaves the band empty.
//! - [`title_bar_hit`]: the same, with the icon's cell (the system menu, or a back button
//!   while the task can go back) and an app that draws its own title bar, whose controls in
//!   the band take the pointer.
//!
//! An app may ask to draw its own title bar, as on Android's desktop windows (a transparent
//! caption bar background): its content then fills the band, as a browser puts its tabs
//! there. The window draws only its caption buttons and the icon's cell over it, never the
//! app's name or a fill, and tells the app how far those reach in from each end
//! ([`CaptionLayout::band`]), so the app keeps its controls clear of them.

use crate::tasks::{Bounds, StatusBarAppearance};

/// Whether a window's caption glyphs are dark (over a light status bar) or light.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum CaptionTone {
    /// Dark glyphs, for a light status bar. A window whose app said nothing starts so,
    /// like a window with a light title bar.
    #[default]
    DarkGlyphs,
    /// Light glyphs, for a dark status bar.
    LightGlyphs,
}

/// The tone of a task window's caption glyphs: dark over a light status bar, light over a
/// dark one. What the app said about its status bar's icons decides; when it said nothing the
/// agent could read, the status bar's color does, if it set one; otherwise the glyphs are
/// dark.
pub fn caption_tone(appearance: StatusBarAppearance, status_bar_color: u32) -> CaptionTone {
    match appearance {
        StatusBarAppearance::Light => CaptionTone::DarkGlyphs,
        StatusBarAppearance::Dark => CaptionTone::LightGlyphs,
        StatusBarAppearance::Unspecified => {
            let alpha = status_bar_color >> 24;
            if alpha < 0x80 {
                return CaptionTone::DarkGlyphs;
            }
            if relative_luminance(status_bar_color) >= 0.5 {
                CaptionTone::DarkGlyphs
            } else {
                CaptionTone::LightGlyphs
            }
        }
    }
}

/// What a window read from the pixels of its app's frame under the title bar: the color to
/// fill a band the app leaves empty ([`band_fill`]) and how light the band is
/// ([`band_luminance`]). The default is a frame that could not be sampled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BandSample {
    pub fill: Option<u32>,
    /// The band's mean relative luminance, in thousandths (0 black, 1000 white).
    pub luminance: Option<u16>,
}

/// Samples of a frame's band (`band`, from inside it) and of the row under it (`below`), each
/// 0xAARRGGBB, as a window reads them.
pub fn band_sample(band: &[u32], below: &[u32]) -> BandSample {
    BandSample {
        fill: band_fill(band, below),
        luminance: band_luminance(band),
    }
}

/// How light the app's pixels under the title bar are: the mean relative luminance of the
/// opaque samples of `band` (0xAARRGGBB each), in thousandths. `None` when fewer than half of
/// them are opaque, as over a band the app leaves empty, which says nothing about what shows
/// under the glyphs.
pub fn band_luminance(band: &[u32]) -> Option<u16> {
    let opaque: Vec<f64> = band
        .iter()
        .filter(|color| *color >> 24 >= 0xF0)
        .map(|color| relative_luminance(*color))
        .collect();
    if opaque.is_empty() || opaque.len() * 2 < band.len() {
        return None;
    }
    let mean = opaque.iter().sum::<f64>() / opaque.len() as f64;
    Some((mean * 1000.0).round().clamp(0.0, 1000.0) as u16)
}

/// Below this band luminance (thousandths) the glyphs turn light, and above
/// [`DARK_GLYPHS_ABOVE`] dark. Black and white glyphs have the same contrast over a luminance
/// of 179; at either bound the tone already shown keeps a contrast of at least 3.6 to 1, so
/// between them it stays, and a band near the middle never makes the glyphs flicker.
pub const LIGHT_GLYPHS_BELOW: u16 = 130;
pub const DARK_GLYPHS_ABOVE: u16 = 240;

/// The tone a window's caption glyphs are drawn in, given the tone the app's status bar
/// reports (`reported`, from [`caption_tone`]), what the window last read from the app's
/// frame under the band (`sample`), whether the app draws its own title bar (`app_drawn`,
/// which is never filled over), and the tone shown now (`shown`).
///
/// The pixels under the glyphs decide whenever they could be read, since an app that draws
/// edge to edge often leaves its status bar transparent, or reports icons for a status bar it
/// does not show, while its own header runs under the band. A band the window fills takes the
/// tone of the fill. Between [`LIGHT_GLYPHS_BELOW`] and [`DARK_GLYPHS_ABOVE`] the shown tone
/// stays. Only a frame that could not be sampled leaves the reported tone.
pub fn shown_tone(
    reported: CaptionTone,
    sample: BandSample,
    app_drawn: bool,
    shown: CaptionTone,
) -> CaptionTone {
    if let Some(fill) = sample.fill.filter(|_| !app_drawn) {
        return caption_tone(StatusBarAppearance::Unspecified, fill);
    }
    match sample.luminance {
        None => reported,
        Some(luminance) if luminance < LIGHT_GLYPHS_BELOW => CaptionTone::LightGlyphs,
        Some(luminance) if luminance > DARK_GLYPHS_ABOVE => CaptionTone::DarkGlyphs,
        Some(_) => shown,
    }
}

/// The luminance of an 0xAARRGGBB color, from 0 (black) to 1 (white), with the sRGB weights.
pub(crate) fn relative_luminance(argb: u32) -> f64 {
    let channel = |shift: u32| {
        let value = f64::from((argb >> shift) & 0xff) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// One of the caption buttons, from left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptionButton {
    Minimize,
    Maximize,
    Close,
}

impl CaptionButton {
    /// The buttons in the order they stand, left to right.
    pub const ALL: [CaptionButton; 3] = [
        CaptionButton::Minimize,
        CaptionButton::Maximize,
        CaptionButton::Close,
    ];

    fn index(self) -> u32 {
        match self {
            CaptionButton::Minimize => 0,
            CaptionButton::Maximize => 1,
            CaptionButton::Close => 2,
        }
    }
}

/// What a point of a task window belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptionHit {
    /// The app: the point goes to the guest.
    Client,
    /// The title bar's drag area: dragging moves the window, a double click maximizes or
    /// restores it, and a right click opens the system menu.
    Caption,
    /// A caption button.
    Button(CaptionButton),
    /// The top resize edge, and its two corners.
    Top,
    TopLeft,
    TopRight,
}

/// How far the parts a window draws over the title bar band reach into it, in client pixels:
/// the band's height, the icon's cell from the left end and the caption buttons from the
/// right end. The guest receives it as the task's caption inset and, for an app that draws
/// its own title bar, as the bounding rectangles of that inset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CaptionBand {
    pub height: u32,
    pub start_width: u32,
    pub end_width: u32,
}

/// What a point of a task window's title bar does, the icon's cell and an app's own title
/// bar included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitleBarHit {
    /// As [`CaptionLayout::hit`] says: the app below the band (or an app's control in it),
    /// the drag area, a caption button or the top edge.
    Window(CaptionHit),
    /// The icon's cell while it shows the icon: it opens the window's system menu.
    SystemMenu,
    /// The icon's cell while it shows the back arrow: a press sends the task Back.
    Back,
}

/// What a window's title bar shows besides its layout: whether the app draws its own title
/// bar, where its controls are in the band (client pixels), and whether the icon's cell
/// shows the back arrow.
#[derive(Debug, Clone, Copy, Default)]
pub struct TitleBarMode<'a> {
    pub app_drawn: bool,
    pub interactive: &'a [Bounds],
    pub back: bool,
}

/// What the client point (`x`, `y`) of a window laid out as `layout` and showing `mode`
/// belongs to. The top resize edge and the caption buttons come first, then the icon's cell,
/// then, in the band of an app that draws its own title bar, the app's controls, which go to
/// the app; the rest of the band drags the window.
pub fn title_bar_hit(layout: &CaptionLayout, x: i32, y: i32, mode: &TitleBarMode) -> TitleBarHit {
    let hit = layout.hit(x, y);
    if hit != CaptionHit::Caption {
        return TitleBarHit::Window(hit);
    }
    if layout.on_icon(x, y) {
        return if mode.back {
            TitleBarHit::Back
        } else {
            TitleBarHit::SystemMenu
        };
    }
    let on_control = mode.app_drawn
        && mode
            .interactive
            .iter()
            .any(|area| x >= area.left && x < area.right && y >= area.top && y < area.bottom);
    if on_control {
        TitleBarHit::Window(CaptionHit::Client)
    } else {
        TitleBarHit::Window(CaptionHit::Caption)
    }
}

/// The width of a caption button at 100%, in pixels.
pub const BUTTON_WIDTH_AT_100: u32 = 46;

/// The side of a caption glyph at 100%, in pixels.
const GLYPH_AT_100: f64 = 10.0;

/// The side of the app's icon in the title bar at 100%, in pixels: the small icon Windows 11
/// draws in a caption.
pub const ICON_AT_100: u32 = 16;

/// The title bar's icon `side` pixels square from an image of `width` x `height` premultiplied
/// BGRA pixels, as premultiplied BGRA rows.
///
/// A whole icon (`logo_only` false), plate and all, is fitted into the square as it is. An
/// adaptive icon's foreground drawn alone (`logo_only`) keeps the margin the plate had around
/// the app's shape, so its logo would look smaller than the icons of other apps; it is cropped
/// to what it draws (its pixels at least [`LOGO_ALPHA`] opaque) and that is fitted into the
/// square, keeping its proportions, with [`logo_padding`] on every side. A foreground that
/// draws (nearly) to its edges already has its own plate and is fitted as a whole icon is.
/// The crop is never smaller than a third of the image, so a tiny mark is not blown up.
pub fn caption_icon(pixels: &[u8], width: u32, height: u32, side: u32, logo_only: bool) -> Vec<u8> {
    let whole = (0.0, 0.0, f64::from(width), f64::from(height));
    if !logo_only {
        return fit_square(pixels, width, height, whole, side, 0);
    }
    let Some((left, top, right, bottom)) = drawn_bounds(pixels, width, height) else {
        return fit_square(pixels, width, height, whole, side, 0);
    };
    let (drawn_width, drawn_height) = (right - left, bottom - top);
    if drawn_width * 20 >= width * 19 && drawn_height * 20 >= height * 19 {
        return fit_square(pixels, width, height, whole, side, 0);
    }
    let least = f64::from(width.min(height)) / 3.0;
    let crop_width = f64::from(drawn_width).max(least);
    let crop_height = f64::from(drawn_height).max(least);
    let (middle_x, middle_y) = (f64::from(left + right) / 2.0, f64::from(top + bottom) / 2.0);
    let crop = (
        middle_x - crop_width / 2.0,
        middle_y - crop_height / 2.0,
        crop_width,
        crop_height,
    );
    fit_square(pixels, width, height, crop, side, logo_padding(side))
}

/// How opaque a pixel of a background-less icon must be to count as part of its logo.
pub const LOGO_ALPHA: u8 = 16;

/// The space left on every side of a background-less icon's logo in a title bar icon `side`
/// pixels square: a sixteenth of the side, at least one pixel once the side allows it, so
/// the logo fills the box a whole icon fills at every scale but for a thin margin.
pub fn logo_padding(side: u32) -> u32 {
    if side < 8 {
        0
    } else {
        ((side + 8) / 16).max(1)
    }
}

/// The smallest rectangle (left, top, right, bottom, right and bottom exclusive) holding every
/// pixel of `pixels` (premultiplied BGRA, `width` x `height`) at least [`LOGO_ALPHA`] opaque;
/// `None` when no pixel is.
fn drawn_bounds(pixels: &[u8], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for y in 0..height {
        for x in 0..width {
            let at = ((y * width + x) * 4 + 3) as usize;
            if pixels.get(at).is_some_and(|alpha| *alpha >= LOGO_ALPHA) {
                bounds = Some(match bounds {
                    None => (x, y, x + 1, y + 1),
                    Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x + 1), b.max(y + 1)),
                });
            }
        }
    }
    bounds
}

/// The `crop` rectangle (left, top, width, height, in source pixels, which may reach past the
/// image, where it is transparent) of `pixels` fitted into a square `side` pixels across, less
/// `padding` on every side, centred and keeping its proportions. Each pixel is the average of
/// the source area it covers.
pub(crate) fn fit_square(
    pixels: &[u8],
    width: u32,
    height: u32,
    crop: (f64, f64, f64, f64),
    side: u32,
    padding: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; side as usize * side as usize * 4];
    let (crop_left, crop_top, crop_width, crop_height) = crop;
    let inner = f64::from(side.saturating_sub(2 * padding));
    if width == 0 || height == 0 || crop_width <= 0.0 || crop_height <= 0.0 || inner <= 0.0 {
        return out;
    }
    if pixels.len() < (width * height * 4) as usize {
        return out;
    }
    // Destination pixels per source pixel.
    let scale = (inner / crop_width).min(inner / crop_height);
    let left = (f64::from(side) - crop_width * scale) / 2.0;
    let top = (f64::from(side) - crop_height * scale) / 2.0;
    let area = 1.0 / (scale * scale);
    let (image_width, image_height) = (f64::from(width), f64::from(height));
    for y in 0..side {
        let y0 = (crop_top + (f64::from(y) - top) / scale).max(crop_top.max(0.0));
        let y1 = (crop_top + (f64::from(y) + 1.0 - top) / scale)
            .min((crop_top + crop_height).min(image_height));
        if y1 <= y0 {
            continue;
        }
        for x in 0..side {
            let x0 = (crop_left + (f64::from(x) - left) / scale).max(crop_left.max(0.0));
            let x1 = (crop_left + (f64::from(x) + 1.0 - left) / scale)
                .min((crop_left + crop_width).min(image_width));
            if x1 <= x0 {
                continue;
            }
            let mut sum = [0.0f64; 4];
            let mut row = y0.floor();
            while row < y1 {
                let weight_y = (row + 1.0).min(y1) - row.max(y0);
                let mut column = x0.floor();
                while column < x1 {
                    let weight = weight_y * ((column + 1.0).min(x1) - column.max(x0));
                    let at = (row as usize * width as usize + column as usize) * 4;
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += f64::from(pixels[at + channel]) * weight;
                    }
                    column += 1.0;
                }
                row += 1.0;
            }
            let at = (y as usize * side as usize + x as usize) * 4;
            for (channel, value) in sum.iter().enumerate() {
                out[at + channel] = (value / area).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// The space before the icon, and between the icon and the app's name, at 100%, in pixels,
/// as a WinUI 3 title bar leaves them.
pub const TITLE_PADDING_AT_100: u32 = 16;

/// The space kept between the end of the app's name and the caption buttons at 100%.
const TITLE_END_AT_100: u32 = 8;

/// The height of the app's name at 100%, in pixels: the caption text of Windows 11.
pub const TITLE_TEXT_AT_100: u32 = 12;

/// How opaque the icon is drawn while the window is not the active one, as Windows dims an
/// inactive caption.
const ICON_INACTIVE_OPACITY: f64 = 0.4;

/// Where a task window's title bar and its buttons are, in client pixels. The client area
/// covers the whole window, so its top row is the window's top row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptionLayout {
    /// The client area's width.
    pub width: u32,
    /// The title bar's height, which the app receives as its top inset.
    pub height: u32,
    /// The width of one caption button.
    pub button_width: u32,
    /// How far down from the top the window can be resized by its top edge; 0 while the
    /// window is maximized, which has no edge to drag.
    pub resize_border: u32,
    /// The side of the app's icon.
    pub icon: u32,
    /// The space before the icon, and between the icon and the name.
    pub padding: u32,
    /// The space between the end of the name and the buttons.
    pub title_end: u32,
}

impl CaptionLayout {
    /// The layout of a window `width` pixels wide, at `dpi` (96 is 100%), with a title bar
    /// `height` pixels high and a top resize edge `resize_border` pixels deep.
    pub fn new(width: u32, height: u32, dpi: u32, resize_border: u32) -> CaptionLayout {
        CaptionLayout {
            width,
            height,
            button_width: scaled(BUTTON_WIDTH_AT_100, dpi),
            resize_border,
            icon: scaled(ICON_AT_100, dpi),
            padding: scaled(TITLE_PADDING_AT_100, dpi),
            title_end: scaled(TITLE_END_AT_100, dpi),
        }
    }

    /// Where the app's icon is: left, top, width and height, in client pixels. It stands
    /// after the left padding, centred in the band as the buttons' glyphs are. `None` when the
    /// window is too narrow for it to end before the buttons.
    pub fn icon_rect(&self) -> Option<(i32, i32, u32, u32)> {
        let left = self.padding as i32;
        if left + self.icon as i32 > self.buttons_left() || self.icon > self.height {
            return None;
        }
        let top = (self.height - self.icon) / 2;
        Some((left, top as i32, self.icon, self.icon))
    }

    /// Where the app's name may be drawn: left, top, width and height, in client pixels. It
    /// starts a padding after the icon and ends before the buttons, so a long name is cut
    /// there. `None` when no room is left for it.
    pub fn title_rect(&self) -> Option<(i32, i32, u32, u32)> {
        let left = (2 * self.padding + self.icon) as i32;
        let right = self.buttons_left() - self.title_end as i32;
        if right <= left {
            return None;
        }
        Some((left, 0, (right - left) as u32, self.height))
    }

    /// The icon's cell: the icon with half the padding on each side, the whole height of the
    /// band. It is the same whether it shows the icon or the back arrow, so the name never
    /// moves and an app drawing its own title bar is told the same area either way. `None`
    /// when the window is too narrow for the icon.
    pub fn icon_cell(&self) -> Option<(i32, i32, u32, u32)> {
        let (left, _, side, _) = self.icon_rect()?;
        let half = self.padding / 2;
        Some((left - half as i32, 0, side + 2 * half, self.height))
    }

    /// Whether the client point (`x`, `y`) is in the icon's cell, which opens the window's
    /// system menu (or goes back, while the cell shows the back arrow). The top resize edge
    /// comes first, as it does for the buttons.
    pub fn on_icon(&self, x: i32, y: i32) -> bool {
        if y < self.resize_border as i32 {
            return false;
        }
        match self.icon_cell() {
            Some((left, top, width, height)) => {
                x >= left && x < left + width as i32 && y >= top && y < top + height as i32
            }
            None => false,
        }
    }

    /// How far the icon's cell and the caption buttons reach into the band from its ends.
    /// They depend on the scale alone, not on the window's width, so a resize never moves
    /// what an app drawing its own title bar is told.
    pub fn band(&self) -> CaptionBand {
        let half = self.padding / 2;
        CaptionBand {
            height: self.height,
            start_width: (self.padding - half) + self.icon + 2 * half,
            end_width: 3 * self.button_width,
        }
    }

    /// Where `button` is: its left edge, top edge, width and height, in client pixels. The
    /// buttons stand at the right end of the title bar, close rightmost. A window too narrow
    /// for all three keeps them in place and lets the left ones run off its left edge.
    pub fn button_rect(&self, button: CaptionButton) -> (i32, i32, u32, u32) {
        let from_right = 3 - button.index();
        let left = self.width as i64 - i64::from(from_right * self.button_width);
        (left as i32, 0, self.button_width, self.height)
    }

    /// The left edge of the buttons, in client pixels; may be negative in a narrow window.
    pub fn buttons_left(&self) -> i32 {
        self.button_rect(CaptionButton::Minimize).0
    }

    /// What the client point (`x`, `y`) belongs to. The top resize edge comes first, so the
    /// window can be resized above its buttons as at any other point of its top edge.
    pub fn hit(&self, x: i32, y: i32) -> CaptionHit {
        if y < 0 || y >= self.height as i32 || x < 0 || x >= self.width as i32 {
            return CaptionHit::Client;
        }
        if y < self.resize_border as i32 {
            let corner = (self.resize_border * 2) as i32;
            return if x < corner {
                CaptionHit::TopLeft
            } else if x >= self.width as i32 - corner {
                CaptionHit::TopRight
            } else {
                CaptionHit::Top
            };
        }
        for button in CaptionButton::ALL {
            let (left, _, width, _) = self.button_rect(button);
            if x >= left && x < left + width as i32 {
                return CaptionHit::Button(button);
            }
        }
        CaptionHit::Caption
    }
}

/// `length` at 100%, at `dpi`, rounded to the nearest pixel.
pub fn scaled(length: u32, dpi: u32) -> u32 {
    let dpi = if dpi == 0 { 96 } else { dpi };
    (u64::from(length) * u64::from(dpi) + 48) as u32 / 96
}

/// The colors the caption buttons are drawn in, each 0xAARRGGBB (not premultiplied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptionPalette {
    /// The background of a button at rest: transparent, so the app shows through, except
    /// where a high contrast theme asks for its own colors.
    pub rest: u32,
    /// The glyphs at rest.
    pub glyph: u32,
    /// The glyphs of a button the pointer is over or presses, other than close.
    pub glyph_highlighted: u32,
    /// The glyphs while the window is not the active one.
    pub glyph_inactive: u32,
    /// The background of a button the pointer is over, and of one being pressed.
    pub hover: u32,
    pub pressed: u32,
    /// The close button's background and glyph while the pointer is over it or presses it.
    pub close_hover: u32,
    pub close_pressed: u32,
    pub close_glyph: u32,
}

impl CaptionPalette {
    /// The colors Windows 11 draws its own caption buttons in, for glyphs of `tone`.
    pub fn of(tone: CaptionTone) -> CaptionPalette {
        let close = CaptionPalette {
            rest: 0,
            glyph: 0,
            glyph_highlighted: 0,
            glyph_inactive: 0,
            hover: 0,
            pressed: 0,
            close_hover: 0xFFC4_2B1C,
            close_pressed: 0xE6C4_2B1C,
            close_glyph: 0xFFFF_FFFF,
        };
        match tone {
            CaptionTone::DarkGlyphs => CaptionPalette {
                glyph: 0xE400_0000,
                glyph_highlighted: 0xE400_0000,
                glyph_inactive: 0x5C00_0000,
                hover: 0x0900_0000,
                pressed: 0x0600_0000,
                ..close
            },
            CaptionTone::LightGlyphs => CaptionPalette {
                glyph: 0xFFFF_FFFF,
                glyph_highlighted: 0xFFFF_FFFF,
                glyph_inactive: 0x5DFF_FFFF,
                hover: 0x0FFF_FFFF,
                pressed: 0x0AFF_FFFF,
                ..close
            },
        }
    }
}

/// What the caption buttons show besides their tone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptionState {
    /// The button the pointer is over, if any.
    pub hovered: Option<CaptionButton>,
    /// The button being pressed, if any.
    pub pressed: Option<CaptionButton>,
    /// Whether the window is maximized, so the middle button restores it.
    pub maximized: bool,
    /// Whether the window is the active one.
    pub active: bool,
}

/// The three caption buttons side by side, as rows of premultiplied BGRA pixels,
/// `3 * layout.button_width` wide and `layout.height` high. Where a button has no
/// background the pixels are transparent, so the app shows through.
pub fn paint_buttons(
    layout: &CaptionLayout,
    dpi: u32,
    palette: &CaptionPalette,
    state: &CaptionState,
) -> Vec<u8> {
    let width = 3 * layout.button_width as usize;
    let height = layout.height as usize;
    let mut pixels = vec![0u8; width * height * 4];
    let scale = f64::from(if dpi == 0 { 96 } else { dpi }) / 96.0;
    let stroke = scale.round().max(1.0);
    for button in CaptionButton::ALL {
        let left = (button.index() * layout.button_width) as usize;
        let close = button == CaptionButton::Close;
        let pressed = state.pressed == Some(button);
        let highlighted = pressed || (state.hovered == Some(button) && state.pressed.is_none());
        let background = match (highlighted, pressed, close) {
            (false, _, _) => palette.rest,
            (true, true, true) => palette.close_pressed,
            (true, true, false) => palette.pressed,
            (true, false, true) => palette.close_hover,
            (true, false, false) => palette.hover,
        };
        let glyph = match (highlighted, close) {
            (true, true) => palette.close_glyph,
            (true, false) => palette.glyph_highlighted,
            (false, _) if state.active => palette.glyph,
            (false, _) => palette.glyph_inactive,
        };
        let center = (
            left as f64 + f64::from(layout.button_width) / 2.0,
            f64::from(layout.height) / 2.0,
        );
        let segments = glyph_segments(button, state.maximized, center, scale, stroke);
        for y in 0..height {
            for x in left..left + layout.button_width as usize {
                let point = (x as f64 + 0.5, y as f64 + 0.5);
                let distance = segments
                    .iter()
                    .map(|segment| distance_to_segment(point, *segment))
                    .fold(f64::INFINITY, f64::min);
                let coverage = (stroke / 2.0 + 0.5 - distance).clamp(0.0, 1.0);
                let pixel = over(premultiply(background, 1.0), premultiply(glyph, coverage));
                let at = (y * width + x) * 4;
                pixels[at..at + 4].copy_from_slice(&pixel);
            }
        }
    }
    pixels
}

/// What the title bar shows besides its buttons.
#[derive(Debug, Clone, Copy, Default)]
pub struct TitleBarContent<'a> {
    /// The color the band is filled with (0xAARRGGBB), when the app leaves it empty; `None`
    /// keeps it transparent, so the app shows through.
    pub fill: Option<u32>,
    /// The app's icon: rows of premultiplied BGRA pixels, `layout.icon` pixels square. For an
    /// app with an adaptive icon it is the icon's foreground layer alone, so no square plate
    /// stands over the app's content.
    pub icon: Option<&'a [u8]>,
    /// The app's name, already laid out and cut to fit: one coverage byte (0 to 255) per
    /// pixel, rows of the width and height of [`CaptionLayout::title_rect`].
    pub title: Option<&'a [u8]>,
    /// Whether the app draws its own title bar: the band then shows only the icon's cell and
    /// the buttons over the app, with no name and no fill.
    pub app_drawn: bool,
    /// The back arrow in the place of the icon, while the task can go back, with the state
    /// the pointer leaves it in; `None` shows the icon.
    pub back: Option<BackButton>,
}

/// The back arrow the icon's cell shows while the task can go back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BackButton {
    /// The pointer is over it.
    pub hovered: bool,
    /// It is being pressed.
    pub pressed: bool,
}

/// The whole title bar as rows of premultiplied BGRA pixels, `layout.width` wide and
/// `layout.height` high: the fill, if any, the app's icon and name at the left, in the
/// glyphs' color and dimmed while the window is inactive, and the buttons at the right.
pub fn paint_title_bar(
    layout: &CaptionLayout,
    dpi: u32,
    palette: &CaptionPalette,
    state: &CaptionState,
    content: &TitleBarContent,
) -> Vec<u8> {
    let width = layout.width as usize;
    let height = layout.height as usize;
    // A high contrast theme gives the band its own background, as it does the buttons. An
    // app that draws its own title bar keeps the band as it drew it.
    let fill = if content.app_drawn {
        None
    } else if palette.rest >> 24 != 0 {
        Some(palette.rest)
    } else {
        content.fill
    };
    let base = fill.map_or([0.0; 4], |color| premultiply(color, 1.0));
    let base = over([0.0; 4], base);
    let mut pixels = Vec::with_capacity(width * height * 4);
    for _ in 0..width * height {
        pixels.extend_from_slice(&base);
    }
    let blend = |pixels: &mut [u8], x: usize, y: usize, top: [f64; 4]| {
        let at = (y * width + x) * 4;
        let bottom = [
            f64::from(pixels[at]) / 255.0,
            f64::from(pixels[at + 1]) / 255.0,
            f64::from(pixels[at + 2]) / 255.0,
            f64::from(pixels[at + 3]) / 255.0,
        ];
        pixels[at..at + 4].copy_from_slice(&over(bottom, top));
    };
    if let (Some(back), Some(cell)) = (content.back, layout.icon_cell()) {
        paint_back(&mut pixels, layout, dpi, palette, state, back, cell);
    } else if let (Some(icon), Some((left, top, side, _))) = (content.icon, layout.icon_rect()) {
        let opacity = if state.active {
            1.0
        } else {
            ICON_INACTIVE_OPACITY
        };
        let side = side as usize;
        if icon.len() >= side * side * 4 {
            for row in 0..side {
                for column in 0..side {
                    let (x, y) = (left as usize + column, top as usize + row);
                    if x >= width || y >= height {
                        continue;
                    }
                    let at = (row * side + column) * 4;
                    let source = [
                        f64::from(icon[at]) / 255.0 * opacity,
                        f64::from(icon[at + 1]) / 255.0 * opacity,
                        f64::from(icon[at + 2]) / 255.0 * opacity,
                        f64::from(icon[at + 3]) / 255.0 * opacity,
                    ];
                    blend(&mut pixels, x, y, source);
                }
            }
        }
    }
    let title = content.title.filter(|_| !content.app_drawn);
    if let (Some(title), Some((left, _, title_width, title_height))) = (title, layout.title_rect())
    {
        let color = if state.active {
            palette.glyph
        } else {
            palette.glyph_inactive
        };
        let (title_width, title_height) = (title_width as usize, title_height as usize);
        if title.len() >= title_width * title_height {
            for row in 0..title_height.min(height) {
                for column in 0..title_width {
                    let coverage = title[row * title_width + column];
                    let x = left as usize + column;
                    if coverage == 0 || x >= width {
                        continue;
                    }
                    let source = premultiply(color, f64::from(coverage) / 255.0);
                    blend(&mut pixels, x, row, source);
                }
            }
        }
    }
    let buttons = paint_buttons(layout, dpi, palette, state);
    let strip = 3 * layout.button_width as usize;
    let buttons_left = layout.buttons_left();
    for y in 0..height {
        for column in 0..strip {
            let x = buttons_left + column as i32;
            if x < 0 || x as usize >= width {
                continue;
            }
            let at = (y * strip + column) * 4;
            let source = [
                f64::from(buttons[at]) / 255.0,
                f64::from(buttons[at + 1]) / 255.0,
                f64::from(buttons[at + 2]) / 255.0,
                f64::from(buttons[at + 3]) / 255.0,
            ];
            blend(&mut pixels, x as usize, y, source);
        }
    }
    pixels
}

/// Draws the back arrow into the icon's `cell` of `pixels` (a whole title bar): the
/// background of a hovered or pressed caption button, and a left arrow in the glyphs' color,
/// as wide as a caption glyph.
fn paint_back(
    pixels: &mut [u8],
    layout: &CaptionLayout,
    dpi: u32,
    palette: &CaptionPalette,
    state: &CaptionState,
    back: BackButton,
    cell: (i32, i32, u32, u32),
) {
    let width = layout.width as usize;
    let scale = f64::from(if dpi == 0 { 96 } else { dpi }) / 96.0;
    let stroke = scale.round().max(1.0);
    let highlighted = back.pressed || back.hovered;
    let background = match (back.pressed, highlighted) {
        (true, _) => palette.pressed,
        (false, true) => palette.hover,
        _ => 0,
    };
    let glyph = if highlighted {
        palette.glyph_highlighted
    } else if state.active {
        palette.glyph
    } else {
        palette.glyph_inactive
    };
    let (left, top, cell_width, cell_height) = cell;
    let half = (GLYPH_AT_100 * scale / 2.0).round();
    let snap = |value: f64| {
        if stroke as i64 % 2 == 1 {
            value.floor() + 0.5
        } else {
            value.round()
        }
    };
    let cx = snap(f64::from(left) + f64::from(cell_width) / 2.0);
    let cy = snap(f64::from(top) + f64::from(cell_height) / 2.0);
    let segments: [Segment; 3] = [
        ((cx - half, cy), (cx + half, cy)),
        ((cx - half, cy), (cx, cy - half)),
        ((cx - half, cy), (cx, cy + half)),
    ];
    for y in top.max(0)..(top + cell_height as i32).min(layout.height as i32) {
        for x in left.max(0)..(left + cell_width as i32).min(layout.width as i32) {
            let point = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let distance = segments
                .iter()
                .map(|segment| distance_to_segment(point, *segment))
                .fold(f64::INFINITY, f64::min);
            let coverage = (stroke / 2.0 + 0.5 - distance).clamp(0.0, 1.0);
            let at = (y as usize * width + x as usize) * 4;
            let bottom = [
                f64::from(pixels[at]) / 255.0,
                f64::from(pixels[at + 1]) / 255.0,
                f64::from(pixels[at + 2]) / 255.0,
                f64::from(pixels[at + 3]) / 255.0,
            ];
            let under = over(bottom, premultiply(background, 1.0));
            let under = under.map(|channel| f64::from(channel) / 255.0);
            pixels[at..at + 4].copy_from_slice(&over(under, premultiply(glyph, coverage)));
        }
    }
}

/// The color to fill the title bar's band with, from sample pixels of an app's frame
/// (0xAARRGGBB each): `band` from inside the band, `below` from the row just under it.
///
/// An app that draws behind the band (its status bar background) keeps it as it drew it. An
/// app that does not, as one with an old theme that draws no system bar background, leaves
/// the band empty or black while its own top (its toolbar) starts below it; the band then
/// takes the color under it, so the title bar always follows the app's top color. `None`
/// leaves the band as the app drew it.
pub fn band_fill(band: &[u32], below: &[u32]) -> Option<u32> {
    let first = *below.first()?;
    if band.is_empty() || first >> 24 < 0xF0 {
        return None;
    }
    if !below.iter().all(|color| alike(*color, first)) {
        return None;
    }
    if !band.iter().all(|color| empty_or_black(*color)) {
        return None;
    }
    // Black under black: the app is black at its top, and the band already is.
    if empty_or_black(first) {
        return None;
    }
    Some(0xFF00_0000 | (first & 0x00FF_FFFF))
}

/// Whether two 0xAARRGGBB colors differ by at most 2 in every channel.
fn alike(a: u32, b: u32) -> bool {
    (0..4).all(|i| channel(a, i).abs_diff(channel(b, i)) <= 2)
}

/// Whether an 0xAARRGGBB color is (nearly) transparent or (nearly) black.
fn empty_or_black(color: u32) -> bool {
    channel(color, 3) < 0x10 || (0..3).all(|i| channel(color, i) <= 0x08)
}

/// Channel `index` of an 0xAARRGGBB color: 0 blue, 1 green, 2 red, 3 alpha.
fn channel(color: u32, index: u32) -> u32 {
    (color >> (8 * index)) & 0xff
}

type Segment = ((f64, f64), (f64, f64));

/// The strokes of `button`'s glyph around `center`. Each stroke runs along the middle of a
/// row or column of pixels, so a stroke of whole pixels lands on whole pixels.
fn glyph_segments(
    button: CaptionButton,
    maximized: bool,
    center: (f64, f64),
    scale: f64,
    stroke: f64,
) -> Vec<Segment> {
    let half = (GLYPH_AT_100 * scale / 2.0).round();
    // The middle of a pixel when the stroke is odd, a pixel edge when it is even.
    let snap = |value: f64| {
        if stroke as i64 % 2 == 1 {
            value.floor() + 0.5
        } else {
            value.round()
        }
    };
    let (cx, cy) = (snap(center.0), snap(center.1));
    let rect = |left: f64, top: f64, right: f64, bottom: f64| {
        vec![
            ((left, top), (right, top)),
            ((right, top), (right, bottom)),
            ((right, bottom), (left, bottom)),
            ((left, bottom), (left, top)),
        ]
    };
    match button {
        CaptionButton::Minimize => vec![((cx - half, cy), (cx + half, cy))],
        CaptionButton::Maximize if !maximized => rect(cx - half, cy - half, cx + half, cy + half),
        CaptionButton::Maximize => {
            let offset = (2.0 * scale).round();
            let mut segments = rect(cx - half, cy - half + offset, cx + half - offset, cy + half);
            segments.extend([
                (
                    (cx - half + offset, cy - half + offset),
                    (cx - half + offset, cy - half),
                ),
                ((cx - half + offset, cy - half), (cx + half, cy - half)),
                ((cx + half, cy - half), (cx + half, cy + half - offset)),
                (
                    (cx + half, cy + half - offset),
                    (cx + half - offset, cy + half - offset),
                ),
            ]);
            segments
        }
        CaptionButton::Close => vec![
            ((cx - half, cy - half), (cx + half, cy + half)),
            ((cx + half, cy - half), (cx - half, cy + half)),
        ],
    }
}

fn distance_to_segment(point: (f64, f64), (a, b): Segment) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((point.0 - a.0) * dx + (point.1 - a.1) * dy) / length).clamp(0.0, 1.0)
    };
    let (x, y) = (a.0 + t * dx, a.1 + t * dy);
    ((point.0 - x).powi(2) + (point.1 - y).powi(2)).sqrt()
}

/// An 0xAARRGGBB color at `coverage`, as premultiplied BGRA in 0..=1.
fn premultiply(argb: u32, coverage: f64) -> [f64; 4] {
    let alpha = f64::from(argb >> 24) / 255.0 * coverage;
    let channel = |shift: u32| f64::from((argb >> shift) & 0xff) / 255.0 * alpha;
    [channel(0), channel(8), channel(16), alpha]
}

/// `top` over `bottom`, both premultiplied, as BGRA bytes.
fn over(bottom: [f64; 4], top: [f64; 4]) -> [u8; 4] {
    let mut out = [0u8; 4];
    for (index, value) in out.iter_mut().enumerate() {
        let mixed = top[index] + bottom[index] * (1.0 - top[3]);
        *value = (mixed * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window 800 pixels wide at 100%, with a title bar of 31 and a top edge of 8.
    fn at_100() -> CaptionLayout {
        CaptionLayout::new(800, 31, 96, 8)
    }

    #[test]
    fn fr22_glyphs_are_dark_on_a_light_status_bar_and_light_on_a_dark_one() {
        assert_eq!(
            caption_tone(StatusBarAppearance::Light, 0),
            CaptionTone::DarkGlyphs
        );
        assert_eq!(
            caption_tone(StatusBarAppearance::Dark, 0),
            CaptionTone::LightGlyphs
        );
        // What the app said wins over the color it set.
        assert_eq!(
            caption_tone(StatusBarAppearance::Light, 0xFF00_0000),
            CaptionTone::DarkGlyphs
        );
    }

    #[test]
    fn fr22_without_an_appearance_the_status_bar_color_decides() {
        let tone = |color| caption_tone(StatusBarAppearance::Unspecified, color);
        assert_eq!(tone(0xFFFF_FFFF), CaptionTone::DarkGlyphs);
        assert_eq!(tone(0xFF1F_1F1F), CaptionTone::LightGlyphs);
        assert_eq!(tone(0xFF67_50A4), CaptionTone::LightGlyphs, "a deep purple");
        assert_eq!(tone(0xFFFF_EB3B), CaptionTone::DarkGlyphs, "a yellow");
        // A transparent status bar says nothing about what is behind it.
        assert_eq!(tone(0x0000_0000), CaptionTone::DarkGlyphs);
        assert_eq!(tone(0x10FF_FFFF), CaptionTone::DarkGlyphs);
    }

    /// A band of samples across the middle of the title bar, all `color`, with the row under
    /// it in `below`.
    fn sampled(color: u32, below: u32) -> BandSample {
        band_sample(&[color; 8], &[below; 8])
    }

    #[test]
    fn fr22_a_transparent_status_bar_over_a_light_header_gets_dark_glyphs() {
        // An app that draws edge to edge: its status bar is transparent and says nothing, and
        // its light header (the Play Store's Games and Apps tabs) runs under the band.
        let reported = caption_tone(StatusBarAppearance::Unspecified, 0x0000_0000);
        for header in [0xFFFF_FFFF, 0xFFF8_F9FA, 0xFFE8_F0FE, 0xFFDA_E2FF] {
            let sample = sampled(header, header);
            assert_eq!(sample.fill, None, "the app draws the band");
            for shown in [CaptionTone::DarkGlyphs, CaptionTone::LightGlyphs] {
                assert_eq!(
                    shown_tone(reported, sample, false, shown),
                    CaptionTone::DarkGlyphs,
                    "{header:08X} while {shown:?} show"
                );
            }
        }
    }

    #[test]
    fn fr22_a_transparent_status_bar_over_a_dark_header_gets_light_glyphs() {
        // The same app in a dark theme: nothing reported, so the flag alone would say dark.
        let reported = caption_tone(StatusBarAppearance::Unspecified, 0x0000_0000);
        assert_eq!(reported, CaptionTone::DarkGlyphs);
        for header in [
            0xFF00_0000,
            0xFF1F_1F1F,
            0xFF14_1218,
            0xFF20_2124,
            0xFF67_50A4,
        ] {
            let sample = sampled(header, header);
            for shown in [CaptionTone::DarkGlyphs, CaptionTone::LightGlyphs] {
                assert_eq!(
                    shown_tone(reported, sample, false, shown),
                    CaptionTone::LightGlyphs,
                    "{header:08X} while {shown:?} show"
                );
            }
        }
    }

    #[test]
    fn fr22_a_dark_video_under_a_transparent_status_bar_gets_light_glyphs() {
        // A full-bleed player (YouTube's Shorts) in the light theme: the app reports dark
        // status bar icons, or nothing, and its dark video runs under the band, with a grey
        // overlay here and there.
        let video = [
            0xFF10_1010,
            0xFF2A_1E18,
            0xFF05_0505,
            0xFF3A_3A3A,
            0xFF80_8080,
            0xFF18_2028,
            0xFF00_0000,
            0xFF20_1810,
        ];
        let sample = band_sample(&video, &video);
        assert_eq!(sample.fill, None);
        for reported in [
            caption_tone(StatusBarAppearance::Light, 0),
            caption_tone(StatusBarAppearance::Unspecified, 0),
        ] {
            assert_eq!(reported, CaptionTone::DarkGlyphs);
            assert_eq!(
                shown_tone(reported, sample, false, reported),
                CaptionTone::LightGlyphs
            );
        }
    }

    #[test]
    fn fr22_a_status_bar_flag_that_disagrees_with_the_pixels_gives_way_to_them() {
        // Dark status bar icons asked for (light glyphs) over a light header, and the reverse.
        let light_header = sampled(0xFFFF_FFFF, 0xFFFF_FFFF);
        let dark_header = sampled(0xFF12_1212, 0xFF12_1212);
        let says_dark = caption_tone(StatusBarAppearance::Dark, 0);
        let says_light = caption_tone(StatusBarAppearance::Light, 0);
        assert_eq!(
            shown_tone(says_dark, light_header, false, says_dark),
            CaptionTone::DarkGlyphs
        );
        assert_eq!(
            shown_tone(says_light, dark_header, false, says_light),
            CaptionTone::LightGlyphs
        );
        // So do the glyphs over an app's own title bar, whose content fills the band.
        assert_eq!(
            shown_tone(says_dark, light_header, true, says_dark),
            CaptionTone::DarkGlyphs
        );
        // A frame that could not be sampled leaves the flag: an empty band, or none read yet.
        let empty = sampled(0x0000_0000, 0x0000_0000);
        assert_eq!(empty, BandSample::default());
        for reported in [says_dark, says_light] {
            assert_eq!(shown_tone(reported, empty, false, says_light), reported);
            assert_eq!(
                shown_tone(reported, BandSample::default(), false, says_dark),
                reported
            );
        }
        // A band left empty above a toolbar takes the fill's tone, whatever the flag says.
        let filled = sampled(0x0000_0000, 0xFFFF_FFFF);
        assert_eq!(filled.fill, Some(0xFFFF_FFFF));
        assert_eq!(
            shown_tone(says_dark, filled, false, says_dark),
            CaptionTone::DarkGlyphs
        );
    }

    #[test]
    fn fr22_a_band_near_the_middle_does_not_make_the_glyphs_flicker() {
        let tone_of = |luminance: u16, shown| {
            let sample = BandSample {
                fill: None,
                luminance: Some(luminance),
            };
            shown_tone(CaptionTone::DarkGlyphs, sample, false, shown)
        };
        // A header whose lightness wavers around the middle (an animation, a scrolled
        // picture) keeps the tone it has, whichever it is.
        let mut shown = CaptionTone::DarkGlyphs;
        for luminance in [200, 150, 230, 140, 179] {
            shown = tone_of(luminance, shown);
            assert_eq!(shown, CaptionTone::DarkGlyphs, "at {luminance}");
        }
        shown = tone_of(100, shown);
        assert_eq!(shown, CaptionTone::LightGlyphs);
        for luminance in [150, 230, 140, 179, 240] {
            shown = tone_of(luminance, shown);
            assert_eq!(shown, CaptionTone::LightGlyphs, "at {luminance}");
        }
        assert_eq!(tone_of(241, shown), CaptionTone::DarkGlyphs);
        // The samples are averaged, and only opaque ones count.
        assert_eq!(band_luminance(&[0xFFFF_FFFF; 8]), Some(1000));
        assert_eq!(band_luminance(&[0xFF00_0000; 8]), Some(0));
        let half = [[0xFFFF_FFFF; 4], [0xFF00_0000; 4]].concat();
        assert_eq!(band_luminance(&half), Some(500));
        let mostly_empty = [&[0xFFFF_FFFF; 3][..], &[0x0000_0000; 5][..]].concat();
        assert_eq!(band_luminance(&mostly_empty), None);
        assert_eq!(band_luminance(&[]), None);
    }

    /// A square image `size` pixels across, transparent but for an opaque white square of
    /// `logo` pixels in its middle, as premultiplied BGRA.
    fn logo_image(size: u32, logo: u32) -> Vec<u8> {
        let start = (size - logo) / 2;
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let inside =
                    (start..start + logo).contains(&x) && (start..start + logo).contains(&y);
                pixels.extend_from_slice(if inside { &[0xFF; 4] } else { &[0; 4] });
            }
        }
        pixels
    }

    /// The columns of a `side` square icon whose middle row is at least half opaque.
    fn drawn_columns(icon: &[u8], side: u32) -> std::ops::Range<u32> {
        let row = side / 2;
        let drawn: Vec<u32> = (0..side)
            .filter(|x| icon[((row * side + x) * 4 + 3) as usize] >= 128)
            .collect();
        match (drawn.first(), drawn.last()) {
            (Some(first), Some(last)) => *first..*last + 1,
            _ => 0..0,
        }
    }

    #[test]
    fn fr22_a_logo_only_caption_icon_is_enlarged_to_the_size_of_a_whole_icon() {
        // A foreground drawn alone at the scale of the whole icon: its logo covers the middle
        // half of the square, where the plate used to surround it.
        let foreground = logo_image(192, 96);
        let whole = [0xFFu8; 4].repeat(192 * 192);
        for dpi in [96, 120, 144, 192] {
            let side = scaled(ICON_AT_100, dpi);
            let padding = logo_padding(side);
            let full = drawn_columns(&caption_icon(&whole, 192, 192, side, false), side);
            let logo = drawn_columns(&caption_icon(&foreground, 192, 192, side, true), side);
            assert_eq!(full, 0..side, "a whole icon fills its box at {dpi}");
            assert_eq!(
                logo,
                padding..side - padding,
                "the logo fills the same box but for the padding at {dpi}"
            );
            assert!(padding >= 1 && padding * 8 <= side, "{padding} of {side}");
            // Unenlarged, it would have been half as wide.
            let as_drawn = drawn_columns(&caption_icon(&foreground, 192, 192, side, false), side);
            assert!(
                as_drawn.len() * 3 < logo.len() * 2,
                "{as_drawn:?} against {logo:?}"
            );
        }
        // A wide logo keeps its proportions: it fills the width, and is centred down the
        // middle.
        let mut wide = vec![0u8; 100 * 100 * 4];
        for y in 40..60 {
            for x in 10..90 {
                let at = (y * 100 + x) * 4;
                wide[at..at + 4].copy_from_slice(&[0xFF; 4]);
            }
        }
        let icon = caption_icon(&wide, 100, 100, 32, true);
        assert_eq!(drawn_columns(&icon, 32), 2..30);
        let alpha = |x: u32, y: u32| icon[((y * 32 + x) * 4 + 3) as usize];
        assert_eq!(alpha(16, 2), 0, "a 4:1 logo is a quarter as tall as wide");
        assert!(alpha(16, 16) > 200);
    }

    #[test]
    fn fr22_a_caption_icon_with_its_background_is_unchanged() {
        // A whole icon, plate and all: fitted as it is, logo_only or not.
        let mut plated = [0xFFu8, 0x80, 0x40, 0xFF].repeat(64 * 64);
        let middle = logo_image(64, 20);
        for (at, pixel) in middle.chunks_exact(4).enumerate() {
            if pixel[3] != 0 {
                plated[at * 4..at * 4 + 4].copy_from_slice(pixel);
            }
        }
        for side in [16, 24, 32] {
            let plain = caption_icon(&plated, 64, 64, side, false);
            assert_eq!(caption_icon(&plated, 64, 64, side, true), plain);
            assert_eq!(drawn_columns(&plain, side), 0..side);
        }
        // At the icon's own size a whole icon comes out pixel for pixel.
        assert_eq!(caption_icon(&plated, 64, 64, 64, false), plated);
        // A foreground that draws nothing stays empty, with nothing to crop to.
        let empty = vec![0u8; 64 * 64 * 4];
        assert!(caption_icon(&empty, 64, 64, 16, true)
            .iter()
            .all(|b| *b == 0));
        // A tiny mark is enlarged at most threefold.
        let dot = logo_image(90, 2);
        let icon = caption_icon(&dot, 90, 90, 30, true);
        let drawn = drawn_columns(&icon, 30);
        assert!(drawn.len() <= 2, "{drawn:?}");
    }

    #[test]
    fn fr22_the_buttons_stand_at_the_right_end_of_the_title_bar() {
        let layout = at_100();
        assert_eq!(layout.button_width, 46);
        assert_eq!(layout.button_rect(CaptionButton::Close), (754, 0, 46, 31));
        assert_eq!(
            layout.button_rect(CaptionButton::Maximize),
            (708, 0, 46, 31)
        );
        assert_eq!(
            layout.button_rect(CaptionButton::Minimize),
            (662, 0, 46, 31)
        );
        assert_eq!(CaptionLayout::new(800, 62, 192, 16).button_width, 92);
        assert_eq!(CaptionLayout::new(800, 47, 144, 12).button_width, 69);
    }

    #[test]
    fn fr22_the_title_bar_band_belongs_to_the_window_and_the_rest_to_the_app() {
        let layout = at_100();
        assert_eq!(layout.hit(400, 20), CaptionHit::Caption);
        assert_eq!(layout.hit(0, 30), CaptionHit::Caption);
        assert_eq!(
            layout.hit(400, 31),
            CaptionHit::Client,
            "the first row below"
        );
        assert_eq!(layout.hit(799, 500), CaptionHit::Client);
        assert_eq!(
            layout.hit(760, 20),
            CaptionHit::Button(CaptionButton::Close)
        );
        assert_eq!(
            layout.hit(753, 20),
            CaptionHit::Button(CaptionButton::Maximize)
        );
        assert_eq!(
            layout.hit(662, 20),
            CaptionHit::Button(CaptionButton::Minimize)
        );
        assert_eq!(layout.hit(661, 20), CaptionHit::Caption);
    }

    #[test]
    fn fr22_the_top_edge_resizes_the_window_unless_it_is_maximized() {
        let layout = at_100();
        assert_eq!(layout.hit(400, 0), CaptionHit::Top);
        assert_eq!(layout.hit(400, 7), CaptionHit::Top);
        assert_eq!(layout.hit(400, 8), CaptionHit::Caption);
        assert_eq!(layout.hit(3, 2), CaptionHit::TopLeft);
        assert_eq!(layout.hit(790, 2), CaptionHit::TopRight);
        assert_eq!(
            layout.hit(770, 2),
            CaptionHit::Top,
            "above the close button"
        );
        let maximized = CaptionLayout::new(800, 31, 96, 0);
        assert_eq!(
            maximized.hit(799, 0),
            CaptionHit::Button(CaptionButton::Close),
            "a maximized window's corner closes it, as in Windows"
        );
    }

    #[test]
    fn fr22_at_every_scale_the_band_is_the_title_bar_height_and_no_more() {
        for (dpi, height, border) in [(96, 31, 8), (144, 46, 11), (192, 62, 16)] {
            let layout = CaptionLayout::new(1200, height, dpi, border);
            for y in border as i32..height as i32 {
                assert_ne!(layout.hit(10, y), CaptionHit::Client, "row {y} at {dpi}");
            }
            assert_eq!(layout.hit(10, height as i32), CaptionHit::Client);
        }
    }

    fn pixel(image: &[u8], layout: &CaptionLayout, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * 3 * layout.button_width + x) * 4) as usize;
        image[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn fr22_the_glyphs_are_drawn_in_the_tone_and_the_rest_is_transparent() {
        let layout = at_100();
        let state = CaptionState {
            active: true,
            ..CaptionState::default()
        };
        for (tone, value) in [
            (CaptionTone::DarkGlyphs, 0u8),
            (CaptionTone::LightGlyphs, 255u8),
        ] {
            let image = paint_buttons(&layout, 96, &CaptionPalette::of(tone), &state);
            assert_eq!(image.len(), (138 * 31 * 4) as usize);
            // A corner of a button is clear, so the app shows through.
            assert_eq!(pixel(&image, &layout, 1, 1), [0, 0, 0, 0]);
            // The middle of the minimize glyph's stroke is drawn in the tone.
            let stroke = pixel(&image, &layout, 23, 15);
            assert!(stroke[3] > 200, "{tone:?}: {stroke:?}");
            let expected = (u32::from(value) * u32::from(stroke[3]) / 255) as u8;
            assert!(stroke[0].abs_diff(expected) <= 1, "{tone:?}: {stroke:?}");
        }
    }

    #[test]
    fn fr22_close_turns_red_under_the_pointer_with_a_white_glyph() {
        let layout = at_100();
        let state = CaptionState {
            hovered: Some(CaptionButton::Close),
            active: true,
            ..CaptionState::default()
        };
        let image = paint_buttons(
            &layout,
            96,
            &CaptionPalette::of(CaptionTone::DarkGlyphs),
            &state,
        );
        // BGRA of #C42B1C.
        assert_eq!(pixel(&image, &layout, 92 + 2, 2), [0x1C, 0x2B, 0xC4, 0xFF]);
        // The middle of the X is white.
        assert_eq!(pixel(&image, &layout, 92 + 23, 15), [255, 255, 255, 255]);
        // The other buttons have no background.
        assert_eq!(pixel(&image, &layout, 2, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn fr22_a_hovered_button_has_a_faint_background_and_a_pressed_one_another() {
        let layout = at_100();
        let palette = CaptionPalette::of(CaptionTone::LightGlyphs);
        let hovered = CaptionState {
            hovered: Some(CaptionButton::Minimize),
            active: true,
            ..CaptionState::default()
        };
        let pressed = CaptionState {
            pressed: Some(CaptionButton::Minimize),
            ..hovered
        };
        let a = pixel(
            &paint_buttons(&layout, 96, &palette, &hovered),
            &layout,
            2,
            2,
        );
        let b = pixel(
            &paint_buttons(&layout, 96, &palette, &pressed),
            &layout,
            2,
            2,
        );
        assert_eq!(a[3], 0x0F);
        assert_eq!(b[3], 0x0A);
    }

    #[test]
    fn fr22_an_inactive_window_draws_fainter_glyphs() {
        let layout = at_100();
        let palette = CaptionPalette::of(CaptionTone::DarkGlyphs);
        let active = CaptionState {
            active: true,
            ..CaptionState::default()
        };
        let a = pixel(
            &paint_buttons(&layout, 96, &palette, &active),
            &layout,
            23,
            15,
        );
        let i = pixel(
            &paint_buttons(&layout, 96, &palette, &CaptionState::default()),
            &layout,
            23,
            15,
        );
        assert!(i[3] < a[3] / 2, "{i:?} against {a:?}");
    }

    /// The top of a window `width` pixels wide whose app draws a status bar of `bar` (opaque
    /// 0xRRGGBB) under the title bar and a toolbar of `toolbar` below it, with the caption
    /// buttons drawn over it as the window draws them: what a light and a dark app window look
    /// like at the top. Rows of BGRA, `height` rows.
    fn window_top(
        width: u32,
        dpi: u32,
        tone: CaptionTone,
        state: &CaptionState,
        bar: u32,
        toolbar: u32,
    ) -> (Vec<u8>, u32) {
        let caption = scaled(31, dpi);
        let height = caption + scaled(56, dpi);
        let layout = CaptionLayout::new(width, caption, dpi, scaled(8, dpi));
        let buttons = paint_buttons(&layout, dpi, &CaptionPalette::of(tone), state);
        let strip = 3 * layout.button_width;
        let left = layout.buttons_left();
        let mut image = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let rgb = if y < caption { bar } else { toolbar };
                let below = [rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8, 255];
                let column = x as i32 - left;
                let pixel = if y < caption && column >= 0 && (column as u32) < strip {
                    let at = ((y * strip + column as u32) * 4) as usize;
                    let top = &buttons[at..at + 4];
                    let keep = 255 - u32::from(top[3]);
                    let mut mixed = [255u8; 4];
                    for (channel, value) in mixed.iter_mut().take(3).enumerate() {
                        let blended =
                            u32::from(top[channel]) + u32::from(below[channel]) * keep / 255;
                        *value = blended.min(255) as u8;
                    }
                    mixed
                } else {
                    below
                };
                image.extend_from_slice(&pixel);
            }
        }
        (image, height)
    }

    /// A 32-bit BMP file of rows of BGRA, top row first.
    fn bmp(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let size = 54 + pixels.len() as u32;
        let mut file = Vec::with_capacity(size as usize);
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&size.to_le_bytes());
        file.extend_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&(width as i32).to_le_bytes());
        // A negative height: the rows run from the top.
        file.extend_from_slice(&(-(height as i32)).to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&[0u8; 24]);
        file.extend_from_slice(pixels);
        file
    }

    /// Writes pictures of the top of a light and a dark app window, at 100% and 200%, at rest
    /// and with the pointer over close, into the folder `DSA_CAPTION_PICTURES` names. CI keeps
    /// them with the run; without the variable the pictures are only checked.
    #[test]
    fn fr22_pictures_of_a_light_and_a_dark_app_window() {
        let folder = std::env::var_os("DSA_CAPTION_PICTURES").map(std::path::PathBuf::from);
        let at_rest = CaptionState {
            active: true,
            ..CaptionState::default()
        };
        let close_hovered = CaptionState {
            hovered: Some(CaptionButton::Close),
            ..at_rest
        };
        let apps = [
            // A light app: white status bar, Material light toolbar; dark glyphs.
            ("light", StatusBarAppearance::Light, 0xFF_FFFF, 0xF3_EDF7),
            // A dark app: near-black status bar and toolbar; light glyphs.
            ("dark", StatusBarAppearance::Dark, 0x14_1218, 0x21_1F26),
        ];
        for (name, appearance, bar, toolbar) in apps {
            let tone = caption_tone(appearance, 0);
            for dpi in [96, 192] {
                for (state_name, state) in [("rest", at_rest), ("close-hover", close_hovered)] {
                    let width = scaled(480, dpi);
                    let (image, height) = window_top(width, dpi, tone, &state, bar, toolbar);
                    // The glyphs stand out from the bar: the middle of the minimize glyph
                    // differs from the bar by more than half the range.
                    let layout = CaptionLayout::new(width, scaled(31, dpi), dpi, 0);
                    let (left, _, button, caption) = layout.button_rect(CaptionButton::Minimize);
                    let x = left as u32 + button / 2;
                    let at = (((caption / 2) * width + x) * 4) as usize;
                    let shown = u32::from(image[at]);
                    assert!(
                        shown.abs_diff(bar & 0xff) > 128,
                        "{name} at {dpi}: glyph {shown} on bar {}",
                        bar & 0xff
                    );
                    if let Some(folder) = &folder {
                        std::fs::create_dir_all(folder).unwrap();
                        let percent = dpi * 100 / 96;
                        let path = folder.join(format!("{name}-app-{percent}-{state_name}.bmp"));
                        std::fs::write(path, bmp(width, height, &image)).unwrap();
                    }
                }
            }
        }
    }

    /// The BGRA pixel at (`x`, `y`) of a whole title bar painted for `layout`.
    fn bar_pixel(image: &[u8], layout: &CaptionLayout, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * layout.width + x) * 4) as usize;
        [image[at], image[at + 1], image[at + 2], image[at + 3]]
    }

    #[test]
    fn fr22_the_title_bar_shows_the_apps_icon_and_name_at_its_left() {
        for dpi in [96, 144, 192] {
            let layout = CaptionLayout::new(scaled(800, dpi), scaled(31, dpi), dpi, 0);
            let (icon_left, icon_top, side, _) = layout.icon_rect().unwrap();
            // After the WinUI padding, the small icon's size, centred in the band.
            assert_eq!(icon_left as u32, scaled(16, dpi), "at {dpi}");
            assert_eq!(side, scaled(16, dpi), "at {dpi}");
            assert_eq!(icon_top as u32, (layout.height - side) / 2, "at {dpi}");
            let (title_left, _, title_width, title_height) = layout.title_rect().unwrap();
            assert_eq!(title_left as u32, scaled(48, dpi), "at {dpi}");
            assert_eq!(title_height, layout.height);
            // An opaque red icon and a name that covers its whole rectangle.
            let icon: Vec<u8> = [0u8, 0, 255, 255].repeat((side * side) as usize);
            let title = vec![255u8; (title_width * title_height) as usize];
            let content = TitleBarContent {
                fill: None,
                icon: Some(&icon),
                title: Some(&title),
                ..TitleBarContent::default()
            };
            for (tone, glyph) in [
                (CaptionTone::DarkGlyphs, 0u8),
                (CaptionTone::LightGlyphs, 255u8),
            ] {
                let palette = CaptionPalette::of(tone);
                let active = CaptionState {
                    active: true,
                    ..CaptionState::default()
                };
                let image = paint_title_bar(&layout, dpi, &palette, &active, &content);
                assert_eq!(image.len(), (layout.width * layout.height * 4) as usize);
                let center = (icon_left as u32 + side / 2, icon_top as u32 + side / 2);
                assert_eq!(
                    bar_pixel(&image, &layout, center.0, center.1),
                    [0, 0, 255, 255]
                );
                // The name takes the glyphs' color.
                let text = bar_pixel(&image, &layout, title_left as u32 + 2, layout.height / 2);
                assert!(text[3] > 200, "{text:?}");
                let expected = u32::from(glyph) * u32::from(text[3]) / 255;
                assert!(u32::from(text[0]).abs_diff(expected) <= 1, "{text:?}");
                // Before the icon, the band stays transparent for the app.
                assert_eq!(bar_pixel(&image, &layout, 1, 1), [0, 0, 0, 0]);
                // Inactive, both are fainter.
                let resting = CaptionState::default();
                let inactive = paint_title_bar(&layout, dpi, &palette, &resting, &content);
                let at_text = (title_left as u32 + 2, layout.height / 2);
                let faint_icon = bar_pixel(&inactive, &layout, center.0, center.1);
                let faint_text = bar_pixel(&inactive, &layout, at_text.0, at_text.1);
                assert!(faint_icon[3] < 128, "{faint_icon:?}");
                assert!(
                    faint_text[3] < text[3] / 2,
                    "{faint_text:?} against {text:?}"
                );
            }
        }
    }

    #[test]
    fn fr22_a_long_name_ends_before_the_caption_buttons() {
        let layout = at_100();
        let (left, _, width, height) = layout.title_rect().unwrap();
        assert_eq!(left + width as i32, layout.buttons_left() - 8);
        let title = vec![255u8; (width * height) as usize];
        let image = paint_title_bar(
            &layout,
            96,
            &CaptionPalette::of(CaptionTone::DarkGlyphs),
            &CaptionState {
                active: true,
                ..CaptionState::default()
            },
            &TitleBarContent {
                title: Some(&title),
                ..TitleBarContent::default()
            },
        );
        // Nothing between the name's end and the buttons, nor under the buttons but glyphs.
        for x in (left as u32 + width)..layout.buttons_left() as u32 {
            assert_eq!(bar_pixel(&image, &layout, x, 3)[3], 0, "at {x}");
        }
        // A window too narrow for a name has none, and one too narrow for the icon no icon.
        let narrow = CaptionLayout::new(3 * 46 + 40, 31, 96, 0);
        assert!(narrow.title_rect().is_none());
        assert!(narrow.icon_rect().is_some());
        let narrower = CaptionLayout::new(3 * 46 + 20, 31, 96, 0);
        assert!(narrower.icon_rect().is_none());
    }

    #[test]
    fn fr22_the_icon_opens_the_system_menu() {
        let layout = at_100();
        assert!(layout.on_icon(20, 15));
        assert!(
            layout.on_icon(16, 30),
            "the band's whole height over the icon"
        );
        assert!(!layout.on_icon(20, 4), "the top resize edge comes first");
        assert!(!layout.on_icon(40, 15));
        assert!(!layout.on_icon(20, 31), "below the band");
    }

    #[test]
    fn fr22_an_app_that_leaves_the_band_empty_gets_its_top_colour_there() {
        let toolbar = 0xFF03_9BE5;
        // An old-theme app: nothing (or black) behind the band, a toolbar under it.
        assert_eq!(band_fill(&[0xFF00_0000; 8], &[toolbar; 8]), Some(toolbar));
        assert_eq!(band_fill(&[0; 8], &[toolbar; 8]), Some(toolbar));
        // An app that draws its own status bar keeps it.
        assert_eq!(band_fill(&[0xFF02_88D1; 8], &[toolbar; 8]), None);
        assert_eq!(band_fill(&[0xFFFF_FFFF; 8], &[0xFFFF_FFFF; 8]), None);
        // Black under black, or no single color under the band, changes nothing.
        assert_eq!(band_fill(&[0xFF00_0000; 8], &[0xFF00_0000; 8]), None);
        let mut picture = [toolbar; 8];
        picture[3] = 0xFF80_4020;
        assert_eq!(band_fill(&[0xFF00_0000; 8], &picture), None);
        // The fill then decides the glyphs' tone by its lightness.
        assert_eq!(
            caption_tone(StatusBarAppearance::Unspecified, 0xFFF5_F5F5),
            CaptionTone::DarkGlyphs
        );
        // And it is painted across the whole band, under the buttons too.
        let layout = at_100();
        let image = paint_title_bar(
            &layout,
            96,
            &CaptionPalette::of(CaptionTone::LightGlyphs),
            &CaptionState::default(),
            &TitleBarContent {
                fill: Some(toolbar),
                ..TitleBarContent::default()
            },
        );
        assert_eq!(bar_pixel(&image, &layout, 2, 2), [0xE5, 0x9B, 0x03, 0xFF]);
        assert_eq!(bar_pixel(&image, &layout, 799, 2), [0xE5, 0x9B, 0x03, 0xFF]);
    }

    #[test]
    fn fr22_the_glyphs_scale_with_the_window() {
        // At 200% the minimize stroke is two pixels thick and twenty long.
        let layout = CaptionLayout::new(800, 62, 192, 16);
        let image = paint_buttons(
            &layout,
            192,
            &CaptionPalette::of(CaptionTone::DarkGlyphs),
            &CaptionState {
                active: true,
                ..CaptionState::default()
            },
        );
        let drawn = |x, y| pixel(&image, &layout, x, y)[3] > 128;
        assert!(drawn(46, 30) && drawn(46, 31));
        assert!(!drawn(46, 28) && !drawn(46, 33));
        assert!(drawn(37, 30) && drawn(54, 30));
        assert!(!drawn(34, 30) && !drawn(58, 30));
    }

    #[test]
    fn fr22_the_icon_becomes_a_back_button_when_the_task_can_go_back() {
        for dpi in [96, 144, 192] {
            let layout = CaptionLayout::new(scaled(800, dpi), scaled(31, dpi), dpi, 0);
            let (left, top, width, height) = layout.icon_cell().unwrap();
            let (icon_left, _, side, _) = layout.icon_rect().unwrap();
            // The cell holds the icon with half the padding on each side, over the band.
            assert_eq!(left, icon_left - (layout.padding / 2) as i32, "at {dpi}");
            assert_eq!(width, side + 2 * (layout.padding / 2), "at {dpi}");
            assert_eq!((top, height), (0, layout.height));
            let middle = (left + width as i32 / 2, layout.height as i32 / 2);
            let icon = TitleBarMode::default();
            let back = TitleBarMode {
                back: true,
                ..TitleBarMode::default()
            };
            assert_eq!(
                title_bar_hit(&layout, middle.0, middle.1, &icon),
                TitleBarHit::SystemMenu
            );
            assert_eq!(
                title_bar_hit(&layout, middle.0, middle.1, &back),
                TitleBarHit::Back
            );
            // Past the cell the band drags the window, whichever the cell shows.
            let after = left + width as i32;
            for mode in [&icon, &back] {
                assert_eq!(
                    title_bar_hit(&layout, after, middle.1, mode),
                    TitleBarHit::Window(CaptionHit::Caption)
                );
            }
            // The cell, the name and what an app is told do not move between the two.
            let band = layout.band();
            assert_eq!(band.start_width, (left + width as i32) as u32);
            assert_eq!(band.end_width, 3 * layout.button_width);
            assert_eq!(band.height, layout.height);

            // The cell draws the icon, or the arrow in the glyphs' color instead of it.
            let red: Vec<u8> = [0u8, 0, 255, 255].repeat((side * side) as usize);
            let palette = CaptionPalette::of(CaptionTone::DarkGlyphs);
            let active = CaptionState {
                active: true,
                ..CaptionState::default()
            };
            let with_icon = TitleBarContent {
                icon: Some(&red),
                ..TitleBarContent::default()
            };
            let with_back = TitleBarContent {
                back: Some(BackButton::default()),
                ..with_icon
            };
            let shown = paint_title_bar(&layout, dpi, &palette, &active, &with_icon);
            let arrow = paint_title_bar(&layout, dpi, &palette, &active, &with_back);
            let (cx, cy) = (middle.0 as u32, middle.1 as u32);
            assert_eq!(bar_pixel(&shown, &layout, cx, cy), [0, 0, 255, 255]);
            let shaft = bar_pixel(&arrow, &layout, cx, cy);
            assert!(
                shaft[3] > 128 && shaft[2] < 64,
                "a dark arrow, not the icon: {shaft:?}"
            );
            // The arrow's head points left: its tip is left of the middle, nothing to the right
            // of the shaft's end above it.
            let tip = (left as u32 + width / 2 - scaled(5, dpi), cy);
            assert!(
                bar_pixel(&arrow, &layout, tip.0, tip.1)[3] > 128,
                "at {dpi}"
            );
            let above_end = (
                left as u32 + width / 2 + scaled(4, dpi),
                cy - scaled(3, dpi),
            );
            assert_eq!(bar_pixel(&arrow, &layout, above_end.0, above_end.1)[3], 0);
            // Hovered, it takes the caption buttons' hover background.
            let hovered = TitleBarContent {
                back: Some(BackButton {
                    hovered: true,
                    pressed: false,
                }),
                ..with_icon
            };
            let hover = paint_title_bar(&layout, dpi, &palette, &active, &hovered);
            let corner = bar_pixel(&hover, &layout, left as u32 + 1, 1);
            assert_eq!(corner[3], (palette.hover >> 24) as u8, "at {dpi}");
        }
    }

    #[test]
    fn fr22_an_app_that_draws_its_own_title_bar_gets_its_controls_in_the_band() {
        let layout = at_100();
        let controls = [Bounds {
            left: 60,
            top: 0,
            right: 300,
            bottom: 31,
        }];
        let app_drawn = TitleBarMode {
            app_drawn: true,
            interactive: &controls,
            back: false,
        };
        // On a control (a tab) the pointer goes to the app.
        assert_eq!(
            title_bar_hit(&layout, 100, 20, &app_drawn),
            TitleBarHit::Window(CaptionHit::Client)
        );
        // Between the controls the band drags the window, and the buttons stay buttons.
        assert_eq!(
            title_bar_hit(&layout, 400, 20, &app_drawn),
            TitleBarHit::Window(CaptionHit::Caption)
        );
        assert_eq!(
            title_bar_hit(&layout, 760, 20, &app_drawn),
            TitleBarHit::Window(CaptionHit::Button(CaptionButton::Close))
        );
        assert_eq!(
            title_bar_hit(&layout, 20, 15, &app_drawn),
            TitleBarHit::SystemMenu
        );
        // A control under the buttons or the icon's cell never takes them.
        let greedy = [Bounds {
            left: 0,
            top: 0,
            right: 800,
            bottom: 31,
        }];
        let greedy = TitleBarMode {
            interactive: &greedy,
            ..app_drawn
        };
        assert_eq!(
            title_bar_hit(&layout, 708, 20, &greedy),
            TitleBarHit::Window(CaptionHit::Button(CaptionButton::Maximize))
        );
        assert_eq!(
            title_bar_hit(&layout, 20, 15, &greedy),
            TitleBarHit::SystemMenu
        );
        assert_eq!(
            title_bar_hit(&layout, 400, 4, &greedy),
            TitleBarHit::Window(CaptionHit::Top)
        );
        // Withdrawn, the band is the window's again.
        let default = TitleBarMode {
            interactive: &controls,
            ..TitleBarMode::default()
        };
        assert_eq!(
            title_bar_hit(&layout, 100, 20, &default),
            TitleBarHit::Window(CaptionHit::Caption)
        );
        // It is told how far the icon's cell and the buttons reach in.
        assert_eq!(
            layout.band(),
            CaptionBand {
                height: 31,
                start_width: 40,
                end_width: 138,
            }
        );
        // No name and no fill are drawn over an app's own title bar: the band stays clear
        // but for the icon and the buttons.
        let (left, _, title_width, title_height) = layout.title_rect().unwrap();
        let title = vec![255u8; (title_width * title_height) as usize];
        let content = TitleBarContent {
            fill: Some(0xFF20_4080),
            title: Some(&title),
            app_drawn: true,
            ..TitleBarContent::default()
        };
        let image = paint_title_bar(
            &layout,
            96,
            &CaptionPalette::of(CaptionTone::DarkGlyphs),
            &CaptionState {
                active: true,
                ..CaptionState::default()
            },
            &content,
        );
        assert_eq!(
            bar_pixel(&image, &layout, left as u32 + 2, 15),
            [0, 0, 0, 0]
        );
        assert_eq!(bar_pixel(&image, &layout, 400, 15), [0, 0, 0, 0]);
    }
}
