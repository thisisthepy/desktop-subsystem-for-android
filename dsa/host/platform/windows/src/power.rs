//! The computer's power as Windows reports it, for the guests' battery state.
//!
//! [`from_system_status`] reads the fields of Windows' `SYSTEM_POWER_STATUS`, and [`follow`]
//! reads them every few seconds and hands each change to the service's [`PowerHub`], which
//! tells every running Android. It also reads the computer's physical memory
//! ([`read_memory`]) and tells the hub when the computer runs short of it, so every Android
//! gives back what it can spare.

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

/// The computer's physical memory, total and available, in KiB, or `None` when Windows
/// cannot say.
#[cfg(windows)]
pub fn read_memory() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: a plain structure Windows fills in.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: a valid pointer to that structure, its length set as Windows requires.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return None;
    }
    Some((status.ullTotalPhys / 1024, status.ullAvailPhys / 1024))
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
    let mut pressure = dsa_host_core::power::MemoryPressure::new();
    while !stop.load(Ordering::Relaxed) {
        if let Some(state) = read() {
            hub.update(state);
        }
        if let Some((total_kb, available_kb)) = read_memory() {
            let was_low = pressure.low();
            let low = pressure.update(total_kb, available_kb);
            if low != was_low {
                eprintln!(
                    "host memory: {} MiB of {} MiB available, {}",
                    available_kb / 1024,
                    total_kb / 1024,
                    if low { "short" } else { "no longer short" }
                );
            }
            hub.update_memory(low);
        }
        let mut waited = std::time::Duration::ZERO;
        while waited < POLL && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(step);
            waited += step;
        }
    }
}

/// What a suspend or resume notification from Windows says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepEvent {
    /// The computer is about to sleep, hibernate or enter modern standby's sleep.
    Sleeping,
    /// The computer woke.
    Woke,
}

/// `PBT_APMSUSPEND`.
const PBT_APMSUSPEND: u32 = 0x4;
/// `PBT_APMRESUMESUSPEND`: woke, and a person is there.
const PBT_APMRESUMESUSPEND: u32 = 0x7;
/// `PBT_APMRESUMEAUTOMATIC`: woke, whatever woke it. Always sent on waking.
const PBT_APMRESUMEAUTOMATIC: u32 = 0x12;

/// The event a power broadcast type of a suspend and resume notification stands for, or
/// `None` for one that says neither.
pub fn sleep_event(broadcast_type: u32) -> Option<SleepEvent> {
    match broadcast_type {
        PBT_APMSUSPEND => Some(SleepEvent::Sleeping),
        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => Some(SleepEvent::Woke),
        _ => None,
    }
}

/// How long the computer's sleep waits for the guests to put their data on disk. Windows
/// gives a suspend notification only a short time.
pub const SLEEP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Hands `event` to `hub`: before the computer sleeps every Android writes its data, waited
/// for at most [`SLEEP_WAIT`]; when it wakes every Android has its clock set.
pub fn on_sleep_event(hub: &dsa_host_core::power::PowerHub, event: SleepEvent) {
    match event {
        SleepEvent::Sleeping => {
            eprintln!("power: the computer is going to sleep");
            if !hub.host_sleeping(SLEEP_WAIT) {
                eprintln!(
                    "power: an Android did not say its data was written within {} s; the \
                     computer sleeps anyway",
                    SLEEP_WAIT.as_secs()
                );
            }
        }
        SleepEvent::Woke => {
            eprintln!("power: the computer woke");
            hub.host_woke(std::time::SystemTime::now());
        }
    }
}

/// `DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS`.
#[cfg(windows)]
#[repr(C)]
struct DeviceNotifySubscribeParameters {
    callback:
        unsafe extern "system" fn(*const std::ffi::c_void, u32, *const std::ffi::c_void) -> u32,
    context: *mut std::ffi::c_void,
}

#[cfg(windows)]
#[link(name = "powrprof")]
extern "system" {
    fn PowerRegisterSuspendResumeNotification(
        flags: u32,
        recipient: *const std::ffi::c_void,
        handle: *mut *mut std::ffi::c_void,
    ) -> u32;
    fn PowerUnregisterSuspendResumeNotification(handle: *mut std::ffi::c_void) -> u32;
}

/// `DEVICE_NOTIFY_CALLBACK`.
#[cfg(windows)]
const DEVICE_NOTIFY_CALLBACK: u32 = 2;

/// Windows' suspend and resume notifications, handed to a hub for as long as this lives.
/// They come for sleep, hibernation and, on a computer with modern standby, when it goes
/// from the screen being off into its sleep.
#[cfg(windows)]
pub struct SleepWatch {
    handle: *mut std::ffi::c_void,
    /// The callback's context; freed only after the registration ends.
    context: *mut std::sync::Arc<dsa_host_core::power::PowerHub>,
    /// The parameters Windows was handed, kept for as long as the registration.
    _params: Box<DeviceNotifySubscribeParameters>,
}

// SAFETY: the handle and context are used only to end the registration, from whichever thread
// drops the watch; the hub behind the context is shared across threads already.
#[cfg(windows)]
unsafe impl Send for SleepWatch {}

#[cfg(windows)]
unsafe extern "system" fn sleep_callback(
    context: *const std::ffi::c_void,
    broadcast_type: u32,
    _setting: *const std::ffi::c_void,
) -> u32 {
    if let Some(event) = sleep_event(broadcast_type) {
        // SAFETY: the context is the hub the watch keeps alive until it ends the registration.
        let hub = unsafe { &*(context as *const std::sync::Arc<dsa_host_core::power::PowerHub>) };
        // A panic must not cross into Windows.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            on_sleep_event(hub, event)
        }));
    }
    0
}

/// Starts handing Windows' suspend and resume notifications to `hub`, until the watch is
/// dropped.
#[cfg(windows)]
pub fn watch_sleep(
    hub: std::sync::Arc<dsa_host_core::power::PowerHub>,
) -> Result<SleepWatch, String> {
    let context = Box::into_raw(Box::new(hub));
    let params = Box::new(DeviceNotifySubscribeParameters {
        callback: sleep_callback,
        context: context.cast(),
    });
    let mut handle: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: the parameters and the context outlive the registration, which the watch ends.
    let code = unsafe {
        PowerRegisterSuspendResumeNotification(
            DEVICE_NOTIFY_CALLBACK,
            (&*params as *const DeviceNotifySubscribeParameters).cast(),
            &mut handle,
        )
    };
    if code != 0 {
        // SAFETY: made above and never handed out.
        drop(unsafe { Box::from_raw(context) });
        return Err(format!(
            "registering for suspend and resume notifications: {}",
            std::io::Error::from_raw_os_error(code as i32)
        ));
    }
    Ok(SleepWatch {
        handle,
        context,
        _params: params,
    })
}

#[cfg(windows)]
impl Drop for SleepWatch {
    fn drop(&mut self) {
        // SAFETY: the registration made in watch_sleep; once it ends no callback runs, so the
        // context can be freed.
        unsafe {
            PowerUnregisterSuspendResumeNotification(self.handle);
            drop(Box::from_raw(self.context));
        }
    }
}
