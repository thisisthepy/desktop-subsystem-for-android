//! The guest display's refresh rate on macOS: the Mac's fastest display, capped at 60 Hz in
//! Low Power Mode.
//!
//! The agent runs in the user's login session, so it reads the displays' current modes from
//! Core Graphics itself, every [`dsa_host_core::guest_refresh::MONITOR_POLL`] ([`follow`]),
//! and hands the fastest to the user's session. Low Power Mode comes with the Mac's power
//! ([`crate::power`]), and with it the session's rule
//! ([`dsa_host_core::guest_refresh::refresh_rate`]) sets the rate the guest's display
//! refreshes at.

/// A display mode's refresh rate as Core Graphics gives it, in whole hertz. Core Graphics
/// gives 0 for a display whose rate it does not name, which is no known rate.
pub fn hz_from_mode_rate(rate: f64) -> u32 {
    if !rate.is_finite() || rate < 1.0 {
        return 0;
    }
    rate.round().min(f64::from(u32::MAX)) as u32
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::Weak;

    use dsa_host_core::guest_refresh::{fastest, follow_monitors, MONITOR_POLL};
    use dsa_host_core::session::Session;

    use super::hz_from_mode_rate;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
        fn CGDisplayModeGetRefreshRate(mode: *mut c_void) -> f64;
        fn CGDisplayModeRelease(mode: *mut c_void);
    }

    /// More displays than any Mac drives.
    const MAX_DISPLAYS: usize = 32;

    /// The refresh rate of the Mac's fastest display, in hertz, or `None` when Core Graphics
    /// names no display's rate.
    pub fn fastest_display_hz() -> Option<u32> {
        let mut displays = [0u32; MAX_DISPLAYS];
        let mut count = 0u32;
        // SAFETY: the buffer holds MAX_DISPLAYS IDs and the count is a valid pointer.
        let error = unsafe {
            CGGetActiveDisplayList(MAX_DISPLAYS as u32, displays.as_mut_ptr(), &mut count)
        };
        if error != 0 {
            return None;
        }
        let rates = displays[..(count as usize).min(MAX_DISPLAYS)]
            .iter()
            .map(|&display| {
                // SAFETY: a display ID Core Graphics just listed; the copied mode is released.
                unsafe {
                    let mode = CGDisplayCopyDisplayMode(display);
                    if mode.is_null() {
                        return 0;
                    }
                    let rate = CGDisplayModeGetRefreshRate(mode);
                    CGDisplayModeRelease(mode);
                    hz_from_mode_rate(rate)
                }
            });
        fastest(rates)
    }

    /// Hands the fastest display's rate to `session` until it is gone. Run it on a thread of
    /// its own.
    pub fn follow(session: Weak<Session>) {
        follow_monitors(session, fastest_display_hz, MONITOR_POLL);
    }
}

#[cfg(target_os = "macos")]
pub use mac::{fastest_display_hz, follow};

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::guest_refresh::refresh_rate;

    #[test]
    fn fr10_a_display_mode_rate_is_whole_hertz_and_zero_is_unknown() {
        assert_eq!(hz_from_mode_rate(120.0), 120);
        assert_eq!(hz_from_mode_rate(59.94), 60);
        assert_eq!(hz_from_mode_rate(0.0), 0);
        assert_eq!(hz_from_mode_rate(f64::NAN), 0);
    }

    #[test]
    fn fr10_battery_saver_caps_the_guest_refresh_rate_in_low_power_mode() {
        use crate::power::{from_power_sources, SAVER_POLL};
        let promotion = hz_from_mode_rate(120.0);
        let low_power = from_power_sources(None, &[], true).power_saver;
        assert_eq!(refresh_rate(promotion, low_power), 60);
        let normal = from_power_sources(None, &[], false).power_saver;
        assert_eq!(refresh_rate(promotion, normal), 120);
        assert_eq!(refresh_rate(hz_from_mode_rate(60.0), low_power), 60);
        assert!(SAVER_POLL < std::time::Duration::from_secs(1));
    }
}
