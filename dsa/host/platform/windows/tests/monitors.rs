//! The record of the user's monitors, as the service reads it when it starts Android.

use std::path::PathBuf;

use dsa_host_core::guest_display::{with_display_size, Monitor, MAX_SIDE};
use dsa_host_windows::monitors::{record_path, recorded_display, write_record};

fn empty_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsa-monitors-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fr4_the_service_sizes_the_display_in_guest_pixels_for_monitors_of_mixed_scales() {
    let dir = empty_dir("scaled");
    // The main monitor at 200% and two at 125%, as the user's session records them.
    write_record(
        &dir,
        &[
            Monitor::at_dpi(2880, 1800, 192).as_main(),
            Monitor::at_dpi(2560, 1440, 120),
            Monitor::at_dpi(2560, 1440, 120),
        ],
    )
    .unwrap();
    let (side, height) = recorded_display(&dir).unwrap();
    assert_eq!(side, height);
    assert!(side >= 3840 || side == MAX_SIDE, "{side}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fr4_the_service_reads_a_record_written_before_monitors_carried_their_scale() {
    let dir = empty_dir("old");
    let path = record_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "2880x1800\n2560x1440\n2560x1440\n").unwrap();
    assert_eq!(recorded_display(&dir), Some((2880, 2880)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fr4_the_service_reads_the_monitors_the_users_session_recorded() {
    let dir = empty_dir("recorded");
    write_record(
        &dir,
        &[Monitor::sized(2880, 1800), Monitor::sized(1080, 1920)],
    )
    .unwrap();
    let size = recorded_display(&dir).unwrap();
    assert_eq!(size, (2880, 2880));
    // What the VM then boots with: the image's own size is gone, so this one wins.
    assert_eq!(
        with_display_size("androidboot.dsa.display_width=1920", size),
        "androidboot.dsa.display_width=2880 androidboot.dsa.display_height=2880"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fr4_android_starts_at_the_main_monitors_density() {
    use dsa_host_core::guest_display::with_display_density;
    use dsa_host_windows::monitors::{recorded_density, write_density_record};
    let dir = empty_dir("density");
    // No record: a 100% monitor's.
    assert_eq!(recorded_density(&dir), 130);
    // The user's session recorded a main monitor at 150%.
    write_density_record(&dir, 144).unwrap();
    let density = recorded_density(&dir);
    assert_eq!(density, 196);
    // What the VM then boots with, for the hwcomposer and SurfaceFlinger alike.
    assert_eq!(
        with_display_density(
            "androidboot.dsa.display_dpi=130 androidboot.lcd_density=130",
            density
        ),
        "androidboot.dsa.display_dpi=196 androidboot.lcd_density=196"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fr4_without_a_record_the_image_keeps_its_size() {
    let dir = empty_dir("none");
    assert_eq!(recorded_display(&dir), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fr4_a_record_that_is_not_text_is_no_record() {
    let dir = empty_dir("damaged");
    let path = record_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(recorded_display(&dir), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fr10_battery_saver_caps_the_guest_refresh_rate_the_users_session_recorded() {
    use dsa_host_core::guest_refresh::refresh_rate;
    use dsa_host_windows::monitors::{recorded_refresh, write_refresh_record};
    let dir = empty_dir("refresh");
    assert_eq!(recorded_refresh(&dir), None);
    write_refresh_record(&dir, 120).unwrap();
    let hz = recorded_refresh(&dir).unwrap();
    assert_eq!(hz, 120);
    assert_eq!(refresh_rate(hz, true), 60);
    assert_eq!(refresh_rate(hz, false), 120);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn fr10_a_monitor_at_the_hardwares_default_rate_has_no_known_rate() {
    use dsa_host_windows::monitors::frequency_hz;
    assert_eq!(frequency_hz(0), 0);
    assert_eq!(frequency_hz(1), 0);
    assert_eq!(frequency_hz(144), 144);
}
