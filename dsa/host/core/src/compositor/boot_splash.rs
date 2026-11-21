//! What an app's window shows while Android is still starting: the DSA logo centred on the
//! splash background of the user's theme, breathing slowly like Android's own boot animation,
//! until the app's window opens and the logo fades out over it.
//!
//! Every host draws the same thing with the same timing, so the rules live here once: how
//! bright the logo is at each moment, how often a frame is needed (and when none is), how the
//! fade into the app ends, how large the logo and the window are, and the pixels of a frame.
//! A host only supplies its clock, its logo image, the window's DPI, the theme and whether the
//! user turned animations off, and puts the pixels on the screen.
//!
//! The window's visible text is gone, but what it says to a screen reader is not: the host
//! names the window with [`crate::ui_text::caption::starting_android`].

use std::time::Duration;

use super::splash::{self, Icon};

/// One breath of the logo: from full brightness down to [`DIM_OPACITY`] and back. Slow and
/// even, as Android's boot animation is, so it reads as "working" and never as an alert.
pub const PULSE_PERIOD: Duration = Duration::from_millis(2400);

/// The logo's opacity at the bottom of a breath. It never fades out completely, so the logo
/// stays recognisable at every moment.
pub const DIM_OPACITY: f32 = 0.45;

/// How long the logo takes to fade out once the app's window is there.
pub const CROSSFADE: Duration = Duration::from_millis(250);

/// The time between two frames while the logo moves: 30 a second, more than smooth for a
/// breath of 2.4 seconds, and half the work of 60.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// The waiting window's client size in DIPs (pixels at 100 %), large enough to show the logo
/// at its full 160 dp.
pub const WINDOW_DIP: (u32, u32) = (400, 320);

/// The logo's opacity, `elapsed` after the window was first shown. It starts at full
/// brightness and follows a cosine, which eases in and out at both ends of a breath. With
/// animations turned off (`reduce_motion`) the logo stands still at full brightness.
pub fn logo_opacity(elapsed: Duration, reduce_motion: bool) -> f32 {
    if reduce_motion {
        return 1.0;
    }
    let period = PULSE_PERIOD.as_secs_f64();
    let phase = (elapsed.as_secs_f64() % period) / period;
    let wave = 0.5 + 0.5 * (std::f64::consts::TAU * phase).cos();
    (f64::from(DIM_OPACITY) + (1.0 - f64::from(DIM_OPACITY)) * wave) as f32
}

/// How long until the next frame is drawn, or `None` when none is needed: the logo stands
/// still with animations off, and a window that is hidden or minimized is not drawn at all,
/// so its timer stops until it is shown again.
pub fn next_frame(reduce_motion: bool, visible: bool) -> Option<Duration> {
    (!reduce_motion && visible).then_some(FRAME_INTERVAL)
}

/// How much of the waiting window is still seen, `since` the app's window opened: from 1 to
/// 0 over [`CROSSFADE`], easing out (fast at first, gentle at the end). With animations
/// turned off it is gone at once.
pub fn crossfade_opacity(since: Duration, reduce_motion: bool) -> f32 {
    if reduce_motion || since >= CROSSFADE {
        return 0.0;
    }
    let t = since.as_secs_f32() / CROSSFADE.as_secs_f32();
    let eased = 1.0 - (1.0 - t) * (1.0 - t);
    (1.0 - eased).clamp(0.0, 1.0)
}

/// Whether the fade into the app has finished, so the waiting window can go and nothing of
/// it is drawn again.
pub fn crossfade_done(since: Duration, reduce_motion: bool) -> bool {
    crossfade_opacity(since, reduce_motion) <= 0.0
}

/// The waiting window's client size in pixels on a monitor of `dpi` dots per inch.
pub fn window_size(dpi: u32) -> (u32, u32) {
    let dpi = dpi.max(96);
    (WINDOW_DIP.0 * dpi / 96, WINDOW_DIP.1 * dpi / 96)
}

/// The edge of the logo in a window of `view` pixels at `dpi`, from an image `source` pixels
/// on its shorter side: the size the app's own splash shows its icon at, so the logo and the
/// app's icon look alike, never scaled up and never more than two fifths of the window.
pub fn logo_edge(view: (u32, u32), dpi: u32, source: u32) -> u32 {
    splash::icon_edge(view, dpi, source)
}

/// The frames of one waiting window: its background and the logo drawn on it once, at full
/// brightness, at the window's size and DPI. A frame at any opacity is then a blend of the
/// two inside the logo's square, so the logo is scaled once, not 30 times a second.
#[derive(Debug, Clone)]
pub struct BootSplash {
    view: (u32, u32),
    fill: [u8; 4],
    full: Vec<u8>,
    square: (usize, usize, usize),
}

impl BootSplash {
    /// The waiting window of `view` pixels at `dpi` with the theme's background (`dark` for
    /// the night one) and `logo` centred on it.
    pub fn new(view: (u32, u32), dark: bool, logo: Option<Icon<'_>>, dpi: u32) -> BootSplash {
        let background = splash::background(dark);
        let source = logo.map(|l| l.size.0.min(l.size.1)).unwrap_or(0);
        let edge = logo_edge(view, dpi, source);
        let full = splash::compose(view, background, logo, edge);
        let edge = (edge as usize).min(view.0 as usize).min(view.1 as usize);
        let left = (view.0 as usize - edge) / 2;
        let top = (view.1 as usize - edge) / 2;
        BootSplash {
            view,
            fill: [
                (background & 0xFF) as u8,
                ((background >> 8) & 0xFF) as u8,
                ((background >> 16) & 0xFF) as u8,
                0xFF,
            ],
            full,
            square: (left, top, if logo.is_some() { edge } else { 0 }),
        }
    }

    /// The window's size in pixels.
    pub fn view(&self) -> (u32, u32) {
        self.view
    }

    /// The frame with the logo at `opacity` (0 to 1), as rows of opaque premultiplied BGRA
    /// pixels, written into `out` (resized to fit), so a host reuses one buffer.
    pub fn frame_into(&self, opacity: f32, out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(&self.full);
        let (left, top, edge) = self.square;
        if edge == 0 {
            return;
        }
        let weight = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
        if weight >= 256 {
            return;
        }
        let width = self.view.0 as usize;
        for y in top..top + edge {
            let row = (y * width + left) * 4;
            for at in (row..row + edge * 4).step_by(4) {
                for c in 0..3 {
                    let logo = u32::from(self.full[at + c]);
                    let under = u32::from(self.fill[c]);
                    out[at + c] = ((logo * weight + under * (256 - weight) + 128) >> 8) as u8;
                }
            }
        }
    }

    /// The frame with the logo at `opacity`, in a new buffer.
    pub fn frame(&self, opacity: f32) -> Vec<u8> {
        let mut out = Vec::new();
        self.frame_into(opacity, &mut out);
        out
    }
}

/// How long the starting window waits for the app's window after Android is ready before it
/// fades out anyway: an app that never opens a window must not leave the logo on the screen.
pub const APP_WINDOW_WAIT: Duration = Duration::from_secs(20);

/// Watches the tasks for the app's first frame, for a host whose starting window lives in
/// another process than the app's window: it fades out once a task of `package` has frames,
/// which is when the app's window shows its first frame.
#[derive(Debug, Clone)]
pub struct AppWindowWait {
    package: String,
    tasks: Vec<i32>,
    shown: bool,
}

impl AppWindowWait {
    /// Starts from the tasks there are (`tasks`) and the tasks whose frames are placed
    /// (`frames`): an app that already shows a window counts as shown.
    pub fn new(
        package: &str,
        tasks: &[crate::tasks::Task],
        frames: &[(i32, u32)],
    ) -> AppWindowWait {
        let mut wait = AppWindowWait {
            package: package.to_string(),
            tasks: Vec::new(),
            shown: false,
        };
        for task in tasks {
            wait.see(&crate::tasks::TaskEvent::Updated(task.clone()));
        }
        for &(task_id, display_id) in frames {
            wait.see(&crate::tasks::TaskEvent::Frames {
                task_id,
                display_id,
            });
        }
        wait
    }

    /// Takes one task event; returns whether the app's window now shows a frame.
    pub fn see(&mut self, event: &crate::tasks::TaskEvent) -> bool {
        use crate::tasks::TaskEvent;
        match event {
            TaskEvent::Updated(task) if task.package_name == self.package => {
                if !self.tasks.contains(&task.task_id) {
                    self.tasks.push(task.task_id);
                }
            }
            TaskEvent::Removed { task_id } => self.tasks.retain(|id| id != task_id),
            TaskEvent::Frames { task_id, .. } if self.tasks.contains(task_id) => {
                self.shown = true;
            }
            _ => {}
        }
        self.shown
    }

    /// Whether the app's window shows a frame.
    pub fn shown(&self) -> bool {
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(task_id: i32, package: &str) -> crate::tasks::Task {
        crate::tasks::Task {
            task_id,
            package_name: package.into(),
            ..Default::default()
        }
    }

    #[test]
    fn fr7_boot_splash_fades_only_when_the_apps_window_shows_a_frame() {
        use crate::tasks::TaskEvent;
        let mut wait = AppWindowWait::new("com.example.notes", &[task(3, "other.app")], &[(3, 0)]);
        assert!(!wait.shown());
        // Another app's frames and the app's task without frames are not enough.
        assert!(!wait.see(&TaskEvent::Frames {
            task_id: 3,
            display_id: 1
        }));
        assert!(!wait.see(&TaskEvent::Updated(task(7, "com.example.notes"))));
        assert!(wait.see(&TaskEvent::Frames {
            task_id: 7,
            display_id: 2
        }));
        // An app that already shows a window counts as shown from the start.
        let open = AppWindowWait::new(
            "com.example.notes",
            &[task(7, "com.example.notes")],
            &[(7, 2)],
        );
        assert!(open.shown());
        // A task that ended before its frames came does not count.
        let mut gone = AppWindowWait::new("com.example.notes", &[], &[]);
        gone.see(&TaskEvent::Updated(task(9, "com.example.notes")));
        gone.see(&TaskEvent::Removed { task_id: 9 });
        assert!(!gone.see(&TaskEvent::Frames {
            task_id: 9,
            display_id: 1
        }));
        assert!(APP_WINDOW_WAIT >= Duration::from_secs(5));
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn fr7_boot_splash_logo_breathes_smoothly_between_dim_and_full() {
        assert!((logo_opacity(ms(0), false) - 1.0).abs() < 1e-6);
        assert!((logo_opacity(ms(1200), false) - DIM_OPACITY).abs() < 1e-6);
        assert!((logo_opacity(ms(2400), false) - 1.0).abs() < 1e-6);
        // Never brighter than full nor dimmer than the floor, and no jump between frames.
        let mut last = logo_opacity(ms(0), false);
        for t in (0..10_000).step_by(33) {
            let now = logo_opacity(ms(t), false);
            assert!((DIM_OPACITY - 1e-6..=1.0 + 1e-6).contains(&now));
            assert!((now - last).abs() < 0.05, "a jump at {t} ms");
            last = now;
        }
    }

    #[test]
    fn fr7_boot_splash_reduce_motion_shows_a_still_logo_and_draws_no_frames() {
        for t in [0, 500, 1200, 9999] {
            assert_eq!(logo_opacity(ms(t), true), 1.0);
        }
        assert_eq!(next_frame(true, true), None);
        assert_eq!(crossfade_opacity(ms(0), true), 0.0);
        assert!(crossfade_done(ms(0), true));
    }

    #[test]
    fn fr7_boot_splash_hidden_window_draws_no_frames() {
        assert_eq!(next_frame(false, false), None);
        assert_eq!(next_frame(false, true), Some(FRAME_INTERVAL));
    }

    #[test]
    fn fr7_boot_splash_crossfade_ends_with_nothing_left_to_draw() {
        assert_eq!(crossfade_opacity(ms(0), false), 1.0);
        let mid = crossfade_opacity(ms(125), false);
        assert!(mid > 0.0 && mid < 0.5, "eases out: {mid}");
        assert_eq!(crossfade_opacity(CROSSFADE, false), 0.0);
        assert_eq!(crossfade_opacity(ms(10_000), false), 0.0);
        assert!(!crossfade_done(ms(100), false));
        assert!(crossfade_done(CROSSFADE, false));
        let mut last = 1.0;
        for t in 0..=250 {
            let now = crossfade_opacity(ms(t), false);
            assert!(now <= last);
            last = now;
        }
    }

    #[test]
    fn fr7_boot_splash_is_drawn_at_the_windows_dpi() {
        assert_eq!(window_size(96), (400, 320));
        assert_eq!(window_size(192), (800, 640));
        assert_eq!(logo_edge(window_size(96), 96, 256), 128);
        assert_eq!(logo_edge(window_size(192), 192, 256), 256);
        assert_eq!(logo_edge(window_size(120), 120, 256), 160);
    }

    #[test]
    fn fr7_boot_splash_follows_the_theme_and_fades_the_logo_into_it() {
        let red = [0u8, 0, 0xFF, 0xFF].repeat(16 * 16);
        let logo = Icon {
            size: (16, 16),
            pixels: &red,
        };
        let at = |p: &[u8], x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            [p[i], p[i + 1], p[i + 2], p[i + 3]]
        };
        for dark in [false, true] {
            let splash = BootSplash::new((100, 100), dark, Some(logo), 96);
            let bg = splash::background(dark);
            let fill = [bg as u8, (bg >> 8) as u8, (bg >> 16) as u8, 0xFF];
            let full = splash.frame(1.0);
            assert_eq!(at(&full, 0, 0), fill);
            assert_eq!(at(&full, 50, 50), [0, 0, 0xFF, 0xFF]);
            let none = splash.frame(0.0);
            assert_eq!(at(&none, 50, 50), fill);
            let half = splash.frame(0.5);
            let centre = at(&half, 50, 50);
            assert_eq!(centre[3], 0xFF);
            // Halfway between the logo's blue (none) and the background's.
            assert_eq!(centre[0], ((u32::from(fill[0]) * 128 + 128) >> 8) as u8);
            assert!(half.chunks(4).all(|p| p[3] == 0xFF));
        }
    }

    #[test]
    fn fr7_boot_splash_without_a_logo_is_its_background() {
        let splash = BootSplash::new((10, 10), true, None, 96);
        assert!(splash
            .frame(0.5)
            .chunks(4)
            .all(|p| p == [0x24, 0x21, 0x20, 0xFF]));
    }
}
