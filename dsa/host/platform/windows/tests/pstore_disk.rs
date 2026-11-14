//! The pstore disk the guest kernel keeps its last log on.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use dsa_host_windows::gpt::{partitions, SECTOR};
use dsa_host_windows::paths::{pstore_disk_beside, MachineRoot, UserRoot};
use dsa_host_windows::pstore_disk::{
    ensure, image, DISK_BYTES, FILE_BYTES, KERNEL_CMDLINE, PARTITION_FIRST_SECTOR,
    PARTITION_LAST_SECTOR, PARTITION_NAME, PARTITION_UUID,
};

fn dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn fr21_the_pstore_disk_has_a_fixed_size_that_never_grows() {
    let disk = image();
    assert_eq!(disk.len() as u64, FILE_BYTES);
    assert_eq!(DISK_BYTES, 4 << 20);
    // A fixed VHD: the disk's bytes, then the footer, whose size fields say the same size.
    let footer = &disk[DISK_BYTES as usize..];
    assert_eq!(&footer[0..8], b"conectix");
    assert_eq!(u32::from_be_bytes(footer[60..64].try_into().unwrap()), 2);
    assert_eq!(
        u64::from_be_bytes(footer[48..56].try_into().unwrap()),
        DISK_BYTES
    );
    // The footer's checksum is the one's complement of the sum of its other bytes.
    let stored = u32::from_be_bytes(footer[64..68].try_into().unwrap());
    let sum: u32 = footer
        .iter()
        .enumerate()
        .filter(|(i, _)| !(64..68).contains(i))
        .map(|(_, &b)| b as u32)
        .sum();
    assert_eq!(stored, !sum);
}

#[test]
fn fr21_the_pstore_disk_has_one_partition_the_kernel_finds_by_uuid() {
    let disk = image();
    let found = partitions(&mut Cursor::new(&disk[..DISK_BYTES as usize])).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, PARTITION_NAME);
    assert_eq!(found[0].start, PARTITION_FIRST_SECTOR * SECTOR);
    assert_eq!(
        found[0].len,
        (PARTITION_LAST_SECTOR - PARTITION_FIRST_SECTOR + 1) * SECTOR
    );
    // The backup header is in the last sector, as the kernel's GPT parser checks.
    let last = &disk[(DISK_BYTES - SECTOR) as usize..DISK_BYTES as usize];
    assert_eq!(&last[0..8], b"EFI PART");
    // The partition's unique GUID, stored with its first three fields little endian.
    let entry = &disk[(2 * SECTOR) as usize..(2 * SECTOR + 128) as usize];
    let guid = &entry[16..32];
    let text = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{}",
        guid[3],
        guid[2],
        guid[1],
        guid[0],
        guid[5],
        guid[4],
        guid[7],
        guid[6],
        guid[8],
        guid[9],
        guid[10..]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(text, PARTITION_UUID);
    assert!(
        KERNEL_CMDLINE.contains(&format!("pstore_blk.blkdev=PARTUUID={PARTITION_UUID}")),
        "{KERNEL_CMDLINE}"
    );
}

#[test]
fn fr21_an_existing_pstore_disk_keeps_its_records() {
    let path = dir("pstore-keeps").join("pstore.vhd");
    ensure(&path).unwrap();
    // The guest wrote a record into the partition.
    let mut disk = std::fs::read(&path).unwrap();
    let at = (PARTITION_FIRST_SECTOR * SECTOR) as usize;
    disk[at..at + 6].copy_from_slice(b"record");
    std::fs::write(&path, &disk).unwrap();

    ensure(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), disk);
}

#[test]
fn fr21_a_file_that_is_not_a_pstore_disk_is_replaced() {
    let path = dir("pstore-replaced").join("pstore.vhd");
    std::fs::write(&path, b"not a disk").unwrap();
    ensure(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), image());

    // The right size but a damaged partition table.
    let mut disk = image();
    disk[SECTOR as usize] = 0;
    std::fs::write(&path, &disk).unwrap();
    ensure(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), image());
}

#[test]
fn fr19_the_pstore_disk_is_in_the_machines_user_folder_and_uninstall_removes_the_old_one() {
    let machine = MachineRoot::at(r"C:\ProgramData\DSA");
    assert_eq!(
        machine.user_pstore_disk("S-1-5-21-1-2-3-1001"),
        PathBuf::from(r"C:\ProgramData\DSA\users\S-1-5-21-1-2-3-1001\pstore.vhd")
    );
    let root = UserRoot::from_dir(Path::new(r"C:\Users\me\AppData\Local\DSA"));
    assert_eq!(
        root.old_pstore_disk(),
        PathBuf::from(r"C:\Users\me\AppData\Local\DSA\pstore.vhd")
    );
    assert_eq!(
        pstore_disk_beside(&root.android_disk()),
        root.old_pstore_disk()
    );
    assert!(!UserRoot::kept_on_uninstall("pstore.vhd"));
}

#[test]
fn fr21_the_pstore_partition_name_is_not_one_of_androids() {
    assert_eq!(PARTITION_NAME, "dsa_pstore");
    let android = [
        "misc",
        "super",
        "userdata",
        "metadata",
        "vbmeta",
        "boot",
        "init_boot",
        "vendor_boot",
    ];
    for name in android {
        assert!(
            !PARTITION_NAME.starts_with(name),
            "{PARTITION_NAME} would share {name}'s by-name link"
        );
    }
}
