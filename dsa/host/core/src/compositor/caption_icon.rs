//! Whether the title bar's icon stands out against the title bar under it, and what it is
//! drawn on when it does not.
//!
//! The title bar draws an adaptive icon's logo alone, enlarged to the size of a whole icon
//! ([`super::caption::caption_icon`]), so no square plate stands over the app. A logo in the
//! colour of the band under it (a white gear on a white title bar) then vanishes. So the
//! window measures how many of the logo's opaque pixels stand out against the band
//! ([`background_luminance`], [`needs_plate`]) and, when too few do, draws the icon on a plate
//! ([`choose`]): the whole icon with the app's own background, as the taskbar shows it, or, for
//! an app without a background that can serve, a small rounded plate of a neutral grey that
//! contrasts with the logo ([`on_neutral_plate`]). The choice is made again whenever the band's
//! colour changes, as when the app switches between light and dark content, with some
//! hysteresis so a band near the limit does not make the plate flicker.

use super::caption::{fit_square, relative_luminance, BandSample, CaptionTone};

/// How the title bar draws the app's icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconLook {
    /// The logo alone (or the whole icon, for an app without an adaptive foreground): it
    /// stands out against the band.
    Bare,
    /// The whole icon, the app's own background plate behind its logo.
    Whole,
    /// The logo on a rounded neutral plate.
    Plate,
}

/// The luminance (0 black, 1 white) of the title bar behind the icon: the colour the band is
/// filled with when the window fills it (never over an app that draws its own title bar), else
/// the band as the app's frame shows it, else what the glyphs' tone says it is (dark glyphs
/// stand on a light band).
pub fn background_luminance(sample: BandSample, app_drawn: bool, tone: CaptionTone) -> f64 {
    if let Some(fill) = sample.fill.filter(|_| !app_drawn) {
        return relative_luminance(fill);
    }
    if let Some(luminance) = sample.luminance {
        return f64::from(luminance) / 1000.0;
    }
    match tone {
        CaptionTone::DarkGlyphs => 1.0,
        CaptionTone::LightGlyphs => 0.0,
    }
}

/// The contrast ratio of two luminances, from 1 (the same) to 21 (black and white).
pub fn contrast_ratio(a: f64, b: f64) -> f64 {
    let (light, dark) = if a >= b { (a, b) } else { (b, a) };
    (light + 0.05) / (dark + 0.05)
}

/// A logo pixel stands out against the band from this contrast ratio on. It is below the 3
/// to 1 of a control's edge on purpose: an icon is read by its whole shape, and a mid-tone
/// colour of a many-coloured logo still shows.
pub const STANDS_OUT_FROM: f64 = 2.0;

/// How opaque (of 255) a pixel must be to count as part of the logo here.
const OPAQUE_FROM: u8 = 128;

/// The luminance of every pixel of `icon` (premultiplied BGRA) at least half opaque, as its
/// own colour.
fn opaque_luminances(icon: &[u8]) -> impl Iterator<Item = f64> + '_ {
    icon.chunks_exact(4)
        .filter(|pixel| pixel[3] >= OPAQUE_FROM)
        .map(|pixel| {
            let alpha = u32::from(pixel[3]);
            let straight = |channel: u8| ((u32::from(channel) * 255 + alpha / 2) / alpha).min(255);
            let argb = 0xFF00_0000
                | (straight(pixel[2]) << 16)
                | (straight(pixel[1]) << 8)
                | straight(pixel[0]);
            relative_luminance(argb)
        })
}

/// Whether `logo` (premultiplied BGRA) needs a plate over a band of luminance `background`:
/// too few of its opaque pixels stand out against it ([`STANDS_OUT_FROM`]). A bare logo takes a
/// plate once fewer than a third of them stand out, and a plated one (`plated`) loses it only
/// once half of them do, so a band near the limit keeps what it has. A logo with no opaque
/// pixel needs none.
pub fn needs_plate(logo: &[u8], background: f64, plated: bool) -> bool {
    let (mut opaque, mut standing) = (0usize, 0usize);
    for luminance in opaque_luminances(logo) {
        opaque += 1;
        if contrast_ratio(luminance, background) >= STANDS_OUT_FROM {
            standing += 1;
        }
    }
    if opaque == 0 {
        return false;
    }
    if plated {
        standing * 2 < opaque
    } else {
        standing * 3 < opaque
    }
}

/// Whether `whole` (premultiplied BGRA) has a background of its own: at least half of it is
/// opaque, as a plate (a circle fills 79% of its square, a squircle more) is and a bare logo
/// is not.
pub fn has_background(whole: &[u8]) -> bool {
    let pixels = whole.len() / 4;
    let opaque = whole
        .chunks_exact(4)
        .filter(|pixel| pixel[3] >= 0xF0)
        .count();
    pixels > 0 && opaque * 2 >= pixels
}

/// The neutral plate behind a light logo, and behind a dark one, 0xAARRGGBB.
pub const DARK_PLATE: u32 = 0xFF4A_4A4A;
pub const LIGHT_PLATE: u32 = 0xFFE6_E6E6;

/// The plate colour that contrasts with `logo`: dark behind a mostly light logo, light behind
/// a dark one.
pub fn plate_colour(logo: &[u8]) -> u32 {
    let (mut sum, mut count) = (0.0, 0usize);
    for luminance in opaque_luminances(logo) {
        sum += luminance;
        count += 1;
    }
    let mean = if count == 0 { 1.0 } else { sum / count as f64 };
    // The luminance whose contrast with black equals its contrast with white.
    if mean >= 0.18 {
        DARK_PLATE
    } else {
        LIGHT_PLATE
    }
}

/// `logo` (premultiplied BGRA, `side` square) on a rounded square plate of [`plate_colour`]
/// filling the icon's box, its corners a quarter of the side, the logo shrunk to leave an
/// eighth of the side free around it.
pub fn on_neutral_plate(logo: &[u8], side: u32) -> Vec<u8> {
    let length = side as usize * side as usize * 4;
    if side == 0 || logo.len() < length {
        return vec![0; length];
    }
    let colour = plate_colour(logo);
    let inset = (side / 8).max(1);
    let whole = (0.0, 0.0, f64::from(side), f64::from(side));
    let shrunk = fit_square(logo, side, side, whole, side, inset);
    let radius = f64::from(side) / 4.0;
    let size = f64::from(side);
    let mut out = vec![0u8; length];
    for y in 0..side as usize {
        for x in 0..side as usize {
            let coverage = rounded_coverage(x as f64 + 0.5, y as f64 + 0.5, size, radius);
            let plate = premultiplied(colour, coverage);
            let at = (y * side as usize + x) * 4;
            let top = [
                f64::from(shrunk[at]) / 255.0,
                f64::from(shrunk[at + 1]) / 255.0,
                f64::from(shrunk[at + 2]) / 255.0,
                f64::from(shrunk[at + 3]) / 255.0,
            ];
            for (channel, slot) in out[at..at + 4].iter_mut().enumerate() {
                // The logo over the plate, both premultiplied.
                let value = top[channel] + plate[channel] * (1.0 - top[3]);
                *slot = (value * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// How much of the pixel centred on (`x`, `y`) a rounded square `size` across with corners of
/// `radius` covers, antialiased over one pixel.
fn rounded_coverage(x: f64, y: f64, size: f64, radius: f64) -> f64 {
    let nearest = |value: f64| value.clamp(radius, size - radius);
    let (dx, dy) = (x - nearest(x), y - nearest(y));
    let outside = (dx * dx + dy * dy).sqrt() - radius;
    let edge = x.min(y).min(size - x).min(size - y);
    let corner = (0.5 - outside).clamp(0.0, 1.0);
    corner.min((edge + 0.5).clamp(0.0, 1.0))
}

/// `argb` premultiplied by `coverage`, as blue, green, red and alpha from 0 to 1.
fn premultiplied(argb: u32, coverage: f64) -> [f64; 4] {
    let alpha = f64::from(argb >> 24) / 255.0 * coverage;
    let channel = |shift: u32| f64::from((argb >> shift) & 0xff) / 255.0 * alpha;
    [channel(0), channel(8), channel(16), alpha]
}

/// The icon the title bar draws, `side` pixels square, as premultiplied BGRA, and how: the
/// bare icon (`logo`, an adaptive icon's foreground, else `whole`) while it stands out against
/// a band of luminance `background`; else the whole icon when it has a background of its own
/// and differs from the bare one; else the bare icon on a neutral plate. `shown` is the look
/// drawn now, which keeps a plate near the limit. `None` when there is no icon at all.
pub fn choose(
    logo: Option<&[u8]>,
    whole: Option<&[u8]>,
    side: u32,
    background: f64,
    shown: Option<IconLook>,
) -> Option<(IconLook, Vec<u8>)> {
    let length = side as usize * side as usize * 4;
    let logo = logo.filter(|pixels| pixels.len() >= length);
    let whole = whole.filter(|pixels| pixels.len() >= length);
    let bare = logo.or(whole)?;
    let plated = matches!(shown, Some(IconLook::Whole | IconLook::Plate));
    if !needs_plate(&bare[..length], background, plated) {
        return Some((IconLook::Bare, bare[..length].to_vec()));
    }
    if let (Some(_), Some(whole)) = (logo, whole) {
        if has_background(&whole[..length]) {
            return Some((IconLook::Whole, whole[..length].to_vec()));
        }
    }
    Some((IconLook::Plate, on_neutral_plate(&bare[..length], side)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `side` square icon: a disc of `colour` (0xAARRGGBB, opaque) of `fraction` of the side
    /// across, on transparency, as premultiplied BGRA.
    fn disc(side: u32, colour: u32, fraction: f64) -> Vec<u8> {
        let mut pixels = vec![0u8; (side * side * 4) as usize];
        let middle = f64::from(side) / 2.0;
        let radius = f64::from(side) * fraction / 2.0;
        for y in 0..side {
            for x in 0..side {
                let (dx, dy) = (f64::from(x) + 0.5 - middle, f64::from(y) + 0.5 - middle);
                if dx * dx + dy * dy <= radius * radius {
                    let at = ((y * side + x) * 4) as usize;
                    pixels[at] = (colour & 0xff) as u8;
                    pixels[at + 1] = ((colour >> 8) & 0xff) as u8;
                    pixels[at + 2] = ((colour >> 16) & 0xff) as u8;
                    pixels[at + 3] = 0xff;
                }
            }
        }
        pixels
    }

    #[test]
    fn fr22_a_low_contrast_caption_icon_gets_a_plate() {
        let side = 32;
        let white_logo = disc(side, 0xFFFF_FFFF, 0.6);
        // The app's whole icon: the white logo on its blue background, edge to edge.
        let mut whole = disc(side, 0xFF1A_73E8, 1.5);
        for (pixel, logo) in whole.chunks_exact_mut(4).zip(white_logo.chunks_exact(4)) {
            if logo[3] == 0xff {
                pixel.copy_from_slice(logo);
            }
        }
        let white_band = background_luminance(
            BandSample {
                fill: None,
                luminance: Some(1000),
            },
            false,
            CaptionTone::DarkGlyphs,
        );
        let dark_band = background_luminance(
            BandSample {
                fill: Some(0xFF20_2020),
                luminance: Some(1000),
            },
            false,
            CaptionTone::LightGlyphs,
        );
        // A white logo on a white title bar: the whole icon, with its plate, as the taskbar
        // shows it.
        let (look, pixels) =
            choose(Some(&white_logo), Some(&whole), side, white_band, None).unwrap();
        assert_eq!(look, IconLook::Whole);
        assert_eq!(pixels, whole);
        // The same logo on a dark title bar stands out and is drawn bare, enlarged as before.
        let (look, pixels) =
            choose(Some(&white_logo), Some(&whole), side, dark_band, None).unwrap();
        assert_eq!(look, IconLook::Bare);
        assert_eq!(pixels, white_logo);
        // Without a background that can serve, a neutral plate that contrasts with the logo:
        // dark behind a white logo, its corners rounded off.
        let bare_whole = white_logo.clone();
        let (look, plated) =
            choose(Some(&white_logo), Some(&bare_whole), side, white_band, None).unwrap();
        assert_eq!(look, IconLook::Plate);
        let at = |x: u32, y: u32| ((y * side + x) * 4) as usize;
        let edge = at(16, 1);
        assert_eq!(plated[edge + 3], 0xff, "the plate fills the icon's box");
        assert_eq!(plated[edge], 0x4A, "a dark grey behind a white logo");
        assert_eq!(plated[at(0, 0) + 3], 0, "its corner is rounded off");
        assert_eq!(plated[at(16, 16)], 0xff, "the logo stays white over it");
        // An icon with no adaptive foreground that fades into the band gets the plate too.
        let (look, _) = choose(None, Some(&white_logo), side, white_band, None).unwrap();
        assert_eq!(look, IconLook::Plate);
        // A dark logo on a dark band: a light plate.
        let black_logo = disc(side, 0xFF00_0000, 0.6);
        let (look, plated) = choose(Some(&black_logo), None, side, 0.0, None).unwrap();
        assert_eq!(look, IconLook::Plate);
        assert_eq!(plated[edge], 0xE6);
        // Nothing to draw, nothing chosen.
        assert!(choose(None, None, side, white_band, None).is_none());
    }

    #[test]
    fn fr22_the_plate_follows_the_band_without_flickering_at_the_limit() {
        // Two fifths of the logo's pixels mid grey and the rest white, over a white band.
        let side = 16;
        let mut logo = disc(side, 0xFFFF_FFFF, 1.5);
        let pixels = logo.len() / 4;
        for (index, pixel) in logo.chunks_exact_mut(4).enumerate() {
            if index * 5 < pixels * 2 {
                pixel[..3].copy_from_slice(&[0x80, 0x80, 0x80]);
            }
        }
        // Two fifths stand out: enough for a bare logo, too few to drop a plate already drawn.
        assert!(!needs_plate(&logo, 1.0, false));
        assert!(needs_plate(&logo, 1.0, true));
        // The band turns dark: every white pixel stands out, and the plate goes.
        assert!(!needs_plate(&logo, 0.0, true));
        // A band the window fills is judged by its fill; one never sampled by the glyphs' tone.
        let filled = BandSample {
            fill: Some(0xFF00_0000),
            luminance: Some(900),
        };
        assert_eq!(
            background_luminance(filled, false, CaptionTone::DarkGlyphs),
            0.0
        );
        assert_eq!(
            background_luminance(filled, true, CaptionTone::DarkGlyphs),
            0.9
        );
        let unread = BandSample::default();
        assert_eq!(
            background_luminance(unread, false, CaptionTone::DarkGlyphs),
            1.0
        );
        assert_eq!(
            background_luminance(unread, false, CaptionTone::LightGlyphs),
            0.0
        );
        assert!((contrast_ratio(1.0, 0.0) - 21.0).abs() < 1e-9);
    }
}
