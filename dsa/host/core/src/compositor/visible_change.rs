//! When a press first changes what a window shows enough to be seen.
//!
//! The first frame an app presents after a press is often a ripple a few percent darker than
//! the button, which nobody notices. While the input trace is on, a task window therefore
//! reads a small square of each frame around the press point and compares it with the same
//! square of the frame that was on the screen when the press came. The first frame in which
//! enough of the square has changed by enough is the first visible change.
//!
//! A pixel counts as changed when one of its colour channels moved by at least
//! [`CHANNEL_THRESHOLD`] of 255, about a tenth of the range; a ripple at its usual strength
//! of a few percent stays below it. A frame counts as a visible change when at least
//! [`VISIBLE_FRACTION`] of the square's pixels changed. The comparison is with the frame
//! before the press, not the frame before this one, so a change that builds up over several
//! frames is caught when it has built up.
//!
//! Only the platform reads pixels; this module only says where and what the pixels mean.

/// The side of the square read around the press point, in guest pixels.
pub const SIDE: u32 = 64;

/// How far one colour channel must move, of 255, for its pixel to count as changed.
pub const CHANNEL_THRESHOLD: u8 = 24;

/// The share of the square's pixels that must change for a frame to be a visible change.
pub const VISIBLE_FRACTION: f32 = 0.10;

/// How many frames after a press are compared before the watch gives up.
pub const MAX_FRAMES: u32 = 240;

/// A rectangle of a frame, in its pixels: `left..right` by `top..bottom`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Area {
    pub fn width(&self) -> u32 {
        self.right - self.left
    }

    pub fn height(&self) -> u32 {
        self.bottom - self.top
    }

    /// The bytes of the area as tightly packed RGBA rows.
    pub fn bytes(&self) -> usize {
        (self.width() * self.height() * 4) as usize
    }
}

/// The square of `side` pixels centred on `point`, moved inside a frame of `frame` pixels
/// and cut to it when the frame is smaller. `None` when the point is outside the frame or the
/// frame is empty.
pub fn area_around(point: (f32, f32), frame: (u32, u32), side: u32) -> Option<Area> {
    let (x, y) = point;
    let (width, height) = frame;
    if width == 0 || height == 0 || side == 0 || !x.is_finite() || !y.is_finite() {
        return None;
    }
    if x < 0.0 || y < 0.0 || x >= width as f32 || y >= height as f32 {
        return None;
    }
    let span = |centre: f32, limit: u32| {
        let side = side.min(limit);
        let highest = i64::from(limit - side);
        let start = (centre as i64 - i64::from(side / 2)).clamp(0, highest) as u32;
        (start, start + side)
    };
    let (left, right) = span(x, width);
    let (top, bottom) = span(y, height);
    Some(Area {
        left,
        top,
        right,
        bottom,
    })
}

/// The share of pixels, from 0 to 1, that differ between two images of the same area
/// (tightly packed RGBA rows) by at least [`CHANNEL_THRESHOLD`] in a colour channel. Alpha
/// is not compared: the window shows no alpha. Images of different lengths compare as
/// wholly changed, since their areas cannot be the same.
pub fn changed_fraction(before: &[u8], after: &[u8]) -> f32 {
    if before.len() != after.len() {
        return 1.0;
    }
    let pixels = before.len() / 4;
    if pixels == 0 {
        return 0.0;
    }
    let changed = before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .filter(|(a, b)| (0..3).any(|c| a[c].abs_diff(b[c]) >= CHANNEL_THRESHOLD))
        .count();
    changed as f32 / pixels as f32
}

/// What one more frame after a press says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// Not enough has changed yet.
    Waiting,
    /// This frame, the `frames`th after the press, is the first visible change: `fraction`
    /// of the area changed.
    Visible { frames: u32, fraction: f32 },
    /// [`MAX_FRAMES`] frames came and none was a visible change; `largest` is the most any
    /// of them changed.
    GaveUp { frames: u32, largest: f32 },
}

/// One press being watched: the area around it, and what the area showed when it came.
#[derive(Debug, Clone)]
pub struct PressWatch {
    area: Area,
    baseline: Vec<u8>,
    frames: u32,
    largest: f32,
}

impl PressWatch {
    /// Watches `area`, which showed `baseline` (tightly packed RGBA rows) when the press came.
    pub fn new(area: Area, baseline: Vec<u8>) -> PressWatch {
        PressWatch {
            area,
            baseline,
            frames: 0,
            largest: 0.0,
        }
    }

    pub fn area(&self) -> Area {
        self.area
    }

    /// The next frame after the press shows `pixels` in the area. Once this returns anything
    /// but [`Verdict::Waiting`], the watch is over.
    pub fn frame(&mut self, pixels: &[u8]) -> Verdict {
        self.frames += 1;
        let fraction = changed_fraction(&self.baseline, pixels);
        self.largest = self.largest.max(fraction);
        if fraction >= VISIBLE_FRACTION {
            Verdict::Visible {
                frames: self.frames,
                fraction,
            }
        } else if self.frames >= MAX_FRAMES {
            Verdict::GaveUp {
                frames: self.frames,
                largest: self.largest,
            }
        } else {
            Verdict::Waiting
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(area: Area, rgb: [u8; 3]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(area.bytes());
        for _ in 0..area.width() * area.height() {
            bytes.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        bytes
    }

    /// `image`, with the first `count` pixels in `rgb` instead.
    fn with_changed(mut bytes: Vec<u8>, count: usize, rgb: [u8; 3]) -> Vec<u8> {
        for pixel in bytes.chunks_exact_mut(4).take(count) {
            pixel[..3].copy_from_slice(&rgb);
        }
        bytes
    }

    #[test]
    fn fr5_the_area_is_centred_on_the_press() {
        let area = area_around((500.0, 300.0), (1000, 800), 64).unwrap();
        assert_eq!(
            area,
            Area {
                left: 468,
                top: 268,
                right: 532,
                bottom: 332,
            }
        );
        assert_eq!(area.bytes(), 64 * 64 * 4);
    }

    #[test]
    fn fr5_the_area_stays_inside_the_frame_at_its_edges() {
        let corner = area_around((2.0, 799.0), (1000, 800), 64).unwrap();
        assert_eq!((corner.left, corner.right), (0, 64));
        assert_eq!((corner.top, corner.bottom), (736, 800));
        let small = area_around((10.0, 10.0), (40, 30), 64).unwrap();
        assert_eq!((small.width(), small.height()), (40, 30));
    }

    #[test]
    fn fr5_a_press_outside_the_frame_has_no_area() {
        assert_eq!(area_around((1000.0, 10.0), (1000, 800), 64), None);
        assert_eq!(area_around((-1.0, 10.0), (1000, 800), 64), None);
        assert_eq!(area_around((10.0, 10.0), (0, 0), 64), None);
        assert_eq!(area_around((f32::NAN, 10.0), (1000, 800), 64), None);
    }

    #[test]
    fn fr5_a_faint_ripple_is_not_a_visible_change() {
        let area = area_around((100.0, 100.0), (200, 200), 64).unwrap();
        let before = image(area, [255, 255, 255]);
        // A ripple of about 6% black over white, over the whole area.
        let ripple = image(area, [240, 240, 240]);
        assert_eq!(changed_fraction(&before, &ripple), 0.0);
        let mut watch = PressWatch::new(area, before);
        assert_eq!(watch.frame(&ripple), Verdict::Waiting);
    }

    #[test]
    fn fr5_a_pressed_state_that_covers_enough_of_the_area_is_visible() {
        let area = area_around((100.0, 100.0), (200, 200), 64).unwrap();
        let before = image(area, [255, 255, 255]);
        let pixels = (area.width() * area.height()) as usize;
        let mut watch = PressWatch::new(area, before.clone());
        // Too few pixels at first, then enough.
        let small = with_changed(before.clone(), pixels / 20, [180, 180, 180]);
        assert_eq!(watch.frame(&small), Verdict::Waiting);
        let large = with_changed(before, pixels / 5, [180, 180, 180]);
        match watch.frame(&large) {
            Verdict::Visible { frames, fraction } => {
                assert_eq!(frames, 2);
                assert!((fraction - 0.2).abs() < 0.01, "{fraction}");
            }
            other => panic!("expected a visible change, got {other:?}"),
        }
    }

    #[test]
    fn fr5_a_change_in_one_channel_counts() {
        let area = area_around((10.0, 10.0), (20, 20), 4).unwrap();
        let before = image(area, [10, 200, 10]);
        let after = image(area, [10, 200, 40]);
        assert_eq!(changed_fraction(&before, &after), 1.0);
    }

    #[test]
    fn fr5_the_watch_gives_up_after_its_frames() {
        let area = area_around((10.0, 10.0), (20, 20), 4).unwrap();
        let before = image(area, [0, 0, 0]);
        let mut watch = PressWatch::new(area, before.clone());
        let slight = with_changed(before.clone(), 1, [200, 200, 200]);
        for _ in 1..MAX_FRAMES {
            assert_eq!(watch.frame(&before), Verdict::Waiting);
        }
        assert_eq!(
            watch.frame(&slight),
            Verdict::GaveUp {
                frames: MAX_FRAMES,
                largest: 1.0 / 16.0,
            }
        );
    }

    #[test]
    fn fr5_images_of_different_areas_compare_as_changed() {
        assert_eq!(changed_fraction(&[0; 8], &[0; 4]), 1.0);
        assert_eq!(changed_fraction(&[], &[]), 0.0);
    }
}
