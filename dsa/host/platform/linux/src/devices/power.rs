//! The computer's power, as the user's Android sees it: AC or battery, the charge, the power
//! saver, the host's sleep, and the host's memory running short.
//!
//! The power comes from UPower's display device, the battery the desktop shows, and the power
//! saver from power-profiles-daemon; where neither runs, from `/sys/class/power_supply`. The
//! sleep comes from logind: DSA holds a delay inhibitor, so before the computer sleeps each
//! Android is told and puts its data on disk, and the sleep waits for it (as long as logind's
//! `InhibitDelayMaxSec` allows). The memory comes from `/proc/meminfo`.
//!
//! These run in the service, which owns the sessions the [`PowerHub`] hands them to.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::power::{MemoryPressure, PowerHub, PowerState, SleepEvent};
use zbus::blocking::Connection;

use super::dbus::proxy;

/// How often [`follow`] reads the computer's power and memory.
pub const POLL: Duration = Duration::from_secs(10);

/// How long the guests get to put their data on disk before the computer sleeps: the same
/// limit on every host, well inside logind's default `InhibitDelayMaxSec`.
pub use dsa_host_core::power::SLEEP_WAIT;

const UPOWER: &str = "org.freedesktop.UPower";

/// UPower's device states.
const STATE_CHARGING: u32 = 1;
const STATE_FULLY_CHARGED: u32 = 4;
const STATE_PENDING_CHARGE: u32 = 5;

/// The power as UPower reports it.
pub fn from_upower(
    on_battery: bool,
    present: bool,
    percentage: f64,
    state: u32,
    power_saver: bool,
) -> PowerState {
    if !present {
        return PowerState {
            power_saver,
            ..PowerState::MAINS_ONLY
        };
    }
    let full = state == STATE_FULLY_CHARGED;
    PowerState {
        ac_online: !on_battery,
        battery_present: true,
        battery_percent: if full {
            100
        } else {
            percentage.round().clamp(0.0, 100.0) as u8
        },
        charging: !full
            && (state == STATE_CHARGING
                || (!on_battery && state == STATE_PENDING_CHARGE && percentage < 100.0)),
        power_saver,
    }
}

/// Whether power-profiles-daemon's active profile is its battery saver.
pub fn is_power_saver_profile(profile: &str) -> bool {
    profile == "power-saver"
}

/// Whether the power saver profile is on, from power-profiles-daemon under either of its bus
/// names. False where it does not run.
fn power_saver(system: &Connection) -> bool {
    active_profile(system).is_some_and(|profile| is_power_saver_profile(&profile))
}

fn active_profile(system: &Connection) -> Option<String> {
    for (service, path, interface) in [
        (
            "org.freedesktop.UPower.PowerProfiles",
            "/org/freedesktop/UPower/PowerProfiles",
            "org.freedesktop.UPower.PowerProfiles",
        ),
        (
            "net.hadess.PowerProfiles",
            "/net/hadess/PowerProfiles",
            "net.hadess.PowerProfiles",
        ),
    ] {
        let profile = proxy(system, service, path.into(), interface)
            .ok()
            .and_then(|p| p.get_property::<String>("ActiveProfile").ok());
        if profile.is_some() {
            return profile;
        }
    }
    None
}

fn read_upower(system: &Connection) -> Option<PowerState> {
    let root = proxy(system, UPOWER, "/org/freedesktop/UPower".into(), UPOWER).ok()?;
    let on_battery: bool = root.get_property("OnBattery").ok()?;
    let display = proxy(
        system,
        UPOWER,
        "/org/freedesktop/UPower/devices/DisplayDevice".into(),
        "org.freedesktop.UPower.Device",
    )
    .ok()?;
    let present: bool = display.get_property("IsPresent").unwrap_or(false);
    let percentage: f64 = display.get_property("Percentage").unwrap_or(100.0);
    let state: u32 = display.get_property("State").unwrap_or(0);
    Some(from_upower(
        on_battery,
        present,
        percentage,
        state,
        power_saver(system),
    ))
}

/// The power from the kernel's power supplies under `root` (`/sys/class/power_supply`).
pub fn read_power_supply(root: &Path) -> PowerState {
    let text = |dir: &Path, name: &str| {
        fs::read_to_string(dir.join(name))
            .map(|t| t.trim().to_string())
            .unwrap_or_default()
    };
    let mut ac_online = None;
    let mut battery: Option<(u8, String)> = None;
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        let dir = entry.path();
        match text(&dir, "type").as_str() {
            "Mains" | "USB" => {
                if text(&dir, "online") == "1" {
                    ac_online = Some(true);
                } else if ac_online.is_none() {
                    ac_online = Some(false);
                }
            }
            "Battery" if text(&dir, "scope") != "Device" && battery.is_none() => {
                let percent = text(&dir, "capacity").parse::<u8>().unwrap_or(100);
                battery = Some((percent.min(100), text(&dir, "status")));
            }
            _ => {}
        }
    }
    match battery {
        None => PowerState::MAINS_ONLY,
        Some((percent, status)) => {
            let discharging = status == "Discharging";
            PowerState {
                ac_online: ac_online.unwrap_or(!discharging),
                battery_present: true,
                battery_percent: percent,
                charging: status == "Charging",
                power_saver: false,
            }
        }
    }
}

/// The memory the computer has and the memory available, in KiB, from `/proc/meminfo`'s
/// text.
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
    };
    Some((field("MemTotal:")?, field("MemAvailable:")?))
}

/// How often [`follow`] reads the power saver profile. It lowers the guest display's refresh
/// rate, which has to follow within a second; it is one property read on the system bus.
pub const SAVER_POLL: Duration = Duration::from_millis(500);

/// Reads the computer's power and memory every [`POLL`], and the power saver profile every
/// [`SAVER_POLL`], until `stop` is set, and hands them to `hub`, which passes on only what
/// changed.
pub fn follow(hub: Arc<PowerHub>, stop: Arc<AtomicBool>) {
    let system = Connection::system().ok();
    let mut pressure = MemoryPressure::new();
    while !stop.load(Ordering::Relaxed) {
        let state = system
            .as_ref()
            .and_then(read_upower)
            .unwrap_or_else(|| read_power_supply(Path::new("/sys/class/power_supply")));
        hub.update(state);
        if let Some((total_kb, available_kb)) = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| parse_meminfo(&t))
        {
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
        let mut waited = Duration::ZERO;
        while waited < POLL && !stop.load(Ordering::Relaxed) {
            thread::sleep(SAVER_POLL);
            waited += SAVER_POLL;
            if let Some(system) = &system {
                hub.update(PowerState {
                    power_saver: power_saver(system),
                    ..state
                });
            }
        }
    }
}

/// Hands logind's sleep and wake to `hub` for as long as the process runs, on a thread of its
/// own. Fails when logind cannot be reached.
pub fn watch_sleep(hub: Arc<PowerHub>) -> Result<thread::JoinHandle<()>, String> {
    let system = Connection::system().map_err(|e| format!("the system bus: {e}"))?;
    let manager = proxy(
        &system,
        "org.freedesktop.login1",
        "/org/freedesktop/login1".into(),
        "org.freedesktop.login1.Manager",
    )?;
    let signals = manager
        .receive_signal("PrepareForSleep")
        .map_err(|e| format!("watching for sleep: {e}"))?;
    let inhibit = move || -> Option<zbus::zvariant::OwnedFd> {
        manager
            .call(
                "Inhibit",
                &(
                    "sleep",
                    "Desktop Subsystem for Android",
                    "Android saves its data before the computer sleeps",
                    "delay",
                ),
            )
            .map_err(|e| eprintln!("power: no sleep delay is held: {e}"))
            .ok()
    };
    thread::Builder::new()
        .name("dsa-sleep".into())
        .spawn(move || {
            let mut held = inhibit();
            for message in signals {
                let Ok(going) = message.body().deserialize::<bool>() else {
                    continue;
                };
                if going {
                    hub.on_sleep_event(SleepEvent::Sleeping);
                    // Releasing the delay lets the sleep go on.
                    held = None;
                } else {
                    hub.on_sleep_event(SleepEvent::Woke);
                    if held.is_none() {
                        held = inhibit();
                    }
                }
            }
            drop(held);
        })
        .map_err(|e| format!("cannot start the sleep watch: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr10_upowers_battery_reaches_the_guest() {
        let state = from_upower(true, true, 42.4, 2, true);
        assert_eq!(
            state,
            PowerState {
                ac_online: false,
                battery_present: true,
                battery_percent: 42,
                charging: false,
                power_saver: true,
            }
        );
        let charging = from_upower(false, true, 80.0, STATE_CHARGING, false);
        assert!(charging.ac_online && charging.charging);
        let full = from_upower(false, true, 99.6, STATE_FULLY_CHARGED, false);
        assert_eq!(full.battery_percent, 100);
        assert!(!full.charging);
    }

    #[test]
    fn fr10_battery_saver_caps_the_guest_refresh_rate_with_the_power_saver_profile() {
        use dsa_host_core::guest_refresh::refresh_rate;
        assert!(is_power_saver_profile("power-saver"));
        assert!(!is_power_saver_profile("balanced"));
        assert!(!is_power_saver_profile("performance"));
        let saver = from_upower(false, false, 0.0, 0, is_power_saver_profile("power-saver"));
        assert_eq!(refresh_rate(120, saver.power_saver), 60);
        let balanced = from_upower(false, false, 0.0, 0, is_power_saver_profile("balanced"));
        assert_eq!(refresh_rate(120, balanced.power_saver), 120);
        // Read often enough that the cap follows within a second.
        assert!(SAVER_POLL < Duration::from_secs(1));
    }

    #[test]
    fn fr10_linuxs_sleep_waits_for_the_guests_at_most_two_seconds() {
        assert!(SLEEP_WAIT <= Duration::from_secs(2), "{SLEEP_WAIT:?}");
        // The wait logind's PrepareForSleep starts is the shared one: with no Android running
        // it does not hold the sleep at all.
        let hub = PowerHub::new();
        let started = std::time::Instant::now();
        assert!(hub.on_sleep_event(SleepEvent::Sleeping));
        assert!(hub.on_sleep_event(SleepEvent::Woke));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn fr10_a_computer_without_a_battery_is_on_mains() {
        assert_eq!(
            from_upower(false, false, 0.0, 0, false),
            PowerState::MAINS_ONLY
        );
    }

    #[test]
    fn fr10_the_kernels_power_supplies_are_read_without_upower() {
        let root = std::env::temp_dir().join(format!("dsa-psu-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (dir, files) in [
            ("AC", vec![("type", "Mains"), ("online", "0")]),
            (
                "BAT0",
                vec![
                    ("type", "Battery"),
                    ("capacity", "57"),
                    ("status", "Discharging"),
                ],
            ),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            for (name, value) in files {
                fs::write(root.join(dir).join(name), value).unwrap();
            }
        }
        let state = read_power_supply(&root);
        assert!(!state.ac_online && state.battery_present && !state.charging);
        assert_eq!(state.battery_percent, 57);
        let _ = fs::remove_dir_all(root);
        assert_eq!(
            read_power_supply(Path::new("/nonexistent")),
            PowerState::MAINS_ONLY
        );
    }

    #[test]
    fn nfr2_meminfo_gives_the_total_and_the_available_memory() {
        let text = "MemTotal:       16314280 kB\nMemFree:  1 kB\nMemAvailable:    8123456 kB\n";
        assert_eq!(parse_meminfo(text), Some((16_314_280, 8_123_456)));
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);
    }
}
