//! The raw copy of an image's system disk and how it is trusted, the same on every host that
//! attaches raw disks.

use std::fs::File;
use std::path::{Path, PathBuf};

use dsa_host_core::direct_mapped_disk::file_sha256;
use dsa_host_core::disk::raw_copy::{
    checked_raw_copy, expected_raw, raw_copy_is_checked, raw_record, verify_raw_copy,
};

fn folder(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("raw-copy")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes the `image.json` of an image whose system disk, as raw, is `data`.
fn raw_described(dir: &Path, data: &[u8]) {
    let path = dir.join("described");
    std::fs::write(&path, data).unwrap();
    let (bytes, sha256) = file_sha256(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let manifest = serde_json::json!({"system_disk_raw": {"bytes": bytes, "sha256": sha256}});
    std::fs::write(dir.join("image.json"), manifest.to_string()).unwrap();
}

/// A host's writer: the copy is the source's bytes.
fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[test]
fn nfr1_a_raw_system_disk_is_used_only_as_the_image_describes_it() {
    let dir = folder("checked");
    let raw = dir.join("system.img");
    std::fs::write(&raw, b"the system disk").unwrap();
    raw_described(&dir, b"the system disk");
    // No VHDX to make it again from: checked now, recorded, and used as it is.
    assert_eq!(checked_raw_copy(&raw, None, &dir, &copy), Ok(false));
    assert!(raw_copy_is_checked(&raw, expected_raw(&dir, &raw).as_ref()));
    // Changed after it was checked: refused, since nothing can make it again.
    std::fs::write(&raw, b"the system disk, changed").unwrap();
    let why = checked_raw_copy(&raw, None, &dir, &copy).unwrap_err();
    assert!(why.contains("not the image's system disk"), "{why}");
}

#[test]
fn nfr1_a_copy_that_changed_is_made_again_from_the_source() {
    let dir = folder("remake");
    let source = dir.join("system.vhdx");
    let raw = dir.join("system.img");
    std::fs::write(&source, b"the system disk").unwrap();
    raw_described(&dir, b"the system disk");
    // No copy yet: made, and reported as made.
    assert_eq!(checked_raw_copy(&raw, Some(&source), &dir, &copy), Ok(true));
    // A checked copy is kept, not written again.
    assert_eq!(
        checked_raw_copy(&raw, Some(&source), &dir, &copy),
        Ok(false)
    );
    // Changed: made again from the source.
    std::fs::write(&raw, b"tampered").unwrap();
    assert_eq!(checked_raw_copy(&raw, Some(&source), &dir, &copy), Ok(true));
    assert_eq!(std::fs::read(&raw).unwrap(), b"the system disk");
    // A source that is not the image's system disk leaves no copy behind.
    std::fs::write(&source, b"another disk").unwrap();
    std::fs::remove_file(&raw).unwrap();
    assert!(checked_raw_copy(&raw, Some(&source), &dir, &copy).is_err());
    assert!(!raw.exists());
}

#[test]
fn nfr1_a_copy_that_cannot_be_written_is_an_error_and_leaves_none() {
    let dir = folder("unwritable");
    let source = dir.join("system.vhdx");
    let raw = dir.join("system.img");
    std::fs::write(&source, b"x").unwrap();
    let broken = |_: &Path, _: &Path| Err("not a VHDX".to_string());
    assert_eq!(
        checked_raw_copy(&raw, Some(&source), &dir, &broken),
        Err("not a VHDX".to_string())
    );
    assert!(!raw.exists());
    // Without a copy or a source there is nothing to use.
    assert!(checked_raw_copy(&raw, None, &dir, &copy).is_err());
}

#[test]
fn nfr1_only_the_system_disk_is_held_to_the_images_hash() {
    let dir = folder("template");
    raw_described(&dir, b"the system disk");
    let template = dir.join("data.img");
    std::fs::write(&template, b"a user disk template").unwrap();
    assert_eq!(expected_raw(&dir, &template), None);
    assert_eq!(checked_raw_copy(&template, None, &dir, &copy), Ok(false));
}

#[test]
fn nfr1_a_tampered_raw_copy_is_removed_when_the_host_checks_it() {
    let dir = folder("verify");
    let source = dir.join("system.vhdx");
    let raw = dir.join("system.img");
    std::fs::write(&source, b"x").unwrap();
    std::fs::write(&raw, b"the system disk").unwrap();
    raw_described(&dir, b"the system disk");
    assert_eq!(verify_raw_copy(&raw, &source, &dir), Ok(true));
    assert!(raw_copy_is_checked(&raw, expected_raw(&dir, &raw).as_ref()));
    // The same size, other bytes, and its time put back: the byte check finds it.
    let before = std::fs::metadata(&raw).unwrap().modified().unwrap();
    std::fs::write(&raw, b"the system disc").unwrap();
    File::options()
        .write(true)
        .open(&raw)
        .unwrap()
        .set_modified(before)
        .unwrap();
    assert_eq!(verify_raw_copy(&raw, &source, &dir), Ok(false));
    assert!(!raw.exists());
    assert!(!raw_record(&raw).exists());
}

#[test]
fn nfr1_a_copy_with_no_source_to_make_it_again_is_left_alone_by_the_check() {
    let dir = folder("verify-no-source");
    let raw = dir.join("system.img");
    std::fs::write(&raw, b"disk").unwrap();
    assert_eq!(
        verify_raw_copy(&raw, &dir.join("system.vhdx"), &dir),
        Ok(false)
    );
    assert!(raw.exists());
}
