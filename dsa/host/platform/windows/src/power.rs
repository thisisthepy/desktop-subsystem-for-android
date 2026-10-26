//! The computer's power as Windows reports it, for the guests' battery state.
//!
//! [`from_system_status`] reads the fields of Windows' `SYSTEM_POWER_STATUS`, and [`follow`]
//! reads them every few seconds and hands each change to the service's [`PowerHub`], which
//! tells every running Android.

use dsa_host_core::power::PowerState;

/// `SYSTEM_POWER_STATUS.ACLineStatus`: on AC power.
const AC_ONLINE: u8 = 1;
/// A field Windows does not know.
const UNKNOWN: u8 = 255;
/// `SYSTEM_POWER_STATUS.BatteryFlag` bits.
const BATTERY_CHARGING: u8 = 8;
const NO_BATTERY: u8 = 128;
/// `SYSTEM_POWER_STATUS.SystemStatusFlag`: battery saver is on.
const SAVER_ON: u8 = 1;

/// The power state for the fields of a `SYSTEM_POWER_STATUS`. A computer without a battery,
/// or one whose battery Windows cannot read, is a full battery on AC power.
pub fn from_system_status(
    ac_line_status: u8,
    battery_flag: u8,
    battery_life_percent: u8,
    system_status_flag: u8,
) -> PowerState {
    let power_saver = system_status_flag & SAVER_ON != 0;
    if battery_flag == UNKNOWN || battery_flag & NO_BATTERY != 0 {
        return PowerState {
            power_saver,
            ..PowerState::MAINS_ONLY
        };
    }
    let charging = battery_flag & BATTERY_CHARGING != 0;
    PowerState {
        // A charging battery means AC power even when Windows cannot say so directly.
        ac_online: ac_line_status == AC_ONLINE || (ac_line_status == UNKNOWN && charging),
        battery_present: true,
        // 255 when Windows does not know the charge.
        battery_percent: battery_life_percent.min(100),
        charging,
        power_saver,
    }
}

/// The computer's power now, or `None` when Windows cannot say.
#[cfg(windows)]
pub fn read() -> Option<PowerState> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: a plain structure Windows fills in.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: a valid pointer to that structure.
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return None;
    }
    Some(from_system_status(
        status.ACLineStatus,
        status.BatteryFlag,
        status.BatteryLifePercent,
        status.SystemStatusFlag,
    ))
}

/// How often [`follow`] reads the computer's power. Reading it is cheap, and a guest that
/// hears of a change some seconds late behaves the same.
pub const POLL: std::time::Duration = std::time::Duration::from_secs(10);

/// Reads the computer's power every [`POLL`] until `stop` is set, and hands it to `hub`,
/// which passes on only what changed.
#[cfg(windows)]
pub fn follow(
    hub: std::sync::Arc<dsa_host_core::power::PowerHub>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let step = std::time::Duration::from_millis(250);
    while !stop.load(Ordering::Relaxed) {
        if let Some(state) = read() {
            hub.update(state);
        }
        let mut waited = std::time::Duration::ZERO;
        while waited < POLL && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(step);
            waited += step;
        }
    }
}
