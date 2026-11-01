//! The record of the user's monitors, as the service reads it when it starts Android.

use std::path::PathBuf;

use dsa_host_core::guest_display::with_display_size;
use dsa_host_windows::monitors::{record_path, recorded_display, write_record};

fn empty_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsa-monitors-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fr4_the_service_reads_the_monitors_the_users_session_recorded() {
    let dir = empty_dir("recorded");
    write_record(&dir, &[(2880, 1800), (1080, 1920)]).unwrap();
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
