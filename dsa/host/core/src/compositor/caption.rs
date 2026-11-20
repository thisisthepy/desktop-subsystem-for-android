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
//! - [`reported_tone`]: whether the buttons' glyphs are dark or light, from what the app told
//!   the system about its status bar, as Android's own status bar takes its icons' tone, and
//!   [`shown_tone`]: the tone actually drawn, which starts from the reported one whenever the
//!   app said something, gives way where the app's frame leaves that tone unreadable all
//!   across the band, and otherwise comes from the pixels of the app's frame under the band
//!   ([`band_sample`]), so the glyphs stay readable over what the app really draws there.
//! - [`paint_buttons`]: the buttons as an image, in the tone and state they are in.
//! - [`paint_title_bar`]: the whole band as an image: the app's icon and name at the left
//!   (the platform lays the name out), the buttons at the right, and a fill when
//!   [`band_fill`] finds that the app leaves the band empty.
//! - [`title_bar_hit`]: the same, with the icon's cell (the system menu, or a back button
//!   while the task can go back) and an app that draws its own title bar, whose controls in
//!   the band take the pointer.
//!
//! An app the user set to start below the title bar gives up that unified look: the guest
//! gives it no top inset to handle, its content begins under the band, and the window paints
//! the band opaque, in the color the app's own decor painted there
//! ([`below_title_bar_sample`]).
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

impl CaptionTone {
    /// The tone in words, for the log.
    pub fn describe(self) -> &'static str {
        match self {
            CaptionTone::DarkGlyphs => "dark glyphs",
            CaptionTone::LightGlyphs => "light glyphs",
        }
    }
}

/// The tone of a task window's caption glyphs as the app asked for it: dark over a light
/// status bar, light over a dark one. What the app said about its status bar's icons decides,
/// as the guest's window manager reports it while the app changes it (the appearance of the
/// window that controls the bars, screen by screen and tab by tab); when it said nothing, the
/// status bar's color does, if it set an opaque one. `None` when the app expressed neither:
/// the window then reads the tone from its pixels ([`shown_tone`]).
pub fn reported_tone(
    appearance: StatusBarAppearance,
    status_bar_color: u32,
) -> Option<CaptionTone> {
    match appearance {
        StatusBarAppearance::Light => Some(CaptionTone::DarkGlyphs),
        StatusBarAppearance::Dark => Some(CaptionTone::LightGlyphs),
        StatusBarAppearance::Unspecified => {
            let alpha = status_bar_color >> 24;
            if alpha < 0x80 {
                return None;
            }
            if relative_luminance(status_bar_color) >= 0.5 {
                Some(CaptionTone::DarkGlyphs)
            } else {
                Some(CaptionTone::LightGlyphs)
            }
        }
    }
}

/// [`reported_tone`], with dark glyphs when the app expressed nothing, as over a light title
/// bar. It also gives the tone of a color the window fills the band with.
pub fn caption_tone(appearance: StatusBarAppearance, status_bar_color: u32) -> CaptionTone {
    reported_tone(appearance, status_bar_color).unwrap_or_default()
}

/// What a window read from the pixels of its app's frame under the title bar: the color to
/// fill a band the app leaves empty ([`band_fill`]), how light the band is
/// ([`band_luminance`]) and how far apart its darkest and lightest parts are
/// ([`band_range`]), and the band's own color when it is one color ([`band_color`]). The
/// default is a frame that could not be sampled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BandSample {
    pub fill: Option<u32>,
    /// The color of the band itself (0xAARRGGBB, opaque), when every sample of it is opaque
    /// and alike; `None` over a band of more than one color.
    pub band: Option<u32>,
    /// The band's mean relative luminance, in thousandths (0 black, 1000 white).
    pub luminance: Option<u16>,
    /// The relative luminance of the band's darkest and of its lightest sample, in
    /// thousandths; `None` unless every sample is opaque.
    pub range: Option<(u16, u16)>,
}

/// Samples of a frame's band (`band`, from inside it) and of the row under it (`below`), each
/// 0xAARRGGBB, as a window reads them.
pub fn band_sample(band: &[u32], below: &[u32]) -> BandSample {
    BandSample {
        fill: band_fill(band, below),
        luminance: band_luminance(band),
        range: band_range(band),
        band: band_color(band),
    }
}

/// The color of a band whose samples (`band`, 0xAARRGGBB each) are all opaque and alike, made
/// fully opaque: what the app's decor painted there, as the framework paints an app's status
/// bar color, or its window background under a transparent one. `None` when the band has no
/// samples, or more than one color (a picture, a gradient, a screen sliding in).
pub fn band_color(band: &[u32]) -> Option<u32> {
    let first = *band.first()?;
    if first >> 24 < 0xF0 || !band.iter().all(|color| alike(*color, first)) {
        return None;
    }
    Some(0xFF00_0000 | (first & 0x00FF_FFFF))
}

/// The surface colors a band takes when the app's frame gives it none: the title bar of a
/// light and of a dark window.
pub const LIGHT_SURFACE: u32 = 0xFFF3_F3F3;
pub const DARK_SURFACE: u32 = 0xFF20_2020;

/// What the window of an app that starts below the title bar takes from `sample`: the same
/// sample with a fill, so the band is painted opaque and the glyphs take the tone of the
/// fill ([`shown_tone`]). `reported` is the tone the app asked for, and `filled` the color
/// the band is filled with now.
///
/// The guest gives such an app no top inset, so the app draws nothing of its own in the band;
/// the guest still composes it, and the framework paints the app's status bar color there, or
/// the window's background when that color is transparent. The fill is, in this order:
///
/// - the color just under the band, when the band is empty or black and the row under it is
///   one color, as for any window ([`band_fill`]): an app with an old theme that paints no
///   bar background still gets its toolbar's color;
/// - the band's own color, when the band is one color ([`band_color`]);
/// - the color it is filled with already, when the band has more than one color (a
///   background that is a picture or a gradient, or a screen sliding in), so the band never
///   flickers through a transition;
/// - else a surface color: [`DARK_SURFACE`] when the app asked for light glyphs, or said
///   nothing and the band is dark, and [`LIGHT_SURFACE`] otherwise.
///
/// A frame that could not be sampled at all leaves the band as it is: unfilled before the
/// first frame, so a window never opens with a color that is not the app's.
pub fn below_title_bar_sample(
    sample: BandSample,
    reported: Option<CaptionTone>,
    filled: Option<u32>,
) -> BandSample {
    let surface = || {
        let dark = match (reported, sample.luminance) {
            (Some(tone), _) => tone == CaptionTone::LightGlyphs,
            (None, Some(luminance)) => luminance < EQUAL_CONTRAST,
            (None, None) => false,
        };
        if dark {
            DARK_SURFACE
        } else {
            LIGHT_SURFACE
        }
    };
    let fill = if sample == BandSample::default() {
        filled
    } else {
        Some(
            sample
                .fill
                .or(sample.band)
                .or(filled)
                .unwrap_or_else(surface),
        )
    };
    BandSample { fill, ..sample }
}

/// The relative luminance of the darkest and of the lightest sample of `band` (0xAARRGGBB
/// each), in thousandths. It says whether a glyph tone can be read anywhere across the band:
/// light glyphs are lost only where even the darkest part is light, dark ones only where even
/// the lightest part is dark. `None` when the band has no samples or one of them is not
/// opaque, which leaves unknown what shows there.
pub fn band_range(band: &[u32]) -> Option<(u16, u16)> {
    if band.is_empty() || band.iter().any(|color| *color >> 24 < 0xF0) {
        return None;
    }
    let thousandths = |color: &u32| {
        (relative_luminance(*color) * 1000.0)
            .round()
            .clamp(0.0, 1000.0) as u16
    };
    let darkest = band.iter().map(thousandths).min()?;
    let lightest = band.iter().map(thousandths).max()?;
    Some((darkest, lightest))
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

/// The band luminance (thousandths) over which black and white glyphs have the same contrast:
/// below it light glyphs stand out more, from it dark ones. It decides the first sample of a
/// window whose tone nothing has decided yet, which has no shown tone to keep.
pub const EQUAL_CONTRAST: u16 = 179;

/// The tone a window's caption glyphs are drawn in, given the tone the app asked for
/// (`reported`, from [`reported_tone`]; `None` when it expressed nothing), what the window
/// last read from the app's frame under the band (`sample`), whether the app draws its own
/// title bar (`app_drawn`, which is never filled over), and the tone shown now (`shown`;
/// `None` while neither a sample nor the app has decided one). `None` comes back only while
/// still nothing decides; the window then draws [`CaptionTone::default`].
///
/// A band the window fills takes the tone of the fill, since the glyphs stand on the window's
/// own color there. Otherwise the app's word is where the tone starts, as Android's status bar
/// icons follow the app's appearance: an average over a header with light and dark parts
/// picks the wrong side as often as the right one, and the app knows which side its bar is
/// meant to be. Its word gives way only where the frame leaves that tone unreadable all across
/// the band ([`readable_tone`]): a screen whose bar is light from end to end while the app
/// never asked for dark icons (a second screen of the task with another bar color, or a bar
/// the app colors itself where it used to set a status bar color) would otherwise show light
/// glyphs on a light bar. An app that expressed
/// nothing has its pixels read: below [`LIGHT_GLYPHS_BELOW`] the glyphs turn light, above
/// [`DARK_GLYPHS_ABOVE`] dark, and between them the shown tone stays. A window whose tone is
/// still undecided has none to keep: its first sample takes the side of [`EQUAL_CONTRAST`]
/// with more contrast, so a header in the middle (a green or a grey one) seen first does not
/// keep the dark glyphs a window starts with. A frame that could not be sampled keeps the
/// shown tone, or leaves it undecided.
pub fn shown_tone(
    reported: Option<CaptionTone>,
    sample: BandSample,
    app_drawn: bool,
    shown: Option<CaptionTone>,
) -> Option<CaptionTone> {
    if let Some(fill) = sample.fill.filter(|_| !app_drawn) {
        return Some(caption_tone(StatusBarAppearance::Unspecified, fill));
    }
    if let Some(reported) = reported {
        return Some(readable_tone(reported, sample, shown));
    }
    let Some(luminance) = sample.luminance else {
        return shown;
    };
    Some(match shown {
        _ if luminance < LIGHT_GLYPHS_BELOW => CaptionTone::LightGlyphs,
        _ if luminance > DARK_GLYPHS_ABOVE => CaptionTone::DarkGlyphs,
        Some(shown) => shown,
        None if luminance < EQUAL_CONTRAST => CaptionTone::LightGlyphs,
        None => CaptionTone::DarkGlyphs,
    })
}

/// Whether a window that reads its band from copies of some frames only (at most one every so
/// often, each read back later) still owes a reading of the frame it shows, once frames stop
/// coming: a copy waits to be read (`copy_waits`), or a frame came (`last_frame`) after the
/// last copy was made (`last_copy`). Without it the band of the last frame of a burst is never
/// read: a new screen that slides in and then stands still would keep the tone of the frames
/// in the middle of its transition.
pub fn band_reading_owed(
    copy_waits: bool,
    last_frame: Option<std::time::Instant>,
    last_copy: Option<std::time::Instant>,
) -> bool {
    copy_waits
        || match (last_frame, last_copy) {
            (Some(frame), Some(copy)) => frame > copy,
            (Some(_), None) => true,
            (None, _) => false,
        }
}

/// How many pixels a window samples across its band, and across the row under it.
pub const BAND_SAMPLES: u32 = 8;

/// Where a window reads the band of its app's frame ([`band_points`]): the columns, spread
/// evenly across the frame, and the two rows, the band's middle one and the one just under
/// the band, where the app's top begins. All in the frame's own pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandPoints {
    pub columns: [u32; BAND_SAMPLES as usize],
    pub inside: u32,
    pub below: u32,
}

/// Where a window reads the band `band` pixels high over a frame `region` pixels large, both
/// in the frame's own pixels. `None` when there is no band, or the frame is too small to hold
/// the band and the row under it.
pub fn band_points(region: (u32, u32), band: u32) -> Option<BandPoints> {
    if band == 0 || region.1 <= band + 2 || region.0 < BAND_SAMPLES {
        return None;
    }
    let mut columns = [0; BAND_SAMPLES as usize];
    for (column, x) in columns.iter_mut().enumerate() {
        let column = column as u64;
        *x = ((2 * column + 1) * u64::from(region.0) / (2 * u64::from(BAND_SAMPLES))) as u32;
    }
    Some(BandPoints {
        columns,
        inside: band / 2,
        below: band + 2,
    })
}

/// What a window reads under a band `band` pixels high from a frame of `width` by `height`
/// pixels given as RGBA rows, top row first ([`band_sample`] at [`band_points`]). The default
/// when the frame holds no band or fewer pixels than its size says.
pub fn frame_band_sample(rgba: &[u8], width: u32, height: u32, band: u32) -> BandSample {
    let Some(points) = band_points((width, height), band) else {
        return BandSample::default();
    };
    if rgba.len() < width as usize * height as usize * 4 {
        return BandSample::default();
    }
    let row = |y: u32| -> Vec<u32> {
        points
            .columns
            .iter()
            .map(|&x| {
                let at = (y as usize * width as usize + x as usize) * 4;
                // RGBA bytes, as 0xAARRGGBB.
                u32::from_be_bytes([rgba[at + 3], rgba[at], rgba[at + 1], rgba[at + 2]])
            })
            .collect()
    };
    band_sample(&row(points.inside), &row(points.below))
}

/// What a band sample says, in words for the log: its fill, its one color, its lightness and
/// how far apart its darkest and lightest parts are.
pub fn describe_band_sample(sample: BandSample) -> String {
    let fill = match sample.fill {
        Some(fill) => format!("filled with #{:06X}", fill & 0x00FF_FFFF),
        None => "no fill".to_string(),
    };
    let band = match sample.band {
        Some(color) => format!("one colour #{:06X}", color & 0x00FF_FFFF),
        None => "not one colour".to_string(),
    };
    let luminance = match sample.luminance {
        Some(luminance) => format!("mean luminance {luminance}"),
        None => "mean luminance unread".to_string(),
    };
    let range = match sample.range {
        Some((darkest, lightest)) => format!("darkest {darkest}, lightest {lightest}"),
        None => "not opaque all across".to_string(),
    };
    format!("{fill}, {band}, {luminance}, {range} (of 1000)")
}

/// Why [`shown_tone`] gave `tone` from `reported`, `sample` and `app_drawn`, in words for the
/// log: the rule that decided and the numbers it decided on.
pub fn tone_reason(
    reported: Option<CaptionTone>,
    sample: BandSample,
    app_drawn: bool,
    tone: Option<CaptionTone>,
) -> String {
    if let Some(fill) = sample.fill.filter(|_| !app_drawn) {
        return format!("the window fills the band with #{:06X}", fill & 0x00FF_FFFF);
    }
    let range =
        |(darkest, lightest): (u16, u16)| format!("darkest {darkest}, lightest {lightest} of 1000");
    match (reported, sample.range) {
        (Some(asked), None) => format!(
            "the app asked for {}, and the band was not read whole, so that stands",
            asked.describe()
        ),
        (Some(asked), Some(read)) if tone == Some(asked) => format!(
            "the app asked for {}, and they can be read on the band ({})",
            asked.describe(),
            range(read)
        ),
        (Some(asked), Some(read)) => format!(
            "the app asked for {}, but they cannot be read anywhere on the band ({})",
            asked.describe(),
            range(read)
        ),
        (None, _) => match sample.luminance {
            Some(luminance) => format!(
                "the app asked for nothing, and the band's mean luminance is {luminance} of 1000"
            ),
            None => "the app asked for nothing, and the band was not read".to_string(),
        },
    }
}

/// Light glyphs the app asked for are lost once even the darkest part of the band is lighter
/// than this (thousandths): white has a contrast under 2 to 1 everywhere on it. They come
/// back once a part of the band is darker than [`LIGHT_GLYPHS_BACK_BELOW`], where white
/// reaches 2.3 to 1; between the two the tone shown stays, so a band near the bound never
/// makes the glyphs flicker.
pub const LIGHT_GLYPHS_LOST_ABOVE: u16 = 500;
pub const LIGHT_GLYPHS_BACK_BELOW: u16 = 400;

/// Dark glyphs the app asked for are lost once even the lightest part of the band is darker
/// than this (thousandths): black has a contrast under 2 to 1 everywhere on it. They come
/// back once a part of the band is lighter than [`DARK_GLYPHS_BACK_ABOVE`], where black
/// reaches 2.6 to 1.
pub const DARK_GLYPHS_LOST_BELOW: u16 = 50;
pub const DARK_GLYPHS_BACK_ABOVE: u16 = 80;

/// The tone drawn for an app that asked for `reported`, over a band read as `sample`, while
/// `shown` shows: `reported`, unless it cannot be read anywhere across the band.
///
/// The app's word stands over any band with a part its tone can be read on, so a header with
/// a picture, a gradient or a mid color keeps what the app asked for. It gives way only when
/// every sample of the band is on the wrong side of the bounds above, which is a bar of the
/// glyphs' own lightness from end to end; the other tone then has a contrast of at least 8
/// to 1 there. A tone that differs from the reported one is one that gave way before (or the
/// app's word just changed), and it returns to the reported one once the band has a part
/// past the second bound. A band that could not be read whole leaves the app's word as it is.
pub fn readable_tone(
    reported: CaptionTone,
    sample: BandSample,
    shown: Option<CaptionTone>,
) -> CaptionTone {
    let Some((darkest, lightest)) = sample.range else {
        return reported;
    };
    let gave_way = shown.is_some_and(|shown| shown != reported);
    match reported {
        CaptionTone::LightGlyphs => {
            let lost = darkest > LIGHT_GLYPHS_LOST_ABOVE
                || (gave_way && darkest >= LIGHT_GLYPHS_BACK_BELOW);
            if lost {
                CaptionTone::DarkGlyphs
            } else {
                CaptionTone::LightGlyphs
            }
        }
        CaptionTone::DarkGlyphs => {
            let lost = lightest < DARK_GLYPHS_LOST_BELOW
                || (gave_way && lightest <= DARK_GLYPHS_BACK_ABOVE);
            if lost {
                CaptionTone::LightGlyphs
            } else {
                CaptionTone::DarkGlyphs
            }
        }
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

impl CaptionBand {
    /// The band a window that lets its OS place the parts over the band is told about:
    /// `height` units high, in a window `width` units wide, at `scale` pixels a unit. `items`
    /// are the left and right edges (units from the window's left) of what the OS keeps over
    /// the band. An item in the left half of the band counts toward `start_width`, how far in
    /// from the band's left end it reaches; one in the right half toward `end_width`, from the
    /// right end. `gap` is the space left after the outermost item, so an app's controls
    /// never touch it. A window that shows no band (a non-positive or non-finite size) gets an
    /// empty one; a bad `scale` counts as 1.
    pub fn from_items(
        width: f64,
        height: f64,
        items: &[(f64, f64)],
        scale: f64,
        gap: f64,
    ) -> CaptionBand {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        if !(width.is_finite() && width > 0.0 && height.is_finite() && height > 0.0) {
            return CaptionBand::default();
        }
        let middle = width / 2.0;
        let (mut start, mut end) = (0.0f64, 0.0f64);
        for &(left, right) in items {
            if !(left.is_finite() && right.is_finite()) || right <= left {
                continue;
            }
            if (left + right) / 2.0 < middle {
                start = start.max(right.max(0.0) + gap);
            } else {
                end = end.max((width - left).max(0.0) + gap);
            }
        }
        let pixels = |units: f64| (units.min(width) * scale).ceil() as u32;
        CaptionBand {
            height: (height * scale).round() as u32,
            start_width: pixels(start),
            end_width: pixels(end),
        }
    }
}

/// A rectangle in the window's client pixels: left, top, width, height.
pub type ClientRect = (i32, i32, u32, u32);

/// One button of the title bar the window draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptionKind {
    /// The back arrow in the icon's cell, while the task can go back.
    Back,
    /// The app's icon in its cell, which opens the window menu.
    SystemMenu,
    Minimize,
    /// Maximize, or Restore while the window is maximized or fills its monitor.
    Maximize,
    Close,
}

impl CaptionKind {
    /// Every kind, in the order they stand in the title bar.
    pub const ALL: [CaptionKind; 5] = [
        CaptionKind::Back,
        CaptionKind::SystemMenu,
        CaptionKind::Minimize,
        CaptionKind::Maximize,
        CaptionKind::Close,
    ];
}

/// The title bar's buttons as the window draws them now: their cells in client pixels and
/// what they say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptionCells {
    /// The icon's cell; `None` in a window too narrow for it.
    pub icon: Option<ClientRect>,
    /// Whether the icon's cell shows the back arrow rather than the app's icon.
    pub back: bool,
    /// The minimize, maximize and close buttons' cells; `None` before the window has laid
    /// its title bar out.
    pub buttons: Option<[ClientRect; 3]>,
    /// Whether the window is maximized or fills its monitor, when the middle button restores.
    pub maximized: bool,
    /// Whether the middle button does anything: a window the user's settings fix in size
    /// cannot be maximized, but one that fills its monitor can always leave it.
    pub can_maximize: bool,
    /// Whether the title bar is hidden: a window that fills its monitor shows it only once
    /// the pointer or the keyboard brings it out.
    pub offscreen: bool,
}

impl CaptionCells {
    /// The buttons of a title bar laid out as `layout`, its icon's cell showing the back arrow
    /// when `back`.
    pub fn of(
        layout: &CaptionLayout,
        back: bool,
        maximized: bool,
        can_maximize: bool,
        offscreen: bool,
    ) -> CaptionCells {
        CaptionCells {
            icon: layout.icon_cell(),
            back,
            buttons: Some(CaptionButton::ALL.map(|button| layout.button_rect(button))),
            maximized,
            can_maximize,
            offscreen,
        }
    }

    /// The buttons there are, left to right.
    pub fn kinds(&self) -> Vec<CaptionKind> {
        CaptionKind::ALL
            .into_iter()
            .filter(|kind| self.cell(*kind).is_some())
            .collect()
    }

    /// Where `kind` is drawn, in client pixels; `None` while the title bar has no such button.
    pub fn cell(&self, kind: CaptionKind) -> Option<ClientRect> {
        match kind {
            CaptionKind::Back => self.icon.filter(|_| self.back),
            CaptionKind::SystemMenu => self.icon.filter(|_| !self.back),
            CaptionKind::Minimize => self.buttons.map(|cells| cells[0]),
            CaptionKind::Maximize => self.buttons.map(|cells| cells[1]),
            CaptionKind::Close => self.buttons.map(|cells| cells[2]),
        }
    }

    /// Whether `kind` does anything when pressed now.
    pub fn enabled(&self, kind: CaptionKind) -> bool {
        kind != CaptionKind::Maximize || self.can_maximize
    }

    /// The button a client point (`x`, `y`) is on; none while the title bar is hidden.
    pub fn at(&self, x: f64, y: f64) -> Option<CaptionKind> {
        if self.offscreen {
            return None;
        }
        self.kinds()
            .into_iter()
            .find(|kind| self.cell(*kind).is_some_and(|cell| in_cell(cell, x, y)))
    }
}

/// Whether the client point (`x`, `y`) is in `cell`.
pub fn in_cell(cell: ClientRect, x: f64, y: f64) -> bool {
    let (left, top, width, height) = cell;
    x >= left as f64
        && x < left as f64 + width as f64
        && y >= top as f64
        && y < top as f64 + height as f64
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
        // Over the caption buttons the resize edge is one pixel deep, as at Windows' own
        // caption buttons. Deeper, a restored window (the only one with a top edge) answers
        // the top of the maximise button with a resize code, so the pointer reaches the button
        // only below it and Windows 11's Snap Layouts flyout, which opens from the hover on
        // the maximise button, hardly ever starts. A maximised window has no edge at all.
        let over_buttons = x >= self.buttons_left();
        let depth = if over_buttons {
            self.resize_border.min(1)
        } else {
            self.resize_border
        };
        if y < depth as i32 {
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
        // So the window is told the app expressed nothing, and reads its pixels.
        assert_eq!(reported_tone(StatusBarAppearance::Unspecified, 0), None);
        assert_eq!(
            reported_tone(StatusBarAppearance::Unspecified, 0x10FF_FFFF),
            None
        );
        assert_eq!(
            reported_tone(StatusBarAppearance::Unspecified, 0xFF1F_1F1F),
            Some(CaptionTone::LightGlyphs)
        );
    }

    /// A band of samples across the middle of the title bar, all `color`, with the row under
    /// it in `below`.
    fn sampled(color: u32, below: u32) -> BandSample {
        band_sample(&[color; 8], &[below; 8])
    }

    #[test]
    fn fr22_a_transparent_status_bar_over_a_light_header_gets_dark_glyphs() {
        // An app that draws edge to edge and expressed nothing: its status bar is transparent,
        // and its light header runs under the band.
        let reported = reported_tone(StatusBarAppearance::Unspecified, 0x0000_0000);
        assert_eq!(reported, None);
        for header in [0xFFFF_FFFF, 0xFFF8_F9FA, 0xFFE8_F0FE, 0xFFDA_E2FF] {
            let sample = sampled(header, header);
            assert_eq!(sample.fill, None, "the app draws the band");
            for shown in [
                None,
                Some(CaptionTone::DarkGlyphs),
                Some(CaptionTone::LightGlyphs),
            ] {
                assert_eq!(
                    shown_tone(reported, sample, false, shown),
                    Some(CaptionTone::DarkGlyphs),
                    "{header:08X} while {shown:?} show"
                );
            }
        }
    }

    #[test]
    fn fr22_a_transparent_status_bar_over_a_dark_header_gets_light_glyphs() {
        // The same app in a dark theme: nothing reported, so the pixels decide.
        let reported = reported_tone(StatusBarAppearance::Unspecified, 0x0000_0000);
        assert_eq!(reported, None);
        for header in [
            0xFF00_0000,
            0xFF1F_1F1F,
            0xFF14_1218,
            0xFF20_2124,
            0xFF67_50A4,
        ] {
            let sample = sampled(header, header);
            for shown in [
                None,
                Some(CaptionTone::DarkGlyphs),
                Some(CaptionTone::LightGlyphs),
            ] {
                assert_eq!(
                    shown_tone(reported, sample, false, shown),
                    Some(CaptionTone::LightGlyphs),
                    "{header:08X} while {shown:?} show"
                );
            }
        }
    }

    #[test]
    fn fr22_a_dark_video_under_a_transparent_status_bar_gets_light_glyphs() {
        // A full-bleed player (YouTube's Shorts) in the light theme: while the player shows,
        // the app asks for light status bar icons (a dark bar), or says nothing, and its dark
        // video runs under the band, with a grey overlay here and there.
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
            reported_tone(StatusBarAppearance::Dark, 0),
            reported_tone(StatusBarAppearance::Unspecified, 0),
        ] {
            for shown in [
                None,
                Some(CaptionTone::DarkGlyphs),
                Some(CaptionTone::LightGlyphs),
            ] {
                assert_eq!(
                    shown_tone(reported, sample, false, shown),
                    Some(CaptionTone::LightGlyphs)
                );
            }
        }
    }

    /// The ways a band the window does not fill can look while either tone can be read
    /// somewhere on it: half and half (a header with a light and a dark part, which an average
    /// gets wrong), mostly light with a dark part, mostly dark with a light part, a mid grey,
    /// not read yet.
    fn unfilled_bands() -> Vec<BandSample> {
        let half = [[0xFFFF_FFFF; 4], [0xFF10_1010; 4]].concat();
        let mostly_light = [&[0xFFFF_FFFF; 7][..], &[0xFF30_3030; 1][..]].concat();
        let mostly_dark = [&[0xFF10_1010; 7][..], &[0xFFF0_F0F0; 1][..]].concat();
        vec![
            band_sample(&half, &half),
            band_sample(&mostly_light, &mostly_light),
            band_sample(&mostly_dark, &mostly_dark),
            sampled(0xFF80_8080, 0xFF80_8080),
            BandSample::default(),
        ]
    }

    #[test]
    fn fr22_light_glyphs_an_app_asked_for_turn_dark_over_a_bar_light_from_end_to_end() {
        // A settings screen opened over a player's main screen, in the same task: its bar is a
        // light grey from end to end, and the app never asks for dark status bar icons (the
        // flag stays clear, which a phone reads as a dark bar). White glyphs cannot be read
        // anywhere on it, so the glyphs turn dark whatever showed before.
        let says_dark = reported_tone(StatusBarAppearance::Dark, 0);
        assert_eq!(says_dark, Some(CaptionTone::LightGlyphs));
        for bar in [0xFFE0_E0E0, 0xFFFF_FFFF, 0xFFF5_F5F5, 0xFFFF_EB3B] {
            let sample = sampled(bar, bar);
            assert_eq!(sample.fill, None, "the app draws the band");
            for app_drawn in [false, true] {
                for shown in [
                    None,
                    Some(CaptionTone::DarkGlyphs),
                    Some(CaptionTone::LightGlyphs),
                ] {
                    assert_eq!(
                        shown_tone(says_dark, sample, app_drawn, shown),
                        Some(CaptionTone::DarkGlyphs),
                        "{bar:08X} while {shown:?} show"
                    );
                }
            }
        }
        // A status bar color the app set is where the tone starts as well: the frame shows
        // the bar the app really drew, light here, where Android no longer draws that color.
        let says_by_color = reported_tone(StatusBarAppearance::Unspecified, 0xFF20_2124);
        assert_eq!(says_by_color, Some(CaptionTone::LightGlyphs));
        assert_eq!(
            shown_tone(
                says_by_color,
                sampled(0xFFE0_E0E0, 0xFFE0_E0E0),
                false,
                Some(CaptionTone::LightGlyphs)
            ),
            Some(CaptionTone::DarkGlyphs)
        );
    }

    #[test]
    fn fr22_dark_glyphs_an_app_asked_for_turn_light_over_a_bar_dark_from_end_to_end() {
        // The other way: an app that keeps its light status bar flag over a screen whose bar
        // is nearly black from end to end.
        let says_light = reported_tone(StatusBarAppearance::Light, 0);
        assert_eq!(says_light, Some(CaptionTone::DarkGlyphs));
        for bar in [0xFF00_0000, 0xFF12_1212, 0xFF20_2124, 0xFF1F_1F1F] {
            let sample = sampled(bar, bar);
            for shown in [
                None,
                Some(CaptionTone::DarkGlyphs),
                Some(CaptionTone::LightGlyphs),
            ] {
                assert_eq!(
                    shown_tone(says_light, sample, false, shown),
                    Some(CaptionTone::LightGlyphs),
                    "{bar:08X} while {shown:?} show"
                );
            }
        }
    }

    #[test]
    fn fr22_a_tone_that_gave_way_returns_with_the_band_and_does_not_flicker() {
        let says_dark = reported_tone(StatusBarAppearance::Dark, 0);
        let bar = |luminance: u16| BandSample {
            fill: None,
            luminance: Some(luminance),
            range: Some((luminance, luminance)),
            band: None,
        };
        // The app's light glyphs stand until the whole bar is past the first bound.
        let mut shown = None;
        for luminance in [100, 300, 450, 500] {
            shown = shown_tone(says_dark, bar(luminance), false, shown);
            assert_eq!(shown, Some(CaptionTone::LightGlyphs), "at {luminance}");
        }
        shown = shown_tone(says_dark, bar(745), false, shown);
        assert_eq!(shown, Some(CaptionTone::DarkGlyphs), "the light grey bar");
        // A bar that wavers between the bounds keeps the dark glyphs it has.
        for luminance in [520, 480, 400, 499, 600] {
            shown = shown_tone(says_dark, bar(luminance), false, shown);
            assert_eq!(shown, Some(CaptionTone::DarkGlyphs), "at {luminance}");
        }
        // Back on the first screen, whose bar is dark: the app's light glyphs return.
        shown = shown_tone(says_dark, bar(30), false, shown);
        assert_eq!(shown, Some(CaptionTone::LightGlyphs));
        // One dark part is enough for them to return: the app's word can be read there.
        let with_dark_part = BandSample {
            fill: None,
            luminance: Some(700),
            range: Some((60, 900)),
            band: None,
        };
        assert_eq!(
            shown_tone(
                says_dark,
                with_dark_part,
                false,
                Some(CaptionTone::DarkGlyphs)
            ),
            Some(CaptionTone::LightGlyphs)
        );
        // And the same for dark glyphs the app asked for.
        let says_light = reported_tone(StatusBarAppearance::Light, 0);
        let mut shown = None;
        for luminance in [900, 200, 60, 50] {
            shown = shown_tone(says_light, bar(luminance), false, shown);
            assert_eq!(shown, Some(CaptionTone::DarkGlyphs), "at {luminance}");
        }
        shown = shown_tone(says_light, bar(6), false, shown);
        assert_eq!(shown, Some(CaptionTone::LightGlyphs));
        for luminance in [40, 70, 80, 55] {
            shown = shown_tone(says_light, bar(luminance), false, shown);
            assert_eq!(shown, Some(CaptionTone::LightGlyphs), "at {luminance}");
        }
        shown = shown_tone(says_light, bar(81), false, shown);
        assert_eq!(shown, Some(CaptionTone::DarkGlyphs));
        // The range is of every sample, and only of a band that is opaque all across.
        assert_eq!(band_range(&[0xFFFF_FFFF; 8]), Some((1000, 1000)));
        let half = [[0xFFFF_FFFF; 4], [0xFF00_0000; 4]].concat();
        assert_eq!(band_range(&half), Some((0, 1000)));
        let one_empty = [&[0xFFFF_FFFF; 7][..], &[0x0000_0000; 1][..]].concat();
        assert_eq!(band_range(&one_empty), None);
        assert_eq!(band_range(&[]), None);
        let grey = sampled(0xFFE0_E0E0, 0xFFE0_E0E0);
        assert!(
            grey.range.is_some_and(
                |(darkest, lightest)| darkest == lightest && darkest > LIGHT_GLYPHS_LOST_ABOVE
            ),
            "{grey:?}"
        );
    }

    #[test]
    fn fr22_the_band_of_the_last_frame_of_a_burst_is_read_too() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let at = |millis: u64| Some(start + Duration::from_millis(millis));
        // No frame yet: nothing to read.
        assert!(!band_reading_owed(false, None, None));
        // A frame that was copied and read: nothing owed, however long nothing comes.
        assert!(!band_reading_owed(false, at(0), at(1)));
        // Its copy still waits to be read.
        assert!(band_reading_owed(true, at(0), at(1)));
        // A screen slides in: the frame at 150 ms came too soon after the copy at 1 ms to be
        // copied, and it is the last one. Its band is still owed.
        assert!(band_reading_owed(false, at(150), at(1)));
        // A frame came before any copy could be made.
        assert!(band_reading_owed(false, at(0), None));
        // Copied after the last frame, and read: settled.
        assert!(!band_reading_owed(false, at(150), at(400)));
    }

    /// A frame of `width` by `height` RGBA pixels, top row first: `top` over its first `bar`
    /// rows and `rest` under them.
    fn still_frame(width: u32, height: u32, bar: u32, top: [u8; 4], rest: [u8; 4]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let color = if y < bar { top } else { rest };
            for _ in 0..width {
                rgba.extend_from_slice(&color);
            }
        }
        rgba
    }

    /// The most opaque pixel of `rect` in a title bar image, laid over a band of the grey
    /// `band`: the color its red channel shows there, and its opacity.
    fn strongest_over(
        image: &[u8],
        layout: &CaptionLayout,
        (left, top, width, height): (i32, i32, u32, u32),
        band: u8,
    ) -> (u8, u8) {
        let mut strongest = [0u8; 4];
        for y in top.max(0) as u32..(top.max(0) as u32 + height).min(layout.height) {
            for x in left.max(0) as u32..(left.max(0) as u32 + width).min(layout.width) {
                let pixel = bar_pixel(image, layout, x, y);
                if pixel[3] > strongest[3] {
                    strongest = pixel;
                }
            }
        }
        // Premultiplied: the pixel's own color, then the band through what it leaves.
        let shown =
            u32::from(strongest[2]) + u32::from(band) * (255 - u32::from(strongest[3])) / 255;
        (shown.min(255) as u8, strongest[3])
    }

    #[test]
    fn fr22_a_still_window_whose_only_frame_has_a_light_band_ends_with_dark_glyphs() {
        // MX Player's settings screen: its bar is a light grey (#E0E0E0) from the window's top
        // to below the band, from end to end, and the app never asks for dark status bar
        // icons, which is read as a dark bar. The window is still: this one frame is all the
        // presenter ever reads.
        let says_dark = reported_tone(StatusBarAppearance::Dark, 0);
        assert_eq!(says_dark, Some(CaptionTone::LightGlyphs));
        const GREY: u8 = 0xE0;
        for dpi in [96, 144, 192] {
            let layout = CaptionLayout::new(scaled(800, dpi), scaled(31, dpi), dpi, 0);
            // The frame in the task's pixels, here the window's: the bar reaches past the band.
            let (width, height) = (layout.width, layout.height * 6);
            let rgba = still_frame(
                width,
                height,
                layout.height * 2,
                [GREY, GREY, GREY, 0xFF],
                [0xFF, 0xFF, 0xFF, 0xFF],
            );
            let sample = frame_band_sample(&rgba, width, height, layout.height);
            assert_eq!(sample.fill, None, "the app draws the band: {sample:?}");
            assert!(
                sample
                    .range
                    .is_some_and(|(darkest, _)| darkest > LIGHT_GLYPHS_LOST_ABOVE),
                "{}",
                describe_band_sample(sample)
            );
            // The app's word comes first and nothing has been read yet: light glyphs.
            let mut shown = shown_tone(says_dark, BandSample::default(), false, None);
            assert_eq!(shown, Some(CaptionTone::LightGlyphs));
            // The one sample turns them dark, and reading the same frame again keeps them so.
            for _ in 0..3 {
                shown = shown_tone(says_dark, sample, false, shown);
                assert_eq!(
                    shown,
                    Some(CaptionTone::DarkGlyphs),
                    "at {dpi}: {}",
                    tone_reason(says_dark, sample, false, shown)
                );
            }
            assert!(
                tone_reason(says_dark, sample, false, shown)
                    .contains("cannot be read anywhere on the band"),
                "{}",
                tone_reason(says_dark, sample, false, shown)
            );

            // Every glyph is drawn in that tone: the name, the back arrow and the buttons.
            let palette = CaptionPalette::of(shown.unwrap_or_default());
            let (_, _, title_width, title_height) = layout.title_rect().unwrap();
            let title = vec![255u8; (title_width * title_height) as usize];
            let content = TitleBarContent {
                fill: sample.fill,
                title: Some(&title),
                back: Some(BackButton::default()),
                ..TitleBarContent::default()
            };
            for active in [true, false] {
                let state = CaptionState {
                    active,
                    ..CaptionState::default()
                };
                let image = paint_title_bar(&layout, dpi, &palette, &state, &content);
                let mut parts = vec![
                    ("name", layout.title_rect().unwrap()),
                    ("back arrow", layout.icon_cell().unwrap()),
                ];
                for button in CaptionButton::ALL {
                    parts.push(("button", layout.button_rect(button)));
                }
                for (part, rect) in parts {
                    let (shown_as, opacity) = strongest_over(&image, &layout, rect, GREY);
                    assert!(opacity > 0, "at {dpi}: the {part} is drawn");
                    assert!(
                        u32::from(shown_as) + 0x40 <= u32::from(GREY),
                        "at {dpi}, active {active}: the {part} shows #{shown_as:02X} on \
                         #{GREY:02X}"
                    );
                }
            }
        }
    }

    #[test]
    fn fr22_the_band_is_read_across_the_frame_in_its_middle_row_and_just_under_it() {
        // The window of R51's screen: 1439 task pixels wide, a band of 58.
        let points = band_points((1439, 1166), 58).unwrap();
        assert_eq!((points.inside, points.below), (29, 60));
        assert_eq!(points.columns[0], 89);
        assert_eq!(points.columns[7], 1349);
        assert!(points.columns.windows(2).all(|pair| pair[0] < pair[1]));
        // No band, or a frame too small for it and the row under it, reads nothing.
        assert_eq!(band_points((1439, 1166), 0), None);
        assert_eq!(band_points((1439, 60), 58), None);
        assert_eq!(band_points((4, 1166), 58), None);
        assert_eq!(
            frame_band_sample(&[], 1439, 1166, 58),
            BandSample::default()
        );
        // A band of two halves reads both.
        let mut rgba = still_frame(80, 40, 40, [0xFF, 0xFF, 0xFF, 0xFF], [0; 4]);
        for y in 0..40usize {
            for x in 0..40usize {
                rgba[(y * 80 + x) * 4..(y * 80 + x) * 4 + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let sample = frame_band_sample(&rgba, 80, 40, 20);
        assert_eq!(sample.range, Some((0, 1000)));
        assert_eq!(sample.band, None);
    }

    #[test]
    fn fr22_the_log_says_which_rule_decided_the_glyphs_tone() {
        let grey = sampled(0xFFE0_E0E0, 0xFFE0_E0E0);
        let says_dark = Some(CaptionTone::LightGlyphs);
        let reason = tone_reason(says_dark, grey, false, Some(CaptionTone::DarkGlyphs));
        assert!(reason.contains("asked for light glyphs"), "{reason}");
        assert!(reason.contains("cannot be read anywhere"), "{reason}");
        assert!(reason.contains("darkest 745, lightest 745"), "{reason}");
        let unread = tone_reason(says_dark, BandSample::default(), false, says_dark);
        assert!(unread.contains("not read whole"), "{unread}");
        let readable = tone_reason(
            says_dark,
            sampled(0xFF20_2124, 0xFF20_2124),
            false,
            says_dark,
        );
        assert!(readable.contains("can be read on the band"), "{readable}");
        let filled = sampled(0x0000_0000, 0xFF33_6699);
        assert!(
            tone_reason(says_dark, filled, false, Some(CaptionTone::LightGlyphs))
                .contains("fills the band with #336699")
        );
        let nothing = tone_reason(None, grey, false, Some(CaptionTone::DarkGlyphs));
        assert!(nothing.contains("asked for nothing"), "{nothing}");
        assert_eq!(CaptionTone::DarkGlyphs.describe(), "dark glyphs");
        assert!(describe_band_sample(grey).contains("one colour #E0E0E0"));
    }

    #[test]
    fn fr22_an_explicit_light_status_bar_gives_dark_glyphs_over_a_band_with_dark_parts() {
        let says_light = reported_tone(StatusBarAppearance::Light, 0);
        assert_eq!(says_light, Some(CaptionTone::DarkGlyphs));
        for app_drawn in [false, true] {
            for band in unfilled_bands() {
                for shown in [
                    None,
                    Some(CaptionTone::DarkGlyphs),
                    Some(CaptionTone::LightGlyphs),
                ] {
                    assert_eq!(
                        shown_tone(says_light, band, app_drawn, shown),
                        Some(CaptionTone::DarkGlyphs),
                        "{band:?} while {shown:?} show"
                    );
                }
            }
        }
    }

    #[test]
    fn fr22_an_explicit_dark_status_bar_gives_light_glyphs_over_a_band_with_light_parts() {
        // The case of a dark header with light parts (a picture, a light tab): the app asked
        // for light icons, and the average of its pixels must not turn the glyphs black
        // while a part of the band is dark enough to read them on.
        let says_dark = reported_tone(StatusBarAppearance::Dark, 0);
        assert_eq!(says_dark, Some(CaptionTone::LightGlyphs));
        for app_drawn in [false, true] {
            for band in unfilled_bands() {
                for shown in [
                    None,
                    Some(CaptionTone::DarkGlyphs),
                    Some(CaptionTone::LightGlyphs),
                ] {
                    assert_eq!(
                        shown_tone(says_dark, band, app_drawn, shown),
                        Some(CaptionTone::LightGlyphs),
                        "{band:?} while {shown:?} show"
                    );
                }
            }
        }
        // An opaque status bar color the app set is its word as well.
        let says_by_color = reported_tone(StatusBarAppearance::Unspecified, 0xFF20_2124);
        let mostly_light = [&[0xFFFF_FFFF; 7][..], &[0xFF30_3030; 1][..]].concat();
        assert_eq!(
            shown_tone(
                says_by_color,
                band_sample(&mostly_light, &mostly_light),
                false,
                Some(CaptionTone::DarkGlyphs)
            ),
            Some(CaptionTone::LightGlyphs)
        );
    }

    #[test]
    fn fr22_a_change_of_appearance_changes_the_tone_at_once() {
        // A tab switch: the app's appearance goes from light to dark and back, and the glyphs
        // follow on the next settle, over the same pixels, with no hysteresis in the way.
        let band = sampled(0xFF80_8080, 0xFF80_8080);
        let mut shown = Some(CaptionTone::DarkGlyphs);
        for (appearance, expected) in [
            (StatusBarAppearance::Light, CaptionTone::DarkGlyphs),
            (StatusBarAppearance::Dark, CaptionTone::LightGlyphs),
            (StatusBarAppearance::Light, CaptionTone::DarkGlyphs),
        ] {
            shown = shown_tone(reported_tone(appearance, 0), band, false, shown);
            assert_eq!(shown, Some(expected), "{appearance:?}");
        }
    }

    #[test]
    fn fr22_an_app_that_expressed_nothing_falls_back_to_its_pixels() {
        let light_header = sampled(0xFFFF_FFFF, 0xFFFF_FFFF);
        let dark_header = sampled(0xFF12_1212, 0xFF12_1212);
        for shown in [
            None,
            Some(CaptionTone::DarkGlyphs),
            Some(CaptionTone::LightGlyphs),
        ] {
            assert_eq!(
                shown_tone(None, light_header, false, shown),
                Some(CaptionTone::DarkGlyphs)
            );
            assert_eq!(
                shown_tone(None, dark_header, false, shown),
                Some(CaptionTone::LightGlyphs)
            );
            // So do the glyphs over an app's own title bar, whose content fills the band.
            assert_eq!(
                shown_tone(None, light_header, true, shown),
                Some(CaptionTone::DarkGlyphs)
            );
            // A frame that could not be sampled keeps the tone shown: an empty band, or none
            // read yet.
            let empty = sampled(0x0000_0000, 0x0000_0000);
            assert_eq!(empty, BandSample::default());
            assert_eq!(shown_tone(None, empty, false, shown), shown);
        }
    }

    #[test]
    fn fr22_a_band_the_window_fills_takes_the_tone_of_the_fill() {
        // A band left empty above a toolbar is painted in the toolbar's color, so the glyphs
        // stand on that color whatever the app said.
        let filled = sampled(0x0000_0000, 0xFFFF_FFFF);
        assert_eq!(filled.fill, Some(0xFFFF_FFFF));
        for reported in [
            None,
            reported_tone(StatusBarAppearance::Dark, 0),
            reported_tone(StatusBarAppearance::Light, 0),
        ] {
            for shown in [None, Some(CaptionTone::LightGlyphs)] {
                assert_eq!(
                    shown_tone(reported, filled, false, shown),
                    Some(CaptionTone::DarkGlyphs)
                );
            }
        }
        // An app that draws its own title bar is never filled over: its word stands.
        assert_eq!(
            shown_tone(
                reported_tone(StatusBarAppearance::Dark, 0),
                filled,
                true,
                Some(CaptionTone::DarkGlyphs)
            ),
            Some(CaptionTone::LightGlyphs)
        );
    }

    /// The tone shown after a sample of `luminance` by an app that expressed nothing, while
    /// `shown` shows.
    fn tone_at(luminance: u16, shown: Option<CaptionTone>) -> Option<CaptionTone> {
        let sample = BandSample {
            fill: None,
            luminance: Some(luminance),
            range: Some((luminance, luminance)),
            band: None,
        };
        shown_tone(None, sample, false, shown)
    }

    #[test]
    fn fr22_a_mid_luminance_band_seen_first_takes_the_tone_with_more_contrast() {
        // A green or grey header from the first frame: no sample has decided the tone yet, so
        // the first one takes the side of the equal-contrast point that stands out more,
        // rather than keeping the dark glyphs a window starts with.
        for luminance in [130, 150, 170, 178] {
            assert_eq!(
                tone_at(luminance, None),
                Some(CaptionTone::LightGlyphs),
                "at {luminance}"
            );
        }
        for luminance in [179, 200, 220, 240] {
            assert_eq!(
                tone_at(luminance, None),
                Some(CaptionTone::DarkGlyphs),
                "at {luminance}"
            );
        }
        // A Material green 800 header (luminance about 155) is such a band: light glyphs, from
        // the first frame.
        let green = sampled(0xFF2E_7D32, 0xFF2E_7D32);
        let luminance = green.luminance.unwrap();
        assert!(
            (LIGHT_GLYPHS_BELOW..EQUAL_CONTRAST).contains(&luminance),
            "{luminance}"
        );
        assert_eq!(
            shown_tone(None, green, false, None),
            Some(CaptionTone::LightGlyphs)
        );
        // A frame that could not be sampled leaves the tone undecided, for the next one.
        assert_eq!(shown_tone(None, BandSample::default(), false, None), None);
    }

    #[test]
    fn fr22_once_decided_a_mid_luminance_band_keeps_the_shown_tone() {
        // The first sample decides light glyphs; the header then wavers on the dark glyphs'
        // side of the crossover, and the light glyphs stay until it passes the upper bound.
        let mut shown = tone_at(160, None);
        assert_eq!(shown, Some(CaptionTone::LightGlyphs));
        for luminance in [200, 230, 179, 240] {
            shown = tone_at(luminance, shown);
            assert_eq!(shown, Some(CaptionTone::LightGlyphs), "at {luminance}");
        }
        // And the other way: dark glyphs decided first stay over a band below the crossover.
        let mut shown = tone_at(210, None);
        assert_eq!(shown, Some(CaptionTone::DarkGlyphs));
        for luminance in [170, 140, 130] {
            shown = tone_at(luminance, shown);
            assert_eq!(shown, Some(CaptionTone::DarkGlyphs), "at {luminance}");
        }
        // A tone the app reported decides too: the band in the middle keeps it afterwards.
        let middle = BandSample {
            fill: None,
            luminance: Some(150),
            range: Some((150, 150)),
            band: None,
        };
        let shown = shown_tone(Some(CaptionTone::DarkGlyphs), middle, false, None);
        assert_eq!(shown, Some(CaptionTone::DarkGlyphs));
        assert_eq!(tone_at(150, shown), Some(CaptionTone::DarkGlyphs));
    }

    #[test]
    fn fr22_a_band_near_the_middle_does_not_make_the_glyphs_flicker() {
        // A header whose lightness wavers around the middle (an animation, a scrolled
        // picture) keeps the tone it has, whichever it is.
        let mut shown = Some(CaptionTone::DarkGlyphs);
        for luminance in [200, 150, 230, 140, 179] {
            shown = tone_at(luminance, shown);
            assert_eq!(shown, Some(CaptionTone::DarkGlyphs), "at {luminance}");
        }
        shown = tone_at(100, shown);
        assert_eq!(shown, Some(CaptionTone::LightGlyphs));
        for luminance in [150, 230, 140, 179, 240] {
            shown = tone_at(luminance, shown);
            assert_eq!(shown, Some(CaptionTone::LightGlyphs), "at {luminance}");
        }
        assert_eq!(tone_at(241, shown), Some(CaptionTone::DarkGlyphs));
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
        assert_eq!(layout.hit(790, 0), CaptionHit::TopRight);
        assert_eq!(layout.hit(770, 0), CaptionHit::Top);
        assert_eq!(
            layout.hit(700, 0),
            CaptionHit::Top,
            "the edge itself, above the buttons"
        );
    }

    #[test]
    fn fr22_the_restored_windows_maximize_button_is_hit_from_its_second_row_down() {
        // A restored window's top edge, 8 pixels deep, took the top quarter of the buttons,
        // and the Snap Layouts flyout opens from a hover on the maximise button.
        let layout = at_100();
        for y in 1..31 {
            assert_eq!(
                layout.hit(720, y),
                CaptionHit::Button(CaptionButton::Maximize),
                "row {y}"
            );
        }
        assert_eq!(layout.hit(720, 0), CaptionHit::Top);
        // Beside the buttons the edge keeps its full depth.
        assert_eq!(layout.hit(400, 7), CaptionHit::Top);
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
    fn fr22_a_window_below_the_title_bar_fills_its_band_with_the_colour_the_app_painted_there() {
        let light = Some(CaptionTone::LightGlyphs);
        // The framework painted the app's status bar color across the band: a deep purple
        // bar over a white screen. The band takes that color, opaque, and light glyphs.
        let purple = band_sample(&[0xFF67_50A4; 8], &[0xFFFF_FFFF; 8]);
        assert_eq!(purple.band, Some(0xFF67_50A4));
        let below = below_title_bar_sample(purple, None, None);
        assert_eq!(below.fill, Some(0xFF67_50A4));
        assert_eq!(
            shown_tone(None, below, false, None),
            Some(CaptionTone::LightGlyphs)
        );
        // A light window background under a transparent status bar: the band is that
        // background, with dark glyphs, even though the app asked for light ones.
        let grey = band_sample(&[0xFFE0_E0E0; 8], &[0xFF21_2121; 8]);
        let below = below_title_bar_sample(grey, light, Some(0xFF67_50A4));
        assert_eq!(below.fill, Some(0xFFE0_E0E0));
        assert_eq!(
            shown_tone(light, below, false, light),
            Some(CaptionTone::DarkGlyphs)
        );
        // A dark theme follows: the band of a dark app is its own dark color.
        let dark = band_sample(&[0xFF12_1212; 8], &[0xFF12_1212; 8]);
        let below = below_title_bar_sample(dark, None, Some(0xFFE0_E0E0));
        assert_eq!(below.fill, Some(0xFF12_1212));
        assert_eq!(
            shown_tone(None, below, false, Some(CaptionTone::DarkGlyphs)),
            Some(CaptionTone::LightGlyphs)
        );
        // An old theme paints no bar background: the band is empty or black over a toolbar of
        // one color, and takes the toolbar's color, as under the unified title bar.
        for empty in [0x0000_0000, 0xFF00_0000] {
            let old = band_sample(&[empty; 8], &[0xFF3F_51B5; 8]);
            assert_eq!(
                below_title_bar_sample(old, None, None).fill,
                Some(0xFF3F_51B5)
            );
        }
        // The band is always opaque: a nearly opaque sample is made whole.
        assert_eq!(band_color(&[0xF8AB_CDEF; 4]), Some(0xFFAB_CDEF));
        // A window under the unified title bar is never filled this way.
        assert_eq!(purple.fill, None);
    }

    #[test]
    fn fr22_a_band_of_more_than_one_colour_keeps_its_fill_or_takes_the_surface_colour() {
        // A screen sliding in: half the band is the old bar and half the new one.
        let mixed = band_sample(
            &[
                0xFF67_50A4,
                0xFF67_50A4,
                0xFF67_50A4,
                0xFF67_50A4,
                0xFFE0_E0E0,
                0xFFE0_E0E0,
                0xFFE0_E0E0,
                0xFFE0_E0E0,
            ],
            &[0xFFFF_FFFF; 8],
        );
        assert_eq!(mixed.band, None);
        // The fill shown stays, so the band does not flicker through the transition.
        assert_eq!(
            below_title_bar_sample(mixed, None, Some(0xFF67_50A4)).fill,
            Some(0xFF67_50A4)
        );
        // With nothing filled yet the band takes a surface color: the dark one for an app
        // that asked for light glyphs, the light one for one that asked for dark glyphs.
        assert_eq!(
            below_title_bar_sample(mixed, Some(CaptionTone::LightGlyphs), None).fill,
            Some(DARK_SURFACE)
        );
        assert_eq!(
            below_title_bar_sample(mixed, Some(CaptionTone::DarkGlyphs), None).fill,
            Some(LIGHT_SURFACE)
        );
        // An app that said nothing gets the surface its band is nearer to.
        let dark_picture = band_sample(
            &[0xFF10_1010, 0xFF30_2020, 0xFF20_3040, 0xFF05_0505],
            &[0xFF10_1010, 0xFF80_8080, 0xFF10_1010, 0xFF10_1010],
        );
        assert_eq!(dark_picture.band, None);
        assert_eq!(
            below_title_bar_sample(dark_picture, None, None).fill,
            Some(DARK_SURFACE)
        );
        let light_picture = band_sample(
            &[0xFFF0_F0F0, 0xFFD0_E0F0, 0xFFFF_FFFF, 0xFFC0_C0C0],
            &[0xFFF0_F0F0, 0xFF80_8080, 0xFFF0_F0F0, 0xFFF0_F0F0],
        );
        assert_eq!(
            below_title_bar_sample(light_picture, None, None).fill,
            Some(LIGHT_SURFACE)
        );
        // The surface's tone is the fill's, as for any filled band.
        assert_eq!(
            shown_tone(
                None,
                below_title_bar_sample(dark_picture, None, None),
                false,
                None
            ),
            Some(CaptionTone::LightGlyphs)
        );
        // Before the first frame nothing is known: the band is left as it is, and a fill
        // shown already stays.
        let unread = BandSample::default();
        assert_eq!(below_title_bar_sample(unread, None, None).fill, None);
        assert_eq!(
            below_title_bar_sample(unread, None, Some(0xFF12_1212)).fill,
            Some(0xFF12_1212)
        );
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

    const GAP: f64 = 8.0;

    #[test]
    fn fr22_band_reaches_past_items_on_each_side() {
        // Three buttons from 7 to 61 units, in a 600 unit window at 2x.
        let band = CaptionBand::from_items(
            600.0,
            28.0,
            &[(7.0, 21.0), (27.0, 41.0), (47.0, 61.0)],
            2.0,
            GAP,
        );
        assert_eq!(band.height, 56);
        assert_eq!(band.start_width, ((61.0 + GAP) * 2.0) as u32);
        assert_eq!(band.end_width, 0);
        // An item added after the buttons widens that end only.
        let buttons = [(7.0, 21.0), (27.0, 41.0), (47.0, 61.0), (68.0, 96.0)];
        let with = CaptionBand::from_items(600.0, 28.0, &buttons, 1.0, GAP);
        assert_eq!(with.start_width, (96.0 + GAP) as u32);
        // Items in the right half count from the right end.
        let right =
            CaptionBand::from_items(600.0, 28.0, &[(539.0, 553.0), (579.0, 593.0)], 1.0, GAP);
        assert_eq!(right.start_width, 0);
        assert_eq!(right.end_width, (600.0 - 539.0 + GAP) as u32);
    }

    #[test]
    fn fr22_hidden_band_is_empty() {
        let items = [(7.0, 21.0)];
        assert_eq!(
            CaptionBand::from_items(600.0, 0.0, &items, 2.0, GAP),
            CaptionBand::default()
        );
        assert_eq!(
            CaptionBand::from_items(600.0, f64::NAN, &items, 2.0, GAP),
            CaptionBand::default()
        );
        assert_eq!(
            CaptionBand::from_items(0.0, 28.0, &items, 2.0, GAP),
            CaptionBand::default()
        );
        // A bad scale counts as 1x rather than as no band.
        assert_eq!(
            CaptionBand::from_items(600.0, 28.0, &[], 0.0, GAP).height,
            28
        );
        // An item that is empty or not a number is skipped.
        let skipped =
            CaptionBand::from_items(600.0, 28.0, &[(10.0, 10.0), (f64::NAN, 5.0)], 1.0, GAP);
        assert_eq!((skipped.start_width, skipped.end_width), (0, 0));
    }

    fn buttons_of(layout: &CaptionLayout, back: bool, offscreen: bool) -> CaptionCells {
        CaptionCells::of(layout, back, false, true, offscreen)
    }

    #[test]
    fn fr22_a_point_in_the_icons_cell_hits_the_back_button() {
        let cell = (4, 0, 40, 32);
        assert!(in_cell(cell, 4.0, 0.0));
        assert!(in_cell(cell, 43.5, 31.5));
        assert!(!in_cell(cell, 44.0, 10.0));
        assert!(!in_cell(cell, 3.9, 10.0));
        assert!(!in_cell(cell, 10.0, 32.0));
    }

    #[test]
    fn fr22_every_caption_button_is_found_at_its_cell_and_a_hidden_bar_has_none() {
        let layout = at_100();
        let cells = buttons_of(&layout, false, false);
        use CaptionKind::*;
        assert_eq!(cells.kinds(), vec![SystemMenu, Minimize, Maximize, Close]);
        for (kind, button) in [
            (CaptionKind::Minimize, CaptionButton::Minimize),
            (CaptionKind::Maximize, CaptionButton::Maximize),
            (CaptionKind::Close, CaptionButton::Close),
        ] {
            let (left, top, width, height) = layout.button_rect(button);
            let middle = (
                left as f64 + width as f64 / 2.0,
                top as f64 + height as f64 / 2.0,
            );
            assert_eq!(cells.at(middle.0, middle.1), Some(kind));
        }
        // Below the band is the app.
        assert_eq!(cells.at(400.0, 40.0), None);
        // The icon's cell is the system menu, or the back arrow while the task can go back.
        let (left, top, _, _) = layout.icon_cell().unwrap();
        let on_icon = (left as f64 + 4.0, top as f64 + 4.0);
        assert_eq!(
            cells.at(on_icon.0, on_icon.1),
            Some(CaptionKind::SystemMenu)
        );
        let back = buttons_of(&layout, true, false);
        assert_eq!(back.at(on_icon.0, on_icon.1), Some(CaptionKind::Back));
        assert_eq!(back.kinds(), vec![Back, Minimize, Maximize, Close]);
        assert_eq!(back.kinds(), vec![Back, Minimize, Maximize, Close]);
        let hidden = buttons_of(&layout, false, true);
        assert_eq!(hidden.at(on_icon.0, on_icon.1), None);
        // The buttons stay in the tree while the bar is hidden, but nothing is hit.
        assert_eq!(hidden.kinds(), cells.kinds());
    }

    #[test]
    fn fr22_a_fixed_size_window_cannot_be_maximized() {
        let cells = CaptionCells {
            can_maximize: false,
            ..buttons_of(&at_100(), false, false)
        };
        assert!(!cells.enabled(CaptionKind::Maximize));
        assert!(cells.enabled(CaptionKind::Close));
        assert!(CaptionCells::default().kinds().is_empty());
    }
}
