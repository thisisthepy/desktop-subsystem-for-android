use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use dsa_host_core::console_log::{
    boot_header, diagnostic_files, old_copy_path, rotate_if_large, utc_timestamp,
};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("core-console-log")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fr2_a_large_console_log_is_moved_aside_and_only_one_old_copy_is_kept() {
    let dir = scratch("rotate");
    let log = dir.join("android-1.log");
    assert_eq!(old_copy_path(&log), dir.join("android-1.log.1"));
    assert!(
        !rotate_if_large(&log, 10).unwrap(),
        "no file, nothing to do"
    );
    std::fs::write(&log, b"0123456789").unwrap();
    assert!(!rotate_if_large(&log, 10).unwrap(), "at the limit it stays");
    std::fs::write(&log, b"first boot, too long").unwrap();
    assert!(rotate_if_large(&log, 10).unwrap());
    assert!(!log.exists());
    std::fs::write(&log, b"second boot, too long").unwrap();
    assert!(rotate_if_large(&log, 10).unwrap());
    assert_eq!(
        std::fs::read_to_string(old_copy_path(&log)).unwrap(),
        "second boot, too long"
    );
}

#[test]
fn fr2_timestamps_are_utc_dates_and_times() {
    assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    assert_eq!(
        utc_timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)),
        "2000-02-29T00:00:00Z"
    );
    assert_eq!(
        utc_timestamp(UNIX_EPOCH + Duration::from_secs(1_792_265_625)),
        "2026-10-17T19:33:45Z"
    );
}

#[test]
fn fr2_each_boot_starts_with_a_header_naming_the_time_image_and_command_line() {
    let header = boot_header(
        "1.2.3",
        UNIX_EPOCH + Duration::from_secs(1_792_265_625),
        Some("17.0.0-1"),
        "console=ttyS0,115200 panic=-1",
    );
    assert_eq!(
        header,
        "==== DSA 1.2.3 boot at 2026-10-17T19:33:45Z, image 17.0.0-1, kernel command line: \
         console=ttyS0,115200 panic=-1\n"
    );
    assert!(boot_header("1", UNIX_EPOCH, None, "").contains("image (unversioned)"));
}

#[test]
fn fr2_the_diagnostics_carry_the_console_log_and_its_older_copy() {
    let log = PathBuf::from("android-1.log");
    let files = diagnostic_files(&log);
    assert_eq!(files[0], ("host/android-console.log", log.clone()));
    assert_eq!(
        files[1],
        (
            "host/android-console.1.log",
            PathBuf::from("android-1.log.1")
        )
    );
}
