//! The computer's power as Windows reports it, as the guest's battery state.

use dsa_host_core::power::PowerState;
use dsa_host_windows::power::from_system_status;

#[test]
fn fr10_a_laptop_on_battery_is_a_discharging_battery_at_its_charge() {
    assert_eq!(
        from_system_status(0, 1, 57, 0),
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
fn fr10_a_charging_laptop_is_on_ac_power_and_charging() {
    let state = from_system_status(1, 8, 40, 0);
    assert!(state.ac_online && state.charging && state.battery_present);
    assert_eq!(state.battery_percent, 40);
    // Windows may not know the AC line while it knows the battery charges.
    assert!(from_system_status(255, 8, 40, 0).ac_online);
}

#[test]
fn fr10_battery_saver_on_the_host_is_battery_saver_in_the_guest() {
    assert!(from_system_status(0, 2, 15, 1).power_saver);
    assert!(from_system_status(1, 128, 255, 1).power_saver);
}

#[test]
fn fr10_a_desktop_without_a_battery_is_a_full_battery_on_ac_power() {
    assert_eq!(from_system_status(1, 128, 255, 0), PowerState::MAINS_ONLY);
    // A battery Windows cannot read counts as none.
    assert_eq!(from_system_status(255, 255, 255, 0), PowerState::MAINS_ONLY);
}

#[test]
fn fr10_an_unknown_charge_reads_as_full() {
    assert_eq!(from_system_status(1, 1, 255, 0).battery_percent, 100);
}

#[cfg(windows)]
#[test]
fn fr10_this_computers_power_can_be_read() {
    assert!(dsa_host_windows::power::read().is_some());
}
