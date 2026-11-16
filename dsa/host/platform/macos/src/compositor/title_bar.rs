//! What a task window's title bar does on a Mac, apart from AppKit: the band an app that
//! draws its own title bar is told about, which points of that band drag the window, what a
//! double click on it does, and the back button's keys and name.
//!
//! macOS draws a task window's title bar itself, with the standard window buttons (close,
//! minimize, zoom) at its leading edge, the app's name in the middle and no icon: a Mac
//! window's title bar shows an icon only for a document, and the Dock and the menu bar
//! already say which app a window belongs to. While the task can go back, a back button
//! (a `chevron.backward` symbol) stands after the window buttons.
//!
//! An app that draws its own title bar gets a transparent, full-size content title bar: its
//! frames run under the band, and only the window buttons and the back button stay over it.
//! The guest is told how high the band is and how far those buttons reach in from each of
//! its ends ([`caption_band`]), so the app keeps its own controls clear of them. In the band,
//! the app's controls take the pointer and the rest drags the window ([`band_hit`]).

use dsa_host_core::compositor::caption::{
    band_points, band_sample, shown_tone, BandSample, CaptionBand, CaptionTone,
};
use dsa_host_core::keys::{keycode, KeyAction, KeyEvent};
use dsa_host_core::tasks::Bounds;
use dsa_host_core::ui_language::UiLanguage;

/// The space left after the window's own buttons in the band, in points, so an app's
/// controls never touch them. A Mac app's toolbar starts about this far after the zoom
/// button.
pub const ITEM_GAP_POINTS: f64 = 8.0;

/// The band an app drawing its own title bar is told about: `height` points high, in a
/// window `width` points wide, at `scale` pixels a point. `items` are the left and right
/// edges (points from the window's left) of what macOS keeps over the band: the window
/// buttons and the back button. An item in the left half of the band counts toward
/// `start_width`, how far in from the band's left end it reaches; one in the right half
/// toward `end_width`, from the right end. The window buttons stand at the left on a Mac (at
/// the right in a right-to-left language), the reverse of Windows, so the guest is told the
/// sides as they are. A window that shows no band (full screen hides it) gets an empty one.
pub fn caption_band(width: f64, height: f64, items: &[(f64, f64)], scale: f64) -> CaptionBand {
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
            start = start.max(right.max(0.0) + ITEM_GAP_POINTS);
        } else {
            end = end.max((width - left).max(0.0) + ITEM_GAP_POINTS);
        }
    }
    let pixels = |points: f64| (points.min(width) * scale).ceil() as u32;
    CaptionBand {
        height: (height * scale).round() as u32,
        start_width: pixels(start),
        end_width: pixels(end),
    }
}

/// What a press in a task window's content goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BandHit {
    /// The app: the press goes to the guest.
    App,
    /// The title bar band outside the app's controls: the press drags the window, a double
    /// click does what the user set for a title bar, and a secondary click opens the window's
    /// menu.
    Drag,
}

/// What the client pixel `point` (from the content's top left) goes to, in a window whose
/// app draws its own title bar over the top `band_height` pixels with its controls at
/// `controls` (client pixels). Below the band, and with no band, everything is the app's.
pub fn band_hit(point: (f64, f64), band_height: u32, controls: &[Bounds]) -> BandHit {
    let (x, y) = point;
    if !(y >= 0.0 && y < f64::from(band_height)) {
        return BandHit::App;
    }
    let on_control = controls.iter().any(|c| {
        x >= f64::from(c.left)
            && x < f64::from(c.right)
            && y >= f64::from(c.top)
            && y < f64::from(c.bottom)
    });
    if on_control {
        BandHit::App
    } else {
        BandHit::Drag
    }
}

/// How a task window's title bar is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chrome {
    /// The unified title bar: transparent, with the app drawn up to the window's top and the
    /// app's name in the band. The window buttons and the back button stand over it.
    Unified,
    /// The app draws its own title bar: as [`Chrome::Unified`], without the name.
    AppDrawn,
    /// AppKit's opaque title bar above the app: the app starts below it.
    Standard,
}

impl Chrome {
    /// The chrome of a window whose app draws its own title bar (`app_drawn`) or was set to
    /// start below the title bar (`below`, which wins).
    pub fn of(app_drawn: bool, below: bool) -> Chrome {
        if below {
            Chrome::Standard
        } else if app_drawn {
            Chrome::AppDrawn
        } else {
            Chrome::Unified
        }
    }

    /// Whether the app's frames run under the title bar.
    pub fn full_size_content(self) -> bool {
        self != Chrome::Standard
    }

    /// Whether the title bar shows the window's name.
    pub fn shows_title(self) -> bool {
        self != Chrome::AppDrawn
    }
}

/// The `NSAppearance` name a title bar over glyphs of `tone` takes: the title's text follows
/// the window's appearance, dark text for dark glyphs.
pub fn appearance_name(tone: CaptionTone) -> &'static str {
    match tone {
        CaptionTone::DarkGlyphs => "NSAppearanceNameAqua",
        CaptionTone::LightGlyphs => "NSAppearanceNameDarkAqua",
    }
}

/// What the title bar looks like over its frames: the tone of its text and a colour for a band
/// the app leaves empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub tone: CaptionTone,
    /// 0xAARRGGBB, opaque; the colour to fill the band with. `None` leaves the band as the
    /// app drew it.
    pub fill: Option<u32>,
}

/// What decides the tone and the fill of a window's title bar: the tone the app asked for,
/// what the window read from the frame's band, and whether the app draws the band itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct BandTone {
    reported: Option<CaptionTone>,
    sample: BandSample,
    app_drawn: bool,
    shown: Option<CaptionTone>,
    told: Option<Look>,
}

impl BandTone {
    fn look(&mut self) -> Option<Look> {
        self.shown = shown_tone(self.reported, self.sample, self.app_drawn, self.shown);
        let look = Look {
            tone: self.shown.unwrap_or_default(),
            fill: self.sample.fill.filter(|_| !self.app_drawn),
        };
        (self.told != Some(look)).then(|| {
            self.told = Some(look);
            look
        })
    }

    /// The tone the app asked for changed. Returns the new look, if it changed.
    pub fn set_reported(&mut self, reported: Option<CaptionTone>) -> Option<Look> {
        self.reported = reported;
        self.look()
    }

    /// The window read `sample` from its newest frame. Returns the new look, if it changed.
    pub fn set_sample(&mut self, sample: BandSample) -> Option<Look> {
        self.sample = sample;
        self.look()
    }

    /// Whether the app draws its own band changed. Returns the new look, if it changed.
    pub fn set_app_drawn(&mut self, app_drawn: bool) -> Option<Look> {
        self.app_drawn = app_drawn;
        self.look()
    }
}

/// What a window reads under a band `band` pixels high from `pixels`, a frame `width` by
/// `height` pixels of BGRA rows `stride` bytes apart, top row first.
pub fn sample_bgra(pixels: &[u8], stride: usize, width: u32, height: u32, band: u32) -> BandSample {
    let Some(points) = band_points((width, height), band) else {
        return BandSample::default();
    };
    let last = points.below as usize * stride + (width as usize) * 4;
    if pixels.len() < last {
        return BandSample::default();
    }
    let row = |y: u32| -> Vec<u32> {
        points
            .columns
            .iter()
            .map(|&x| {
                let at = y as usize * stride + x as usize * 4;
                u32::from_be_bytes([pixels[at + 3], pixels[at + 2], pixels[at + 1], pixels[at]])
            })
            .collect()
    };
    band_sample(&row(points.inside), &row(points.below))
}

/// The gap between the zoom button and the window's name, in points.
pub const NAME_GAP: f64 = 14.0;
/// The back button's size and its distance from the title bar's right end, in points.
pub const BACK_SIZE: (f64, f64) = (28.0, 22.0);
pub const BACK_MARGIN: f64 = 8.0;

/// Where the title bar's own parts stand, in the title bar's coordinates (points; `y` is the
/// bottom of the part in AppKit's coordinates, so only the centre line matters).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleLayout {
    /// The window's name: left edge, bottom edge and width. It is as high as the text.
    pub name: (f64, f64, f64),
    /// The back button: left edge, bottom edge, width and height; `None` while it is hidden.
    pub back: Option<(f64, f64, f64, f64)>,
}

/// Lays out the name and the back button in a title bar `bar_width` wide. The name starts
/// [`NAME_GAP`] after the zoom button (`buttons_right`) whether or not the back button shows,
/// and ends before the back button, which stands at the right end. Both are centred on the
/// window buttons' centre line (`centre_y`), so the name has the same space above and below it
/// as they do. `text` is the name's size at its natural width.
pub fn title_layout(
    bar_width: f64,
    centre_y: f64,
    buttons_right: f64,
    text: (f64, f64),
    back: bool,
) -> TitleLayout {
    let name_left = buttons_right + NAME_GAP;
    let back_left = bar_width - BACK_MARGIN - BACK_SIZE.0;
    let end = if back {
        back_left - NAME_GAP
    } else {
        bar_width - BACK_MARGIN
    };
    let width = text.0.min((end - name_left).max(0.0)).max(0.0);
    TitleLayout {
        name: (name_left, centre_y - text.1 / 2.0, width),
        back: back.then_some((
            back_left,
            centre_y - BACK_SIZE.1 / 2.0,
            BACK_SIZE.0,
            BACK_SIZE.1,
        )),
    }
}

/// What a double click on a title bar does, as the user chose in System Settings (Desktop &
/// Dock, "Double-click a window's title bar to").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DoubleClick {
    Zoom,
    Minimize,
    Nothing,
}

/// The double click action for the global `AppleActionOnDoubleClick` default (`Maximize`,
/// `Minimize`, `None`, or `Fill` on newer macOS), falling back to the older
/// `AppleMiniaturizeOnDoubleClick` when it is not set. Zoom is macOS's default.
pub fn double_click_action(action: Option<&str>, minimize_legacy: bool) -> DoubleClick {
    match action.map(str::trim) {
        Some(a) if a.eq_ignore_ascii_case("minimize") => DoubleClick::Minimize,
        Some(a) if a.eq_ignore_ascii_case("none") => DoubleClick::Nothing,
        Some(a) if !a.is_empty() => DoubleClick::Zoom,
        _ if minimize_legacy => DoubleClick::Minimize,
        _ => DoubleClick::Zoom,
    }
}

/// The key events a click on the back button sends the task: Back pressed and released, at
/// `time_ms` of the host's clock.
pub fn back_keys(task_id: i32, time_ms: u32) -> [KeyEvent; 2] {
    let key = |action| KeyEvent {
        task_id,
        action,
        key_code: keycode::BACK,
        repeat_count: 0,
        meta_state: 0,
        event_time_ms: time_ms,
        canceled: false,
    };
    [key(KeyAction::Down), key(KeyAction::Up)]
}

/// The back button's name for VoiceOver and its tooltip, in `lang`, as macOS's own back
/// buttons are named.
pub fn back_label(lang: UiLanguage) -> &'static str {
    match lang {
        UiLanguage::English => "Back",
        UiLanguage::Korean => "뒤로",
        UiLanguage::Japanese => "戻る",
        UiLanguage::ChineseSimplified => "返回",
        UiLanguage::ChineseTraditional => "返回",
        UiLanguage::Spanish => "Atrás",
        UiLanguage::French => "Retour",
        UiLanguage::German => "Zurück",
        UiLanguage::PortugueseBrazil => "Voltar",
        UiLanguage::Russian => "Назад",
        UiLanguage::Italian => "Indietro",
        UiLanguage::Vietnamese => "Quay lại",
        UiLanguage::Indonesian => "Kembali",
        UiLanguage::Turkish => "Geri",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, band: u32, band_bgra: [u8; 4], app_bgra: [u8; 4]) -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..height {
            let color = if y < band { band_bgra } else { app_bgra };
            for _ in 0..width {
                pixels.extend_from_slice(&color);
            }
        }
        pixels
    }

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    #[test]
    fn fr22_every_task_window_has_the_unified_title_bar_unless_the_app_starts_below_it() {
        assert_eq!(Chrome::of(false, false), Chrome::Unified);
        assert!(Chrome::Unified.full_size_content() && Chrome::Unified.shows_title());
        assert_eq!(Chrome::of(true, false), Chrome::AppDrawn);
        assert!(Chrome::AppDrawn.full_size_content() && !Chrome::AppDrawn.shows_title());
        assert_eq!(Chrome::of(false, true), Chrome::Standard);
        assert_eq!(Chrome::of(true, true), Chrome::Standard);
        assert!(!Chrome::Standard.full_size_content() && Chrome::Standard.shows_title());
    }

    #[test]
    fn fr22_dark_glyphs_take_the_light_appearance_and_light_glyphs_the_dark_one() {
        assert_eq!(
            appearance_name(CaptionTone::DarkGlyphs),
            "NSAppearanceNameAqua"
        );
        assert_eq!(
            appearance_name(CaptionTone::LightGlyphs),
            "NSAppearanceNameDarkAqua"
        );
    }

    #[test]
    fn fr22_the_apps_tone_is_where_the_band_starts() {
        let mut tone = BandTone::default();
        let look = tone.set_reported(Some(CaptionTone::LightGlyphs)).unwrap();
        assert_eq!((look.tone, look.fill), (CaptionTone::LightGlyphs, None));
        // The same look again is not news.
        assert_eq!(tone.set_reported(Some(CaptionTone::LightGlyphs)), None);
    }

    #[test]
    fn fr22_light_glyphs_an_app_asked_for_turn_dark_over_a_bar_light_from_end_to_end() {
        let mut tone = BandTone::default();
        tone.set_reported(Some(CaptionTone::LightGlyphs));
        let pixels = frame(80, 80, 24, WHITE, WHITE);
        let sample = sample_bgra(&pixels, 80 * 4, 80, 80, 24);
        let look = tone.set_sample(sample).unwrap();
        assert_eq!(look.tone, CaptionTone::DarkGlyphs);
    }

    #[test]
    fn fr22_dark_glyphs_an_app_asked_for_turn_light_over_a_bar_dark_from_end_to_end() {
        let mut tone = BandTone::default();
        tone.set_reported(Some(CaptionTone::DarkGlyphs));
        let pixels = frame(80, 80, 24, BLACK, BLACK);
        let sample = sample_bgra(&pixels, 80 * 4, 80, 80, 24);
        let look = tone.set_sample(sample).unwrap();
        assert_eq!(look.tone, CaptionTone::LightGlyphs);
    }

    #[test]
    fn fr22_an_app_that_leaves_the_band_empty_gets_its_top_colour_there() {
        let mut tone = BandTone::default();
        // A black band over a white app: filled white, and the title over it is dark.
        let pixels = frame(80, 80, 24, BLACK, WHITE);
        let sample = sample_bgra(&pixels, 80 * 4, 80, 80, 24);
        let look = tone.set_sample(sample).unwrap();
        assert_eq!(look.fill, Some(0xFFFFFFFF));
        assert_eq!(look.tone, CaptionTone::DarkGlyphs);
        // An app that draws its own band is never filled over.
        let mut drawn = BandTone::default();
        drawn.set_app_drawn(true);
        assert_eq!(drawn.set_sample(sample).map(|look| look.fill), Some(None));
    }

    #[test]
    fn fr22_a_frame_too_small_for_its_band_reads_as_nothing() {
        let pixels = frame(80, 20, 24, WHITE, WHITE);
        assert_eq!(
            sample_bgra(&pixels, 80 * 4, 80, 20, 24),
            BandSample::default()
        );
        assert_eq!(
            sample_bgra(&pixels[..10], 80 * 4, 80, 80, 24),
            BandSample::default()
        );
    }

    #[test]
    fn fr22_macos_the_title_is_centred_on_the_traffic_lights() {
        // Window buttons centred 16 points from the bar's bottom; a 16 point high name.
        let layout = title_layout(600.0, 16.0, 70.0, (80.0, 16.0), false);
        let (_, bottom, _) = layout.name;
        assert_eq!(bottom + 8.0, 16.0);
        let with_back = title_layout(600.0, 16.0, 70.0, (80.0, 16.0), true);
        assert_eq!(with_back.name.1, layout.name.1);
    }

    #[test]
    fn fr22_macos_the_back_button_stands_at_the_right_end_of_the_title_bar() {
        let layout = title_layout(600.0, 16.0, 70.0, (80.0, 16.0), true);
        let (left, bottom, width, height) = layout.back.unwrap();
        assert_eq!(left + width + BACK_MARGIN, 600.0);
        assert_eq!(bottom + height / 2.0, 16.0);
        // The name starts after the window buttons, back button or not.
        assert_eq!(layout.name.0, 70.0 + NAME_GAP);
        assert_eq!(
            title_layout(600.0, 16.0, 70.0, (80.0, 16.0), false).back,
            None
        );
    }

    #[test]
    fn fr22_macos_a_long_name_ends_before_the_back_button() {
        let layout = title_layout(300.0, 16.0, 70.0, (900.0, 16.0), true);
        let (left, ..) = layout.back.unwrap();
        assert_eq!(layout.name.0 + layout.name.2 + NAME_GAP, left);
        // A window too narrow for any name leaves it no width.
        assert_eq!(
            title_layout(100.0, 16.0, 70.0, (50.0, 16.0), true).name.2,
            0.0
        );
    }

    #[test]
    fn fr22_the_band_reaches_past_the_window_buttons_from_the_left() {
        // The three window buttons from 7 to 67 points, in a 600 point window at 2x.
        let band = caption_band(600.0, 28.0, &[(7.0, 21.0), (27.0, 41.0), (47.0, 61.0)], 2.0);
        assert_eq!(band.height, 56);
        assert_eq!(band.start_width, ((61.0 + ITEM_GAP_POINTS) * 2.0) as u32);
        assert_eq!(band.end_width, 0);
    }

    #[test]
    fn fr22_the_back_button_widens_the_bands_left_end() {
        let buttons = [(7.0, 21.0), (27.0, 41.0), (47.0, 61.0)];
        let without = caption_band(600.0, 28.0, &buttons, 1.0);
        let mut with_back = buttons.to_vec();
        with_back.push((68.0, 96.0));
        let with = caption_band(600.0, 28.0, &with_back, 1.0);
        assert_eq!(with.height, without.height);
        assert_eq!(with.start_width, (96.0 + ITEM_GAP_POINTS) as u32);
        assert!(with.start_width > without.start_width);
    }

    #[test]
    fn fr22_right_to_left_buttons_count_from_the_right_end() {
        let band = caption_band(600.0, 28.0, &[(539.0, 553.0), (579.0, 593.0)], 1.0);
        assert_eq!(band.start_width, 0);
        assert_eq!(band.end_width, (600.0 - 539.0 + ITEM_GAP_POINTS) as u32);
    }

    #[test]
    fn fr22_a_window_without_a_band_tells_the_app_none() {
        assert_eq!(
            caption_band(600.0, 0.0, &[(7.0, 21.0)], 2.0),
            CaptionBand::default()
        );
        assert_eq!(
            caption_band(600.0, f64::NAN, &[(7.0, 21.0)], 2.0),
            CaptionBand::default()
        );
        // A bad scale counts as 1x rather than as no band.
        assert_eq!(caption_band(600.0, 28.0, &[], 0.0).height, 28);
    }

    #[test]
    fn fr22_the_apps_controls_take_the_pointer_and_the_rest_of_the_band_drags() {
        let tab = Bounds {
            left: 200,
            top: 0,
            right: 400,
            bottom: 56,
        };
        assert_eq!(band_hit((300.0, 20.0), 56, &[tab]), BandHit::App);
        assert_eq!(band_hit((450.0, 20.0), 56, &[tab]), BandHit::Drag);
        // Below the band the app has everything.
        assert_eq!(band_hit((450.0, 56.0), 56, &[tab]), BandHit::App);
        // No band: nothing drags.
        assert_eq!(band_hit((450.0, 0.0), 0, &[]), BandHit::App);
    }

    #[test]
    fn fr22_a_double_click_on_the_band_does_what_the_user_set() {
        assert_eq!(double_click_action(None, false), DoubleClick::Zoom);
        assert_eq!(
            double_click_action(Some("Maximize"), true),
            DoubleClick::Zoom
        );
        assert_eq!(double_click_action(Some("Fill"), false), DoubleClick::Zoom);
        assert_eq!(
            double_click_action(Some("Minimize"), false),
            DoubleClick::Minimize
        );
        assert_eq!(
            double_click_action(Some("None"), false),
            DoubleClick::Nothing
        );
        assert_eq!(double_click_action(None, true), DoubleClick::Minimize);
    }

    #[test]
    fn fr22_the_back_button_sends_the_task_back_down_and_up() {
        let [down, up] = back_keys(7, 1234);
        assert_eq!((down.task_id, down.key_code), (7, keycode::BACK));
        assert_eq!(down.action, KeyAction::Down);
        assert_eq!(up.action, KeyAction::Up);
        assert_eq!(up.key_code, keycode::BACK);
        assert_eq!((down.event_time_ms, up.event_time_ms), (1234, 1234));
        assert!(!down.canceled && !up.canceled);
    }

    #[test]
    fn fr22_the_back_button_is_named_in_the_users_language() {
        assert_eq!(back_label(UiLanguage::Korean), "뒤로");
        assert_eq!(back_label(UiLanguage::English), "Back");
        assert_eq!(back_label(UiLanguage::Japanese), "戻る");
        assert_eq!(back_label(UiLanguage::German), "Zurück");
    }

    #[test]
    fn fr17_the_back_button_has_a_name_in_every_language() {
        for lang in UiLanguage::ALL {
            let label = back_label(lang);
            assert!(!label.trim().is_empty(), "{lang:?}");
            assert!(!crate::ui_language::has_em_dash(label), "{lang:?}");
            if lang != UiLanguage::English {
                assert_ne!(label, "Back", "{lang:?} is not translated");
            }
        }
    }
}
