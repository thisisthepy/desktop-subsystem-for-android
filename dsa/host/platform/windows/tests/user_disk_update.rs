//! A user's Android disk on a real attached VHDX: made from the image's template, and
//! given a new image's Android while keeping the user's data (Windows only).
//!
//! It attaches disks to the machine, so it runs only when `DSA_SERVICE_TESTS=1` (CI sets
//! it on the Windows runner) and needs administrator rights.
#![cfg(windows)]

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use dsa_host_windows::gpt::{partitions, SECTOR};
use dsa_host_windows::user_disk::{
    bring_disk_to, disk_image_version, prepare_user_data_disk, record_disk_image_version,
    AttachedDisk,
};

const LAYOUT: [(&str, u64); 5] = [
    ("misc", 2048),
    ("metadata", 2048),
    ("vbmeta_a", 2048),
    ("super", 8192),
    ("userdata", 8192),
];

/// Makes a VHDX at `path` with LAYOUT, each partition filled with `fill`.
fn make_disk(path: &Path, fill: u8) {
    make_disk_with(path, &LAYOUT, |_| fill);
}

/// Makes a VHDX at `path` with `layout`, each partition filled with `fill(name)`.
fn make_disk_with(path: &Path, layout: &[(&str, u64)], fill: impl Fn(&str) -> u8) {
    prepare_user_data_disk(path, 1).unwrap();
    let attached = AttachedDisk::attach(path, false).unwrap();
    let mut disk = attached.open(true).unwrap();
    let mut header = vec![0u8; 512];
    header[0..8].copy_from_slice(b"EFI PART");
    header[72..80].copy_from_slice(&2u64.to_le_bytes());
    header[80..84].copy_from_slice(&128u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());
    let mut table = vec![0u8; 128 * 128];
    let mut lba: u64 = 2048;
    for (i, (name, sectors)) in layout.iter().enumerate() {
        let entry = &mut table[i * 128..(i + 1) * 128];
        entry[0] = 1;
        entry[16] = i as u8 + 1;
        entry[32..40].copy_from_slice(&lba.to_le_bytes());
        entry[40..48].copy_from_slice(&(lba + sectors - 1).to_le_bytes());
        for (j, u) in name.encode_utf16().enumerate() {
            entry[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
        disk.seek(SeekFrom::Start(lba * SECTOR)).unwrap();
        disk.write_all(&vec![fill(name); (sectors * SECTOR) as usize])
            .unwrap();
        lba += sectors;
    }
    let mut first = vec![0u8; 512];
    disk.seek(SeekFrom::Start(SECTOR)).unwrap();
    first.copy_from_slice(&header);
    disk.write_all(&first).unwrap();
    disk.seek(SeekFrom::Start(2 * SECTOR)).unwrap();
    disk.write_all(&table).unwrap();
    disk.flush().unwrap();
}

fn read_partition(path: &Path, name: &str) -> Vec<u8> {
    let attached = AttachedDisk::attach(path, true).unwrap();
    let mut disk = attached.open(false).unwrap();
    let p = partitions(&mut disk)
        .unwrap()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap();
    let mut data = vec![0u8; p.len as usize];
    disk.seek(SeekFrom::Start(p.start)).unwrap();
    disk.read_exact(&mut data).unwrap();
    data
}

#[test]
fn fr14_a_users_disk_gets_the_new_android_and_keeps_its_data() {
    if std::env::var_os("DSA_SERVICE_TESTS").is_none() {
        eprintln!("skipped: set DSA_SERVICE_TESTS=1 to attach disks");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("disk-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let template_v1 = dir.join("v1").join("disk.vhdx");
    let template_v2 = dir.join("v2").join("disk.vhdx");
    make_disk(&template_v1, 0x11);
    make_disk(&template_v2, 0x22);

    let user = dir.join("S-1-5-21-1-2-3-1001").join("data.vhdx");
    bring_disk_to(&user, &template_v1, "v1").unwrap();
    assert_eq!(disk_image_version(&user).as_deref(), Some("v1"));
    assert!(
        read_partition(&user, "super").iter().all(|&b| b == 0x11),
        "a new disk is the template"
    );

    // The user's data changes, as it would while Android runs.
    {
        let attached = AttachedDisk::attach(&user, false).unwrap();
        let mut disk = attached.open(true).unwrap();
        let p = partitions(&mut disk)
            .unwrap()
            .into_iter()
            .find(|p| p.name == "userdata")
            .unwrap();
        disk.seek(SeekFrom::Start(p.start)).unwrap();
        disk.write_all(&vec![0x77; p.len as usize]).unwrap();
    }

    bring_disk_to(&user, &template_v2, "v2").unwrap();
    assert_eq!(disk_image_version(&user).as_deref(), Some("v2"));
    assert!(
        read_partition(&user, "super").iter().all(|&b| b == 0x22),
        "Android is the new image's"
    );
    assert!(read_partition(&user, "vbmeta_a").iter().all(|&b| b == 0x22));
    assert!(
        read_partition(&user, "userdata").iter().all(|&b| b == 0x77),
        "the user's data stays"
    );
    assert!(
        read_partition(&user, "metadata").iter().all(|&b| b == 0x11),
        "metadata stays"
    );

    // Bringing a disk to the version it already holds changes nothing.
    bring_disk_to(&user, &template_v1, "v2").unwrap();
    assert!(read_partition(&user, "super").iter().all(|&b| b == 0x22));
}

#[test]
fn fr14_a_blank_disk_made_before_any_image_was_downloaded_becomes_the_template() {
    if std::env::var_os("DSA_SERVICE_TESTS").is_none() {
        eprintln!("skipped: set DSA_SERVICE_TESTS=1 to attach disks");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("disk-blank-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let template = dir.join("v1").join("disk.vhdx");
    make_disk(&template, 0x11);

    // 0.1.1 made an empty disk when a user asked for Android before the first image was in.
    let user = dir.join("user").join("android.vhdx");
    prepare_user_data_disk(&user, 1).unwrap();
    assert_eq!(disk_image_version(&user), None);

    bring_disk_to(&user, &template, "v1").unwrap();
    assert_eq!(disk_image_version(&user).as_deref(), Some("v1"));
    assert!(
        read_partition(&user, "super").iter().all(|&b| b == 0x11),
        "the blank disk is replaced by the template"
    );
}

#[test]
fn fr14_an_unused_disk_of_an_older_layout_becomes_the_new_template() {
    if std::env::var_os("DSA_SERVICE_TESTS").is_none() {
        eprintln!("skipped: set DSA_SERVICE_TESTS=1 to attach disks");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("disk-layout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let newer: [(&str, u64); 6] = [
        ("misc", 2048),
        ("metadata", 2048),
        ("vbmeta_a", 2048),
        ("boot_a", 2048),
        ("super", 8192),
        ("userdata", 8192),
    ];
    let template = dir.join("v2").join("disk.vhdx");
    make_disk_with(&template, &newer, |_| 0x22);

    // Made from an image without boot_a, and Android never booted from it.
    let unused = dir.join("unused").join("android.vhdx");
    make_disk_with(
        &unused,
        &LAYOUT,
        |name| if name == "userdata" { 0 } else { 0x11 },
    );
    record_disk_image_version(&unused, "v1").unwrap();
    bring_disk_to(&unused, &template, "v2").unwrap();
    assert_eq!(disk_image_version(&unused).as_deref(), Some("v2"));
    assert!(read_partition(&unused, "boot_a").iter().all(|&b| b == 0x22));

    // The same layout with Android data in it is left alone, with the reason.
    let used = dir.join("used").join("android.vhdx");
    make_disk_with(&used, &LAYOUT, |_| 0x11);
    record_disk_image_version(&used, "v1").unwrap();
    let why = bring_disk_to(&used, &template, "v2").unwrap_err();
    assert!(why.contains("holds Android data"), "{why}");
    assert!(read_partition(&used, "userdata").iter().all(|&b| b == 0x11));
}

#[test]
fn fr14_two_disk_data_template_creates_user_disk_and_preserves_data_on_update() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("two-disk-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let template = dir.join("v1").join("data.vhdx");
    std::fs::create_dir_all(template.parent().unwrap()).unwrap();
    std::fs::write(&template, b"data-template-v1").unwrap();

    let user = dir.join("S-1-5-21-1-2-3-1001").join("data.vhdx");
    bring_disk_to(&user, &template, "v1").unwrap();
    assert_eq!(disk_image_version(&user).as_deref(), Some("v1"));
    assert_eq!(std::fs::read(&user).unwrap(), b"data-template-v1");

    // Simulate user writing actual data into their disk
    std::fs::write(&user, b"user-stored-android-data").unwrap();

    // Now update to v2 using v2 data.vhdx
    let template_v2 = dir.join("v2").join("data.vhdx");
    std::fs::create_dir_all(template_v2.parent().unwrap()).unwrap();
    std::fs::write(&template_v2, b"data-template-v2").unwrap();

    bring_disk_to(&user, &template_v2, "v2").unwrap();
    assert_eq!(disk_image_version(&user).as_deref(), Some("v2"));
    // User data MUST NOT be overwritten!
    assert_eq!(std::fs::read(&user).unwrap(), b"user-stored-android-data");
}

#[test]
fn fr14_a_disk_from_a_one_disk_image_loses_androids_partitions_and_keeps_its_data() {
    if std::env::var_os("DSA_SERVICE_TESTS").is_none() {
        eprintln!("skipped: set DSA_SERVICE_TESTS=1 to attach disks");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("one-to-two-disks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // The user's disk from an image that kept Android on it, used by Android since.
    let user = dir.join("S-1-5-21-1-2-3-1001").join("data.vhdx");
    make_disk_with(&user, &LAYOUT, |name| match name {
        "userdata" => 0x77,
        "metadata" => 0x6d,
        _ => 0x11,
    });
    record_disk_image_version(&user, "v1").unwrap();
    // The new image keeps Android on a disk of its own; its data template has only these.
    let template = dir.join("v2").join("data.vhdx");
    make_disk_with(&template, &[("metadata", 2048), ("userdata", 8192)], |_| 0);

    bring_disk_to(&user, &template, "v2").unwrap();

    assert_eq!(disk_image_version(&user).as_deref(), Some("v2"));
    let names: Vec<String> = {
        let attached = AttachedDisk::attach(&user, true).unwrap();
        let mut disk = attached.open(false).unwrap();
        partitions(&mut disk)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect()
    };
    assert_eq!(
        names,
        ["metadata", "userdata"],
        "no stale Android partition is left"
    );
    assert!(
        read_partition(&user, "userdata").iter().all(|&b| b == 0x77),
        "the user's data stays"
    );
    assert!(read_partition(&user, "metadata").iter().all(|&b| b == 0x6d));
}
