//! Sizes on macOS: windows are measured in points, the guest in pixels, and a display's
//! backing scale (2 on a Retina display) is between them.

use dsa_host_core::compositor::frame_buffers::content_region;

/// The smallest a task window's content may be, in points, so its title bar buttons and a
/// usable part of the app always fit.
pub const MIN_CONTENT_POINTS: (f64, f64) = (240.0, 160.0);

/// The task's size in guest pixels for a window content of `points` on a display of backing
/// scale `scale`. Never 0 in either direction.
pub fn task_pixels(points: (f64, f64), scale: f64) -> (u32, u32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let axis = |points: f64| -> u32 {
        let pixels = (points.max(0.0) * scale).round();
        if pixels >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            (pixels as u32).max(1)
        }
    };
    (axis(points.0), axis(points.1))
}

/// The window content in points that shows a task of `pixels` one to one at `scale`.
pub fn window_points(pixels: (u32, u32), scale: f64) -> (f64, f64) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (f64::from(pixels.0) / scale, f64::from(pixels.1) / scale)
}

/// How one frame is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The layer's drawable size: the frame's content, in pixels.
    pub drawable: (u32, u32),
    /// The region of the frame's buffer copied, from its top left.
    pub copy: (u32, u32),
}

/// Where a frame whose buffer is `buffer` pixels and whose content is `content` pixels goes.
/// Only the content is shown, and the drawable takes exactly its size: the layer pins the
/// drawable to the window's top left without scaling it, so a frame smaller than the window
/// leaves a margin for a moment and a larger one is cut, but neither is ever stretched.
pub fn place(content: (u32, u32), buffer: (u32, u32)) -> Option<Placement> {
    let region = content_region(content, buffer);
    if region.0 == 0 || region.1 == 0 {
        return None;
    }
    Some(Placement {
        drawable: region,
        copy: region,
    })
}

/// A rectangle in points: origin at the bottom left, as AppKit measures a screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The gap between a picture-in-picture window and the screen's edges, in points.
pub const PIP_MARGIN: f64 = 16.0;

/// Where a task of `task` pixels goes in picture in picture on a screen whose usable area
/// (without the menu bar and the Dock) is `visible`: in the bottom right corner, a third of
/// the screen's width (or of its height for a tall task), keeping the task's shape.
pub fn picture_in_picture(visible: Rect, task: (u32, u32)) -> Rect {
    let (task_w, task_h) = (f64::from(task.0.max(1)), f64::from(task.1.max(1)));
    let aspect = task_w / task_h;
    let (mut width, mut height) = if aspect >= 1.0 {
        let width = visible.width / 3.0;
        (width, width / aspect)
    } else {
        let height = visible.height / 3.0;
        (height * aspect, height)
    };
    let room = (
        (visible.width - 2.0 * PIP_MARGIN).max(1.0),
        (visible.height - 2.0 * PIP_MARGIN).max(1.0),
    );
    let shrink = (room.0 / width).min(room.1 / height).min(1.0);
    width *= shrink;
    height *= shrink;
    Rect {
        x: visible.x + visible.width - PIP_MARGIN - width,
        y: visible.y + PIP_MARGIN,
        width,
        height,
    }
}

/// The layer's contents scale that shows a drawable at `frame_scale` window pixels per
/// drawable pixel on a display of backing scale `backing`: each of the task's pixels is then
/// `frame_scale` device pixels, and the layer scales the drawable from its top left. A task
/// that keeps its density on a display of another scale is shown so, the same size to the eye.
pub fn contents_scale(backing: f64, (numerator, denominator): (u32, u32)) -> f64 {
    let backing = if backing.is_finite() && backing > 0.0 {
        backing
    } else {
        1.0
    };
    if numerator == 0 || denominator == 0 {
        return backing;
    }
    backing * f64::from(denominator) / f64::from(numerator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr4_a_task_that_keeps_its_density_is_shown_at_its_windows_scale() {
        // One to one on a Retina display.
        assert_eq!(contents_scale(2.0, (1, 1)), 2.0);
        // A task at 1x on a Retina display: two device pixels a task pixel, one point.
        assert_eq!(contents_scale(2.0, (2, 1)), 1.0);
        // A task at twice the density of a display at 1x: half a device pixel a task pixel.
        assert_eq!(contents_scale(1.0, (1, 2)), 2.0);
        assert_eq!(contents_scale(f64::NAN, (0, 0)), 1.0);
    }

    #[test]
    fn fr4_a_retina_window_asks_the_guest_for_twice_its_points() {
        assert_eq!(task_pixels((640.0, 400.0), 2.0), (1280, 800));
        assert_eq!(window_points((1280, 800), 2.0), (640.0, 400.0));
    }

    #[test]
    fn fr4_a_size_rounds_to_whole_pixels_and_never_to_zero() {
        assert_eq!(task_pixels((100.3, 0.0), 1.5), (150, 1));
        assert_eq!(task_pixels((10.0, 10.0), f64::NAN), (10, 10));
    }

    #[test]
    fn fr4_a_frame_is_shown_at_its_content_size_and_never_stretched() {
        let placed = place((800, 600), (1024, 768)).unwrap();
        assert_eq!(placed.drawable, (800, 600));
        assert_eq!(placed.copy, (800, 600));
        // A sender that does not give its content size fills the whole buffer.
        assert_eq!(place((0, 0), (64, 32)).unwrap().drawable, (64, 32));
        // Content larger than its buffer is clipped to the buffer.
        assert_eq!(place((2000, 10), (64, 32)).unwrap().copy, (64, 10));
        assert_eq!(place((0, 0), (0, 0)), None);
    }

    #[test]
    fn fr3_picture_in_picture_sits_in_the_bottom_right_keeping_the_tasks_shape() {
        let screen = Rect {
            x: 0.0,
            y: 80.0,
            width: 1440.0,
            height: 820.0,
        };
        let wide = picture_in_picture(screen, (1920, 1080));
        assert!((wide.width - 480.0).abs() < 1e-9);
        assert!((wide.width / wide.height - 1920.0 / 1080.0).abs() < 1e-9);
        assert!((wide.x + wide.width - (1440.0 - PIP_MARGIN)).abs() < 1e-9);
        assert!((wide.y - (80.0 + PIP_MARGIN)).abs() < 1e-9);

        let tall = picture_in_picture(screen, (1080, 1920));
        assert!((tall.height - 820.0 / 3.0).abs() < 1e-9);
        assert!(tall.width < tall.height);
    }
}
