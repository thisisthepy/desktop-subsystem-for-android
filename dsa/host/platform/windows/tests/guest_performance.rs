//! How the performance the guest's power hints ask for sets the VM's process.

use dsa_host_core::power_hints::Performance;
use dsa_host_windows::guest_performance::{
    settings, ABOVE_NORMAL_PRIORITY, BELOW_NORMAL_PRIORITY, EXECUTION_SPEED, NORMAL_PRIORITY,
};

#[test]
fn pr4_full_speed_is_never_throttled_and_runs_above_normal() {
    let full = settings(Performance::Full);
    assert_eq!(full.throttling_control, EXECUTION_SPEED);
    assert_eq!(full.throttling_state, 0);
    assert_eq!(full.priority, ABOVE_NORMAL_PRIORITY);
}

#[test]
fn pr4_normal_leaves_throttling_to_windows() {
    let normal = settings(Performance::Normal);
    assert_eq!(normal.throttling_control, 0);
    assert_eq!(normal.throttling_state, 0);
    assert_eq!(normal.priority, NORMAL_PRIORITY);
}

#[test]
fn pr4_frugal_is_throttled_and_runs_below_normal() {
    let efficient = settings(Performance::Efficient);
    assert_eq!(efficient.throttling_control, EXECUTION_SPEED);
    assert_eq!(efficient.throttling_state, EXECUTION_SPEED);
    assert_eq!(efficient.priority, BELOW_NORMAL_PRIORITY);
}

#[cfg(windows)]
#[test]
fn pr4_the_settings_are_windows_own_values() {
    use windows_sys::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    assert_eq!(EXECUTION_SPEED, PROCESS_POWER_THROTTLING_EXECUTION_SPEED);
    assert_eq!(NORMAL_PRIORITY, NORMAL_PRIORITY_CLASS);
    assert_eq!(ABOVE_NORMAL_PRIORITY, ABOVE_NORMAL_PRIORITY_CLASS);
    assert_eq!(BELOW_NORMAL_PRIORITY, BELOW_NORMAL_PRIORITY_CLASS);
}
