//! The user's Android disk as a raw file: the disk core's backend on Linux and macOS, run
//! through the core's algorithm.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use dsa_host_core::disk::gpt::partitions;
use dsa_host_core::disk::{bring_disk_to, disk_image_version, DiskBackend};
use dsa_host_unix::raw_disk::RawDiskBackend;
use dsa_host_unix::vhdx::{
    guid_bytes, BAT_REGION, FILE_PARAMETERS, LOGICAL_SECTOR_SIZE, METADATA_REGION,
    VIRTUAL_DISK_SIZE,
};

const SECTOR: usize = 512;
const MIB: usize = 1 << 20;

fn folder(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("raw-disk")
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

fn byte_of(disk: &Path, mib: usize) -> u8 {
    std::fs::read(disk).unwrap()[mib * MIB + 7]
}

fn put(file: &mut [u8], at: usize, bytes: &[u8]) {
    file[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A dynamic VHDX of three 1 MiB blocks: the first and last hold data, the middle one was
/// never written.
fn small_vhdx(log: bool) -> Vec<u8> {
    let mut file = vec![0u8; 5 * MIB];
    put(&mut file, 0, b"vhdxfile");
    let header = 64 * 1024;
    put(&mut file, header, b"head");
    put(&mut file, header + 8, &1u64.to_le_bytes());
    if log {
        put(&mut file, header + 48, &[1u8; 16]);
    }
    let regions = 192 * 1024;
    put(&mut file, regions, b"regi");
    put(&mut file, regions + 8, &2u32.to_le_bytes());
    for (i, (id, at)) in [(BAT_REGION, MIB), (METADATA_REGION, 2 * MIB)]
        .into_iter()
        .enumerate()
    {
        let entry = regions + 16 + i * 32;
        put(&mut file, entry, &guid_bytes(id));
        put(&mut file, entry + 16, &(at as u64).to_le_bytes());
        put(&mut file, entry + 24, &(MIB as u32).to_le_bytes());
    }
    let meta = 2 * MIB;
    put(&mut file, meta, b"metadata");
    put(&mut file, meta + 10, &3u16.to_le_bytes());
    let items: [(&str, Vec<u8>); 3] = [
        (
            FILE_PARAMETERS,
            [(MIB as u32).to_le_bytes(), 0u32.to_le_bytes()].concat(),
        ),
        (VIRTUAL_DISK_SIZE, (3 * MIB as u64).to_le_bytes().to_vec()),
        (LOGICAL_SECTOR_SIZE, 512u32.to_le_bytes().to_vec()),
    ];
    for (i, (id, value)) in items.iter().enumerate() {
        let entry = meta + 32 + i * 32;
        let offset = 64 * 1024 + i * 64;
        put(&mut file, entry, &guid_bytes(id));
        put(&mut file, entry + 16, &(offset as u32).to_le_bytes());
        put(&mut file, entry + 20, &(value.len() as u32).to_le_bytes());
        put(&mut file, meta + offset, value);
    }
    let bat = MIB;
    put(&mut file, bat, &(6u64 | (3u64 << 20)).to_le_bytes());
    put(&mut file, bat + 16, &(6u64 | (4u64 << 20)).to_le_bytes());
    file[3 * MIB..4 * MIB].fill(0xa5);
    file[4 * MIB + 100] = 0x5a;
    file
}

#[test]
fn fr28_a_new_raw_disk_is_a_copy_of_the_template() {
    let dir = folder("new");
    let template = dir.join("disk.img");
    std::fs::write(
        &template,
        gpt_disk(&[("super", 1, 0x11), ("userdata", 1, 0)]),
    )
    .unwrap();
    let disk = dir.join("user").join("android.img");
    bring_disk_to(&RawDiskBackend, &disk, &template, "16.0.0-1").unwrap();
    assert_eq!(
        std::fs::read(&disk).unwrap(),
        std::fs::read(&template).unwrap()
    );
    assert_eq!(disk_image_version(&disk).as_deref(), Some("16.0.0-1"));
}

#[test]
fn fr28_a_raw_disk_of_another_version_gets_android_and_keeps_the_users_data() {
    let dir = folder("update");
    let old = dir.join("old").join("disk.img");
    let new = dir.join("new").join("disk.img");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::write(&old, gpt_disk(&[("super", 1, 0x11), ("userdata", 1, 0)])).unwrap();
    std::fs::write(&new, gpt_disk(&[("super", 1, 0x22), ("userdata", 1, 0)])).unwrap();
    let disk = dir.join("android.img");
    bring_disk_to(&RawDiskBackend, &disk, &old, "1").unwrap();
    // Android writes the user's data.
    let mut bytes = std::fs::read(&disk).unwrap();
    bytes[2 * MIB..3 * MIB].fill(0x99);
    std::fs::write(&disk, bytes).unwrap();
    bring_disk_to(&RawDiskBackend, &disk, &new, "2").unwrap();
    assert_eq!(byte_of(&disk, 1), 0x22, "Android comes from the new image");
    assert_eq!(byte_of(&disk, 2), 0x99, "the user's data stays");
    assert_eq!(disk_image_version(&disk).as_deref(), Some("2"));
}

#[test]
fn fr28_a_raw_disk_from_a_one_disk_image_drops_androids_partitions_for_a_two_disk_one() {
    let dir = folder("two");
    let single = dir.join("disk.img");
    let data = dir.join("data.img");
    std::fs::write(&single, gpt_disk(&[("super", 1, 0x11), ("userdata", 1, 0)])).unwrap();
    std::fs::write(&data, gpt_disk(&[("userdata", 1, 0)])).unwrap();
    let disk = dir.join("android.img");
    bring_disk_to(&RawDiskBackend, &disk, &single, "1").unwrap();
    bring_disk_to(&RawDiskBackend, &disk, &data, "2").unwrap();
    let names: Vec<String> = partitions(&mut File::open(&disk).unwrap())
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["userdata"]);
}

#[test]
fn d9_an_empty_raw_disk_is_sparse_and_an_existing_one_is_kept() {
    let dir = folder("empty");
    let disk = dir.join("android.img");
    RawDiskBackend.create(&disk, None, 1).unwrap();
    assert_eq!(std::fs::metadata(&disk).unwrap().len(), 1 << 30);
    std::fs::write(&disk, b"kept").unwrap();
    RawDiskBackend.create(&disk, None, 1).unwrap();
    assert_eq!(std::fs::read(&disk).unwrap(), b"kept");
}

#[test]
fn d9_a_vhdx_template_is_read_through_and_copied_as_a_raw_disk() {
    let dir = folder("vhdx");
    let vhdx = dir.join("disk.vhdx");
    std::fs::write(&vhdx, small_vhdx(false)).unwrap();
    let mut device = RawDiskBackend.open(&vhdx, false).unwrap();
    let mut byte = [0u8; 1];
    device.read_exact(&mut byte).unwrap();
    assert_eq!(byte[0], 0xa5);
    device.seek(SeekFrom::Start(2 * MIB as u64 + 100)).unwrap();
    device.read_exact(&mut byte).unwrap();
    assert_eq!(byte[0], 0x5a);
    // The end of the disk reads nothing, and a VHDX is never written.
    assert_eq!(device.seek(SeekFrom::End(0)).unwrap(), 3 * MIB as u64);
    assert_eq!(device.read(&mut byte).unwrap(), 0);
    assert!(device.write(&byte).is_err());

    let disk = dir.join("user").join("android.img");
    RawDiskBackend.create(&disk, Some(&vhdx), 0).unwrap();
    let raw = std::fs::read(&disk).unwrap();
    assert_eq!(raw.len(), 3 * MIB);
    assert_eq!(raw[0], 0xa5);
    assert_eq!(raw[2 * MIB + 100], 0x5a);
}
