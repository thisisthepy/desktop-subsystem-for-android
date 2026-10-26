//! How long a resized window shows its last frame stretched before a frame of its new size
//! arrives.
//!
//! Stretching the last frame only covers the wait for the guest to draw at the new size; a
//! person should not notice it. [`ResizeLatency`] measures the wait for each window: from
//! the moment its client area took a size to the first frame of exactly that size. A drag
//! changes the size many times, so each measurement starts at the latest change. A window
//! that waits longer than [`STUCK`] is reported once, because a stretched frame left on the
//! screen is a defect, not a delay.

use std::fmt;
use std::time::{Duration, Instant};

/// A wait a person may notice. The summary counts the waits at least this long.
pub const NOTICEABLE: Duration = Duration::from_millis(100);

/// A wait after which the window counts as stuck on a stretched frame.
pub const STUCK: Duration = Duration::from_secs(2);

/// How many of the latest waits the summary covers.
const KEPT: usize = 512;

/// One window's waits for a frame of its size.
#[derive(Debug, Clone, Default)]
pub struct ResizeLatency {
    /// The size of the last frame shown; (0, 0) before the first.
    frame: (u32, u32),
    /// The client size that has no frame of its size yet, and since when.
    waiting: Option<((u32, u32), Instant)>,
    stuck_reported: bool,
    /// The latest waits, oldest first.
    samples: Vec<Duration>,
    /// Every wait measured, including those no longer kept.
    total: usize,
}

/// The waits a window measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencySummary {
    /// How many waits were measured in all.
    pub count: usize,
    /// Over the latest waits (up to 512):
    pub median: Duration,
    pub p95: Duration,
    pub max: Duration,
    /// How many of them were at least [`NOTICEABLE`].
    pub noticeable: usize,
}

impl fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} resizes: a frame of the new size came after {} ms (median), {} ms (95th \
             percentile), {} ms (longest); {} took {} ms or more",
            self.count,
            self.median.as_millis(),
            self.p95.as_millis(),
            self.max.as_millis(),
            self.noticeable,
            NOTICEABLE.as_millis()
        )
    }
}

impl ResizeLatency {
    pub fn new() -> ResizeLatency {
        ResizeLatency::default()
    }

    /// The window's client area is now `view`. Until a frame has been shown there is
    /// nothing to stretch, so nothing is measured.
    pub fn view_changed(&mut self, view: (u32, u32), now: Instant) {
        self.stuck_reported = false;
        if self.frame == (0, 0) || view == self.frame {
            self.waiting = None;
        } else {
            self.waiting = Some((view, now));
        }
    }

    /// A frame of `frame` pixels was shown. Returns how long the window waited for it, when
    /// it is the first frame of the window's size.
    pub fn frame_shown(&mut self, frame: (u32, u32), now: Instant) -> Option<Duration> {
        self.frame = frame;
        let (view, since) = self.waiting?;
        if view != frame {
            return None;
        }
        self.waiting = None;
        let waited = now.saturating_duration_since(since);
        if self.samples.len() == KEPT {
            self.samples.remove(0);
        }
        self.samples.push(waited);
        self.total += 1;
        Some(waited)
    }

    /// The size the window has waited for longer than [`STUCK`], and how long, the first
    /// time it is asked after that.
    pub fn stuck(&mut self, now: Instant) -> Option<((u32, u32), Duration)> {
        let (view, since) = self.waiting?;
        let waited = now.saturating_duration_since(since);
        if self.stuck_reported || waited < STUCK {
            return None;
        }
        self.stuck_reported = true;
        Some((view, waited))
    }

    /// How many waits have been measured.
    pub fn count(&self) -> usize {
        self.total
    }

    /// The waits so far, or `None` before the first.
    pub fn summary(&self) -> Option<LatencySummary> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let at = |fraction: f64| {
            let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
            sorted[index.min(sorted.len() - 1)]
        };
        Some(LatencySummary {
            count: self.total,
            median: at(0.5),
            p95: at(0.95),
            max: sorted[sorted.len() - 1],
            noticeable: sorted.iter().filter(|wait| **wait >= NOTICEABLE).count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn fr4_the_wait_runs_from_the_resize_to_the_first_frame_of_that_size() {
        let mut latency = ResizeLatency::new();
        let start = Instant::now();
        latency.frame_shown((400, 300), start);
        latency.view_changed((640, 480), start);
        // A frame of another size is still the old picture, stretched.
        assert_eq!(latency.frame_shown((400, 300), start + ms(10)), None);
        assert_eq!(
            latency.frame_shown((640, 480), start + ms(45)),
            Some(ms(45))
        );
        // Later frames of the same size are not waits.
        assert_eq!(latency.frame_shown((640, 480), start + ms(60)), None);
        assert_eq!(latency.count(), 1);
    }

    #[test]
    fn fr4_a_drag_measures_from_its_last_size() {
        let mut latency = ResizeLatency::new();
        let start = Instant::now();
        latency.frame_shown((400, 300), start);
        latency.view_changed((500, 300), start);
        latency.view_changed((600, 300), start + ms(30));
        assert_eq!(latency.frame_shown((500, 300), start + ms(40)), None);
        assert_eq!(
            latency.frame_shown((600, 300), start + ms(90)),
            Some(ms(60))
        );
    }

    #[test]
    fn fr4_nothing_is_measured_before_the_first_frame_or_without_a_stretch() {
        let mut latency = ResizeLatency::new();
        let start = Instant::now();
        latency.view_changed((640, 480), start);
        assert_eq!(latency.frame_shown((640, 480), start + ms(500)), None);
        latency.view_changed((640, 480), start + ms(600));
        assert_eq!(latency.frame_shown((640, 480), start + ms(700)), None);
        assert_eq!(latency.summary(), None);
    }

    #[test]
    fn fr4_a_window_left_stretched_is_reported_once() {
        let mut latency = ResizeLatency::new();
        let start = Instant::now();
        latency.frame_shown((400, 300), start);
        latency.view_changed((640, 480), start);
        assert_eq!(latency.stuck(start + STUCK / 2), None);
        assert_eq!(latency.stuck(start + STUCK), Some(((640, 480), STUCK)));
        assert_eq!(latency.stuck(start + STUCK * 2), None);
        // A new size starts a new wait.
        latency.view_changed((700, 480), start + STUCK * 2);
        assert_eq!(
            latency.stuck(start + STUCK * 4),
            Some(((700, 480), STUCK * 2))
        );
    }

    #[test]
    fn fr4_the_summary_counts_the_waits_a_person_may_notice() {
        let mut latency = ResizeLatency::new();
        let mut now = Instant::now();
        latency.frame_shown((100, 100), now);
        for (step, wait) in [20, 30, 40, 150, 25].into_iter().enumerate() {
            let size = (200 + step as u32, 100);
            latency.view_changed(size, now);
            now += ms(wait);
            latency.frame_shown(size, now);
        }
        let summary = latency.summary().unwrap();
        assert_eq!(summary.count, 5);
        assert_eq!(summary.median, ms(30));
        assert_eq!(summary.max, ms(150));
        assert_eq!(summary.p95, ms(150));
        assert_eq!(summary.noticeable, 1);
        assert!(summary.to_string().contains("5 resizes"));
    }
}
