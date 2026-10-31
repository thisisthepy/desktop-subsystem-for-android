//! The Android VM's console log: rotation, the boot header and the lines a failed start
//! reports.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use dsa_host_windows::console_log::{
    boot_header, clean_line, failure_report, old_copy_path, rotate_if_large, utc_timestamp,
    ConsoleLog, ConsoleTail, ROTATE_AT_BYTES, TAIL_LINES,
};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("console-log")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read(path: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned()
}

#[test]
fn fr2_a_log_over_the_limit_is_moved_aside_with_one_old_copy() {
    let dir = scratch("rotate");
    let log = dir.join("android-S-1-5-21-1.log");
    assert_eq!(old_copy_path(&log), dir.join("android-S-1-5-21-1.log.1"));

    assert!(
        !rotate_if_large(&log, 10).unwrap(),
        "no file, nothing to do"
    );
    std::fs::write(&log, b"0123456789").unwrap();
    assert!(!rotate_if_large(&log, 10).unwrap(), "at the limit it stays");
    assert_eq!(read(&log), "0123456789");

    std::fs::write(&log, b"first boot, too long").unwrap();
    assert!(rotate_if_large(&log, 10).unwrap());
    assert!(!log.exists());
    assert_eq!(read(&old_copy_path(&log)), "first boot, too long");

    std::fs::write(&log, b"second boot, too long").unwrap();
    assert!(rotate_if_large(&log, 10).unwrap());
    assert_eq!(
        read(&old_copy_path(&log)),
        "second boot, too long",
        "only one old copy is kept"
    );
    assert!(!dir.join("android-S-1-5-21-1.log.2").exists());
}

#[test]
fn fr2_the_rotation_limit_is_about_four_megabytes() {
    assert_eq!(ROTATE_AT_BYTES, 4 * 1024 * 1024);
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
        UNIX_EPOCH + Duration::from_secs(1_792_265_625),
        Some("17.0.0-1"),
        "earlycon=uart8250,io,0x3f8,115200 console=ttyS0,115200 panic=-1",
    );
    assert!(header.starts_with("==== DSA "), "{header}");
    assert!(header.contains("2026-10-17T19:33:45Z"), "{header}");
    assert!(header.contains("image 17.0.0-1"), "{header}");
    assert!(
        header.contains(
            "kernel command line: earlycon=uart8250,io,0x3f8,115200 console=ttyS0,115200 panic=-1"
        ),
        "{header}"
    );
    assert!(header.ends_with('\n'));
    assert_eq!(header.lines().count(), 1);
    assert!(boot_header(UNIX_EPOCH, None, "").contains("image (unversioned)"));
}

#[test]
fn fr2_the_tail_keeps_the_last_non_empty_lines_across_pieces() {
    let mut tail = ConsoleTail::new(3);
    tail.push(b"[    0.000000] Linux version 6.18\r\n\r\n");
    tail.push(b"[    1.2] init: first stage");
    tail.push(b" mount\r\n   \r\n[    2.0] init: second");
    assert_eq!(
        tail.lines(),
        [
            "[    0.000000] Linux version 6.18",
            "[    1.2] init: first stage mount",
            "[    2.0] init: second",
        ],
        "a line still being written counts"
    );
    tail.push(b" stage\n[    3.0] Kernel panic - not syncing\n");
    assert_eq!(
        tail.lines(),
        [
            "[    1.2] init: first stage mount",
            "[    2.0] init: second stage",
            "[    3.0] Kernel panic - not syncing",
        ]
    );
}

#[test]
fn fr2_a_multibyte_character_split_between_pieces_survives() {
    let mut tail = ConsoleTail::new(TAIL_LINES);
    let text = "init: 한국어\n".as_bytes();
    tail.push(&text[..8]);
    tail.push(&text[8..]);
    assert_eq!(tail.lines(), ["init: 한국어"]);
}

#[test]
fn fr2_console_lines_are_shown_without_escapes_or_control_characters() {
    assert_eq!(
        clean_line(b"\x1b[0;31mfailed\x1b[0m to mount\t/data\x07\r"),
        "failed to mount /data"
    );
    assert_eq!(clean_line(b"bad \xff byte"), "bad \u{fffd} byte");
    let long = clean_line(&[b'x'; 1000]);
    assert!(long.len() < 400 && long.ends_with("..."), "{long}");
}

#[test]
fn fr2_a_failed_start_shows_the_last_lines_and_where_the_whole_boot_is() {
    let path = Path::new(r"C:\ProgramData\DSA\logs\android-S-1-5-21-1.log");
    let report = failure_report(
        path,
        &["init: first stage mount".into(), "Kernel panic".into()],
        None,
    );
    assert_eq!(
        report,
        format!(
            "Last lines of the Android console (the whole boot is in {}):\n  \
             init: first stage mount\n  Kernel panic",
            path.display()
        )
    );
    let silent = failure_report(path, &[], Some("the console log cannot be written: denied"));
    assert!(silent.contains("printed nothing"), "{silent}");
    assert!(silent.contains(&path.display().to_string()), "{silent}");
    assert!(silent.ends_with("cannot be written: denied."), "{silent}");
}

#[test]
fn fr2_the_console_is_appended_after_a_header_for_each_boot() {
    let dir = scratch("append");
    let path = dir.join("logs").join("android-S-1-5-21-1.log");

    let mut first = ConsoleLog::begin_boot(&path, "==== boot 1\n");
    first.record(b"Linux version 6.18\r\n");
    first.record(b"init: half a line");
    assert_eq!(first.write_error(), None);
    drop(first);

    let mut second = ConsoleLog::begin_boot(&path, "==== boot 2\n");
    second.record(b"Kernel panic\n");
    second.note("the Android VM never opened its console");
    assert_eq!(second.tail(), ["Kernel panic"], "only this boot's lines");
    assert!(second.failure_report().contains("Kernel panic"));
    drop(second);

    assert_eq!(
        read(&path),
        "==== boot 1\nLinux version 6.18\r\ninit: half a line\n==== boot 2\nKernel panic\n\
         \n==== the Android VM never opened its console\n"
    );
}

#[test]
fn fr2_a_large_log_is_rotated_when_a_boot_starts() {
    let dir = scratch("rotate-on-boot");
    let path = dir.join("android-S-1-5-21-1.log");
    std::fs::write(&path, vec![b'x'; ROTATE_AT_BYTES as usize + 1]).unwrap();
    let mut log = ConsoleLog::begin_boot(&path, "==== boot\n");
    log.record(b"fresh\n");
    drop(log);
    assert_eq!(read(&path), "==== boot\nfresh\n");
    assert_eq!(
        std::fs::metadata(old_copy_path(&path)).unwrap().len(),
        ROTATE_AT_BYTES + 1
    );
}

#[test]
fn fr2_a_log_that_cannot_be_written_still_keeps_the_last_lines() {
    let dir = scratch("unwritable");
    // A folder where the file should be: opening it for writing fails on every platform.
    let path = dir.join("android-S-1-5-21-1.log");
    std::fs::create_dir_all(&path).unwrap();
    let mut log = ConsoleLog::begin_boot(&path, "==== boot\n");
    log.record(b"Kernel panic\n");
    assert_eq!(log.tail(), ["Kernel panic"]);
    let why = log.write_error().expect("the failure is reported");
    assert!(why.contains("cannot be written"), "{why}");
    let report = log.failure_report();
    assert!(report.contains("Kernel panic"), "{report}");
    assert!(report.contains("cannot be written"), "{report}");
}
