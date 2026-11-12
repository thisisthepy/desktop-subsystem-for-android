//! What a task window's title bar shows and what its points do, with no Wayland in it.
//!
//! The band over the app's top inset carries, from the left: the icon's cell, the app's name
//! and, at the right, the caption buttons, as on the Windows host
//! ([`paint_title_bar`]). The icon is the app's adaptive icon's foreground layer alone when
//! it has one, so no square plate stands over the app's content, else its whole launcher
//! icon; the desktop's app menu and dock keep the whole icon. While the task can go back the
//! cell shows a back arrow instead, which sends the task Back; at the first screen it shows
//! the icon again and opens the window menu.
//!
//! An app may draw its own title bar (a transparent caption bar background, as on Android's
//! desktop windows). The window then draws no name and no fill, only the icon's cell and the
//! buttons over the app, and the points of the band on the app's controls go to the app. The
//! guest gives the controls' rectangles in the task's pixels, and the window manager turns
//! them into the window's (the task's at the window's frame scale) before they come here, so
//! they are used as they come.

use dsa_host_core::compositor::caption::{
    band_sample, caption_icon, caption_tone, paint_title_bar, title_bar_hit, BackButton,
    BandSample, CaptionLayout, CaptionPalette, CaptionState, CaptionTone, TitleBarContent,
    TitleBarHit, TitleBarMode,
};
use dsa_host_core::tasks::{AppInfo, Bounds, StatusBarAppearance};

use crate::shell::png::{self, Rgba};

/// The PNG the title bar of `app`'s windows shows: the adaptive icon's foreground layer when
/// the guest gave one, else the whole launcher icon.
pub fn caption_icon_png(app: &AppInfo) -> &[u8] {
    if app.caption_icon_png.is_empty() {
        &app.icon_png
    } else {
        &app.caption_icon_png
    }
}

/// Whether the title bar of `app`'s windows shows an adaptive icon's foreground alone, with
/// no plate behind it ([`caption_icon_png`]).
pub fn caption_icon_is_logo(app: &AppInfo) -> bool {
    !app.caption_icon_png.is_empty()
}

/// An app's title bar icon, decoded once and kept at the side the window draws it.
#[derive(Debug, Clone, Default)]
pub struct CaptionIcon {
    image: Option<Rgba>,
    /// Whether it is an adaptive icon's foreground alone, which is enlarged to the size of a
    /// whole icon ([`caption_icon`]).
    logo: bool,
    scaled: Option<(u32, Vec<u8>)>,
}

impl CaptionIcon {
    /// The whole icon in `png`, background and all; one that does not decode leaves the cell
    /// empty.
    pub fn new(png: &[u8]) -> CaptionIcon {
        CaptionIcon {
            image: (!png.is_empty()).then(|| png::decode(png).ok()).flatten(),
            logo: false,
            scaled: None,
        }
    }

    /// An adaptive icon's foreground in `png`, drawn without its plate: its logo is cropped
    /// to what it draws and fitted to the box a whole icon fills.
    pub fn logo(png: &[u8]) -> CaptionIcon {
        CaptionIcon {
            logo: true,
            ..CaptionIcon::new(png)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.image.is_none()
    }

    /// The icon `side` pixels square, as premultiplied BGRA rows.
    pub fn at(&mut self, side: u32) -> Option<&[u8]> {
        let image = self.image.as_ref()?;
        if side == 0 {
            return None;
        }
        if self.scaled.as_ref().map(|(at, _)| *at) != Some(side) {
            let pixels = if self.logo {
                let bgra = premultiplied_bgra(image);
                caption_icon(&bgra, image.width, image.height, side, true)
            } else {
                scale_premultiplied(image, side)
            };
            self.scaled = Some((side, pixels));
        }
        self.scaled.as_ref().map(|(_, pixels)| pixels.as_slice())
    }
}

/// `image` (straight RGBA) as premultiplied BGRA rows.
fn premultiplied_bgra(image: &Rgba) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.pixels.len());
    for pixel in image.pixels.chunks_exact(4) {
        let alpha = u32::from(pixel[3]);
        let premultiply = |value: u8| ((u32::from(value) * alpha + 127) / 255) as u8;
        out.extend_from_slice(&[
            premultiply(pixel[2]),
            premultiply(pixel[1]),
            premultiply(pixel[0]),
            pixel[3],
        ]);
    }
    out
}

/// `image` fitted into a square of `side` pixels, centred, as premultiplied BGRA rows. Each
/// pixel is the average of the source area it covers, so a large icon shrinks without
/// shimmering and its edges keep their partial coverage.
pub fn scale_premultiplied(image: &Rgba, side: u32) -> Vec<u8> {
    let (width, height) = (f64::from(image.width), f64::from(image.height));
    let mut out = vec![0u8; side as usize * side as usize * 4];
    if image.width == 0 || image.height == 0 || side == 0 {
        return out;
    }
    let scale = (f64::from(side) / width).min(f64::from(side) / height);
    let (left, top) = (
        (f64::from(side) - width * scale) / 2.0,
        (f64::from(side) - height * scale) / 2.0,
    );
    let area = 1.0 / (scale * scale);
    for y in 0..side {
        let y0 = ((f64::from(y) - top) / scale).max(0.0);
        let y1 = ((f64::from(y) + 1.0 - top) / scale).min(height);
        if y1 <= y0 {
            continue;
        }
        for x in 0..side {
            let x0 = ((f64::from(x) - left) / scale).max(0.0);
            let x1 = ((f64::from(x) + 1.0 - left) / scale).min(width);
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
                    let at = (row as usize * image.width as usize + column as usize) * 4;
                    let pixel = &image.pixels[at..at + 4];
                    let alpha = f64::from(pixel[3]) / 255.0;
                    sum[0] += f64::from(pixel[2]) * alpha * weight;
                    sum[1] += f64::from(pixel[1]) * alpha * weight;
                    sum[2] += f64::from(pixel[0]) * alpha * weight;
                    sum[3] += f64::from(pixel[3]) * weight;
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

/// Text as one coverage byte (0 to 255) per pixel, rows from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

/// `text` placed in a rectangle `width` x `height` pixels: at its left, centred top to
/// bottom, cut at its right edge. This is what [`TitleBarContent::title`] takes.
pub fn place_text(text: &Coverage, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; width as usize * height as usize];
    let top = (i64::from(height) - i64::from(text.height)) / 2;
    for row in 0..text.height as i64 {
        let y = top + row;
        if y < 0 || y >= i64::from(height) {
            continue;
        }
        let columns = text.width.min(width) as usize;
        let from = row as usize * text.width as usize;
        let to = y as usize * width as usize;
        out[to..to + columns].copy_from_slice(&text.alpha[from..from + columns]);
    }
    out
}

/// What a window draws in its band besides the caption buttons.
#[derive(Debug, Clone, Copy, Default)]
pub struct BandArt<'a> {
    /// The app's icon, `layout.icon` pixels square, premultiplied BGRA ([`CaptionIcon::at`]).
    pub icon: Option<&'a [u8]>,
    /// The app's name as coverage, the size of the layout's title rectangle ([`place_text`]).
    pub title: Option<&'a [u8]>,
    /// The color the band is filled with, when the app leaves it empty ([`frame_band_sample`]).
    pub fill: Option<u32>,
}

/// What the title bar of one window shows besides its layout and buttons.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleBar {
    /// Whether the app draws its own title bar.
    pub app_drawn: bool,
    /// Where the app's controls are in the band, in the window's (the task's) pixels.
    pub interactive: Vec<Bounds>,
    /// Whether the task can go back, so the icon's cell shows the back arrow.
    pub back: bool,
    /// Where the pointer leaves the back arrow.
    pub back_state: BackButton,
}

impl TitleBar {
    /// The app starts or stops drawing its own title bar, with its controls at `interactive`.
    pub fn set_mode(&mut self, app_drawn: bool, interactive: Vec<Bounds>) {
        self.app_drawn = app_drawn;
        self.interactive = if app_drawn { interactive } else { Vec::new() };
    }

    /// The task can go back, or is at its first screen again.
    pub fn set_back(&mut self, back: bool) {
        self.back = back;
        if !back {
            self.back_state = BackButton::default();
        }
    }

    /// The pointer is over the back arrow or not. Returns whether its look changed.
    pub fn hover_back(&mut self, hovered: bool) -> bool {
        let hovered = hovered && self.back;
        let changed = self.back_state.hovered != hovered;
        self.back_state.hovered = hovered;
        changed
    }

    /// The back arrow is pressed or let go. Returns whether its look changed.
    pub fn press_back(&mut self, pressed: bool) -> bool {
        let pressed = pressed && self.back;
        let changed = self.back_state.pressed != pressed;
        self.back_state.pressed = pressed;
        changed
    }

    /// What the window point (`x`, `y`) of the band belongs to.
    pub fn hit(&self, layout: &CaptionLayout, x: i32, y: i32) -> TitleBarHit {
        title_bar_hit(
            layout,
            x,
            y,
            &TitleBarMode {
                app_drawn: self.app_drawn,
                interactive: &self.interactive,
                back: self.back,
            },
        )
    }

    /// The band as premultiplied BGRA rows, `layout.width` x `layout.height`: the icon (or
    /// the back arrow), the name and the buttons, in the tone the app's status bar calls for,
    /// or the tone of `fill` when the band is filled.
    pub fn paint(
        &self,
        layout: &CaptionLayout,
        dpi: u32,
        tone: CaptionTone,
        state: &CaptionState,
        art: BandArt,
    ) -> Vec<u8> {
        let fill = art.fill.filter(|_| !self.app_drawn);
        let tone = match fill {
            Some(color) => caption_tone(StatusBarAppearance::Unspecified, color),
            None => tone,
        };
        paint_title_bar(
            layout,
            dpi,
            &CaptionPalette::of(tone),
            state,
            &TitleBarContent {
                fill,
                icon: art.icon,
                title: art.title,
                app_drawn: self.app_drawn,
                back: self.back.then_some(self.back_state),
            },
        )
    }
}

/// What a frame shows under its band ([`BandSample`]): the color to fill the band with when
/// the app leaves it empty or black (the app's color just under the band), and how light the
/// band is, which the caption glyphs' tone follows. `pixels` are the frame's `XRGB8888` rows
/// (blue, green, red, unused in memory), `width` x `height`, and the band is `band` rows
/// high. The default when the frame has no band or its rows cannot be read.
pub fn frame_band_sample(pixels: &[u8], width: u32, height: u32, band: u32) -> BandSample {
    let Some([inside, below]) = crate::compositor::frames::band_probe_rows(band, height) else {
        return BandSample::default();
    };
    let row = width as usize * 4;
    let row_at = |y: u32| pixels.get(y as usize * row..(y as usize + 1) * row);
    match (row_at(inside), row_at(below)) {
        (Some(inside), Some(below)) => rows_band_sample(inside, below, width),
        _ => BandSample::default(),
    }
}

/// The band's sample ([`BandSample`]) from the two rows
/// [`crate::compositor::frames::band_probe_rows`] names, as `XRGB8888` rows `width` pixels
/// wide: `inside` from the middle of the band, `below` from just under it. This is all a GPU
/// frame reads back for the band.
pub fn rows_band_sample(inside: &[u8], below: &[u8], width: u32) -> BandSample {
    if width == 0 {
        return BandSample::default();
    }
    const SAMPLES: u32 = 16;
    let color = |row: &[u8], x: u32| -> Option<u32> {
        let at = x as usize * 4;
        let pixel = row.get(at..at + 4)?;
        Some(
            0xFF00_0000
                | (u32::from(pixel[2]) << 16)
                | (u32::from(pixel[1]) << 8)
                | u32::from(pixel[0]),
        )
    };
    let columns: Vec<u32> = (0..SAMPLES)
        .map(|index| (2 * index + 1) * width / (2 * SAMPLES))
        .collect();
    let inside: Option<Vec<u32>> = columns.iter().map(|&x| color(inside, x)).collect();
    let below: Option<Vec<u32>> = columns.iter().map(|&x| color(below, x)).collect();
    match (inside, below) {
        (Some(inside), Some(below)) => band_sample(&inside, &below),
        _ => BandSample::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_band_fill(pixels: &[u8], width: u32, height: u32, band: u32) -> Option<u32> {
        frame_band_sample(pixels, width, height, band).fill
    }

    fn rows_band_fill(inside: &[u8], below: &[u8], width: u32) -> Option<u32> {
        rows_band_sample(inside, below, width).fill
    }
    use dsa_host_core::compositor::caption::{CaptionButton, CaptionHit};

    fn app(icon: &[u8], caption_icon: &[u8]) -> AppInfo {
        AppInfo {
            package_name: "com.example.notes".into(),
            icon_png: icon.to_vec(),
            caption_icon_png: caption_icon.to_vec(),
            ..AppInfo::default()
        }
    }

    fn solid_png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        png::encode(
            &Rgba {
                width,
                height,
                pixels: rgba.repeat((width * height) as usize),
            },
            true,
        )
    }

    #[test]
    fn fr22_the_title_bar_icon_is_the_adaptive_foreground() {
        let full = solid_png(4, 4, [0, 0, 0xff, 0xff]);
        let foreground = solid_png(4, 4, [0xff, 0, 0, 0xff]);
        // An adaptive icon: the title bar takes its foreground layer alone.
        let adaptive = app(&full, &foreground);
        assert_eq!(caption_icon_png(&adaptive), foreground.as_slice());
        // No adaptive icon: the whole icon.
        let plain = app(&full, &[]);
        assert_eq!(caption_icon_png(&plain), full.as_slice());
        // The foreground is what is drawn, red, in premultiplied BGRA.
        let mut icon = CaptionIcon::new(caption_icon_png(&adaptive));
        let pixels = icon.at(2).unwrap().to_vec();
        assert_eq!(pixels, [0, 0, 0xff, 0xff].repeat(4));
        // Drawn in the band, at the icon's place.
        let layout = CaptionLayout::new(400, 32, 96, 0);
        let side = layout.icon;
        let band = TitleBar::default().paint(
            &layout,
            96,
            CaptionTone::DarkGlyphs,
            &CaptionState {
                active: true,
                ..CaptionState::default()
            },
            BandArt {
                icon: icon.at(side),
                ..BandArt::default()
            },
        );
        let (left, top, _, _) = layout.icon_rect().unwrap();
        let at = ((top as usize + 2) * 400 + left as usize + 2) * 4;
        assert_eq!(&band[at..at + 4], &[0, 0, 0xff, 0xff]);
    }

    #[test]
    fn fr22_a_logo_only_title_bar_icon_fills_the_box_of_a_whole_icon() {
        // A foreground whose logo covers the middle half, where the plate surrounded it.
        let mut pixels = Vec::new();
        for y in 0..64u32 {
            for x in 0..64u32 {
                let inside = (16..48).contains(&x) && (16..48).contains(&y);
                pixels.extend_from_slice(if inside { &[0xff, 0, 0, 0xff] } else { &[0; 4] });
            }
        }
        let foreground = png::encode(
            &Rgba {
                width: 64,
                height: 64,
                pixels,
            },
            true,
        );
        let adaptive = app(&solid_png(4, 4, [0, 0, 0xff, 0xff]), &foreground);
        assert!(caption_icon_is_logo(&adaptive));
        assert!(!caption_icon_is_logo(&app(&foreground, &[])));
        let opaque_columns = |icon: &[u8], side: u32| {
            let row = side / 2;
            (0..side)
                .filter(|x| icon[((row * side + x) * 4 + 3) as usize] >= 128)
                .count() as u32
        };
        for side in [16, 20, 24, 32] {
            let padding = dsa_host_core::compositor::caption::logo_padding(side);
            let logo = CaptionIcon::logo(&foreground).at(side).unwrap().to_vec();
            assert_eq!(opaque_columns(&logo, side), side - 2 * padding, "at {side}");
            // Red, premultiplied BGRA, in the middle.
            let middle = (((side / 2) * side + side / 2) * 4) as usize;
            assert_eq!(&logo[middle..middle + 4], &[0, 0, 0xff, 0xff]);
            // The same image as a whole icon is drawn as it is: half the box.
            let whole = CaptionIcon::new(&foreground).at(side).unwrap().to_vec();
            assert_eq!(opaque_columns(&whole, side), side / 2, "at {side}");
        }
    }

    #[test]
    fn fr22_a_large_icon_shrinks_by_averaging_and_keeps_its_shape() {
        // Left half opaque white, right half transparent, shrunk to 2 x 2.
        let mut pixels = Vec::new();
        for _ in 0..4 {
            pixels.extend([[0xff; 4], [0xff; 4], [0; 4], [0; 4]].concat());
        }
        let image = Rgba {
            width: 4,
            height: 4,
            pixels,
        };
        let scaled = scale_premultiplied(&image, 2);
        assert_eq!(&scaled[0..4], &[0xff; 4]);
        assert_eq!(&scaled[4..8], &[0; 4]);
        // A wide image is centred in its square, with transparent rows above and below.
        let wide = Rgba {
            width: 4,
            height: 2,
            pixels: [0xff; 4].repeat(8),
        };
        let scaled = scale_premultiplied(&wide, 4);
        assert_eq!(&scaled[0..4], &[0; 4]);
        assert_eq!(&scaled[4 * 4..4 * 4 + 4], &[0xff; 4]);
    }

    #[test]
    fn fr22_the_icon_becomes_a_back_button_when_the_task_can_go_back() {
        let layout = CaptionLayout::new(400, 32, 96, 4);
        let (left, _, width, height) = layout.icon_cell().unwrap();
        let (x, y) = (left + width as i32 / 2, height as i32 / 2);
        let mut bar = TitleBar::default();
        assert_eq!(bar.hit(&layout, x, y), TitleBarHit::SystemMenu);
        let before = layout.band();
        bar.set_back(true);
        assert_eq!(bar.hit(&layout, x, y), TitleBarHit::Back);
        // The cell keeps its place and size, and the guest is told the same band.
        assert_eq!(layout.icon_cell(), Some((left, 0, width, height)));
        assert_eq!(layout.band(), before);
        // Hover and press change the arrow's look, as the caption buttons' do.
        assert!(bar.hover_back(true));
        assert!(!bar.hover_back(true));
        assert!(bar.press_back(true));
        let state = CaptionState {
            active: true,
            ..CaptionState::default()
        };
        let pressed = bar.paint(
            &layout,
            96,
            CaptionTone::DarkGlyphs,
            &state,
            BandArt::default(),
        );
        bar.press_back(false);
        bar.hover_back(false);
        let rest = bar.paint(
            &layout,
            96,
            CaptionTone::DarkGlyphs,
            &state,
            BandArt::default(),
        );
        let corner = ((2 * 400) + left as usize + 1) * 4;
        assert_ne!(&pressed[corner..corner + 4], &rest[corner..corner + 4]);
        // The arrow is drawn where the icon was: some pixel of the cell is not clear.
        let cell_drawn = (0..height as usize).any(|row| {
            (left as usize..left as usize + width as usize).any(|column| {
                let at = (row * 400 + column) * 4;
                rest[at + 3] != 0
            })
        });
        assert!(cell_drawn);
        // Back at the first screen: the icon and the window menu again.
        bar.hover_back(true);
        bar.set_back(false);
        assert_eq!(bar.hit(&layout, x, y), TitleBarHit::SystemMenu);
        assert_eq!(bar.back_state, BackButton::default());
    }

    #[test]
    fn fr22_an_app_that_draws_its_title_bar_gets_the_pointer_on_its_controls() {
        let layout = CaptionLayout::new(400, 32, 96, 4);
        let mut bar = TitleBar::default();
        let tab = Bounds {
            left: 100,
            top: 0,
            right: 200,
            bottom: 32,
        };
        // The default title bar keeps the whole band, controls or not.
        bar.set_mode(false, vec![tab]);
        assert!(bar.interactive.is_empty());
        assert_eq!(
            bar.hit(&layout, 150, 16),
            TitleBarHit::Window(CaptionHit::Caption)
        );
        bar.set_mode(true, vec![tab]);
        assert_eq!(
            bar.hit(&layout, 150, 16),
            TitleBarHit::Window(CaptionHit::Client)
        );
        // The rest of the band still drags, the buttons and the icon's cell stay the window's.
        assert_eq!(
            bar.hit(&layout, 250, 16),
            TitleBarHit::Window(CaptionHit::Caption)
        );
        assert_eq!(
            bar.hit(&layout, 390, 16),
            TitleBarHit::Window(CaptionHit::Button(CaptionButton::Close))
        );
        let (left, _, _, _) = layout.icon_cell().unwrap();
        assert_eq!(bar.hit(&layout, left + 2, 16), TitleBarHit::SystemMenu);
    }

    #[test]
    fn fr22_an_app_drawn_title_bar_shows_no_name_and_no_fill() {
        let layout = CaptionLayout::new(400, 32, 96, 0);
        let (title_left, _, title_width, title_height) = layout.title_rect().unwrap();
        let title = vec![0xff; (title_width * title_height) as usize];
        let state = CaptionState {
            active: true,
            ..CaptionState::default()
        };
        let mut bar = TitleBar::default();
        let at = ((16 * 400) + title_left as usize + 4) * 4;
        let drawn = bar.paint(
            &layout,
            96,
            CaptionTone::DarkGlyphs,
            &state,
            BandArt {
                icon: None,
                title: Some(&title),
                fill: Some(0xFF20_4060),
            },
        );
        assert_ne!(drawn[at + 3], 0, "the default title bar draws the name");
        bar.set_mode(true, Vec::new());
        let app_drawn = bar.paint(
            &layout,
            96,
            CaptionTone::DarkGlyphs,
            &state,
            BandArt {
                icon: None,
                title: Some(&title),
                fill: Some(0xFF20_4060),
            },
        );
        assert_eq!(&app_drawn[at..at + 4], &[0, 0, 0, 0]);
        // The buttons stay.
        let (close_left, _, close_width, _) = layout.button_rect(CaptionButton::Close);
        let middle = ((16 * 400) + (close_left + close_width as i32 / 2) as usize) * 4;
        assert_ne!(app_drawn[middle + 3], 0);
    }

    #[test]
    fn fr22_an_app_that_leaves_the_band_empty_gets_its_top_colour_there() {
        let (width, height, band) = (64u32, 48u32, 16u32);
        let mut pixels = Vec::new();
        for y in 0..height {
            for _ in 0..width {
                let pixel = if y < band {
                    [0, 0, 0, 0xff]
                } else {
                    [0x30, 0x60, 0x90, 0xff]
                };
                pixels.extend_from_slice(&pixel);
            }
        }
        assert_eq!(
            frame_band_fill(&pixels, width, height, band),
            Some(0xFF90_6030)
        );
        // An app that draws behind the band keeps it.
        let mut drawn = pixels.clone();
        for pixel in drawn[..(width * band * 4) as usize].chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0x10, 0x80, 0x10, 0xff]);
        }
        assert_eq!(frame_band_fill(&drawn, width, height, band), None);
        assert_eq!(frame_band_fill(&pixels, width, height, 0), None);
    }

    #[test]
    fn fr22_a_gpu_frame_fills_its_band_from_the_two_rows_read_back() {
        let width = 64u32;
        let row = |pixel: [u8; 4]| pixel.repeat(width as usize);
        let empty = row([0, 0, 0, 0xff]);
        let toolbar = row([0x30, 0x60, 0x90, 0xff]);
        assert_eq!(rows_band_fill(&empty, &toolbar, width), Some(0xFF90_6030));
        assert_eq!(rows_band_fill(&toolbar, &toolbar, width), None);
        // Rows shorter than the width say nothing.
        assert_eq!(rows_band_fill(&empty[..8], &toolbar, width), None);
        assert_eq!(rows_band_fill(&empty, &toolbar, 0), None);
    }

    #[test]
    fn fr22_a_frame_tells_how_light_its_band_is() {
        use dsa_host_core::compositor::caption::shown_tone;
        let (width, height, band) = (64u32, 48u32, 16u32);
        // An app drawing edge to edge with a transparent status bar: its header runs under
        // the band, light or dark, whatever its status bar reports.
        let frame = |header: [u8; 4]| -> Vec<u8> { header.repeat((width * height) as usize) };
        let light = frame_band_sample(&frame([0xFA, 0xF9, 0xF8, 0xff]), width, height, band);
        let dark = frame_band_sample(&frame([0x24, 0x21, 0x20, 0xff]), width, height, band);
        assert!(light.luminance.is_some_and(|l| l > 900), "{light:?}");
        assert!(dark.luminance.is_some_and(|l| l < 50), "{dark:?}");
        for reported in [CaptionTone::DarkGlyphs, CaptionTone::LightGlyphs] {
            assert_eq!(
                shown_tone(reported, light, false, reported),
                CaptionTone::DarkGlyphs
            );
            assert_eq!(
                shown_tone(reported, dark, false, reported),
                CaptionTone::LightGlyphs
            );
        }
        // A frame with no band, or rows that cannot be read, says nothing.
        assert_eq!(
            frame_band_sample(&frame([0xff; 4]), width, height, 0),
            BandSample::default()
        );
        assert_eq!(
            frame_band_sample(&[], width, height, band),
            BandSample::default()
        );
    }

    #[test]
    fn fr22_the_name_sits_at_the_left_centred_in_the_band() {
        let text = Coverage {
            width: 3,
            height: 2,
            alpha: vec![1, 2, 3, 4, 5, 6],
        };
        let placed = place_text(&text, 2, 4);
        assert_eq!(placed, vec![0, 0, 1, 2, 4, 5, 0, 0]);
    }
}
