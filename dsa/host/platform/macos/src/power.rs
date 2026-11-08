//! The Mac's power as the user's Android sees it: AC or battery, the charge, Low Power Mode,
//! the Mac's sleep and wake, and the Mac running short of memory.
//!
//! The agent runs in the user's login session and reads it all itself, on one thread of its
//! own ([`watch`]) with its own run loop:
//!
//! - The battery comes from IOKit's power sources (`IOPSCopyPowerSourcesInfo`), read again
//!   whenever IOKit says they changed (`IOPSNotificationCreateRunLoopSource`) and every
//!   [`POLL`] in case a change goes unannounced. [`from_power_sources`] turns them into the
//!   state every host hands its guests.
//! - Low Power Mode (`NSProcessInfo.isLowPowerModeEnabled`) is the guest's battery saver. It
//!   is read every [`SAVER_POLL`], because it caps the guest display's refresh rate, which
//!   has to follow within a second.
//! - The sleep comes from IOKit's root power domain (`IORegisterForSystemPower`). Before the
//!   Mac sleeps every Android is told and puts its data on disk, and the agent holds the sleep
//!   until they answer or [`dsa_host_core::power::SLEEP_WAIT`] passes, then lets it go on
//!   (`IOAllowPowerChange`). When the Mac wakes every Android has its clock set.
//!
//! - The memory comes from the kernel's memory pressure level
//!   (`kern.memorystatus_vm_pressure_level`), read every [`SAVER_POLL`]. While macOS says
//!   memory is under pressure ([`memory_low_at`]), every Android is told the Mac is short of
//!   memory and gives back what it can spare, cached app processes first.
//!
//! Each reading goes to the agent's [`dsa_host_core::power::PowerHub`], which passes on only
//! what changed, the same as on the other hosts.

use std::time::Duration;

use dsa_host_core::power::{PowerState, SleepEvent};

/// How often [`watch`] reads Low Power Mode: often enough that the guest's display follows
/// within a second. Reading it is a property of the process's info.
pub const SAVER_POLL: Duration = Duration::from_millis(500);

/// How often [`watch`] reads the power sources without IOKit saying they changed.
pub const POLL: Duration = Duration::from_secs(10);

/// `kIOPSInternalBatteryType`: a battery inside the Mac, as opposed to a UPS.
pub const INTERNAL_BATTERY: &str = "InternalBattery";
/// `kIOPSACPowerValue`: the Mac runs on AC power.
pub const AC_POWER: &str = "AC Power";

/// One power source's description, the fields of IOKit's dictionary the guest needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PowerSource {
    /// `Type`: `InternalBattery` for the Mac's own battery, `UPS` for one plugged in.
    pub kind: String,
    /// `Is Present`.
    pub present: bool,
    /// `Current Capacity`, out of `max_capacity`. On a Mac both are in percent.
    pub current_capacity: i64,
    /// `Max Capacity`.
    pub max_capacity: i64,
    /// `Is Charging`.
    pub charging: bool,
    /// `Is Charged`: full and no longer charging.
    pub charged: bool,
    /// `Power Source State`: `AC Power` while the source is plugged in, `Battery Power` while
    /// it powers the Mac.
    pub state: String,
}

/// The power state for the Mac's power sources. `providing` is the type of the source the Mac
/// draws from now (`IOPSGetProvidingPowerSourceType`), when IOKit names one. A Mac without an
/// internal battery (a Mac mini, a Mac Studio, an iMac) is a full battery on AC power.
pub fn from_power_sources(
    providing: Option<&str>,
    sources: &[PowerSource],
    low_power_mode: bool,
) -> PowerState {
    let Some(battery) = sources
        .iter()
        .find(|source| source.present && source.kind == INTERNAL_BATTERY)
    else {
        return PowerState {
            power_saver: low_power_mode,
            ..PowerState::MAINS_ONLY
        };
    };
    let percent = if battery.max_capacity > 0 {
        (battery.current_capacity.max(0) * 100 + battery.max_capacity / 2) / battery.max_capacity
    } else {
        100
    };
    let ac_online = match providing {
        Some(kind) => kind == AC_POWER,
        None => battery.state == AC_POWER,
    };
    PowerState {
        ac_online,
        battery_present: true,
        battery_percent: if battery.charged {
            100
        } else {
            percent.clamp(0, 100) as u8
        },
        charging: battery.charging && !battery.charged,
        power_saver: low_power_mode,
    }
}

/// `kIOMessageCanSystemSleep`: the Mac asks whether it may sleep because it is idle.
pub const CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
/// `kIOMessageSystemWillSleep`: the Mac is about to sleep.
pub const SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
/// `kIOMessageSystemWillNotSleep`: a sleep it asked about does not happen.
pub const SYSTEM_WILL_NOT_SLEEP: u32 = 0xe000_0290;
/// `kIOMessageSystemHasPoweredOn`: the Mac woke and its devices are back.
pub const SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;
/// `kIOMessageSystemWillPowerOn`: the Mac starts to wake; its devices are not back yet.
pub const SYSTEM_WILL_POWER_ON: u32 = 0xe000_0320;

/// The sleep event a message from the root power domain stands for, or `None` for one that
/// says neither. The Mac is awake again only once its devices are back, so the guest's clock
/// is set then.
pub fn sleep_event(message_type: u32) -> Option<SleepEvent> {
    match message_type {
        SYSTEM_WILL_SLEEP => Some(SleepEvent::Sleeping),
        SYSTEM_HAS_POWERED_ON => Some(SleepEvent::Woke),
        _ => None,
    }
}

/// Whether the Mac waits for an answer (`IOAllowPowerChange`) to a message from the root
/// power domain. It waits up to 30 seconds for each, so every one is answered: an idle sleep
/// is allowed at once, and a sleep once the guests have put their data on disk.
pub fn needs_answer(message_type: u32) -> bool {
    matches!(message_type, CAN_SYSTEM_SLEEP | SYSTEM_WILL_SLEEP)
}

/// `kern.memorystatus_vm_pressure_level` (and `DISPATCH_MEMORYPRESSURE_*`): memory is fine.
pub const PRESSURE_NORMAL: i32 = 1;
/// The system is under memory pressure and asks processes to free what they can.
pub const PRESSURE_WARN: i32 = 2;
/// The system is critically short of memory.
pub const PRESSURE_CRITICAL: i32 = 4;

/// Whether the Mac is short of memory at the kernel's pressure `level`, as the guests are
/// told: short at warn and critical, no longer short once the level is normal again. The
/// kernel already keeps a margin between its levels, so no threshold is added here. A level
/// it does not name leaves `was_low` as it was.
pub fn memory_low_at(level: i32, was_low: bool) -> bool {
    match level {
        PRESSURE_WARN | PRESSURE_CRITICAL => true,
        PRESSURE_NORMAL => false,
        _ => was_low,
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::Cell;
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    use dsa_host_core::power::{PowerHub, PowerState};

    use super::{
        from_power_sources, memory_low_at, needs_answer, sleep_event, PowerSource, POLL, SAVER_POLL,
    };

    type CFTypeRef = *const c_void;
    type IONotificationPortRef = *mut c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopDefaultMode: CFTypeRef;
        fn CFRelease(object: CFTypeRef);
        fn CFGetTypeID(object: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFStringCreateWithCString(
            allocator: CFTypeRef,
            text: *const c_char,
            encoding: u32,
        ) -> CFTypeRef;
        fn CFStringGetCString(
            string: CFTypeRef,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFRunLoopGetCurrent() -> CFTypeRef;
        fn CFRunLoopAddSource(run_loop: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
        fn CFRunLoopRemoveSource(run_loop: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
        fn CFRunLoopRunInMode(mode: CFTypeRef, seconds: f64, return_after_source: u8) -> i32;
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFTypeRef;
        fn IOPSGetPowerSourceDescription(blob: CFTypeRef, source: CFTypeRef) -> CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(blob: CFTypeRef) -> CFTypeRef;
        fn IOPSNotificationCreateRunLoopSource(
            callback: extern "C" fn(*mut c_void),
            context: *mut c_void,
        ) -> CFTypeRef;
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            port: *mut IONotificationPortRef,
            callback: extern "C" fn(*mut c_void, u32, u32, *mut c_void),
            notifier: *mut u32,
        ) -> u32;
        fn IODeregisterForSystemPower(notifier: *mut u32) -> i32;
        fn IONotificationPortGetRunLoopSource(port: IONotificationPortRef) -> CFTypeRef;
        fn IONotificationPortDestroy(port: IONotificationPortRef);
        fn IOAllowPowerChange(root: u32, notification: isize) -> i32;
        fn IOServiceClose(connect: u32) -> i32;
    }

    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;
    /// `kCFNumberSInt64Type`.
    const SINT64: isize = 4;
    /// `kCFRunLoopRunFinished`: the run loop has no source to wait on.
    const RUN_FINISHED: i32 = 1;
    /// `kCFRunLoopRunHandledSource`: a source fired.
    const RUN_HANDLED_SOURCE: i32 = 4;

    /// The value under `key` in a Core Foundation dictionary, not retained.
    ///
    /// # Safety
    ///
    /// `dictionary` is a valid `CFDictionary`.
    unsafe fn value(dictionary: CFTypeRef, key: &CStr) -> CFTypeRef {
        // SAFETY: a NUL-terminated UTF-8 key; the string made is released below.
        let key = unsafe { CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8) };
        if key.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller's dictionary and the key just made.
        let value = unsafe { CFDictionaryGetValue(dictionary, key) };
        // SAFETY: made above.
        unsafe { CFRelease(key) };
        value
    }

    /// The text of a `CFString`, or `None` for something else.
    ///
    /// # Safety
    ///
    /// `object` is null or a valid Core Foundation object.
    unsafe fn string(object: CFTypeRef) -> Option<String> {
        // SAFETY: a valid object, as the caller says.
        if object.is_null() || unsafe { CFGetTypeID(object) != CFStringGetTypeID() } {
            return None;
        }
        let mut buffer = [0 as c_char; 256];
        // SAFETY: the buffer holds its length.
        let ok =
            unsafe { CFStringGetCString(object, buffer.as_mut_ptr(), buffer.len() as isize, UTF8) };
        if ok == 0 {
            return None;
        }
        // SAFETY: Core Foundation wrote a NUL-terminated string into the buffer.
        Some(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// The value of a `CFBoolean`, or `None` for something else.
    ///
    /// # Safety
    ///
    /// `object` is null or a valid Core Foundation object.
    unsafe fn boolean(object: CFTypeRef) -> Option<bool> {
        // SAFETY: a valid object, as the caller says.
        if object.is_null() || unsafe { CFGetTypeID(object) != CFBooleanGetTypeID() } {
            return None;
        }
        // SAFETY: a CFBoolean.
        Some(unsafe { CFBooleanGetValue(object) } != 0)
    }

    /// The value of a `CFNumber` as a whole number, or `None` for something else.
    ///
    /// # Safety
    ///
    /// `object` is null or a valid Core Foundation object.
    unsafe fn number(object: CFTypeRef) -> Option<i64> {
        // SAFETY: a valid object, as the caller says.
        if object.is_null() || unsafe { CFGetTypeID(object) != CFNumberGetTypeID() } {
            return None;
        }
        let mut out = 0i64;
        // SAFETY: a CFNumber, read into an i64.
        unsafe { CFNumberGetValue(object, SINT64, (&mut out as *mut i64).cast()) };
        Some(out)
    }

    /// One power source's description dictionary as a [`PowerSource`].
    ///
    /// # Safety
    ///
    /// `description` is a valid `CFDictionary`.
    unsafe fn power_source(description: CFTypeRef) -> PowerSource {
        // SAFETY: the caller's dictionary; every value is checked for its type.
        unsafe {
            PowerSource {
                kind: string(value(description, c"Type")).unwrap_or_default(),
                present: boolean(value(description, c"Is Present")).unwrap_or(false),
                current_capacity: number(value(description, c"Current Capacity")).unwrap_or(0),
                max_capacity: number(value(description, c"Max Capacity")).unwrap_or(0),
                charging: boolean(value(description, c"Is Charging")).unwrap_or(false),
                charged: boolean(value(description, c"Is Charged")).unwrap_or(false),
                state: string(value(description, c"Power Source State")).unwrap_or_default(),
            }
        }
    }

    /// The Mac's power sources now, and the type of the one it draws from.
    pub fn power_sources() -> (Option<String>, Vec<PowerSource>) {
        // SAFETY: a copy the caller owns, released below.
        let blob = unsafe { IOPSCopyPowerSourcesInfo() };
        if blob.is_null() {
            return (None, Vec::new());
        }
        // SAFETY: the blob just copied; the type is the blob's, read before it is released.
        let providing = unsafe { string(IOPSGetProvidingPowerSourceType(blob)) };
        let mut sources = Vec::new();
        // SAFETY: the blob; the list is a copy released below.
        let list = unsafe { IOPSCopyPowerSourcesList(blob) };
        if !list.is_null() {
            // SAFETY: a CFArray of the blob's sources, each described by a dictionary the
            // blob owns.
            unsafe {
                for index in 0..CFArrayGetCount(list) {
                    let description =
                        IOPSGetPowerSourceDescription(blob, CFArrayGetValueAtIndex(list, index));
                    if !description.is_null() {
                        sources.push(power_source(description));
                    }
                }
                CFRelease(list);
            }
        }
        // SAFETY: copied above.
        unsafe { CFRelease(blob) };
        (providing, sources)
    }

    /// Whether Low Power Mode is on. False on a Mac or macOS without it.
    pub fn low_power_mode() -> bool {
        let Ok(class) = crate::objc::class(c"NSProcessInfo") else {
            return false;
        };
        // SAFETY: NSProcessInfo's shared instance and a BOOL property it has had since
        // macOS 12; one that does not respond is taken to be off.
        unsafe {
            let info: *mut AnyObject = msg_send![class, processInfo];
            if info.is_null() {
                return false;
            }
            let responds: bool = msg_send![
                info,
                respondsToSelector: objc2::sel!(isLowPowerModeEnabled)
            ];
            if !responds {
                return false;
            }
            let on: bool = msg_send![info, isLowPowerModeEnabled];
            on
        }
    }

    /// The Mac's power now.
    pub fn read() -> PowerState {
        let (providing, sources) = power_sources();
        from_power_sources(providing.as_deref(), &sources, low_power_mode())
    }

    /// The kernel's memory pressure level now, or `None` when it cannot be read.
    pub fn memory_pressure_level() -> Option<i32> {
        let mut level: i32 = 0;
        let mut size = std::mem::size_of::<i32>();
        // SAFETY: a NUL-terminated name, and a buffer of `size` bytes for the integer.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.memorystatus_vm_pressure_level".as_ptr(),
                (&mut level as *mut i32).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (result == 0).then_some(level)
    }

    /// What the root power domain's callback needs: the hub, and the connection its answers
    /// go to, set once the registration made it. Both live on the watch's thread.
    struct SleepContext {
        hub: Arc<PowerHub>,
        root: Cell<u32>,
    }

    extern "C" fn power_sources_changed(_context: *mut c_void) {
        // Nothing to do here: the run loop returns after this source and the watch reads the
        // sources then.
    }

    extern "C" fn system_power(
        refcon: *mut c_void,
        _service: u32,
        message_type: u32,
        argument: *mut c_void,
    ) {
        // SAFETY: the context the watch keeps alive until it ends the registration, used only
        // on the watch's thread.
        let context = unsafe { &*(refcon as *const SleepContext) };
        if let Some(event) = sleep_event(message_type) {
            // A panic must not cross into IOKit.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                context.hub.on_sleep_event(event);
            }));
        }
        if needs_answer(message_type) {
            // SAFETY: the root power domain's connection, and the notification's own ID.
            unsafe { IOAllowPowerChange(context.root.get(), argument as isize) };
        }
    }

    /// Follows the Mac's power and sleep on a thread of its own and hands them to `hub` until
    /// `stop` is set.
    pub fn watch(
        hub: Arc<PowerHub>,
        stop: Arc<AtomicBool>,
    ) -> Result<thread::JoinHandle<()>, String> {
        thread::Builder::new()
            .name("dsa-power".into())
            .spawn(move || run(hub, stop))
            .map_err(|e| format!("cannot start following the Mac's power: {e}"))
    }

    fn run(hub: Arc<PowerHub>, stop: Arc<AtomicBool>) {
        // SAFETY: this thread's own run loop, and the default mode Core Foundation names.
        let (run_loop, mode) = unsafe { (CFRunLoopGetCurrent(), kCFRunLoopDefaultMode) };

        // SAFETY: a callback that does nothing and no context.
        let sources_changed = unsafe {
            IOPSNotificationCreateRunLoopSource(power_sources_changed, std::ptr::null_mut())
        };
        if sources_changed.is_null() {
            eprintln!("power: IOKit does not say when the battery changes; it is read every 10 s");
        } else {
            // SAFETY: this thread's run loop and a source made for it.
            unsafe { CFRunLoopAddSource(run_loop, sources_changed, mode) };
        }

        let context = Box::into_raw(Box::new(SleepContext {
            hub: Arc::clone(&hub),
            root: Cell::new(0),
        }));
        let mut port: IONotificationPortRef = std::ptr::null_mut();
        let mut notifier = 0u32;
        // SAFETY: the context outlives the registration, which ends below before it is freed.
        let root = unsafe {
            IORegisterForSystemPower(context.cast(), &mut port, system_power, &mut notifier)
        };
        let sleep_source = if root == 0 || port.is_null() {
            eprintln!(
                "power: the Mac's sleep cannot be followed; Android is not told before it sleeps"
            );
            std::ptr::null()
        } else {
            // SAFETY: the context made above; no callback runs before the run loop does.
            unsafe { (*context).root.set(root) };
            // SAFETY: the port the registration made; the source belongs to the port.
            let source = unsafe { IONotificationPortGetRunLoopSource(port) };
            // SAFETY: this thread's run loop and the port's source.
            unsafe { CFRunLoopAddSource(run_loop, source, mode) };
            source
        };

        let mut battery = read();
        hub.update(battery);
        let mut read_at = Instant::now();
        let mut memory_low = false;
        while !stop.load(Ordering::Relaxed) {
            // SAFETY: runs this thread's run loop, where the callbacks above are called.
            let result = unsafe { CFRunLoopRunInMode(mode, SAVER_POLL.as_secs_f64(), 1) };
            if result == RUN_FINISHED {
                // No source to wait on: wait here instead.
                thread::sleep(SAVER_POLL);
            }
            if result == RUN_HANDLED_SOURCE || read_at.elapsed() >= POLL {
                battery = read();
                read_at = Instant::now();
            }
            hub.update(PowerState {
                power_saver: low_power_mode(),
                ..battery
            });
            if let Some(level) = memory_pressure_level() {
                let low = memory_low_at(level, memory_low);
                if low != memory_low {
                    eprintln!(
                        "host memory: macOS memory pressure level {level}, {}",
                        if low { "short" } else { "no longer short" }
                    );
                    memory_low = low;
                }
                hub.update_memory(low);
            }
        }

        // SAFETY: the sources and the registration made above, ended before the context they
        // call into is freed.
        unsafe {
            if !sleep_source.is_null() {
                CFRunLoopRemoveSource(run_loop, sleep_source, mode);
                IODeregisterForSystemPower(&mut notifier);
                IOServiceClose(root);
                IONotificationPortDestroy(port);
            }
            if !sources_changed.is_null() {
                CFRunLoopRemoveSource(run_loop, sources_changed, mode);
                CFRelease(sources_changed);
            }
            drop(Box::from_raw(context));
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::{low_power_mode, memory_pressure_level, power_sources, read, watch};
