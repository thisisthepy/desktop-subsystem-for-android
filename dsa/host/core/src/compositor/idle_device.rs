//! When the window manager lets go of the graphics device its task windows are drawn with, and
//! when it makes the device ready again.
//!
//! The window manager draws every task window with one graphics device of its own: the
//! textures the renderer copies frames into, each window's swapchains, and the copies it keeps
//! of a window's last frame are all made on it. The device and its driver hold tens of MiB
//! beyond those resources (command buffers, upload pools mapped write-combined, the runtime's
//! and the driver's heap allocations), and they hold it for as long as the device lives, so a
//! window manager whose windows had all closed kept 80 to 106 MiB of private memory for days
//! where a fresh one holds 10 to 26 MiB.
//!
//! [`IdleDevice`] decides. Once no window has existed for [`DEVICE_RELEASE_DELAY`] (no task has
//! a window and no window program is connected), the device is due to be let go of
//! ([`IdleDevice::release_due`]); the platform lets go of it only when nothing made on it is
//! still alive, so a window is never left without its device. When a window appears again
//! while the device is gone, the platform is told to make it ahead of the window's first
//! frame ([`IdleDevice::windows_open`]), on a thread of its own, so the window's first frame
//! does not wait for the device any longer than the first window after a start does.
//!
//! Minimized windows still exist: their device stays, so a restore shows at once. Video
//! decoders make devices of their own and are not counted here.
//!
//! It names no window system.

use std::time::{Duration, Instant};

/// How long no window must exist before the window manager lets go of its graphics device. A
/// window closed and opened again at once (an app restarted, a window program that starts
/// again after a crash, a user who reopens what they just closed) keeps the device it has;
/// short enough that the memory is back before anyone looks for it.
pub const DEVICE_RELEASE_DELAY: Duration = Duration::from_secs(10);

/// How long after a release that could not go through (something made on the device was still
/// alive) it is tried again.
pub const DEVICE_RELEASE_RETRY: Duration = Duration::from_secs(5);

/// What the window manager does with its graphics device, as [`IdleDevice`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRelease {
    /// Nothing to do.
    Keep,
    /// Let go of the device now, if nothing holds it.
    Release,
}

/// Follows whether any task window exists, and says when the window manager's graphics device
/// goes and when it is made ready again.
#[derive(Debug, Clone)]
pub struct IdleDevice {
    delay: Duration,
    /// How many windows exist now: task windows and connected window programs.
    windows: usize,
    /// Since when no window has existed; `None` while one does.
    idle_since: Option<Instant>,
    /// The earliest moment of the next release attempt in this idle stretch.
    next_attempt: Option<Instant>,
    /// A release in this idle stretch found the device held, and said so.
    held_said: bool,
}

impl IdleDevice {
    /// Starts with no window, as at start-up, when the device is not made yet either.
    pub fn new(now: Instant) -> IdleDevice {
        IdleDevice::with_delay(DEVICE_RELEASE_DELAY, now)
    }

    /// [`IdleDevice::new`] with another delay before a release.
    pub fn with_delay(delay: Duration, now: Instant) -> IdleDevice {
        IdleDevice {
            delay,
            windows: 0,
            idle_since: Some(now),
            next_attempt: None,
            held_said: false,
        }
    }

    /// Takes in how many windows exist now (task windows, and window programs connected that
    /// may show one). Returns true when windows exist again after none did: the device is to
    /// be made ready now, ahead of the first frame, when it is not made.
    pub fn windows_open(&mut self, windows: usize, now: Instant) -> bool {
        let was_idle = self.windows == 0;
        self.windows = windows;
        if windows > 0 {
            self.idle_since = None;
            self.next_attempt = None;
            self.held_said = false;
            return was_idle;
        }
        if self.idle_since.is_none() {
            self.idle_since = Some(now);
        }
        false
    }

    /// Whether a window exists now.
    pub fn in_use(&self) -> bool {
        self.windows > 0
    }

    /// How long no window has existed; zero while one does.
    pub fn idle_for(&self, now: Instant) -> Duration {
        self.idle_since
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }

    /// Whether to let go of the device now, given whether it is `made`: once no window has
    /// existed for the delay, and again after [`DEVICE_RELEASE_RETRY`] while an attempt found
    /// it still held. A device made again while no window exists is let go of again.
    pub fn release_due(&mut self, made: bool, now: Instant) -> DeviceRelease {
        let Some(since) = self.idle_since else {
            return DeviceRelease::Keep;
        };
        if !made || now.saturating_duration_since(since) < self.delay {
            return DeviceRelease::Keep;
        }
        if self.next_attempt.is_some_and(|at| now < at) {
            return DeviceRelease::Keep;
        }
        DeviceRelease::Release
    }

    /// Takes in what a release did: `released` when the device went. When it did not
    /// (something made on it is still alive), the next attempt waits [`DEVICE_RELEASE_RETRY`].
    /// Returns true when that is to be said in the log: once per idle stretch.
    pub fn release_tried(&mut self, released: bool, now: Instant) -> bool {
        if released {
            self.next_attempt = None;
            return false;
        }
        self.next_attempt = Some(now + DEVICE_RELEASE_RETRY);
        let say = !self.held_said;
        self.held_said = true;
        say
    }
}

/// The log line for a device let go of while no window existed.
pub fn released_line(idle_for: Duration, memory: &str) -> String {
    format!(
        "no task window has existed for {} s: let go of the task windows' graphics device and \
         every texture, swapchain and frame copy made on it, and gave back the heap's free \
         memory; {memory}",
        idle_for.as_secs()
    )
}

/// The log line for a device made again for a window, after it was let go of.
pub fn remade_line(took: Duration, ahead: bool) -> String {
    let when = if ahead {
        "ahead of the window's first frame"
    } else {
        "for the window's first frame"
    };
    format!(
        "made the task windows' graphics device again {when}, after it was let go of while no \
         window existed: it took {} ms",
        took.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn nfr1_window_manager_releases_its_device_when_the_last_window_closes() {
        let start = Instant::now();
        let mut device = IdleDevice::new(start);
        assert!(device.windows_open(1, at(start, 1)));
        assert_eq!(device.release_due(true, at(start, 60)), DeviceRelease::Keep);
        // The last window closes.
        assert!(!device.windows_open(0, at(start, 100)));
        assert_eq!(
            device.release_due(true, at(start, 105)),
            DeviceRelease::Keep
        );
        assert_eq!(
            device.release_due(true, at(start, 100) + DEVICE_RELEASE_DELAY),
            DeviceRelease::Release
        );
        assert!(!device.release_tried(true, at(start, 110)));
        // Gone: nothing more to let go of.
        assert_eq!(
            device.release_due(false, at(start, 200)),
            DeviceRelease::Keep
        );
    }

    #[test]
    fn nfr1_window_manager_keeps_its_device_while_a_window_is_open_or_minimized() {
        let start = Instant::now();
        let mut device = IdleDevice::new(start);
        // A minimized window still exists: its count stays.
        device.windows_open(2, at(start, 1));
        device.windows_open(1, at(start, 2));
        for second in [10, 100, 10_000] {
            assert_eq!(
                device.release_due(true, at(start, second)),
                DeviceRelease::Keep
            );
        }
        assert!(device.in_use());
    }

    #[test]
    fn nfr1_a_window_reopened_within_the_delay_keeps_the_device() {
        let start = Instant::now();
        let mut device = IdleDevice::new(start);
        device.windows_open(1, at(start, 1));
        device.windows_open(0, at(start, 20));
        // Opened again five seconds later: the device is to be ready, which it still is, and
        // it is not let go of when the delay of the first close has passed.
        assert!(device.windows_open(1, at(start, 25)));
        assert_eq!(
            device.release_due(true, at(start, 20) + DEVICE_RELEASE_DELAY),
            DeviceRelease::Keep
        );
        // The idle stretch starts again at the next close.
        device.windows_open(0, at(start, 40));
        assert_eq!(device.release_due(true, at(start, 45)), DeviceRelease::Keep);
        assert_eq!(
            device.release_due(true, at(start, 40) + DEVICE_RELEASE_DELAY),
            DeviceRelease::Release
        );
    }

    #[test]
    fn nfr1_a_window_that_opens_after_the_release_has_its_device_made_ahead() {
        let start = Instant::now();
        let mut device = IdleDevice::new(start);
        device.windows_open(1, at(start, 1));
        device.windows_open(0, at(start, 2));
        assert_eq!(
            device.release_due(true, at(start, 30)),
            DeviceRelease::Release
        );
        device.release_tried(true, at(start, 30));
        // A window program connects: the device is to be made now, once, not at each change.
        assert!(device.windows_open(1, at(start, 40)));
        assert!(!device.windows_open(2, at(start, 40)));
        assert!(!device.windows_open(1, at(start, 41)));
    }

    #[test]
    fn nfr1_a_release_that_finds_the_device_held_is_tried_again_and_said_once() {
        let start = Instant::now();
        let mut device = IdleDevice::with_delay(Duration::from_secs(10), start);
        let due = at(start, 10);
        assert_eq!(device.release_due(true, due), DeviceRelease::Release);
        assert!(device.release_tried(false, due), "said the first time");
        assert_eq!(device.release_due(true, at(start, 11)), DeviceRelease::Keep);
        let again = due + DEVICE_RELEASE_RETRY;
        assert_eq!(device.release_due(true, again), DeviceRelease::Release);
        assert!(
            !device.release_tried(false, again),
            "said once per idle stretch"
        );
        assert_eq!(
            device.release_due(true, again + DEVICE_RELEASE_RETRY),
            DeviceRelease::Release
        );
        assert!(!device.release_tried(true, again + DEVICE_RELEASE_RETRY));
    }

    #[test]
    fn nfr1_a_device_made_while_no_window_exists_is_let_go_of_again() {
        let start = Instant::now();
        let mut device = IdleDevice::new(start);
        assert_eq!(
            device.release_due(false, at(start, 60)),
            DeviceRelease::Keep
        );
        // Something made it (a frame for a window that has gone since): it goes again.
        assert_eq!(
            device.release_due(true, at(start, 61)),
            DeviceRelease::Release
        );
    }

    #[test]
    fn nfr1_the_release_and_the_remake_are_logged_for_measuring() {
        let line = released_line(Duration::from_secs(10), "private 101 -> 24 MiB");
        assert!(line.starts_with("no task window has existed for 10 s: let go of"));
        assert!(line.ends_with("; private 101 -> 24 MiB"));
        assert_eq!(
            remade_line(Duration::from_millis(42), true),
            "made the task windows' graphics device again ahead of the window's first frame, \
             after it was let go of while no window existed: it took 42 ms"
        );
        assert!(!line.contains('\u{2014}'));
    }
}
