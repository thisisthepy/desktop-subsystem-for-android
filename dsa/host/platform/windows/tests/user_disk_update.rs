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
    bring_disk_to, disk_image_version, prepare_user_data_disk, AttachedDisk,
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
    for (i, (name, sectors)) in LAYOUT.iter().enumerate() {
        let entry = &mut table[i * 128..(i + 1) * 128];
        entry[0] = 1;
        entry[16] = i as u8 + 1;
        entry[32..40].copy_from_slice(&lba.to_le_bytes());
        entry[40..48].copy_from_slice(&(lba + sectors - 1).to_le_bytes());
        for (j, u) in name.encode_utf16().enumerate() {
            entry[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
        disk.seek(SeekFrom::Start(lba * SECTOR)).unwrap();
        disk.write_all(&vec![fill; (sectors * SECTOR) as usize])
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
