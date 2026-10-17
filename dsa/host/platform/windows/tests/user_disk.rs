//! Each user's data disk, created for real (Windows only).
#![cfg(windows)]

use std::path::PathBuf;

use dsa_host_windows::user_disk::{access_list, prepare_user_data_disk};

fn fresh_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("user-disk")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn pr5_a_new_users_data_disk_is_created_as_a_growing_vhdx() {
    let disk = fresh_dir("new")
        .join("S-1-5-21-1-2-3-1001")
        .join("data.vhdx");
    prepare_user_data_disk(&disk, 8).unwrap();
    let bytes = std::fs::read(&disk).unwrap();
    assert_eq!(&bytes[..8], b"vhdxfile", "the disk is a VHDX");
    assert!(
        bytes.len() < 64 << 20,
        "a new disk starts small and grows ({} bytes)",
        bytes.len()
    );
}

#[test]
fn pr5_an_existing_data_disk_is_kept() {
    let disk = fresh_dir("keep")
        .join("S-1-5-21-1-2-3-1001")
        .join("data.vhdx");
    prepare_user_data_disk(&disk, 8).unwrap();
    let before = std::fs::read(&disk).unwrap();
    prepare_user_data_disk(&disk, 16).unwrap();
    assert_eq!(std::fs::read(&disk).unwrap(), before);
}

#[test]
fn pr5_only_the_system_administrators_and_vms_may_open_a_users_folder() {
    let disk = fresh_dir("acl")
        .join("S-1-5-21-1-2-3-1001")
        .join("data.vhdx");
    prepare_user_data_disk(&disk, 8).unwrap();
    let folder = access_list(disk.parent().unwrap()).unwrap();
    assert!(
        folder.starts_with("D:P"),
        "the folder inherits nothing: {folder}"
    );
    for allowed in ["SY", "BA", "S-1-5-83-0"] {
        assert!(
            folder.contains(&format!(";;;{allowed})")),
            "{allowed} missing: {folder}"
        );
    }
    for denied in ["BU", "AU", "WD", "IU"] {
        assert!(
            !folder.contains(&format!(";;;{denied})")),
            "{denied} allowed: {folder}"
        );
    }
    let file = access_list(&disk).unwrap();
    assert!(
        !file.contains(";;;BU)") && !file.contains(";;;AU)"),
        "{file}"
    );
}
