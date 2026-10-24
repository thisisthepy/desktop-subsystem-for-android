//! GUID partition tables: finding a disk's partitions by name and copying them between
//! disks.
//!
//! A user's Android disk and the image's disk template have the same partitions. When the
//! image changes, the partitions that hold Android itself (`super` and the verified boot
//! data) are copied from the new template into the user's disk, and the ones that hold the
//! user's data (`misc`, `metadata`, `userdata`) stay. This module does that on anything that
//! reads and writes like a disk, so it is tested on files; the Windows service runs it on
//! attached VHDX disks.

use std::io::{self, Read, Seek, SeekFrom, Write};

/// The sector size of every disk DSA makes.
pub const SECTOR: u64 = 512;

/// One partition: its name and where it lies, in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub start: u64,
    pub len: u64,
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

/// How much of the start of a disk [`is_blank`] reads: the partition table, or the
/// superblock of a file system written straight onto the disk, is well within it.
const BLANK_PROBE: usize = 1 << 20;

/// Whether nothing was ever written to the start of `disk`: no partition table and no file
/// system. A disk created before any image was there to copy is blank, and holds no data.
pub fn is_blank(disk: &mut (impl Read + Seek)) -> io::Result<bool> {
    let mut start = vec![0u8; BLANK_PROBE];
    disk.seek(SeekFrom::Start(0))?;
    let mut read = 0;
    while read < start.len() {
        match disk.read(&mut start[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(start[..read].iter().all(|&b| b == 0))
}

/// Reads the partition table of `disk`.
pub fn partitions(disk: &mut (impl Read + Seek)) -> io::Result<Vec<Partition>> {
    let mut header = [0u8; SECTOR as usize];
    disk.seek(SeekFrom::Start(SECTOR))?;
    disk.read_exact(&mut header)?;
    if &header[0..8] != b"EFI PART" {
        return Err(invalid("the disk has no GUID partition table"));
    }
    let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
    let count = u32::from_le_bytes(header[80..84].try_into().unwrap()) as usize;
    let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap()) as usize;
    if !(128..=4096).contains(&entry_size) || count > 1024 {
        return Err(invalid(
            "the partition table's entry size or count is not plausible",
        ));
    }
    // Read whole sectors, which raw disk devices require.
    let bytes = (count * entry_size).div_ceil(SECTOR as usize) * SECTOR as usize;
    let mut table = vec![0u8; bytes];
    let entries_offset = entries_lba
        .checked_mul(SECTOR)
        .ok_or_else(|| invalid("partition table location overflows"))?;
    disk.seek(SeekFrom::Start(entries_offset))?;
    disk.read_exact(&mut table)?;
    let mut found = Vec::new();
    for entry in table.chunks(entry_size).take(count) {
        if entry[0..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        if last < first {
            return Err(invalid("partition last sector is before first sector"));
        }
        let sectors = last
            .checked_sub(first)
            .and_then(|diff| diff.checked_add(1))
            .ok_or_else(|| invalid("partition sector count overflows"))?;
        let len = sectors
            .checked_mul(SECTOR)
            .ok_or_else(|| invalid("partition length in bytes overflows"))?;
        let start = first
            .checked_mul(SECTOR)
            .ok_or_else(|| invalid("partition start in bytes overflows"))?;
        let units: Vec<u16> = entry[56..128]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        found.push(Partition {
            name: String::from_utf16_lossy(&units),
            start,
            len,
        });
    }
    Ok(found)
}

/// Whether `to` can take the partitions named in `names` from `from`: every one of them on
/// `from` is on `to` too, at least as large.
pub fn fits(
    from: &mut (impl Read + Seek),
    to: &mut (impl Read + Seek),
    names: &[&str],
) -> io::Result<bool> {
    let source = partitions(from)?;
    let target = partitions(to)?;
    Ok(names
        .iter()
        .all(|name| match source.iter().find(|p| p.name == *name) {
            None => true,
            Some(src) => target.iter().any(|p| p.name == *name && p.len >= src.len),
        }))
}

/// Whether nothing was ever written to the start of the partition `name`: Android formats
/// userdata on its first boot, so a blank one means Android never ran from this disk.
pub fn partition_is_blank(disk: &mut (impl Read + Seek), name: &str) -> io::Result<bool> {
    let part = partitions(disk)?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| invalid(format!("the disk has no {name} partition")))?;
    let mut start = vec![0u8; part.len.min(BLANK_PROBE as u64) as usize];
    disk.seek(SeekFrom::Start(part.start))?;
    disk.read_exact(&mut start)?;
    Ok(start.iter().all(|&b| b == 0))
}

/// Copies the partitions named in `names` from `from` to `to`, which must both have them,
/// with the destination's at least as large. Copies in whole megabytes, aligned, as raw
/// disk devices need, and zeroes what is left of a larger destination partition.
pub fn copy_partitions(
    from: &mut (impl Read + Seek),
    to: &mut (impl Read + Write + Seek),
    names: &[&str],
) -> io::Result<()> {
    let source = partitions(from)?;
    let target = partitions(to)?;
    for name in names {
        let find = |list: &[Partition], side: &str| {
            list.iter()
                .find(|p| p.name == *name)
                .cloned()
                .ok_or_else(|| invalid(format!("the {side} disk has no {name} partition")))
        };
        let src = find(&source, "source")?;
        let dst = find(&target, "destination")?;
        if dst.len < src.len {
            return Err(invalid(format!(
                "the {name} partition is {} bytes on the destination but {} on the source",
                dst.len, src.len
            )));
        }
        const CHUNK: u64 = 1 << 20;
        let mut buffer = vec![0u8; CHUNK as usize];
        let mut done = 0;
        while done < dst.len {
            let n = CHUNK.min(dst.len - done) as usize;
            if done < src.len {
                let take = n.min((src.len - done) as usize);
                from.seek(SeekFrom::Start(src.start + done))?;
                from.read_exact(&mut buffer[..take])?;
                buffer[take..n].fill(0);
            } else {
                buffer[..n].fill(0);
            }
            to.seek(SeekFrom::Start(dst.start + done))?;
            to.write_all(&buffer[..n])?;
            done += n as u64;
        }
    }
    to.flush()
}
