//! The DSA logo of the Linux host (its app icon in the hicolor theme,
//! `io.github.thisisthepy.DSA.svg`), drawn at any size for the window shown while Android
//! starts. The icon is a few rounded rectangles, circles and two strokes, so it is drawn from
//! their shapes at the exact pixel size the window's scale asks for: sharp at every scale, with
//! no image decoder and no SVG library in the process.

/// One shape of the icon, in its 64 by 64 units.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// A rounded rectangle: left, top, width, height, corner radius.
    Rounded(f32, f32, f32, f32, f32),
    /// A rounded rectangle cut off below `y`: the title bar, whose lower corners are square.
    RoundedAbove(f32, f32, f32, f32, f32, f32),
    /// A circle: centre and radius.
    Circle(f32, f32, f32),
    /// A stroke with round caps: from, to, width.
    Stroke(f32, f32, f32, f32, f32),
}

/// The colour of a shape, `0xRRGGBB`, and its opacity.
type Paint = (u32, f32);

/// The icon's shapes, back to front. The window and its parts are in the icon's group,
/// `translate(32 31) scale(.66) translate(-32 -32)`, applied by [`group`].
fn shapes() -> [(Shape, Paint, bool); 9] {
    [
        (
            Shape::Rounded(5.0, 9.0, 54.0, 51.0, 11.0),
            (0x08402f, 1.0),
            false,
        ),
        (
            Shape::Rounded(5.0, 5.0, 54.0, 51.0, 11.0),
            (0x2fd37c, 1.0),
            false,
        ),
        (
            Shape::Stroke(22.0, 14.0, 16.5, 5.0, 4.4),
            (0xffffff, 1.0),
            true,
        ),
        (
            Shape::Stroke(42.0, 14.0, 47.5, 5.0, 4.4),
            (0xffffff, 1.0),
            true,
        ),
        (
            Shape::Rounded(4.0, 12.0, 56.0, 46.0, 9.0),
            (0x0c5a43, 1.0),
            true,
        ),
        (
            Shape::RoundedAbove(4.0, 12.0, 56.0, 46.0, 9.0, 25.0),
            (0xffffff, 1.0),
            true,
        ),
        (Shape::Circle(40.0, 18.5, 2.3), (0x0c5a43, 1.0), true),
        (Shape::Circle(48.0, 18.5, 2.3), (0x0c5a43, 1.0), true),
        (
            Shape::Rounded(12.0, 32.0, 40.0, 18.0, 4.0),
            (0xffffff, 0.3),
            true,
        ),
    ]
}

/// The group's transform: icon units of the window into icon units of the whole.
const GROUP_SCALE: f32 = 0.66;

#[cfg(test)]
fn group(point: (f32, f32)) -> (f32, f32) {
    (
        32.0 + GROUP_SCALE * (point.0 - 32.0),
        31.0 + GROUP_SCALE * (point.1 - 32.0),
    )
}

/// The signed distance from `p` to a rounded rectangle centred at `c` with half size `h`.
fn rounded_distance(p: (f32, f32), c: (f32, f32), h: (f32, f32), r: f32) -> f32 {
    let r = r.min(h.0).min(h.1);
    let q = ((p.0 - c.0).abs() - h.0 + r, (p.1 - c.1).abs() - h.1 + r);
    let outside = (q.0.max(0.0).powi(2) + q.1.max(0.0).powi(2)).sqrt();
    outside + q.0.max(q.1).min(0.0) - r
}

/// The signed distance from `p` to `shape`, in icon units of the shape's own space.
fn distance(shape: Shape, p: (f32, f32)) -> f32 {
    match shape {
        Shape::Rounded(x, y, w, h, r) => {
            rounded_distance(p, (x + w / 2.0, y + h / 2.0), (w / 2.0, h / 2.0), r)
        }
        Shape::RoundedAbove(x, y, w, h, r, cut) => {
            let rounded = rounded_distance(p, (x + w / 2.0, y + h / 2.0), (w / 2.0, h / 2.0), r);
            rounded.max(p.1 - cut)
        }
        Shape::Circle(cx, cy, r) => ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt() - r,
        Shape::Stroke(x0, y0, x1, y1, width) => {
            let (dx, dy) = (x1 - x0, y1 - y0);
            let length = dx * dx + dy * dy;
            let t = if length > 0.0 {
                (((p.0 - x0) * dx + (p.1 - y0) * dy) / length).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (nx, ny) = (x0 + t * dx, y0 + t * dy);
            ((p.0 - nx).powi(2) + (p.1 - ny).powi(2)).sqrt() - width / 2.0
        }
    }
}

/// The logo, `edge` pixels square, as rows of premultiplied BGRA pixels with its transparent
/// surround.
pub fn draw(edge: u32) -> Vec<u8> {
    let edge_px = edge as usize;
    let mut pixels = vec![0u8; edge_px * edge_px * 4];
    if edge == 0 {
        return pixels;
    }
    let unit = 64.0 / edge as f32;
    for y in 0..edge_px {
        for x in 0..edge_px {
            let p = ((x as f32 + 0.5) * unit, (y as f32 + 0.5) * unit);
            let at = (y * edge_px + x) * 4;
            let mut out = [0f32; 4];
            for (shape, (colour, opacity), grouped) in shapes() {
                if opacity <= 0.0 {
                    continue;
                }
                // Distances in the group are in its own units, which are smaller on screen.
                let (point, scale) = if grouped {
                    let inverse = (
                        32.0 + (p.0 - 32.0) / GROUP_SCALE,
                        32.0 + (p.1 - 31.0) / GROUP_SCALE,
                    );
                    (inverse, GROUP_SCALE)
                } else {
                    (p, 1.0)
                };
                let pixels_away = distance(shape, point) * scale / unit;
                let coverage = (0.5 - pixels_away).clamp(0.0, 1.0) * opacity;
                if coverage <= 0.0 {
                    continue;
                }
                let channel = |shift: u32| ((colour >> shift) & 0xFF) as f32 / 255.0;
                let source = [channel(0), channel(8), channel(16)];
                for c in 0..3 {
                    out[c] = source[c] * coverage + out[c] * (1.0 - coverage);
                }
                out[3] = coverage + out[3] * (1.0 - coverage);
            }
            for c in 0..4 {
                pixels[at + c] = (out[c] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(pixels: &[u8], edge: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * edge + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    }

    #[test]
    fn fr7_boot_splash_linux_logo_is_drawn_at_any_size() {
        for edge in [16, 64, 128, 256, 333] {
            let pixels = draw(edge);
            assert_eq!(pixels.len(), (edge * edge * 4) as usize);
            // The corners are outside the plate.
            assert_eq!(at(&pixels, edge, 0, 0)[3], 0);
            // Premultiplied: no channel above its alpha.
            assert!(pixels
                .chunks(4)
                .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        }
        // The window's dark body in the middle, the title bar white above it.
        let pixels = draw(64);
        assert_eq!(at(&pixels, 64, 32, 44), [0x43, 0x5a, 0x0c, 0xFF]);
        assert_eq!(at(&pixels, 64, 24, 18), [0xFF, 0xFF, 0xFF, 0xFF]);
        // The green plate beside the window.
        assert_eq!(at(&pixels, 64, 7, 30), [0x7c, 0xd3, 0x2f, 0xFF]);
        assert_eq!(group((32.0, 32.0)), (32.0, 31.0));
        assert!(draw(0).is_empty());
    }
}
