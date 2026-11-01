//! When a window's presented frames actually reached the screen.
//!
//! A present only queues a frame; the screen shows it at a later vertical blank. A swapchain
//! counts its presents, and its frame statistics name the latest present that has been
//! shown and the performance counter time of the blank it was shown at. [`PresentWatch`]
//! keeps the counts of the presents not yet known to be shown, and turns each reading of the
//! statistics into the presents it shows to have reached the screen.

use std::collections::VecDeque;
use std::time::Duration;

/// The most presents kept waiting for statistics. Statistics that never come (a swapchain
/// that does not report them) must not grow the list without end.
pub const MAX_PENDING: usize = 64;

/// One reading of a swapchain's frame statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStatistics {
    /// The count of the latest present that has reached the screen.
    pub present_count: u32,
    /// The performance counter at the vertical blank it was shown at.
    pub sync_qpc: i64,
}

/// A present that reached the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShownPresent {
    /// The swapchain's count of the present.
    pub present: u32,
    /// The vertical blank, on the host clock, by which it was on the screen.
    pub vblank: Duration,
    /// Whether this present itself was shown at `vblank`. When false, a later present was,
    /// so this one was shown at an earlier blank or replaced before it was shown.
    pub exact: bool,
}

/// The presents of one swapchain not yet known to be on the screen.
#[derive(Debug, Default)]
pub struct PresentWatch {
    pending: VecDeque<u32>,
}

impl PresentWatch {
    pub fn new() -> PresentWatch {
        PresentWatch::default()
    }

    /// A present went out, with the swapchain's count `present_count`.
    pub fn presented(&mut self, present_count: u32) {
        if self
            .pending
            .back()
            .is_some_and(|last| *last >= present_count)
        {
            // A new swapchain counts from the start again: the old one's presents are gone.
            self.pending.clear();
        }
        if self.pending.len() == MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back(present_count);
    }

    /// How many presents wait for statistics.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// The presents that `statistics` show to have reached the screen, oldest first, each
    /// once. `frequency` is the performance counter's ticks a second.
    pub fn statistics(&mut self, statistics: FrameStatistics, frequency: i64) -> Vec<ShownPresent> {
        let vblank = qpc_to_duration(statistics.sync_qpc, frequency);
        let mut shown = Vec::new();
        while let Some(present) = self.pending.front().copied() {
            if present > statistics.present_count {
                break;
            }
            self.pending.pop_front();
            shown.push(ShownPresent {
                present,
                vblank,
                exact: present == statistics.present_count,
            });
        }
        shown
    }

    /// The swapchain is gone or remade: no earlier present will be reported.
    pub fn forget(&mut self) {
        self.pending.clear();
    }
}

/// A performance counter reading of `ticks` at `frequency` ticks a second, as time since the
/// counter's origin.
pub fn qpc_to_duration(ticks: i64, frequency: i64) -> Duration {
    if ticks <= 0 || frequency <= 0 {
        return Duration::ZERO;
    }
    let (ticks, frequency) = (ticks as u128, frequency as u128);
    Duration::from_nanos((ticks * 1_000_000_000 / frequency) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQUENCY: i64 = 10_000_000;

    #[test]
    fn fr5_each_present_is_reported_once_when_the_statistics_reach_it() {
        let mut watch = PresentWatch::new();
        watch.presented(1);
        watch.presented(2);
        watch.presented(3);
        let shown = watch.statistics(
            FrameStatistics {
                present_count: 2,
                sync_qpc: 20_000_000,
            },
            FREQUENCY,
        );
        assert_eq!(
            shown,
            vec![
                ShownPresent {
                    present: 1,
                    vblank: Duration::from_secs(2),
                    exact: false
                },
                ShownPresent {
                    present: 2,
                    vblank: Duration::from_secs(2),
                    exact: true
                },
            ]
        );
        // The same reading again reports nothing new.
        let again = FrameStatistics {
            present_count: 2,
            sync_qpc: 20_000_000,
        };
        assert!(watch.statistics(again, FREQUENCY).is_empty());
        let later = FrameStatistics {
            present_count: 3,
            sync_qpc: 20_166_667,
        };
        let shown = watch.statistics(later, FREQUENCY);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].present, 3);
        assert_eq!(shown[0].vblank, Duration::from_nanos(2_016_666_700));
        assert_eq!(watch.pending(), 0);
    }

    #[test]
    fn fr5_a_new_swapchain_and_missing_statistics_keep_the_watch_bounded() {
        let mut watch = PresentWatch::new();
        watch.presented(5);
        watch.presented(1);
        assert_eq!(watch.pending(), 1);
        for count in 2..200 {
            watch.presented(count);
        }
        assert_eq!(watch.pending(), MAX_PENDING);
    }

    #[test]
    fn fr5_performance_counter_ticks_become_time() {
        assert_eq!(
            qpc_to_duration(10_000_000, FREQUENCY),
            Duration::from_secs(1)
        );
        assert_eq!(
            qpc_to_duration(1_234_567, FREQUENCY),
            Duration::from_nanos(123_456_700)
        );
        assert_eq!(qpc_to_duration(5, 0), Duration::ZERO);
    }
}
