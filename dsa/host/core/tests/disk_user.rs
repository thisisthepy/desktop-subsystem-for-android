//! A user's Android disk across image updates, through the platform-neutral algorithm on a
//! backend of plain files. The hosts' own backends (an attached VHDX, a raw file) are
//! checked where they run.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use dsa_host_core::disk::gpt::partitions;
use dsa_host_core::disk::{
    bring_disk_to, disk_image_version, record_disk_image_version, version_file, DiskBackend,
};

const SECTOR: usize = 512;
const MIB: usize = 1 << 20;

/// Disks that are plain files. `placeholders` makes `has_partitions` say no, as a host does
/// for a file that is not a disk it can read.
#[derive(Default)]
struct Files {
    placeholders: bool,
    written_beside: RefCell<Vec<PathBuf>>,
}

impl DiskBackend for Files {
    type Device = File;

    fn open(&self, disk: &Path, write: bool) -> Result<File, String> {
        OpenOptions::new()
            .read(true)
            .write(write)
            .open(disk)
            .map_err(|e| format!("opening {}: {e}", disk.display()))
    }

    fn create(&self, disk: &Path, template: Option<&Path>, size_gb: u32) -> Result<(), String> {
        std::fs::create_dir_all(disk.parent().unwrap()).map_err(|e| e.to_string())?;
        match template {
            Some(template) => {
                let partial = disk.with_extension("img.new");
                std::fs::copy(template, &partial).map_err(|e| e.to_string())?;
                std::fs::rename(&partial, disk).map_err(|e| e.to_string())
            }
            None => File::create(disk)
                .and_then(|f| f.set_len(u64::from(size_gb) << 30))
                .map_err(|e| e.to_string()),
        }
    }

    fn write_beside(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        self.written_beside.borrow_mut().push(path.to_path_buf());
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }

    fn has_partitions(&self, _path: &Path) -> Result<bool, String> {
        Ok(!self.placeholders)
    }
}

fn folder(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("disk-user")
        .join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A raw disk with a primary GPT naming `parts` (name, MiB, fill), one MiB apart from the
/// start.
fn gpt_disk(parts: &[(&str, usize, u8)]) -> Vec<u8> {
    let total: usize = 1 + parts.iter().map(|(_, mib, _)| mib).sum::<usize>();
    let mut disk = vec![0u8; total * MIB];
    let header = SECTOR;
    disk[header..header + 8].copy_from_slice(b"EFI PART");
    disk[header + 72..header + 80].copy_from_slice(&2u64.to_le_bytes());
    disk[header + 80..header + 84].copy_from_slice(&128u32.to_le_bytes());
    disk[header + 84..header + 88].copy_from_slice(&128u32.to_le_bytes());
    let mut at = MIB;
    for (i, (name, mib, fill)) in parts.iter().enumerate() {
        let entry = 2 * SECTOR + i * 128;
        disk[entry] = 1 + i as u8;
        let first = (at / SECTOR) as u64;
        let last = ((at + mib * MIB) / SECTOR - 1) as u64;
        disk[entry + 32..entry + 40].copy_from_slice(&first.to_le_bytes());
        disk[entry + 40..entry + 48].copy_from_slice(&last.to_le_bytes());
        for (j, unit) in name.encode_utf16().enumerate() {
            disk[entry + 56 + j * 2..entry + 58 + j * 2].copy_from_slice(&unit.to_le_bytes());
        }
        disk[at..at + mib * MIB].fill(*fill);
        at += mib * MIB;
    }
    disk
}

fn write_disk(path: &Path, parts: &[(&str, usize, u8)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, gpt_disk(parts)).unwrap();
}

fn byte_of(disk: &Path, mib: usize) -> u8 {
    std::fs::read(disk).unwrap()[mib * MIB + 7]
}

fn names(disk: &Path) -> Vec<String> {
    partitions(&mut File::open(disk).unwrap())
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect()
}

/// Android writes the user's data into the MiB at `mib`.
fn write_user_data(disk: &Path, mib: usize, byte: u8) {
    let mut bytes = std::fs::read(disk).unwrap();
    bytes[mib * MIB..(mib + 1) * MIB].fill(byte);
    std::fs::write(disk, bytes).unwrap();
}

#[test]
fn fr28_new_disk_is_a_template_copy() {
    let dir = folder("new");
    let template = dir.join("disk.img");
    write_disk(&template, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    let disk = dir.join("user").join("android.img");
    bring_disk_to(&Files::default(), &disk, &template, "16.0.0-1").unwrap();
    assert_eq!(
        std::fs::read(&disk).unwrap(),
        std::fs::read(&template).unwrap()
    );
    assert_eq!(disk_image_version(&disk).as_deref(), Some("16.0.0-1"));
}

#[test]
fn fr28_other_version_gets_android_partitions_and_keeps_userdata() {
    let dir = folder("update");
    let old = dir.join("old").join("disk.img");
    let new = dir.join("new").join("disk.img");
    write_disk(&old, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    write_disk(&new, &[("super", 1, 0x22), ("userdata", 1, 0)]);
    let disk = dir.join("android.img");
    let backend = Files::default();
    bring_disk_to(&backend, &disk, &old, "1").unwrap();
    write_user_data(&disk, 2, 0x99);
    bring_disk_to(&backend, &disk, &new, "2").unwrap();
    assert_eq!(byte_of(&disk, 1), 0x22, "Android comes from the new image");
    assert_eq!(byte_of(&disk, 2), 0x99, "the user's data stays");
    assert_eq!(disk_image_version(&disk).as_deref(), Some("2"));
}

#[test]
fn fr28_a_disk_at_the_image_version_is_not_touched() {
    let dir = folder("same");
    let old = dir.join("old").join("disk.img");
    let other = dir.join("other").join("disk.img");
    write_disk(&old, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    write_disk(&other, &[("super", 1, 0x22), ("userdata", 1, 0)]);
    let disk = dir.join("android.img");
    let backend = Files::default();
    bring_disk_to(&backend, &disk, &old, "1").unwrap();
    bring_disk_to(&backend, &disk, &other, "1").unwrap();
    assert_eq!(byte_of(&disk, 1), 0x11);
}

#[test]
fn fr28_data_template_drops_stale_partitions() {
    let dir = folder("two");
    let single = dir.join("disk.img");
    let data = dir.join("data.img");
    write_disk(&single, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    write_disk(&data, &[("userdata", 1, 0)]);
    let disk = dir.join("android.img");
    let backend = Files::default();
    bring_disk_to(&backend, &disk, &single, "1").unwrap();
    write_user_data(&disk, 2, 0x77);
    bring_disk_to(&backend, &disk, &data, "2").unwrap();
    assert_eq!(names(&disk), ["userdata"]);
    assert_eq!(
        byte_of(&disk, 2),
        0x77,
        "the user's data stays where it was"
    );
    assert_eq!(disk_image_version(&disk).as_deref(), Some("2"));
}

#[test]
fn fr28_data_template_drops_stale_partitions_of_a_disk_recorded_at_its_image() {
    let dir = folder("recorded");
    let data = dir.join("data.img");
    write_disk(&data, &[("userdata", 1, 0)]);
    let disk = dir.join("android.img");
    write_disk(&disk, &[("super", 1, 0x11), ("userdata", 1, 0x77)]);
    let backend = Files::default();
    record_disk_image_version(&backend, &disk, "2").unwrap();
    bring_disk_to(&backend, &disk, &data, "2").unwrap();
    assert_eq!(names(&disk), ["userdata"]);
}

#[test]
fn fr28_blank_unrecorded_disk_is_replaced() {
    let dir = folder("empty");
    let disk = dir.join("android.img");
    let backend = Files::default();
    backend.create(&disk, None, 0).unwrap();
    File::options()
        .write(true)
        .open(&disk)
        .unwrap()
        .set_len(4 * MIB as u64)
        .unwrap();
    assert_eq!(disk_image_version(&disk), None);
    let template = dir.join("disk.img");
    write_disk(&template, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    bring_disk_to(&backend, &disk, &template, "1").unwrap();
    assert_eq!(byte_of(&disk, 1), 0x11);
    assert_eq!(disk_image_version(&disk).as_deref(), Some("1"));
}

#[test]
fn fr28_unused_disk_with_new_layout_is_replaced() {
    let dir = folder("layout");
    let old = dir.join("old").join("disk.img");
    let new = dir.join("new").join("disk.img");
    write_disk(&old, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    write_disk(
        &new,
        &[("boot_a", 1, 0x22), ("super", 1, 0x22), ("userdata", 1, 0)],
    );
    let disk = dir.join("android.img");
    let backend = Files::default();
    bring_disk_to(&backend, &disk, &old, "1").unwrap();
    bring_disk_to(&backend, &disk, &new, "2").unwrap();
    assert_eq!(names(&disk), ["boot_a", "super", "userdata"]);
    assert_eq!(disk_image_version(&disk).as_deref(), Some("2"));
}

#[test]
fn fr28_used_disk_with_new_layout_is_left_as_it_is() {
    let dir = folder("layout-used");
    let old = dir.join("old").join("disk.img");
    let new = dir.join("new").join("disk.img");
    write_disk(&old, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    write_disk(
        &new,
        &[("boot_a", 1, 0x22), ("super", 1, 0x22), ("userdata", 1, 0)],
    );
    let disk = dir.join("android.img");
    let backend = Files::default();
    bring_disk_to(&backend, &disk, &old, "1").unwrap();
    write_user_data(&disk, 2, 0x77);
    let why = bring_disk_to(&backend, &disk, &new, "2").unwrap_err();
    assert!(why.contains("holds Android data"), "{why}");
    assert_eq!(byte_of(&disk, 2), 0x77);
    assert_eq!(disk_image_version(&disk).as_deref(), Some("1"));
}

#[test]
fn fr28_unused_data_disk_without_a_template_partition_becomes_the_template() {
    let dir = folder("gains-misc");
    let data = dir.join("v2").join("data.img");
    write_disk(
        &data,
        &[("misc", 1, 0), ("metadata", 1, 0), ("userdata", 1, 0)],
    );
    let disk = dir.join("android.img");
    write_disk(&disk, &[("metadata", 1, 0), ("userdata", 1, 0)]);
    let backend = Files::default();
    record_disk_image_version(&backend, &disk, "1").unwrap();
    bring_disk_to(&backend, &disk, &data, "2").unwrap();
    assert_eq!(names(&disk), ["misc", "metadata", "userdata"]);
    assert_eq!(disk_image_version(&disk).as_deref(), Some("2"));
}

#[test]
fn fr28_used_data_disk_without_a_template_partition_is_left_as_it_is() {
    let dir = folder("used-data");
    let data = dir.join("v2").join("data.img");
    write_disk(
        &data,
        &[("misc", 1, 0), ("metadata", 1, 0), ("userdata", 1, 0)],
    );
    let disk = dir.join("android.img");
    write_disk(&disk, &[("metadata", 1, 0), ("userdata", 1, 0x77)]);
    let backend = Files::default();
    record_disk_image_version(&backend, &disk, "1").unwrap();
    let why = bring_disk_to(&backend, &disk, &data, "2").unwrap_err();
    assert!(why.contains("misc"), "{why}");
    assert_eq!(byte_of(&disk, 2), 0x77);
    assert_eq!(disk_image_version(&disk).as_deref(), Some("1"));
}

#[test]
fn fr28_a_data_template_that_is_not_a_readable_disk_keeps_the_users_disk() {
    let dir = folder("placeholder");
    let template = dir.join("v1").join("data.vhdx");
    std::fs::create_dir_all(template.parent().unwrap()).unwrap();
    std::fs::write(&template, b"data-template-v1").unwrap();
    let backend = Files {
        placeholders: true,
        ..Files::default()
    };
    let disk = dir.join("user").join("data.vhdx");
    bring_disk_to(&backend, &disk, &template, "v1").unwrap();
    assert_eq!(std::fs::read(&disk).unwrap(), b"data-template-v1");
    std::fs::write(&disk, b"user-stored-android-data").unwrap();
    let v2 = dir.join("v2").join("data.vhdx");
    std::fs::create_dir_all(v2.parent().unwrap()).unwrap();
    std::fs::write(&v2, b"data-template-v2").unwrap();
    bring_disk_to(&backend, &disk, &v2, "v2").unwrap();
    assert_eq!(disk_image_version(&disk).as_deref(), Some("v2"));
    assert_eq!(std::fs::read(&disk).unwrap(), b"user-stored-android-data");
}

#[test]
fn d9_the_version_record_is_written_through_the_backend_beside_the_disk() {
    let dir = folder("beside");
    let template = dir.join("disk.img");
    write_disk(&template, &[("super", 1, 0x11), ("userdata", 1, 0)]);
    let disk = dir.join("android.img");
    let backend = Files::default();
    bring_disk_to(&backend, &disk, &template, "1").unwrap();
    assert_eq!(*backend.written_beside.borrow(), [version_file(&disk)]);
}
