//! The raw disks of the macOS host: an image's disk read as a raw disk, and the pstore disk.
//! The user's disk across image updates is checked in the host core and on `RawDiskBackend`.

use std::path::PathBuf;

use dsa_host_macos::user_disk::{ensure_pstore_disk, raw_disk};

mod common;
use common::{small_vhdx, MIB};

fn folder(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("user-disk")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fr2_a_raw_disk_is_used_as_it_is() {
    let dir = folder("raw");
    let raw = dir.join("system.img");
    std::fs::write(&raw, [0u8; 4096]).unwrap();
    assert_eq!(raw_disk(&raw, &dir.join("cache")).unwrap(), raw);
}

#[test]
fn fr21_the_pstore_disk_is_made_once_and_kept() {
    let dir = folder("pstore");
    let disk = dir.join("pstore.img");
    ensure_pstore_disk(&disk).unwrap();
    let bytes = std::fs::read(&disk).unwrap();
    assert_eq!(bytes.len(), 4 << 20);
    assert_eq!(&bytes[512..520], b"EFI PART");
    // A record the kernel wrote stays.
    let mut kept = bytes.clone();
    kept[2 << 20] = 7;
    std::fs::write(&disk, &kept).unwrap();
    ensure_pstore_disk(&disk).unwrap();
    assert_eq!(std::fs::read(&disk).unwrap()[2 << 20], 7);
}

#[test]
fn nfr1_a_vhdx_disk_is_converted_once_and_checked_like_on_every_raw_host() {
    let dir = folder("converted");
    let vhdx = dir.join("system.vhdx");
    std::fs::write(&vhdx, small_vhdx(false)).unwrap();
    let cache = dir.join("cache");
    let raw = raw_disk(&vhdx, &cache).unwrap();
    assert_eq!(raw, cache.join("system.img"));
    assert_eq!(std::fs::metadata(&raw).unwrap().len(), 3 * MIB as u64);
    // The check is recorded beside the copy, as on Linux.
    assert!(cache.join("system.img.json").exists());
    // A copy that changed is made again from the VHDX.
    std::fs::write(&raw, b"tampered").unwrap();
    assert_eq!(raw_disk(&vhdx, &cache).unwrap(), raw);
    assert_eq!(std::fs::metadata(&raw).unwrap().len(), 3 * MIB as u64);
    // A copy the image does not describe is refused, and none is left.
    let manifest = serde_json::json!({"system_disk_raw": {"bytes": 1, "sha256": "00"}});
    std::fs::write(dir.join("image.json"), manifest.to_string()).unwrap();
    std::fs::remove_file(&raw).unwrap();
    assert!(raw_disk(&vhdx, &cache).is_err());
    assert!(!raw.exists());
}
