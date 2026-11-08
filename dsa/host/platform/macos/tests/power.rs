//! The Mac's power as IOKit reports it, as the guest's battery state, and the Mac's sleep.

use std::time::{Duration, Instant};

use dsa_host_core::power::{PowerHub, PowerState, SleepEvent, SLEEP_WAIT};
use dsa_host_macos::power::{
    from_power_sources, needs_answer, sleep_event, PowerSource, AC_POWER, CAN_SYSTEM_SLEEP,
    INTERNAL_BATTERY, SAVER_POLL, SYSTEM_HAS_POWERED_ON, SYSTEM_WILL_NOT_SLEEP,
    SYSTEM_WILL_POWER_ON, SYSTEM_WILL_SLEEP,
};

fn battery(percent: i64, state: &str, charging: bool, charged: bool) -> PowerSource {
    PowerSource {
        kind: INTERNAL_BATTERY.into(),
        present: true,
        current_capacity: percent,
        max_capacity: 100,
        charging,
        charged,
        state: state.into(),
    }
}

#[test]
fn fr10_a_macbook_on_battery_is_a_discharging_battery_at_its_charge() {
    assert_eq!(
        from_power_sources(
            Some("Battery Power"),
            &[battery(57, "Battery Power", false, false)],
            false
        ),
        PowerState {
            ac_online: false,
            battery_present: true,
            battery_percent: 57,
            charging: false,
            power_saver: false,
        }
    );
}

#[test]
fn fr10_a_charging_macbook_is_on_ac_power_and_charging() {
    let state = from_power_sources(Some(AC_POWER), &[battery(40, AC_POWER, true, false)], false);
    assert!(state.ac_online && state.charging && state.battery_present);
    assert_eq!(state.battery_percent, 40);
    // Without the providing source, the battery's own state says where the power comes from.
    assert!(from_power_sources(None, &[battery(40, AC_POWER, true, false)], false).ac_online);
    // A full battery on AC power is charged, not charging.
    let full = from_power_sources(Some(AC_POWER), &[battery(99, AC_POWER, true, true)], false);
    assert!(!full.charging);
    assert_eq!(full.battery_percent, 100);
}

#[test]
fn fr10_a_capacity_not_in_percent_is_turned_into_percent() {
    let mut source = battery(2_500, "Battery Power", false, false);
    source.max_capacity = 5_000;
    assert_eq!(
        from_power_sources(None, &[source.clone()], false).battery_percent,
        50
    );
    // A source that gives no maximum reads as full.
    source.max_capacity = 0;
    assert_eq!(
        from_power_sources(None, &[source], false).battery_percent,
        100
    );
}

#[test]
fn fr10_low_power_mode_on_the_mac_is_battery_saver_in_the_guest() {
    let on_battery = [battery(15, "Battery Power", false, false)];
    assert!(from_power_sources(Some("Battery Power"), &on_battery, true).power_saver);
    assert!(!from_power_sources(Some("Battery Power"), &on_battery, false).power_saver);
    // A Mac without a battery has Low Power Mode too.
    assert!(from_power_sources(Some(AC_POWER), &[], true).power_saver);
    // Read often enough that the guest's display follows within a second.
    assert!(SAVER_POLL < Duration::from_secs(1));
}

#[test]
fn fr10_a_mac_without_a_battery_is_a_full_battery_on_ac_power() {
    assert_eq!(
        from_power_sources(Some(AC_POWER), &[], false),
        PowerState::MAINS_ONLY
    );
    // A UPS is not the Mac's battery, and a battery that is not there is none.
    let ups = PowerSource {
        kind: "UPS".into(),
        ..battery(30, "Battery Power", false, false)
    };
    let missing = PowerSource {
        present: false,
        ..battery(30, "Battery Power", false, false)
    };
    assert_eq!(
        from_power_sources(Some(AC_POWER), &[ups, missing], false),
        PowerState::MAINS_ONLY
    );
}

#[test]
fn fr10_the_root_power_domains_messages_are_the_macs_sleep() {
    assert_eq!(sleep_event(SYSTEM_WILL_SLEEP), Some(SleepEvent::Sleeping));
    assert_eq!(sleep_event(SYSTEM_HAS_POWERED_ON), Some(SleepEvent::Woke));
    // Asking whether it may sleep, a sleep that does not happen, and the start of waking
    // before the devices are back are not the sleep itself.
    assert_eq!(sleep_event(CAN_SYSTEM_SLEEP), None);
    assert_eq!(sleep_event(SYSTEM_WILL_NOT_SLEEP), None);
    assert_eq!(sleep_event(SYSTEM_WILL_POWER_ON), None);
}

#[test]
fn fr10_the_mac_is_answered_for_every_sleep_it_waits_on() {
    assert!(needs_answer(CAN_SYSTEM_SLEEP));
    assert!(needs_answer(SYSTEM_WILL_SLEEP));
    assert!(!needs_answer(SYSTEM_HAS_POWERED_ON));
    assert!(!needs_answer(SYSTEM_WILL_NOT_SLEEP));
}

#[test]
fn fr10_the_macs_sleep_waits_for_the_guests_at_most_two_seconds() {
    assert_eq!(SLEEP_WAIT, Duration::from_secs(2));
    // With no Android running, the Mac's sleep is not held at all.
    let hub = PowerHub::new();
    let started = Instant::now();
    assert!(hub.on_sleep_event(sleep_event(SYSTEM_WILL_SLEEP).unwrap()));
    assert!(hub.on_sleep_event(sleep_event(SYSTEM_HAS_POWERED_ON).unwrap()));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[cfg(target_os = "macos")]
#[test]
fn fr10_this_macs_power_can_be_read() {
    let state = dsa_host_macos::power::read();
    assert!(state.battery_percent <= 100);
}

#[test]
fn nfr2_the_macs_memory_pressure_is_what_the_guest_is_told() {
    use dsa_host_macos::power::{memory_low_at, PRESSURE_CRITICAL, PRESSURE_NORMAL, PRESSURE_WARN};
    // Warn and critical tell the guest the Mac is short of memory, whatever it was before.
    assert!(memory_low_at(PRESSURE_WARN, false));
    assert!(memory_low_at(PRESSURE_CRITICAL, false));
    assert!(memory_low_at(PRESSURE_CRITICAL, true));
    // Normal again ends the shortage.
    assert!(!memory_low_at(PRESSURE_NORMAL, true));
    assert!(!memory_low_at(PRESSURE_NORMAL, false));
    // A level the kernel does not name changes nothing.
    assert!(memory_low_at(0, true));
    assert!(!memory_low_at(3, false));
}

#[test]
fn nfr2_the_macs_memory_pressure_reaches_the_hub_only_when_it_changes() {
    use dsa_host_macos::power::{memory_low_at, PRESSURE_NORMAL, PRESSURE_WARN};
    let hub = PowerHub::new();
    let mut low = false;
    for level in [PRESSURE_NORMAL, PRESSURE_WARN, PRESSURE_WARN] {
        low = memory_low_at(level, low);
        hub.update_memory(low);
    }
    assert!(hub.memory_low());
    low = memory_low_at(PRESSURE_NORMAL, low);
    hub.update_memory(low);
    assert!(!hub.memory_low());
}

#[cfg(target_os = "macos")]
#[test]
fn nfr2_this_macs_memory_pressure_can_be_read() {
    let level = dsa_host_macos::power::memory_pressure_level().expect("the sysctl");
    assert!([1, 2, 4].contains(&level), "level {level}");
}
