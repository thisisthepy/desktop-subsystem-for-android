//! What a task window shows before its app's first frame arrives.
//!
//! The window opens as soon as the guest reports the task, and the guest's first frame (its
//! splash screen, drawn by the agent as the starting window) follows a moment later. A window
//! with nothing in it would show the desktop through it, so until that frame the window shows a
//! splash of its own: the app's icon centred on the colour Android puts behind a splash screen
//! whose theme names none. The guest's own splash, in the app's colours, then replaces it.

/// The splash background of a day theme that sets none: white, as Android uses.
pub const DAY_BACKGROUND: u32 = 0xFFFF_FFFF;

/// The splash background of a night theme that sets none, as Android uses.
pub const NIGHT_BACKGROUND: u32 = 0xFF20_2124;

/// The splash icon's size in density-independent pixels: the size Android's splash screen
/// shows an app's masked launcher icon at, and the agent's starting window with it, so the
/// icon keeps its size when the guest's splash replaces this one.
pub const ICON_DP: u32 = 160;

/// The icon never takes more than this share of the window's shorter side, in fifths.
const ICON_SHARE_FIFTHS: u32 = 2;

/// The splash background for the user's theme, as `0xAARRGGBB`.
pub fn background(dark: bool) -> u32 {
    if dark {
        NIGHT_BACKGROUND
    } else {
        DAY_BACKGROUND
    }
}

/// The edge of the icon in a window of `view` client pixels on a monitor of `dpi` dots per inch
/// (96 is 100 %), drawn from an image `source` pixels on its shorter side: 160 dp at the
/// monitor's scale, rounded to the nearest pixel, and never more than two fifths of the shorter
/// side, so a small window still shows its background around the icon.
///
/// Nor is it ever larger than the image: an icon scaled up is blurred, and a sharp icon a
/// little smaller is better than a blurred one at full size. The window programs carry an
/// image large enough for 160 dp up to 320 %.
pub fn icon_edge(view: (u32, u32), dpi: u32, source: u32) -> u32 {
    let dpi = dpi.max(96);
    let scaled = (ICON_DP * dpi + 48) / 96;
    let limit = view.0.min(view.1) * ICON_SHARE_FIFTHS / 5;
    scaled.min(limit).min(source)
}

/// An icon image: rows of premultiplied BGRA pixels, `size` pixels large.
#[derive(Debug, Clone, Copy)]
pub struct Icon<'a> {
    pub size: (u32, u32),
    pub pixels: &'a [u8],
}

/// The splash of a window of `view` client pixels: rows of premultiplied BGRA pixels, every one
/// of them opaque. `background` is `0xAARRGGBB` and is drawn opaque whatever its alpha. The
/// icon, when there is one, is scaled to a square of `edge` pixels and drawn over the centre.
pub fn compose(view: (u32, u32), background: u32, icon: Option<Icon<'_>>, edge: u32) -> Vec<u8> {
    let (width, height) = (view.0 as usize, view.1 as usize);
    let fill = [
        (background & 0xFF) as u8,
        ((background >> 8) & 0xFF) as u8,
        ((background >> 16) & 0xFF) as u8,
        0xFF,
    ];
    let mut pixels = Vec::with_capacity(width * height * 4);
    for _ in 0..width * height {
        pixels.extend_from_slice(&fill);
    }
    let Some(icon) = icon else {
        return pixels;
    };
    let (source_width, source_height) = (icon.size.0 as usize, icon.size.1 as usize);
    if source_width == 0
        || source_height == 0
        || icon.pixels.len() < source_width * source_height * 4
        || edge == 0
    {
        return pixels;
    }
    let edge = (edge as usize).min(width).min(height);
    let left = (width - edge) / 2;
    let top = (height - edge) / 2;
    let columns = coverage(source_width, edge);
    let rows = coverage(source_height, edge);
    for (y, row) in rows.iter().enumerate() {
        for (x, column) in columns.iter().enumerate() {
            // Each source pixel under this one, weighted by how much of it this one covers:
            // an image the size of the square is copied as it is, and a larger one is
            // averaged down without dropping or doubling any of its pixels.
            let mut sum = [0u64; 4];
            for &(sy, wy) in row {
                for &(sx, wx) in column {
                    let weight = u64::from(wy) * u64::from(wx);
                    let at = (sy * source_width + sx) * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += u64::from(icon.pixels[at + channel]) * weight;
                    }
                }
            }
            let whole = u64::from(WEIGHT_ONE) * u64::from(WEIGHT_ONE);
            let source = sum.map(|total| ((total + whole / 2) / whole) as u32);
            let at = ((top + y) * width + left + x) * 4;
            // Premultiplied "over": the icon, then the background through what it leaves.
            let alpha = source[3].min(255);
            for channel in 0..3 {
                let under = u32::from(pixels[at + channel]);
                let value = source[channel] + (under * (255 - alpha) + 127) / 255;
                pixels[at + channel] = value.min(255) as u8;
            }
        }
    }
    pixels
}

/// The weights of one destination pixel's coverage add up to this.
const WEIGHT_ONE: u32 = 1 << 12;

/// For each of `edge` destination pixels along one direction, the source pixels (of `source`)
/// it covers and how much of each, in [`WEIGHT_ONE`]ths: the area filter that scales an image
/// down with no pixel counted twice or skipped. The weights of each destination pixel add up
/// to exactly [`WEIGHT_ONE`].
fn coverage(source: usize, edge: usize) -> Vec<Vec<(usize, u32)>> {
    (0..edge)
        .map(|d| {
            // The destination pixel spans [d, d + 1) of `edge`, which is
            // [d * source, (d + 1) * source) in units of 1 / edge source pixels.
            let start = d * source;
            let end = (d + 1) * source;
            let span = (end - start) as u64;
            let mut taps = Vec::new();
            let mut given = 0u32;
            let mut s = start / edge;
            while s * edge < end && s < source {
                let from = start.max(s * edge);
                let to = end.min((s + 1) * edge);
                let weight = ((to - from) as u64 * u64::from(WEIGHT_ONE) / span) as u32;
                taps.push((s, weight));
                given += weight;
                s += 1;
            }
            // What rounding left over goes to the largest tap, so the weights add up exactly.
            if let Some(largest) = taps.iter_mut().max_by_key(|(_, w)| *w) {
                largest.1 += WEIGHT_ONE - given;
            }
            taps
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
    }

    /// A square icon of one premultiplied BGRA colour.
    fn solid_icon(edge: u32, bgra: [u8; 4]) -> Vec<u8> {
        bgra.repeat((edge * edge) as usize)
    }

    #[test]
    fn fr3_a_window_before_its_first_frame_is_opaque_everywhere() {
        let icon = solid_icon(64, [0, 0, 0, 0]);
        for background in [DAY_BACKGROUND, NIGHT_BACKGROUND, 0x0012_3456] {
            let pixels = compose(
                (300, 200),
                background,
                Some(Icon {
                    size: (64, 64),
                    pixels: &icon,
                }),
                80,
            );
            assert_eq!(pixels.len(), 300 * 200 * 4);
            assert!(pixels.chunks(4).all(|p| p[3] == 0xFF));
        }
    }

    #[test]
    fn fr3_the_splash_centres_the_apps_icon_on_its_background() {
        // A red icon on a night background.
        let icon = solid_icon(32, [0, 0, 0xFF, 0xFF]);
        let pixels = compose(
            (400, 300),
            NIGHT_BACKGROUND,
            Some(Icon {
                size: (32, 32),
                pixels: &icon,
            }),
            100,
        );
        let night = [0x24, 0x21, 0x20, 0xFF];
        let red = [0, 0, 0xFF, 0xFF];
        assert_eq!(pixel(&pixels, 400, 200, 150), red);
        // The icon spans 150..250 across and 100..200 down.
        assert_eq!(pixel(&pixels, 400, 150, 100), red);
        assert_eq!(pixel(&pixels, 400, 249, 199), red);
        assert_eq!(pixel(&pixels, 400, 149, 150), night);
        assert_eq!(pixel(&pixels, 400, 250, 150), night);
        assert_eq!(pixel(&pixels, 400, 200, 99), night);
        assert_eq!(pixel(&pixels, 400, 200, 200), night);
        assert_eq!(pixel(&pixels, 400, 0, 0), night);
    }

    #[test]
    fn fr3_the_background_shows_through_the_icons_transparent_pixels() {
        // Half transparent blue, premultiplied: 0x80 of blue at alpha 0x80.
        let icon = solid_icon(8, [0x80, 0, 0, 0x80]);
        let pixels = compose(
            (20, 20),
            DAY_BACKGROUND,
            Some(Icon {
                size: (8, 8),
                pixels: &icon,
            }),
            8,
        );
        let centre = pixel(&pixels, 20, 10, 10);
        // White through 0x7F of the icon's remaining coverage, plus the blue.
        assert_eq!(centre, [0xFF, 0x7F, 0x7F, 0xFF]);
    }

    #[test]
    fn fr3_a_window_without_an_icon_shows_its_background_alone() {
        let pixels = compose((10, 10), DAY_BACKGROUND, None, 4);
        assert!(pixels.chunks(4).all(|p| p == [0xFF, 0xFF, 0xFF, 0xFF]));
        // An icon whose pixels do not match its size is left out rather than read past.
        let short = [0u8; 12];
        let pixels = compose(
            (10, 10),
            NIGHT_BACKGROUND,
            Some(Icon {
                size: (4, 4),
                pixels: &short,
            }),
            4,
        );
        assert!(pixels.chunks(4).all(|p| p == [0x24, 0x21, 0x20, 0xFF]));
    }

    #[test]
    fn fr3_the_splash_background_follows_the_users_theme() {
        assert_eq!(background(false), DAY_BACKGROUND);
        assert_eq!(background(true), NIGHT_BACKGROUND);
    }

    #[test]
    fn fr3_the_icon_keeps_its_size_to_the_eye_and_fits_a_small_window() {
        // 160 dp, as the guest's splash shows the app's icon, at 100%, 125% and 200%.
        assert_eq!(icon_edge((1000, 800), 96, 512), 160);
        assert_eq!(icon_edge((1000, 800), 120, 512), 200);
        assert_eq!(icon_edge((1000, 800), 192, 512), 320);
        // Two fifths of the shorter side at most.
        assert_eq!(icon_edge((400, 200), 96, 512), 80);
        // A window with no area draws no icon.
        assert_eq!(icon_edge((0, 0), 96, 512), 0);
    }

    #[test]
    fn fr3_the_splash_icon_is_never_scaled_up() {
        // A program that carries only a 256 pixel icon shows it at 256 on a 200% monitor
        // rather than blurred to 320.
        assert_eq!(icon_edge((2000, 1600), 192, 256), 256);
        assert_eq!(icon_edge((2000, 1600), 120, 256), 200);
        for dpi in [96, 120, 144, 192, 240, 288] {
            for source in [48, 256, 512] {
                assert!(icon_edge((4000, 4000), dpi, source) <= source);
            }
        }
    }

    #[test]
    fn fr3_an_icon_the_size_of_its_square_is_copied_pixel_for_pixel() {
        // A one-pixel checker would turn grey under any resampling.
        let mut icon = Vec::new();
        for y in 0..8u32 {
            for x in 0..8u32 {
                icon.extend_from_slice(if (x + y) % 2 == 0 {
                    &[0, 0, 0, 0xFF]
                } else {
                    &[0xFF, 0xFF, 0xFF, 0xFF]
                });
            }
        }
        let pixels = compose(
            (8, 8),
            NIGHT_BACKGROUND,
            Some(Icon {
                size: (8, 8),
                pixels: &icon,
            }),
            8,
        );
        assert_eq!(pixels, icon);
    }

    #[test]
    fn fr3_a_larger_icon_is_averaged_down_by_the_area_each_pixel_covers() {
        // Three columns, black, white, black, into two pixels: each takes one and a half
        // source pixels, a black one whole and half of the white one.
        let icon = [[0, 0, 0, 0xFF], [0xFF, 0xFF, 0xFF, 0xFF], [0, 0, 0, 0xFF]].concat();
        let pixels = compose(
            (2, 1),
            DAY_BACKGROUND,
            Some(Icon {
                size: (3, 1),
                pixels: &icon,
            }),
            1,
        );
        // A 1 pixel square of the 3 by 1 image: the average of all three.
        assert_eq!(pixel(&pixels, 2, 0, 0), [0x55, 0x55, 0x55, 0xFF]);
        for (source, edge) in [(512, 320), (512, 200), (256, 160), (3, 2), (7, 3)] {
            for taps in coverage(source, edge) {
                assert_eq!(taps.iter().map(|(_, w)| *w).sum::<u32>(), WEIGHT_ONE);
            }
        }
        let halves = coverage(3, 2);
        // Two thirds of the first pixel and a third of the second, with the rounding's
        // leftover on the larger share.
        assert_eq!(halves[0], vec![(0, 2731), (1, 1365)]);
        assert_eq!(halves[1], vec![(1, 1365), (2, 2731)]);
    }
}
