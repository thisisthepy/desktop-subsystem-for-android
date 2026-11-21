//! The pstore disk: a small disk of fixed size where the guest kernel keeps its last log.
//!
//! The VM ends when the guest panics or resets, and the next start is a new VM with empty
//! memory, so a log kept in RAM (ramoops) is gone by then. The guest kernel's pstore/blk
//! writes its console, an oops's report and Android's last log lines (pmsg) to this disk
//! instead, and shows them in `/sys/fs/pstore` at the next boot.
//!
//! On Windows the disk is a fixed VHD: the disk's bytes followed by a 512-byte footer, so it
//! never grows and this module writes it as a plain file, with no Windows API. The other hosts
//! attach the disk's bytes alone, as a raw file ([`raw_image`], [`ensure_raw`]). It holds one GPT partition
//! with a fixed partition UUID, [`PARTITION_UUID`], which the kernel command line names
//! ([`KERNEL_CMDLINE`]). Finding the partition by its UUID does not depend on the order the
//! guest names its disks in.
//!
//! The disk is not the user's data: uninstalling removes it, and a disk that is missing or
//! not the one this module makes is written again from scratch.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use super::gpt::{crc32, SECTOR};
use crate::vhdx::guid_bytes;

/// The size of the disk the guest sees: 4 MiB.
pub const DISK_BYTES: u64 = 4 << 20;
/// The size of the file: the disk and the VHD footer.
pub const FILE_BYTES: u64 = DISK_BYTES + FOOTER_BYTES;
const FOOTER_BYTES: u64 = 512;
const SECTORS: u64 = DISK_BYTES / SECTOR;

/// The UUID of the disk's one partition, as the kernel prints and looks it up.
pub const PARTITION_UUID: &str = "5f8b3c2e-9a41-4d6e-b07c-1e2d3f4a5b6c";
/// The partition's name in the partition table. Android makes a `/dev/block/by-name` link for
/// the partitions of every disk, so the name is DSA's own and never one of Android's.
pub const PARTITION_NAME: &str = "dsa_pstore";
const DISK_GUID: &str = "2c7d9e1f-6b3a-4f58-8e0d-a9b8c7d6e5f4";
/// GPT's type for Linux file system data, the generic Linux type.
const LINUX_DATA_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";

const ENTRIES: u64 = 128;
const ENTRY_BYTES: u64 = 128;
const ENTRY_SECTORS: u64 = ENTRIES * ENTRY_BYTES / SECTOR;
const FIRST_USABLE: u64 = 2 + ENTRY_SECTORS;
const LAST_USABLE: u64 = SECTORS - 2 - ENTRY_SECTORS;
/// The partition starts at 1 MiB, as partitioning tools align them.
pub const PARTITION_FIRST_SECTOR: u64 = 2048;
pub const PARTITION_LAST_SECTOR: u64 = LAST_USABLE;

/// The pstore/blk settings for the kernel command line: the partition, writing without a
/// storage driver's own panic support (the generic block path), and how the space is split.
/// An oops record takes 64 KiB and there are as many as the rest of the partition holds, the
/// console and pmsg take 256 KiB each.
pub const KERNEL_CMDLINE: &str = "pstore_blk.blkdev=PARTUUID=5f8b3c2e-9a41-4d6e-b07c-1e2d3f4a5b6c \
     pstore_blk.best_effort=y pstore_blk.kmsg_size=64 pstore_blk.console_size=256 \
     pstore_blk.pmsg_size=256";

fn gpt_header(current: u64, backup: u64, entries_lba: u64, entries_crc: u32) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&current.to_le_bytes());
    h[32..40].copy_from_slice(&backup.to_le_bytes());
    h[40..48].copy_from_slice(&FIRST_USABLE.to_le_bytes());
    h[48..56].copy_from_slice(&LAST_USABLE.to_le_bytes());
    h[56..72].copy_from_slice(&guid_bytes(DISK_GUID));
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// The VHD disk geometry for `sectors`, by the algorithm of the VHD format.
fn geometry(sectors: u64) -> (u16, u8, u8) {
    let total = sectors.min(65535 * 16 * 255);
    let (mut spt, mut heads, mut cth);
    if total >= 65535 * 16 * 63 {
        spt = 255;
        heads = 16;
        cth = total / spt;
    } else {
        spt = 17;
        cth = total / spt;
        heads = cth.div_ceil(1024);
        if heads < 4 {
            heads = 4;
        }
        if cth >= heads * 1024 || heads > 16 {
            spt = 31;
            heads = 16;
            cth = total / spt;
        }
        if cth >= heads * 1024 {
            spt = 63;
            heads = 16;
            cth = total / spt;
        }
    }
    ((cth / heads) as u16, heads as u8, spt as u8)
}

/// The fixed VHD footer for a disk of [`DISK_BYTES`].
fn vhd_footer() -> [u8; 512] {
    let mut f = [0u8; 512];
    f[0..8].copy_from_slice(b"conectix");
    f[8..12].copy_from_slice(&2u32.to_be_bytes());
    f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    f[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    f[28..32].copy_from_slice(b"dsa ");
    f[32..36].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    f[36..40].copy_from_slice(b"Wi2k");
    f[40..48].copy_from_slice(&DISK_BYTES.to_be_bytes());
    f[48..56].copy_from_slice(&DISK_BYTES.to_be_bytes());
    let (cylinders, heads, spt) = geometry(SECTORS);
    f[56..58].copy_from_slice(&cylinders.to_be_bytes());
    f[58] = heads;
    f[59] = spt;
    // Disk type 2: fixed.
    f[60..64].copy_from_slice(&2u32.to_be_bytes());
    f[68..84].copy_from_slice(&guid_bytes(DISK_GUID));
    let sum: u32 = f.iter().map(|&b| b as u32).sum();
    f[64..68].copy_from_slice(&(!sum).to_be_bytes());
    f
}

/// The whole file of a new, empty pstore disk.
pub fn image() -> Vec<u8> {
    let mut disk = vec![0u8; FILE_BYTES as usize];
    let at = |lba: u64| (lba * SECTOR) as usize;

    // The protective MBR: one partition of type 0xEE over the whole disk.
    let mbr = &mut disk[0..512];
    mbr[446 + 1..446 + 4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr[446 + 4] = 0xEE;
    mbr[446 + 5..446 + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&((SECTORS - 1) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    let mut entries = vec![0u8; (ENTRIES * ENTRY_BYTES) as usize];
    entries[0..16].copy_from_slice(&guid_bytes(LINUX_DATA_TYPE));
    entries[16..32].copy_from_slice(&guid_bytes(PARTITION_UUID));
    entries[32..40].copy_from_slice(&PARTITION_FIRST_SECTOR.to_le_bytes());
    entries[40..48].copy_from_slice(&PARTITION_LAST_SECTOR.to_le_bytes());
    for (i, unit) in PARTITION_NAME.encode_utf16().enumerate() {
        entries[56 + i * 2..58 + i * 2].copy_from_slice(&unit.to_le_bytes());
    }
    let entries_crc = crc32(&entries);
    let backup_entries = LAST_USABLE + 1;

    disk[at(1)..at(2)].copy_from_slice(&gpt_header(1, SECTORS - 1, 2, entries_crc));
    disk[at(2)..at(2) + entries.len()].copy_from_slice(&entries);
    disk[at(backup_entries)..at(backup_entries) + entries.len()].copy_from_slice(&entries);
    disk[at(SECTORS - 1)..at(SECTORS)].copy_from_slice(&gpt_header(
        SECTORS - 1,
        1,
        backup_entries,
        entries_crc,
    ));
    disk[DISK_BYTES as usize..].copy_from_slice(&vhd_footer());
    disk
}

/// The whole file of a new, empty raw pstore disk: [`image`] without the VHD footer.
pub fn raw_image() -> Vec<u8> {
    let mut disk = image();
    disk.truncate(DISK_BYTES as usize);
    disk
}

/// Whether the file at `path` is a pstore disk this module made, as `fresh` is: the right
/// size, its footer when it has one, and its partition table, whatever the partition holds.
fn is_pstore_disk(path: &Path, fresh: &[u8]) -> io::Result<bool> {
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() != fresh.len() as u64 {
        return Ok(false);
    }
    let mut start = vec![0u8; (FIRST_USABLE * SECTOR) as usize];
    file.read_exact(&mut start)?;
    if start[..] != fresh[..start.len()] {
        return Ok(false);
    }
    let mut footer = vec![0u8; fresh.len() - DISK_BYTES as usize];
    file.seek(SeekFrom::Start(DISK_BYTES))?;
    file.read_exact(&mut footer)?;
    Ok(footer[..] == fresh[DISK_BYTES as usize..])
}

/// Makes sure the pstore disk at `path` exists, as a fixed VHD. One this module made is kept
/// as it is, with the records in it; anything else at `path` is replaced by an empty disk.
pub fn ensure(path: &Path) -> Result<(), String> {
    ensure_image(path, image())
}

/// Makes sure the raw pstore disk at `path` exists, the same disk as [`ensure`] makes without
/// the VHD footer, for the hosts whose VMM attaches raw disks.
pub fn ensure_raw(path: &Path) -> Result<(), String> {
    ensure_image(path, raw_image())
}

fn ensure_image(path: &Path, fresh: Vec<u8>) -> Result<(), String> {
    match is_pstore_disk(path, &fresh) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            let path = path.display();
            return Err(format!("the pstore disk {path} could not be read: {e}"));
        }
    }
    std::fs::write(path, fresh).map_err(|e| {
        let path = path.display();
        format!("the pstore disk {path} could not be written: {e}")
    })
}
