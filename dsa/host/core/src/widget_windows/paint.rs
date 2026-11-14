//! The pictures of a widget window, drawn on the CPU as premultiplied BGRA rows (the layout of
//! a Wayland `ARGB8888` buffer, an X11 32-bit image and a little-endian `CGImage` with its
//! alpha first): the widget's image at its own size, never scaled, and a widget being edited,
//! its image raised a little over a soft shadow with a border and its handles around it.
//!
//! The Windows host draws the same parts with its Fluent canvas; the Linux and macOS hosts draw
//! them here, in their desktop's colors ([`Look`]).

use super::geometry::{
    edit_layout, edit_margin, px_of_dip, unscaled_at, EDIT_BORDER_DIP, EDIT_RADIUS_DIP, LIFT_DIP,
};

/// A color as `0xAARRGGBB`, its alpha not premultiplied.
pub type Argb = u32;

/// An image: `width` by `height` premultiplied BGRA pixels, rows from the top.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    /// An image of straight RGBA pixels (as PNG decodes them), premultiplied into BGRA.
    pub fn from_rgba(width: u32, height: u32, rgba: &[u8]) -> Image {
        let mut pixels = Vec::with_capacity(rgba.len());
        for pixel in rgba.chunks_exact(4) {
            let alpha = u32::from(pixel[3]);
            let times = |channel: u8| ((u32::from(channel) * alpha + 127) / 255) as u8;
            pixels.extend_from_slice(&[
                times(pixel[2]),
                times(pixel[1]),
                times(pixel[0]),
                pixel[3],
            ]);
        }
        Image {
            width,
            height,
            pixels,
        }
    }
}

/// The colors of a widget being edited, in the desktop's own style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// The border and the handles' rims of a widget being edited: the desktop's accent.
    pub accent: Argb,
    /// The inside of the handles.
    pub handle: Argb,
    /// The shadow under the lifted widget, at its darkest.
    pub shadow: Argb,
}

impl Look {
    /// The look on a light or a dark desktop, with `accent` its accent color.
    pub fn new(dark: bool, accent: Argb) -> Look {
        if dark {
            Look {
                accent,
                handle: 0xFF2D_2D2D,
                shadow: 0x5C00_0000,
            }
        } else {
            Look {
                accent,
                handle: 0xFFFF_FFFF,
                shadow: 0x3800_0000,
            }
        }
    }
}

/// A canvas over premultiplied BGRA rows.
pub struct Canvas<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a mut [u8],
}

/// How far the point (`px`, `py`) is outside the rectangle at `x`, `y` of `w` by `h` with its
/// corners rounded to `r`: negative inside.
fn rounded_distance(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let qx = (px - cx).abs() - (w / 2.0 - r);
    let qy = (py - cy).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

impl Canvas<'_> {
    /// Lays `color` over the pixel at `x`, `y`, covering `coverage` (0 to 1) of it.
    fn blend(&mut self, x: usize, y: usize, color: Argb, coverage: f32) {
        let alpha = (color >> 24) as f32 / 255.0 * coverage.clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let at = (y * self.width + x) * 4;
        let Some(pixel) = self.pixels.get_mut(at..at + 4) else {
            return;
        };
        let channel = |shift: u32| ((color >> shift) & 0xff) as f32 * alpha;
        let top = [channel(0), channel(8), channel(16), alpha * 255.0];
        let keep = 1.0 - alpha;
        for (out, over) in pixel.iter_mut().zip(top) {
            *out = (f32::from(*out) * keep + over).round().clamp(0.0, 255.0) as u8;
        }
    }

    /// The pixels a shape at `x`, `y` of `w` by `h`, grown by `grow`, may touch.
    fn span(&self, x: f32, y: f32, w: f32, h: f32, grow: f32) -> (usize, usize, usize, usize) {
        let left = (x - grow).floor().max(0.0) as usize;
        let top = (y - grow).floor().max(0.0) as usize;
        let right = ((x + w + grow).ceil().max(0.0) as usize).min(self.width);
        let bottom = ((y + h + grow).ceil().max(0.0) as usize).min(self.height);
        (left, top, right, bottom)
    }

    /// Fills the rectangle at `x`, `y` of `w` by `h` with its corners rounded to `radius`,
    /// its edge smoothed over a pixel.
    pub fn fill_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Argb) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (left, top, right, bottom) = self.span(x, y, w, h, 1.0);
        for py in top..bottom {
            for px in left..right {
                let d = rounded_distance(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, radius);
                let coverage = (0.5 - d).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend(px, py, color, coverage);
                }
            }
        }
    }

    /// Strokes the edge of the rectangle `rect` (left, top, width and height) with its corners
    /// rounded to `radius`, `thickness` wide, inside it.
    pub fn stroke_rounded(
        &mut self,
        rect: (f32, f32, f32, f32),
        radius: f32,
        thickness: f32,
        color: Argb,
    ) {
        let (x, y, w, h) = rect;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (left, top, right, bottom) = self.span(x, y, w, h, 1.0);
        let (ix, iy, iw, ih) = (
            x + thickness,
            y + thickness,
            w - 2.0 * thickness,
            h - 2.0 * thickness,
        );
        let inner_radius = (radius - thickness).max(0.0);
        for py in top..bottom {
            for px in left..right {
                let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                let outer = (0.5 - rounded_distance(cx, cy, x, y, w, h, radius)).clamp(0.0, 1.0);
                let inner = if iw > 0.0 && ih > 0.0 {
                    (0.5 - rounded_distance(cx, cy, ix, iy, iw, ih, inner_radius)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let coverage = (outer - inner).max(0.0);
                if coverage > 0.0 {
                    self.blend(px, py, color, coverage);
                }
            }
        }
    }

    /// Lays `image` over the canvas with its top left corner at `x`, `y`.
    pub fn draw_image(&mut self, image: &Image, x: i32, y: i32) {
        let (iw, ih) = (image.width as usize, image.height as usize);
        for row in 0..ih {
            let ty = y + row as i32;
            if ty < 0 || ty as usize >= self.height {
                continue;
            }
            for column in 0..iw {
                let tx = x + column as i32;
                if tx < 0 || tx as usize >= self.width {
                    continue;
                }
                let from = (row * iw + column) * 4;
                let Some(source) = image.pixels.get(from..from + 4) else {
                    continue;
                };
                let to = (ty as usize * self.width + tx as usize) * 4;
                let keep = 255 - u32::from(source[3]);
                for (out, over) in self.pixels[to..to + 4].iter_mut().zip(source) {
                    let under = u32::from(*out) * keep / 255;
                    *out = (u32::from(*over) + under).min(255) as u8;
                }
            }
        }
    }
}

/// The picture of a widget being edited, `width` by `height` premultiplied BGRA pixels on a
/// monitor of `dpi`: a soft shadow under the lifted widget, `lifted` (the picture of its
/// lifted rectangle) over it, a border in the accent color and a handle on each edge `resize_mode` lets
/// it be resized along. Every pixel of the widget stays just visible enough to be clicked;
/// the room around it is clear but for the shadow.
pub fn draw_edit(
    lifted: &Image,
    width: usize,
    height: usize,
    dpi: u32,
    resize_mode: u32,
    look: &Look,
) -> Vec<u8> {
    let layout = edit_layout(width as i32, height as i32, dpi, resize_mode);
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = Canvas {
        width,
        height,
        pixels: &mut pixels,
    };
    let (lx, ly, lw, lh) = layout.lifted;
    let radius = px_of_dip(EDIT_RADIUS_DIP, dpi) as f32;
    // The shadow: layers that spread from the widget into the room around it, each adding a
    // little, so it is darkest under the widget and fades out, a little below it as a raised
    // surface's is.
    if look.shadow >> 24 != 0 {
        const LAYERS: i32 = 6;
        let room = (edit_margin(dpi) - px_of_dip(LIFT_DIP, dpi)).max(1) as f32;
        let drop = px_of_dip(2, dpi) as f32;
        let alpha = ((look.shadow >> 24) as f32 / LAYERS as f32)
            .round()
            .max(1.0) as u32;
        let layer = (alpha.min(255) << 24) | (look.shadow & 0x00FF_FFFF);
        for step in 1..=LAYERS {
            let spread = room * step as f32 / LAYERS as f32;
            canvas.fill_rounded(
                lx as f32 - spread,
                ly as f32 - spread + drop,
                lw as f32 + 2.0 * spread,
                lh as f32 + 2.0 * spread,
                radius + spread,
                layer,
            );
        }
    }
    canvas.draw_image(lifted, lx, ly);
    // A widget's own clear pixels stay clickable.
    for row in ly.max(0)..(ly + lh).min(height as i32) {
        for column in lx.max(0)..(lx + lw).min(width as i32) {
            let at = (row as usize * width + column as usize) * 4 + 3;
            if canvas.pixels[at] == 0 {
                canvas.pixels[at] = 1;
            }
        }
    }
    let border = px_of_dip(EDIT_BORDER_DIP, dpi).max(1) as f32;
    let (x, y, w, h) = (lx as f32, ly as f32, lw as f32, lh as f32);
    canvas.stroke_rounded((x, y, w, h), radius, border, look.accent);
    let rim = (px_of_dip(3, dpi) as f32 / 2.0).max(1.0);
    for (_, handle) in &layout.handles {
        let (r, d) = (handle.radius, handle.radius * 2.0);
        let (left, top) = (handle.cx - r, handle.cy - r);
        canvas.fill_rounded(left, top, d, d, r, look.handle);
        canvas.stroke_rounded((left, top, d, d), r, rim, look.accent);
    }
    pixels
}

/// The picture of a widget lifted for editing in a window of `width` by `height` pixels on a
/// monitor of `dpi`: its `image` (as Android drew it) at its own size, raised a little over a
/// soft shadow, with the accent border and the handles around it ([`draw_edit`]). The image is
/// never scaled to the lifted size.
pub fn draw_lifted(
    image: &Image,
    width: usize,
    height: usize,
    dpi: u32,
    resize_mode: u32,
    look: &Look,
) -> Vec<u8> {
    let layout = edit_layout(width as i32, height as i32, dpi, resize_mode);
    let (lx, ly, lw, lh) = layout.lifted;
    let (ix, iy, _, _) = layout.image;
    let room = (lw.max(1) as usize, lh.max(1) as usize);
    let at = ((ix - lx).max(0) as usize, (iy - ly).max(0) as usize);
    let from = (image.width as usize, image.height as usize);
    let lifted = Image {
        width: room.0 as u32,
        height: room.1 as u32,
        pixels: unscaled_at(&image.pixels, from, room, at),
    };
    draw_edit(&lifted, width, height, dpi, resize_mode, look)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::{RESIZE_HORIZONTAL, RESIZE_VERTICAL};

    fn alpha(pixels: &[u8], width: usize, x: usize, y: usize) -> u8 {
        pixels[(y * width + x) * 4 + 3]
    }

    #[test]
    fn fr12_a_picture_from_android_is_premultiplied() {
        // Half see-through red, straight RGBA, becomes premultiplied BGRA.
        let image = Image::from_rgba(1, 1, &[255, 0, 0, 128]);
        assert_eq!(image.pixels, vec![0, 0, 128, 128]);
    }

    #[test]
    fn fr12_a_widget_being_edited_is_drawn_lifted_over_a_shadow_with_its_border_and_handles() {
        let dpi = 96;
        let look = Look::new(false, 0xFF35_84E4);
        let margin = edit_margin(dpi) as usize;
        let (width, height) = (180 + 2 * margin, 110 + 2 * margin);
        let layout = edit_layout(width as i32, height as i32, dpi, RESIZE_HORIZONTAL);
        let (_, _, lw, lh) = layout.lifted;
        let lifted = Image {
            width: lw as u32,
            height: lh as u32,
            pixels: vec![0; (lw * lh * 4) as usize],
        };
        let pixels = draw_edit(&lifted, width, height, dpi, RESIZE_HORIZONTAL, &look);
        // The corners of the room are clear, the shadow is under the widget, the widget's own
        // clear pixels can still be clicked, and the left handle is drawn in the accent.
        assert_eq!(alpha(&pixels, width, 0, 0), 0);
        assert!(alpha(&pixels, width, width / 2, height - 3) > 0);
        assert!(alpha(&pixels, width, width / 2, height / 2) >= 1);
        let (hit, handle) = layout.handles[0];
        assert_eq!(hit, super::super::geometry::Hit::Left);
        let at = (handle.cy as usize * width + handle.cx as usize) * 4;
        assert_eq!(pixels[at + 3], 255);
        // Only the edges it resizes along have handles.
        assert_eq!(layout.handles.len(), 2);
        let both = edit_layout(
            width as i32,
            height as i32,
            dpi,
            RESIZE_HORIZONTAL | RESIZE_VERTICAL,
        );
        assert_eq!(both.handles.len(), 4);
    }

    #[test]
    fn fr12_a_lifted_widget_keeps_its_image_at_its_own_size() {
        // An opaque widget of 180 x 110 pixels, lifted: its image is raised by the lift, not
        // stretched to the lifted rectangle, so the lifted rectangle's bottom rows show only
        // the shadow's room and the border, never the widget's pixels.
        let dpi = 96;
        let look = Look::new(true, 0xFF35_84E4);
        let margin = edit_margin(dpi) as usize;
        let (width, height) = (180 + 2 * margin, 110 + 2 * margin);
        let image = Image {
            width: 180,
            height: 110,
            pixels: [10, 20, 30, 255].repeat(180 * 110),
        };
        let pixels = draw_lifted(&image, width, height, dpi, 0, &look);
        let layout = edit_layout(width as i32, height as i32, dpi, 0);
        let (ix, iy, iw, ih) = layout.image;
        assert_eq!((iw, ih), (180, 110), "the image keeps its own size");
        let at = |x: i32, y: i32| (y as usize * width + x as usize) * 4;
        // The middle of the image is the widget's own color.
        let middle = at(ix + iw / 2, iy + ih / 2);
        assert_eq!(&pixels[middle..middle + 4], &[10, 20, 30, 255]);
        // Below the raised image, inside the lifted rectangle and the border, is not the
        // widget's color: the image was not stretched to fill it.
        let border = px_of_dip(EDIT_BORDER_DIP, dpi);
        let (_, ly, _, lh) = layout.lifted;
        let below = iy + ih;
        assert!(below < ly + lh - border, "room under the raised image");
        let under = at(ix + iw / 2, below);
        assert_ne!(&pixels[under..under + 4], &[10, 20, 30, 255]);
    }
}
