//! The mouse, the trackpad and the pen on macOS, as the window input the platform-neutral
//! pointer mapper takes.

use dsa_host_core::input::MouseButton;

/// A view's point (in points, from its bottom left, as AppKit gives it) as a client pixel
/// from the top left, which is what the task's frames and the guest measure from.
pub fn client_pixel(point: (f64, f64), view_height: f64, scale: f64) -> (f64, f64) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (point.0 * scale, (view_height - point.1) * scale)
}

/// The mouse button an `NSEvent`'s button number names. A click with Control held is the
/// secondary click on a Mac, and reaches the guest as the right button.
pub fn button(number: isize, control_held: bool) -> Option<MouseButton> {
    Some(match number {
        0 if control_held => MouseButton::Right,
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        3 => MouseButton::Back,
        4 => MouseButton::Forward,
        _ => return None,
    })
}

/// How many points of a trackpad's precise scroll make one wheel notch: a notch scrolls about
/// three lines of text.
pub const POINTS_PER_NOTCH: f32 = 40.0;

/// A scroll event's deltas as wheel notches (horizontal, vertical; positive is right and away
/// from the user). `precise` is a trackpad or Magic Mouse, whose deltas are in points; a
/// wheel's are in lines, one a notch. The deltas already follow the user's natural scrolling
/// setting, so the guest scrolls the way every Mac app does. macOS counts a horizontal delta
/// positive toward the left, the guest toward the right.
pub fn scroll_notches(delta: (f64, f64), precise: bool) -> (f32, f32) {
    let (x, y) = (delta.0 as f32, delta.1 as f32);
    let (x, y) = if precise {
        (x / POINTS_PER_NOTCH, y / POINTS_PER_NOTCH)
    } else {
        (x, y)
    };
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    (finite(-x), finite(y))
}

/// `NSEventSubtypeTabletPoint`: a mouse event that a pen on a graphics tablet made.
pub const SUBTYPE_TABLET_POINT: i16 = 1;

/// A pen's pressure as the guest takes it: from 0 to 1, `None` when unknown.
pub fn pen_pressure(pressure: f32) -> Option<f32> {
    pressure.is_finite().then(|| pressure.clamp(0.0, 1.0))
}

/// A pen's tilt in degrees, (toward the right, toward the user), from AppKit's scaled tilt:
/// -1 to 1 on each axis, where 1 lies flat, and y points up the screen.
pub fn pen_tilt((x, y): (f64, f64)) -> (f32, f32) {
    let degrees = |value: f64| {
        if value.is_finite() {
            (value.clamp(-1.0, 1.0) * 90.0) as f32
        } else {
            0.0
        }
    };
    (degrees(x), -degrees(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr5_a_pens_tilt_is_in_degrees_toward_the_right_and_the_user() {
        assert_eq!(pen_tilt((0.0, 0.0)), (0.0, 0.0));
        assert_eq!(pen_tilt((0.5, 0.0)), (45.0, 0.0));
        // AppKit's y points up the screen; the guest's points down, toward the user.
        assert_eq!(pen_tilt((0.0, 1.0)), (0.0, -90.0));
        assert_eq!(pen_tilt((2.0, f64::NAN)), (90.0, 0.0));
    }

    #[test]
    fn fr5_a_point_is_measured_from_the_top_left_in_pixels() {
        assert_eq!(client_pixel((10.0, 290.0), 300.0, 2.0), (20.0, 20.0));
        assert_eq!(client_pixel((0.0, 0.0), 300.0, 1.0), (0.0, 300.0));
    }

    #[test]
    fn fr5_control_click_is_the_secondary_click() {
        assert_eq!(button(0, false), Some(MouseButton::Left));
        assert_eq!(button(0, true), Some(MouseButton::Right));
        assert_eq!(button(1, false), Some(MouseButton::Right));
        assert_eq!(button(3, false), Some(MouseButton::Back));
        assert_eq!(button(4, false), Some(MouseButton::Forward));
        assert_eq!(button(9, false), None);
    }

    #[test]
    fn fr5_a_trackpad_scrolls_smoothly_and_a_wheel_by_notches() {
        assert_eq!(scroll_notches((0.0, 1.0), false), (0.0, 1.0));
        assert_eq!(scroll_notches((0.0, 20.0), true), (0.0, 0.5));
        assert_eq!(scroll_notches((40.0, 0.0), true), (-1.0, 0.0));
        assert_eq!(scroll_notches((f64::NAN, 1.0), false), (0.0, 1.0));
    }

    #[test]
    fn fr5_a_pens_pressure_stays_between_zero_and_one() {
        assert_eq!(pen_pressure(0.5), Some(0.5));
        assert_eq!(pen_pressure(1.5), Some(1.0));
        assert_eq!(pen_pressure(f32::NAN), None);
    }
}
