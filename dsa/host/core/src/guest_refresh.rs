//! How fast the guest's display refreshes: as fast as the user's fastest monitor, and no
//! faster than 60 Hz while the host's battery saver is on.
//!
//! Each host reads its own battery saver (Windows' energy saver, macOS' Low Power Mode, the
//! Linux power-saver profile) and its monitors, and this one rule turns them into the rate the
//! guest is told ([`refresh_rate`]). The session sends it as `DisplayRefresh` when it changes
//! and each time the agent connects, and the agent holds the guest's display at it, so the
//! change takes effect within a frame or two, without restarting Android or touching a window.
//! A battery saver that saves nothing on a 60 Hz monitor changes nothing there.
//!
//! While the user drags a task window to a new size, the guest refreshes at no more than
//! [`LIVE_RESIZE_CAP_HZ`] ([`LiveResizes`]), and goes back to the monitor's rate when the drag
//! ends. Every frame of a resize costs the app a layout, a draw and a round trip to the host,
//! so at 144 or 240 Hz the app spends the drag catching up with frames nobody sees, and each
//! new size waits behind them. A 60 Hz monitor sees no change.
//!
//! The guest's display offers a fixed set of rates ([`GUEST_RATES_HZ`]): the guest's
//! hwcomposer (`dsa/guest/hwcomposer/src/refresh_rates.h`) lists the same ones, one display
//! configuration each. A monitor's rate is lowered to the nearest of them at or below it, so a
//! 59.94 Hz monitor, which Windows reports as 59 Hz, is 60 and an 85 Hz one is 75.

use std::collections::HashMap;
use std::sync::Weak;
use std::time::{Duration, Instant};

use crate::session::Session;

/// The refresh rates the guest's display offers, slowest first. The hwcomposer offers the
/// same list.
pub const GUEST_RATES_HZ: [u32; 8] = [60, 75, 90, 100, 120, 144, 165, 240];

/// The fastest the guest's display refreshes while the host's battery saver is on.
pub const SAVER_CAP_HZ: u32 = 60;

/// How far below one of [`GUEST_RATES_HZ`] a monitor's reported rate may be and still count as
/// that rate: Windows rounds 59.94 Hz down to 59 and 119.88 Hz to 119.
const ROUNDING_HZ: u32 = 1;

/// The guest rate a monitor that refreshes at `monitor_hz` shows best: the fastest of
/// [`GUEST_RATES_HZ`] at or below it, and the slowest for a monitor slower than every one.
pub fn guest_rate_for(monitor_hz: u32) -> u32 {
    GUEST_RATES_HZ
        .iter()
        .copied()
        .filter(|rate| *rate <= monitor_hz.saturating_add(ROUNDING_HZ))
        .max()
        .unwrap_or(GUEST_RATES_HZ[0])
}

/// The rule: the rate the guest's display runs at for a fastest monitor of `monitor_hz`, with
/// the host's battery saver on or off. The saver caps it at [`SAVER_CAP_HZ`]; a monitor at
/// that rate or slower is the same either way.
pub fn refresh_rate(monitor_hz: u32, battery_saver: bool) -> u32 {
    let rate = guest_rate_for(monitor_hz);
    if battery_saver {
        rate.min(SAVER_CAP_HZ)
    } else {
        rate
    }
}

/// The fastest the guest's display refreshes while a task window is being resized live.
pub const LIVE_RESIZE_CAP_HZ: u32 = 60;

/// How long a task stays in a live resize after its last live step, when no step that ends
/// it comes: a window program that died in the middle of a drag never sends one. A drag that
/// holds still longer than this lets the rate go back up until the next step.
pub const LIVE_RESIZE_EXPIRY: Duration = Duration::from_secs(5);

/// [`refresh_rate`], held at [`LIVE_RESIZE_CAP_HZ`] or below while `resizing`.
pub fn refresh_rate_while(monitor_hz: u32, battery_saver: bool, resizing: bool) -> u32 {
    let rate = refresh_rate(monitor_hz, battery_saver);
    if resizing {
        rate.min(LIVE_RESIZE_CAP_HZ)
    } else {
        rate
    }
}

/// The tasks whose windows are being resized live: a resize step marked live starts or
/// continues one, the step that is not live (the end of the drag, which reconfigures the task)
/// ends it, and one that hears nothing for [`LIVE_RESIZE_EXPIRY`] ends by itself. Several
/// windows can be dragged at once; the guest is held at the lower rate until the last ends.
#[derive(Debug, Default)]
pub struct LiveResizes {
    last_step: HashMap<i32, Instant>,
}

impl LiveResizes {
    /// A resize step of `task_id`, live or not, at `now`. Returns whether any task is in a
    /// live resize changed.
    pub fn step(&mut self, task_id: i32, live: bool, now: Instant) -> bool {
        let before = self.any();
        if live {
            self.last_step.insert(task_id, now);
        } else {
            self.last_step.remove(&task_id);
        }
        before != self.any()
    }

    /// Ends every live resize whose last step is [`LIVE_RESIZE_EXPIRY`] or more before `now`.
    /// Returns whether any task is in a live resize changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.any();
        self.last_step
            .retain(|_, step| now.saturating_duration_since(*step) < LIVE_RESIZE_EXPIRY);
        before != self.any()
    }

    /// Ends every live resize, as when the guest starts again and has no task left.
    pub fn clear(&mut self) {
        self.last_step.clear();
    }

    /// Whether any task window is being resized live.
    pub fn any(&self) -> bool {
        !self.last_step.is_empty()
    }
}

/// The fastest of `rates`, in hertz, ignoring the zeros a platform gives for a rate it does
/// not know. `None` when none is known.
pub fn fastest(rates: impl IntoIterator<Item = u32>) -> Option<u32> {
    rates.into_iter().filter(|hz| *hz > 0).max()
}

/// The most a recorded rate can be; anything above is not a monitor's.
const MAX_RECORDED_HZ: u32 = 1000;

/// The text a user's session records its fastest monitor's rate in, where the service, which
/// cannot see the user's monitors, reads it.
pub fn record_text(hz: u32) -> String {
    format!("{hz}\n")
}

/// The rate in a record, or `None` when it holds no plausible rate. The user's own session
/// writes it, so anything else is ignored.
pub fn parse_record(text: &str) -> Option<u32> {
    let hz: u32 = text.lines().next()?.trim().parse().ok()?;
    (1..=MAX_RECORDED_HZ).contains(&hz).then_some(hz)
}

/// How often [`follow_monitors`] reads the monitors. A monitor's rate changes only when the
/// user changes it or plugs in another, so a few seconds late is soon enough; the battery
/// saver, which needs to be quick, comes with the host's power instead.
pub const MONITOR_POLL: Duration = Duration::from_secs(2);

/// Reads the user's fastest monitor with `read` every `every`, and hands each rate to the
/// session, which tells the guest only what changed. A read that knows no monitor leaves the
/// last rate. Returns once the session is gone; run it on a thread of its own.
pub fn follow_monitors(
    session: Weak<Session>,
    mut read: impl FnMut() -> Option<u32>,
    every: Duration,
) {
    loop {
        let rate = read();
        let Some(session) = session.upgrade() else {
            return;
        };
        if let Some(hz) = rate {
            session.set_monitor_refresh(hz);
        }
        drop(session);
        std::thread::sleep(every);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr10_battery_saver_caps_the_guest_refresh_rate() {
        // A 120 Hz monitor: 60 with the saver on, back to 120 when it is off.
        assert_eq!(refresh_rate(120, true), 60);
        assert_eq!(refresh_rate(120, false), 120);
        // A 60 Hz monitor does not change.
        assert_eq!(refresh_rate(60, true), 60);
        assert_eq!(refresh_rate(60, false), 60);
        // Nor does a slower one, which the guest shows at its slowest rate either way.
        assert_eq!(refresh_rate(50, true), 60);
        assert_eq!(refresh_rate(50, false), 60);
        assert_eq!(refresh_rate(240, true), 60);
        assert_eq!(refresh_rate(240, false), 240);
    }

    #[test]
    fn fr10_a_monitor_rate_is_lowered_to_a_rate_the_guest_offers() {
        assert_eq!(guest_rate_for(59), 60);
        assert_eq!(guest_rate_for(119), 120);
        assert_eq!(guest_rate_for(143), 144);
        assert_eq!(guest_rate_for(85), 75);
        assert_eq!(guest_rate_for(360), 240);
        assert_eq!(guest_rate_for(0), 60);
        for rate in GUEST_RATES_HZ {
            assert_eq!(guest_rate_for(rate), rate);
        }
    }

    #[test]
    fn fr10_the_fastest_known_monitor_counts() {
        assert_eq!(fastest([60, 144, 0]), Some(144));
        assert_eq!(fastest([0, 0]), None);
        assert_eq!(fastest([]), None);
    }

    #[test]
    fn fr10_a_live_resize_holds_the_guest_at_60_hz_and_a_60_hz_monitor_is_unchanged() {
        assert_eq!(refresh_rate_while(144, false, true), 60);
        assert_eq!(refresh_rate_while(144, false, false), 144);
        assert_eq!(refresh_rate_while(240, true, false), 60);
        assert_eq!(refresh_rate_while(60, false, true), 60);
        assert_eq!(refresh_rate_while(60, false, false), 60);
    }

    #[test]
    fn fr10_live_resizes_end_with_the_last_window_and_expire_when_no_end_comes() {
        let start = Instant::now();
        let mut resizes = LiveResizes::default();
        assert!(!resizes.any());
        // One window, then a second while the first is still dragged.
        assert!(resizes.step(1, true, start));
        assert!(!resizes.step(1, true, start + Duration::from_millis(100)));
        assert!(!resizes.step(2, true, start + Duration::from_millis(200)));
        // The first ends: the second still holds the rate down.
        assert!(!resizes.step(1, false, start + Duration::from_millis(300)));
        assert!(resizes.any());
        assert!(resizes.step(2, false, start + Duration::from_millis(400)));
        assert!(!resizes.any());
        // An end that was never preceded by a live step changes nothing.
        assert!(!resizes.step(3, false, start));
        // A window that never ends its drag lets go after the expiry, not before.
        assert!(resizes.step(4, true, start));
        assert!(!resizes.expire(start + LIVE_RESIZE_EXPIRY - Duration::from_millis(1)));
        assert!(resizes.any());
        assert!(resizes.expire(start + LIVE_RESIZE_EXPIRY));
        assert!(!resizes.any());
    }

    #[test]
    fn fr10_a_recorded_monitor_rate_reads_back_and_junk_is_no_rate() {
        assert_eq!(parse_record(&record_text(144)), Some(144));
        assert_eq!(parse_record(" 120 \n"), Some(120));
        assert_eq!(parse_record(""), None);
        assert_eq!(parse_record("fast\n"), None);
        assert_eq!(parse_record("0\n"), None);
        assert_eq!(parse_record("99999\n"), None);
    }
}
