//! What a widget window is on a Linux desktop, with no Wayland or X11 in it: which kind of
//! surface it can be, and how the desktop's units, buttons and keys come to the widget desk's
//! (`dsa_host_core::widget_windows::desk`).
//!
//! A widget belongs to the desktop, below every other window. A Wayland toplevel can neither
//! place itself nor stay below the others, so a widget window is, in order:
//!
//! 1. a layer surface (wlr-layer-shell, which KDE Plasma, COSMIC and the wlroots desktops
//!    have) on the bottom layer, above the wallpaper and below every window, anchored to the
//!    top left corner of its output with its place as margins;
//! 2. else an X11 window through XWayland (GNOME), of the desktop type (as the desktop icons'
//!    window is) and kept below, on every workspace, in neither the dash nor the window
//!    switcher, placed where the widget is, which the X11 window managers honor;
//! 3. else an ordinary borderless toplevel, which the desktop places where it likes and stacks
//!    with the other windows. It is moved with the desktop's own move, and the log says that
//!    this desktop cannot keep it on the desktop.
//!
//! Sizes follow the monitor's scale as everywhere else: the desk measures in pixels at 96 x the
//! output's scale, so a widget of 180 dp is 108 logical pixels wide at any scale.

use dsa_host_core::widget_windows::desk::{Button, Key};
use dsa_host_core::widget_windows::paint::{Image, Look};
use dsa_host_core::widget_windows::Hit;

/// The kind of surface the widget windows are on this desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetShell {
    /// A layer surface on the bottom layer.
    Layer,
    /// An X11 window of the desktop type, through XWayland.
    X11,
    /// An ordinary toplevel.
    Toplevel,
}

/// The widget windows' kind on a desktop that has a layer shell or not, and an X server
/// (XWayland) or not.
pub fn widget_shell(layer_shell: bool, x11: bool) -> WidgetShell {
    if layer_shell {
        WidgetShell::Layer
    } else if x11 {
        WidgetShell::X11
    } else {
        WidgetShell::Toplevel
    }
}

/// What the log says once about the widgets on a desktop of `shell`: nothing where they sit on
/// the desktop as they should, and what the user loses where they cannot.
pub fn shell_note(shell: WidgetShell) -> Option<&'static str> {
    match shell {
        WidgetShell::Layer => None,
        WidgetShell::X11 => Some(
            "widgets: this desktop has no layer shell, so each widget is an X11 desktop window \
             through XWayland: kept below the other windows and on every workspace, but drawn \
             at a scale of 100% and enlarged by the desktop on a monitor of a higher scale",
        ),
        WidgetShell::Toplevel => Some(
            "widgets: this desktop has neither a layer shell nor an X server, so each widget is \
             an ordinary window: the desktop places it where it likes and may stack it above \
             other windows, and it is moved with the desktop's own move",
        ),
    }
}

/// The density of an output of `scale`: 96 at a scale of 1.
pub fn dpi_of_scale(scale: u32) -> u32 {
    96 * scale.max(1)
}

/// `px` of the desk's pixels at `scale` in logical pixels, rounded down, for a place.
pub fn logical_place(px: i32, scale: u32) -> i32 {
    px.div_euclid(scale.max(1) as i32)
}

/// `px` of the desk's pixels at `scale` in logical pixels, rounded up, for a size, at least 1.
pub fn logical_size(px: i32, scale: u32) -> i32 {
    (px.max(1) as u32).div_ceil(scale.max(1)) as i32
}

/// A point on a layer surface, `surface` logical pixels from its corner, in the pixels of the
/// desk's window. The desktop gives the point from where it had put the surface when the
/// point was taken (`applied`, the surface's corner in logical pixels), which lags behind the
/// place the desk has moved the window to (`desk_origin`, in its pixels) while a move is on
/// its way; the point is taken from the place it was measured from, so a drag never runs away.
pub fn desk_local(
    applied: (i32, i32),
    surface: (f64, f64),
    scale: u32,
    desk_origin: (i32, i32),
) -> (i32, i32) {
    let scale = scale.max(1) as i32;
    let x = applied.0 * scale + (surface.0 * f64::from(scale)).floor() as i32;
    let y = applied.1 * scale + (surface.1 * f64::from(scale)).floor() as i32;
    (x - desk_origin.0, y - desk_origin.1)
}

/// The picture of a window as the buffer of a surface at `scale` without a viewport: as large
/// as a whole number of logical pixels, its extra rows and columns clear.
pub fn padded(image: &Image, scale: u32) -> Image {
    let scale = scale.max(1);
    let (width, height) = (
        image.width.div_ceil(scale) * scale,
        image.height.div_ceil(scale) * scale,
    );
    if (width, height) == (image.width, image.height) {
        return image.clone();
    }
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let row = image.width as usize * 4;
    for y in 0..image.height as usize {
        let from = y * row;
        let to = y * width as usize * 4;
        if let Some(source) = image.pixels.get(from..from + row) {
            pixels[to..to + row].copy_from_slice(source);
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

/// The evdev codes of the buttons a widget answers.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;

/// The button an evdev button code is to a widget: the left one presses as a finger does,
/// the right one opens its menu.
pub fn button(code: u32) -> Option<Button> {
    match code {
        BTN_LEFT => Some(Button::Primary),
        BTN_RIGHT => Some(Button::Secondary),
        _ => None,
    }
}

/// The X11 buttons a widget answers: 1 is the left, 3 the right.
pub fn x11_button(detail: u8) -> Option<Button> {
    match detail {
        1 => Some(Button::Primary),
        3 => Some(Button::Secondary),
        _ => None,
    }
}

/// The key an evdev key code is to a widget window: Escape ends an edit; Enter, Space and the
/// menu key open its menu.
pub fn key(code: u32) -> Option<Key> {
    const ESC: u32 = 1;
    const ENTER: u32 = 28;
    const SPACE: u32 = 57;
    const KP_ENTER: u32 = 96;
    const COMPOSE: u32 = 127;
    match code {
        ESC => Some(Key::Escape),
        ENTER | SPACE | KP_ENTER | COMPOSE => Some(Key::Menu),
        _ => None,
    }
}

/// The accent the desktop has when the portal names none: GNOME's blue.
pub const DEFAULT_ACCENT: u32 = 0xFF35_84E4;

/// The widget windows' look on a dark or light desktop with `accent` (0 for none).
pub fn look(dark: bool, accent: u32) -> Look {
    let accent = if accent >> 24 == 0 {
        DEFAULT_ACCENT
    } else {
        accent
    };
    Look::new(dark, accent)
}

/// What the cursor shows over a part of a widget window, by its `cursor-shape-v1` name: the
/// arrows of an edge or a handle it is resized by, the arrow elsewhere.
pub fn cursor_name(hit: Hit) -> &'static str {
    match hit {
        Hit::Left => "w-resize",
        Hit::Right => "e-resize",
        Hit::Top => "n-resize",
        Hit::Bottom => "s-resize",
        Hit::TopLeft => "nw-resize",
        Hit::TopRight => "ne-resize",
        Hit::BottomLeft => "sw-resize",
        Hit::BottomRight => "se-resize",
        Hit::Client | Hit::Outside => "default",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr12_a_widget_sits_on_the_desktop_layer_wherever_the_desktop_allows() {
        assert_eq!(widget_shell(true, true), WidgetShell::Layer);
        assert_eq!(widget_shell(false, true), WidgetShell::X11);
        assert_eq!(widget_shell(false, false), WidgetShell::Toplevel);
        assert_eq!(shell_note(WidgetShell::Layer), None);
        // Where a widget cannot be kept on the desktop, the log says what the user loses.
        assert!(shell_note(WidgetShell::X11).unwrap().contains("XWayland"));
        assert!(shell_note(WidgetShell::Toplevel)
            .unwrap()
            .contains("above other windows"));
    }

    #[test]
    fn fr12_a_widgets_size_follows_its_outputs_scale_as_96_dpi_times_the_scale() {
        assert_eq!(dpi_of_scale(1), 96);
        assert_eq!(dpi_of_scale(2), 192);
        assert_eq!(dpi_of_scale(0), 96);
        // Android draws its desktop at 131 dpi times the scale, rounded to an even density: at
        // a scale of 2 that is 262, so 180 x 110 dp is 295 x 180 pixels, 148 x 90 logical
        // pixels.
        let px = dsa_host_core::widget_windows::window_size(
            &dsa_host_core::widgets::Placement {
                x: 0,
                y: 0,
                width_dp: 180,
                height_dp: 110,
                resized: false,
            },
            dpi_of_scale(2),
        );
        assert_eq!(dsa_host_core::widget_windows::android_density(192), 262);
        assert_eq!(px, (295, 180));
        assert_eq!((logical_size(px.0, 2), logical_size(px.1, 2)), (148, 90));
        assert_eq!(logical_size(215, 2), 108);
        assert_eq!(logical_place(-3, 2), -2);
        assert_eq!(logical_place(64, 2), 32);
    }

    #[test]
    fn fr12_a_drag_is_measured_from_where_the_desktop_had_the_widget() {
        // The desk moved the window to 140 x 100 pixels; the desktop still has it at 60 x 50
        // logical pixels (120 x 100) when it gives the pointer at 10.5 x 4 on it.
        assert_eq!(desk_local((60, 50), (10.5, 4.0), 2, (140, 100)), (1, 8));
        // Once it is where the desk put it, the point is on it as given.
        assert_eq!(desk_local((70, 50), (10.5, 4.0), 2, (140, 100)), (21, 8));
    }

    #[test]
    fn fr12_a_picture_is_padded_to_whole_logical_pixels() {
        let image = Image {
            width: 3,
            height: 1,
            pixels: vec![9; 12],
        };
        let out = padded(&image, 2);
        assert_eq!((out.width, out.height), (4, 2));
        assert_eq!(&out.pixels[0..12], &[9; 12]);
        assert!(out.pixels[12..].iter().all(|&b| b == 0));
        assert_eq!(padded(&image, 1), image);
    }

    #[test]
    fn fr12_buttons_and_keys_of_a_widget_window() {
        assert_eq!(button(BTN_LEFT), Some(Button::Primary));
        assert_eq!(button(BTN_RIGHT), Some(Button::Secondary));
        assert_eq!(button(0x112), None);
        assert_eq!(x11_button(1), Some(Button::Primary));
        assert_eq!(x11_button(3), Some(Button::Secondary));
        assert_eq!(x11_button(4), None);
        assert_eq!(key(1), Some(Key::Escape));
        assert_eq!(key(28), Some(Key::Menu));
        assert_eq!(key(127), Some(Key::Menu));
        assert_eq!(key(30), None);
        assert_eq!(cursor_name(Hit::BottomRight), "se-resize");
        assert_eq!(cursor_name(Hit::Client), "default");
        assert_eq!(look(true, 0).accent, DEFAULT_ACCENT);
        assert_eq!(look(false, 0xFFAA_0000).accent, 0xFFAA_0000);
    }
}
