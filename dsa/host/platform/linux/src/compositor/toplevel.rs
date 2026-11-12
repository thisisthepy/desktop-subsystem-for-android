//! One task window's state as the desktop sees it, with no Wayland in it.
//!
//! The compositor proposes sizes and states (`xdg_toplevel.configure`); the window turns them
//! into what the window rules want to hear (resized, dragging, focused, shown), in the guest's
//! pixels. The window never stretches a frame: it takes the size of the frame it shows, so a
//! proposed size only becomes the window's size once the task has drawn a frame of it, as on
//! Windows. Sizes on the wire are in the desktop's logical units, which the output's scale
//! turns into pixels.

use dsa_host_core::compositor::caption::{scaled, CaptionBand, CaptionLayout};
use dsa_host_core::compositor::window_mode::{ModeState, WindowMode};
use dsa_host_core::compositor::window_sizing::{to_task_length, window_size_of, BASE_DPI};
use dsa_host_core::input::WindowGeometry;

/// The title bar's height at 100%, in pixels: the height of a caption with its top border
/// on Windows 11, so the apps lay out the same on both hosts.
pub const CAPTION_AT_100: u32 = 32;

/// How deep the resize edges a window draws itself are, at 100%.
pub const RESIZE_BORDER_AT_100: u32 = 6;

/// The states `xdg_toplevel.configure` carries that the window rules use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct States {
    pub maximized: bool,
    pub fullscreen: bool,
    pub resizing: bool,
    pub activated: bool,
    /// The window is not shown at all (minimized, or on another workspace).
    pub suspended: bool,
}

/// What a configure means to the window rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reported {
    /// The user (or the desktop) gave the window this client size, in pixels.
    Resized { width: u32, height: u32 },
    /// The user started dragging the window's edge, or let go.
    Dragging(bool),
    /// The window became the active one, or stopped being it.
    Focus(bool),
    /// The window was hidden, or shown again.
    Shown(bool),
}

/// One task window.
#[derive(Debug, Clone)]
pub struct Toplevel {
    pub task_id: i32,
    pub package: String,
    pub title: String,
    /// The output scale the window is on: one logical unit is this many pixels.
    pub scale: u32,
    /// The size of the frame the window shows now, in pixels.
    pub shown: (u32, u32),
    /// The task's size in the guest, which pointer input is measured in.
    pub task: (u32, u32),
    /// How large the window shows each of the task's pixels (window pixels per task pixels):
    /// 1:1 unless the task keeps its density on a monitor of another scale.
    pub frame_scale: (u32, u32),
    pub max: Option<(u32, u32)>,
    pub min: Option<(u32, u32)>,
    pub mode: ModeState,
    pub resizable: bool,
    pub states: States,
    /// The last size the desktop proposed, in pixels.
    proposed: Option<(u32, u32)>,
    /// The title bar height last told to the guest.
    caption_told: Option<CaptionBand>,
}

impl Toplevel {
    pub fn new(task_id: i32, package: &str, title: &str, size: (u32, u32)) -> Toplevel {
        Toplevel {
            task_id,
            package: package.into(),
            title: title.into(),
            scale: 1,
            shown: size,
            task: size,
            frame_scale: (1, 1),
            max: None,
            min: None,
            mode: ModeState::new(),
            resizable: true,
            states: States::default(),
            proposed: None,
            caption_told: None,
        }
    }

    /// The window's DPI: 96 at a scale of 1.
    pub fn dpi(&self) -> u32 {
        BASE_DPI * self.scale.max(1)
    }

    /// A size in logical units as pixels.
    pub fn to_pixels(&self, (width, height): (i32, i32)) -> (u32, u32) {
        let scale = self.scale.max(1);
        (width.max(0) as u32 * scale, height.max(0) as u32 * scale)
    }

    /// A size in pixels as logical units, rounded up so no pixel is lost.
    pub fn to_logical(&self, (width, height): (u32, u32)) -> (i32, i32) {
        let scale = self.scale.max(1);
        (width.div_ceil(scale) as i32, height.div_ceil(scale) as i32)
    }

    /// A point in logical units as pixels of the window.
    pub fn point_to_pixels(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let scale = f64::from(self.scale.max(1));
        (x * scale, y * scale)
    }

    /// The sizes pointer input is converted between.
    pub fn geometry(&self) -> WindowGeometry {
        WindowGeometry {
            client: self.shown,
            task: self.task,
            scale: self.frame_scale,
        }
    }

    /// Whether the window draws its title bar: an ordinary window does; a full screen or
    /// picture-in-picture one shows only the app.
    pub fn has_caption(&self) -> bool {
        self.mode.effective() == WindowMode::Normal
    }

    /// The title bar's height in pixels; 0 when the window has none.
    pub fn caption_height(&self) -> u32 {
        if self.has_caption() {
            scaled(CAPTION_AT_100, self.dpi())
        } else {
            0
        }
    }

    /// The rows of the task's frame the title bar covers: [`Self::caption_height`] in the
    /// task's pixels, as the window shows the frame at its frame scale. A frame read back
    /// (a screenshot, the band's sample) is in the task's pixels, not the window's.
    pub fn caption_rows_in_frame(&self) -> u32 {
        to_task_length(self.caption_height(), self.frame_scale)
    }

    /// The title bar's layout over the frame shown now. A maximized or full screen window,
    /// or one the user may not resize, has no edges to drag.
    pub fn caption_layout(&self) -> CaptionLayout {
        let border = if self.states.maximized || self.states.fullscreen || !self.resizable {
            0
        } else {
            scaled(RESIZE_BORDER_AT_100, self.dpi()).min(self.caption_height())
        };
        CaptionLayout::new(self.shown.0, self.caption_height(), self.dpi(), border)
    }

    /// The title bar's band when the guest has not been told it yet: its height, which is the
    /// app's top inset, and how far the icon's cell and the caption buttons reach in from its
    /// ends, which an app drawing its own title bar keeps clear. A window with no title bar
    /// has an empty band.
    pub fn caption_change(&mut self) -> Option<CaptionBand> {
        let height = self.caption_height();
        let band = if height == 0 {
            CaptionBand::default()
        } else {
            CaptionBand {
                height,
                ..CaptionLayout::new(0, height, self.dpi(), 0).band()
            }
        };
        if self.caption_told == Some(band) {
            return None;
        }
        self.caption_told = Some(band);
        Some(band)
    }

    /// `xdg_toplevel.configure`: the desktop proposes `logical` (0 on an axis leaves it to
    /// the window) with `states`.
    pub fn configure(&mut self, logical: (i32, i32), states: States) -> Vec<Reported> {
        let mut reported = Vec::new();
        let before = self.states;
        self.states = states;
        if states.activated != before.activated {
            reported.push(Reported::Focus(states.activated));
        }
        if states.suspended != before.suspended {
            reported.push(Reported::Shown(!states.suspended));
        }
        if states.resizing && !before.resizing {
            reported.push(Reported::Dragging(true));
        }
        if logical.0 > 0 && logical.1 > 0 {
            let size = self.to_pixels(logical);
            if Some(size) != self.proposed && size != self.shown {
                reported.push(Reported::Resized {
                    width: size.0,
                    height: size.1,
                });
            }
            self.proposed = Some(size);
        }
        if !states.resizing && before.resizing {
            reported.push(Reported::Dragging(false));
        }
        reported
    }

    /// The output scale changed (`wl_surface.preferred_buffer_scale`, or the output the
    /// window entered). Returns the new DPI when it changed.
    pub fn rescale(&mut self, scale: u32) -> Option<u32> {
        let scale = scale.max(1);
        if scale == self.scale {
            return None;
        }
        self.scale = scale;
        self.proposed = None;
        Some(self.dpi())
    }

    /// The window shows a frame filling `size` of its pixels: that is its size now. A frame
    /// shown at a scale fills its own size at that scale.
    pub fn frame_shown(&mut self, size: (u32, u32)) {
        self.shown = size;
    }

    /// The pixels a frame of `frame` task pixels fills at the window's frame scale. A frame
    /// of the task size the window's size, or the size the desktop last proposed, was turned
    /// into is that size exactly, though rounding it back at the scale can land a pixel off.
    pub fn frame_in_window(&self, frame: (u32, u32)) -> (u32, u32) {
        let mut views = vec![self.shown];
        views.extend(self.proposed);
        window_size_of(frame, self.frame_scale, &views)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Toplevel {
        Toplevel::new(7, "com.example", "Example", (400, 600))
    }

    #[test]
    fn fr4_a_scaled_frame_of_the_proposed_size_fills_exactly_that_size() {
        use dsa_host_core::compositor::window_sizing::{to_task_size, to_window_size};
        let mut window = window();
        // A task kept at 130 dpi on a 200% output: 262 window pixels per 130 task pixels.
        window.frame_scale = (131, 65);
        let states = States::default();
        window.configure((1009, 700), states);
        let asked = to_task_size((1009, 700), window.frame_scale);
        assert_eq!(asked, (501, 347));
        assert_eq!(to_window_size(asked, window.frame_scale), (1010, 699));
        // The frame drawn at the size asked for fills the size proposed, not a pixel off it.
        assert_eq!(window.frame_in_window(asked), (1009, 700));
        window.frame_shown(window.frame_in_window(asked));
        assert_eq!(window.shown, (1009, 700));
        // Another frame fills its own size at the scale.
        assert_eq!(window.frame_in_window((500, 347)), (1008, 699));
    }

    #[test]
    fn fr4_a_proposed_size_is_reported_once_in_pixels() {
        let mut window = window();
        window.rescale(2);
        let states = States {
            activated: true,
            ..States::default()
        };
        assert_eq!(
            window.configure((300, 200), states),
            vec![
                Reported::Focus(true),
                Reported::Resized {
                    width: 600,
                    height: 400
                }
            ]
        );
        assert_eq!(window.configure((300, 200), states), vec![]);
        assert_eq!(
            window.configure((0, 0), states),
            vec![],
            "no size: the window decides"
        );
    }

    #[test]
    fn fr4_the_window_keeps_the_size_of_the_frame_it_shows_until_a_new_one_comes() {
        let mut window = window();
        window.configure((500, 700), States::default());
        assert_eq!(
            window.shown,
            (400, 600),
            "nothing is stretched to the new size"
        );
        window.frame_shown((500, 700));
        assert_eq!(window.geometry().client, (500, 700));
    }

    #[test]
    fn fr4_dragging_an_edge_starts_and_ends_a_live_resize() {
        let mut window = window();
        let dragging = States {
            resizing: true,
            ..States::default()
        };
        assert_eq!(
            window.configure((410, 600), dragging),
            vec![
                Reported::Dragging(true),
                Reported::Resized {
                    width: 410,
                    height: 600
                }
            ]
        );
        assert_eq!(
            window.configure((420, 600), States::default()),
            vec![
                Reported::Resized {
                    width: 420,
                    height: 600
                },
                Reported::Dragging(false)
            ]
        );
    }

    #[test]
    fn fr4_moving_to_a_scaled_output_changes_the_dpi() {
        let mut window = window();
        assert_eq!(window.dpi(), 96);
        assert_eq!(window.rescale(2), Some(192));
        assert_eq!(window.rescale(2), None);
        assert_eq!(window.to_logical((401, 600)), (201, 300));
    }

    #[test]
    fn fr10_a_minimized_window_says_it_is_hidden() {
        let mut window = window();
        let hidden = States {
            suspended: true,
            ..States::default()
        };
        assert_eq!(
            window.configure((0, 0), hidden),
            vec![Reported::Shown(false)]
        );
        assert_eq!(
            window.configure((0, 0), States::default()),
            vec![Reported::Shown(true)]
        );
    }

    #[test]
    fn fr22_the_title_bar_scales_and_goes_away_in_full_screen() {
        let mut window = window();
        assert_eq!(window.caption_height(), 32);
        assert_eq!(window.caption_change().map(|band| band.height), Some(32));
        assert_eq!(window.caption_change(), None);
        window.rescale(2);
        assert_eq!(window.caption_change().map(|band| band.height), Some(64));
        window.mode.toggle_fullscreen();
        assert_eq!(window.caption_height(), 0);
        assert_eq!(window.caption_change(), Some(CaptionBand::default()));
    }

    #[test]
    fn fr22_a_screenshot_of_a_window_at_another_scale_leaves_out_the_bands_rows_of_the_frame() {
        use dsa_host_core::compositor::window_sizing::{density_for_dpi, scale_between};
        let mut window = window();
        assert_eq!(window.caption_rows_in_frame(), 32);
        // A task kept at 100% (130 dpi) shown at 125% (164 dpi): the 32 window pixels of the
        // band cover 25 rows of the task's frame, not 32.
        window.frame_scale = scale_between(density_for_dpi(120), density_for_dpi(96));
        assert_eq!(window.frame_scale, (82, 65));
        assert_eq!(window.caption_height(), 32);
        assert_eq!(window.caption_rows_in_frame(), 25);
        // At 200% a task kept at 100% is shown at 262 per 130: 64 window pixels, 32 rows.
        window.rescale(2);
        window.frame_scale = scale_between(density_for_dpi(192), density_for_dpi(96));
        assert_eq!(window.caption_rows_in_frame(), 32);
        window.mode.toggle_fullscreen();
        assert_eq!(window.caption_rows_in_frame(), 0);
    }

    #[test]
    fn fr22_a_maximized_window_has_no_resize_edge_in_its_title_bar() {
        let mut window = window();
        assert!(window.caption_layout().resize_border > 0);
        window.configure(
            (0, 0),
            States {
                maximized: true,
                ..States::default()
            },
        );
        assert_eq!(window.caption_layout().resize_border, 0);
    }

    #[test]
    fn fr22_the_guest_hears_how_far_the_icon_cell_and_buttons_reach_into_the_band() {
        let mut window = window();
        // At 100%: the icon's cell ends 40 px in from the left (16 px padding, a 16 px icon
        // and half the padding after it), and the three 46 px buttons take 138 px at the right.
        assert_eq!(
            window.caption_change(),
            Some(CaptionBand {
                height: 32,
                start_width: 40,
                end_width: 138,
            })
        );
        // They scale with the window, and a resize changes nothing the guest is told.
        window.frame_shown((900, 700));
        assert_eq!(window.caption_change(), None);
        window.rescale(2);
        assert_eq!(
            window.caption_change(),
            Some(CaptionBand {
                height: 64,
                start_width: 80,
                end_width: 276,
            })
        );
    }
}
