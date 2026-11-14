//! Finding the system partition in an emulator image's disk, while the disk is decompressed.
//!
//! An emulator image's `system.img` is a whole disk: a GPT partition table, then a `super`
//! partition holding Android's dynamic partitions (`system`, `vendor`, `product` and the rest),
//! described by its own metadata (liblp). Only `system` is needed, and it is about a fifth of
//! the disk, so the disk is never written out whole: [`Extract`] watches the decompressed bytes
//! go by, reads the tables from the first few megabytes, and writes only the bytes of `system`
//! to a file of their own. A disk whose `system` is a plain GPT partition, and a bare ext4
//! image, are read the same way.

use std::io::{Seek, SeekFrom, Write};

use sha2::{Digest, Sha256};

/// The sector size of an emulator disk and of liblp's extents.
const SECTOR: u64 = 512;
/// The most of a disk read before its tables must have been found.
const HEAD_LIMIT: usize = 64 << 20;
/// Where liblp's geometry is in `super`, and its size; the primary metadata follows the
/// geometry and its backup.
const LP_GEOMETRY_OFFSET: u64 = 4096;
const LP_GEOMETRY_SIZE: u64 = 4096;
const LP_GEOMETRY_MAGIC: u32 = 0x616C_4467;
const LP_HEADER_MAGIC: u32 = 0x414C_5030;
/// liblp's extent kinds.
const LP_TARGET_LINEAR: u32 = 0;
const LP_TARGET_ZERO: u32 = 1;
/// The signature of an ext4 superblock, at byte 1080.
const EXT4_MAGIC: u16 = 0xEF53;
/// The signature of an erofs superblock, at byte 1024, as a bare vendor image may have.
const EROFS_MAGIC: u32 = 0xE0F5_E1E2;
/// The signature of an Android sparse image, which is not read here.
const SPARSE_MAGIC: u32 = 0xED26_FF3A;

/// A run of the disk that belongs to the partition: `length` bytes from `source` in the
/// disk, at `target` in the partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub source: u64,
    pub length: u64,
    pub target: u64,
}

fn u16_at(data: &[u8], at: u64) -> Option<u16> {
    let at = usize::try_from(at).ok()?;
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], at: u64) -> Option<u32> {
    let at = usize::try_from(at).ok()?;
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn u64_at(data: &[u8], at: u64) -> Option<u64> {
    let at = usize::try_from(at).ok()?;
    let bytes = data.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

fn bytes_at(data: &[u8], at: u64, length: u64) -> Option<&[u8]> {
    let at = usize::try_from(at).ok()?;
    let end = at.checked_add(usize::try_from(length).ok()?)?;
    data.get(at..end)
}

/// A name in a fixed-size, NUL-padded field.
fn field_name(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Whether `name` is the `target` partition's: `target`, or the first slot's `{target}_a`.
fn is_partition(name: &str, target: &str) -> bool {
    name == target || (name.starts_with(target) && &name[target.len()..] == "_a")
}

/// What the start of a disk says about where its system partition is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// More of the disk is needed to tell.
    NeedMore,
    /// The partition's runs, and its size.
    System(Vec<Span>, u64),
}

/// Where the `target` partition is in a disk of `disk_size` bytes that starts with `head`.
pub fn locate_partition(head: &[u8], disk_size: u64, target: &str) -> Result<Found, String> {
    if u32_at(head, 0) == Some(SPARSE_MAGIC) {
        return Err(format!(
            "the image's disk with the {target} partition is a sparse image, which DSA does not \
             read"
        ));
    }
    if head.len() < 1082 {
        return Ok(Found::NeedMore);
    }
    if bytes_at(head, SECTOR, 8) != Some(b"EFI PART".as_slice()) {
        if u16_at(head, 1080) != Some(EXT4_MAGIC) && u32_at(head, 1024) != Some(EROFS_MAGIC) {
            return Err(format!(
                "the image's disk with the {target} partition is neither a disk nor an ext4 or \
                 erofs image"
            ));
        }
        let whole = Span {
            source: 0,
            length: disk_size,
            target: 0,
        };
        return Ok(Found::System(vec![whole], disk_size));
    }
    let found = gpt(head, target)?;
    let Some((name, first, length)) = found else {
        return Ok(Found::NeedMore);
    };
    if first.saturating_add(length) > disk_size {
        return Err(format!("the disk's {name} partition ends after the disk"));
    }
    if is_partition(&name, target) {
        let span = Span {
            source: first,
            length,
            target: 0,
        };
        return Ok(Found::System(vec![span], length));
    }
    match lp(head, first, length, target)? {
        Some((spans, size)) => Ok(Found::System(spans, size)),
        None => Ok(Found::NeedMore),
    }
}

/// The GPT partition to read: `super` (preferred) or the `target` partition, as its name,
/// start and length in bytes. `None` while the table is not all in `head`.
fn gpt(head: &[u8], target: &str) -> Result<Option<(String, u64, u64)>, String> {
    let damaged = || "the disk's partition table is damaged".to_string();
    let table = u64_at(head, SECTOR + 72).ok_or_else(damaged)?;
    let count = u32_at(head, SECTOR + 80).ok_or_else(damaged)?;
    let size = u32_at(head, SECTOR + 84).ok_or_else(damaged)?;
    if count == 0 || count > 1024 || !(128..=4096).contains(&size) {
        return Err(damaged());
    }
    let start = table.checked_mul(SECTOR).ok_or_else(damaged)?;
    let length = u64::from(count) * u64::from(size);
    let Some(entries) = bytes_at(head, start, length) else {
        return Ok(None);
    };
    let mut matched = None;
    for entry in entries.chunks_exact(size as usize) {
        if entry[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let units: Vec<u16> = entry[56..128]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        let name = String::from_utf16_lossy(&units);
        if last < first {
            return Err(damaged());
        }
        let found = (name.clone(), first * SECTOR, (last - first + 1) * SECTOR);
        if name == "super" {
            return Ok(Some(found));
        }
        if is_partition(&name, target) {
            matched = Some(found);
        }
    }
    match matched {
        Some(found) => Ok(Some(found)),
        None => Err(format!(
            "the disk has neither a super nor a {target} partition"
        )),
    }
}

/// The `target` partition's runs in the `super` partition at `start`, `length` bytes long, and
/// the partition's size, from its liblp metadata. `None` while the metadata is not all in
/// `head`.
fn lp(
    head: &[u8],
    start: u64,
    length: u64,
    target: &str,
) -> Result<Option<(Vec<Span>, u64)>, String> {
    let damaged = |what: &str| format!("the super partition's {what} is damaged");
    let geometry = start + LP_GEOMETRY_OFFSET;
    let Some(magic) = u32_at(head, geometry) else {
        return Ok(None);
    };
    if magic != LP_GEOMETRY_MAGIC {
        return Err(damaged("geometry"));
    }
    let header = geometry + 2 * LP_GEOMETRY_SIZE;
    let Some(header_size) = u32_at(head, header + 8) else {
        return Ok(None);
    };
    let Some(tables_size) = u32_at(head, header + 44) else {
        return Ok(None);
    };
    if u32_at(head, header) != Some(LP_HEADER_MAGIC) || header_size < 128 {
        return Err(damaged("metadata"));
    }
    let tables_at = header + u64::from(header_size);
    let Some(tables) = bytes_at(head, tables_at, u64::from(tables_size)) else {
        return Ok(None);
    };
    let checksum = bytes_at(head, header + 48, 32).ok_or_else(|| damaged("metadata"))?;
    if Sha256::digest(tables).as_slice() != checksum {
        return Err(damaged("metadata (its checksum does not match)"));
    }
    let descriptor = |index: u64| -> Option<(u32, u32, u32)> {
        let at = header + 80 + 12 * index;
        let offset = u32_at(head, at)?;
        let count = u32_at(head, at + 4)?;
        let size = u32_at(head, at + 8)?;
        Some((offset, count, size))
    };
    let partitions = descriptor(0).ok_or_else(|| damaged("metadata"))?;
    let extents = descriptor(1).ok_or_else(|| damaged("metadata"))?;
    let extent = |index: u32| -> Option<(u64, u32, u64, u32)> {
        let at = u64::from(extents.0) + u64::from(index) * u64::from(extents.2);
        let sectors = u64_at(tables, at)?;
        let kind = u32_at(tables, at + 8)?;
        let data = u64_at(tables, at + 12)?;
        let source = u32_at(tables, at + 20)?;
        Some((sectors, kind, data, source))
    };
    for index in 0..partitions.1 {
        let at = u64::from(partitions.0) + u64::from(index) * u64::from(partitions.2);
        let entry = bytes_at(tables, at, 52).ok_or_else(|| damaged("partition table"))?;
        if !is_partition(&field_name(&entry[..36]), target) {
            continue;
        }
        let first = u32::from_le_bytes(entry[40..44].try_into().unwrap());
        let count = u32::from_le_bytes(entry[44..48].try_into().unwrap());
        if count > extents.1 || first > extents.1 - count {
            return Err(damaged("partition table"));
        }
        let mut spans = Vec::new();
        let mut size = 0u64;
        for index in first..first + count {
            let bad = || damaged("extents");
            let (sectors, kind, data, source) = extent(index).ok_or_else(bad)?;
            let bytes = sectors.checked_mul(SECTOR).ok_or_else(bad)?;
            match kind {
                LP_TARGET_LINEAR if source == 0 => {
                    let offset = data.checked_mul(SECTOR).ok_or_else(bad)?;
                    if offset.saturating_add(bytes) > length {
                        return Err(bad());
                    }
                    spans.push(Span {
                        source: start + offset,
                        length: bytes,
                        target: size,
                    });
                }
                LP_TARGET_ZERO => {}
                _ => return Err(bad()),
            }
            size += bytes;
        }
        return Ok(Some((spans, size)));
    }
    Err(format!("the super partition holds no {target} partition"))
}

/// Takes a disk's bytes in order and writes the partition's bytes to `out`.
pub struct Extract<W> {
    out: W,
    disk_size: u64,
    partition: &'static str,
    head: Vec<u8>,
    spans: Option<Vec<Span>>,
    /// The partition's size.
    size: u64,
    /// How far into the disk the bytes taken reach.
    position: u64,
    /// Where the last run of the partition ends in the disk.
    end: u64,
}

impl<W: Write + Seek> Extract<W> {
    /// Writes the system partition of a disk of `disk_size` bytes to `out`.
    #[cfg(test)]
    pub fn new(out: W, disk_size: u64) -> Extract<W> {
        Self::for_partition(out, disk_size, "system")
    }

    /// Writes the `partition` of a disk of `disk_size` bytes to `out`.
    pub fn for_partition(out: W, disk_size: u64, partition: &'static str) -> Extract<W> {
        Extract {
            out,
            disk_size,
            partition,
            head: Vec::new(),
            spans: None,
            size: 0,
            position: 0,
            end: 0,
        }
    }

    /// Takes the next bytes of the disk. Returns whether more are needed.
    pub fn take(&mut self, bytes: &[u8]) -> Result<bool, String> {
        if self.spans.is_some() {
            return self.route(bytes);
        }
        self.head.extend_from_slice(bytes);
        match locate_partition(&self.head, self.disk_size, self.partition)? {
            Found::NeedMore if self.head.len() as u64 >= self.disk_size => {
                Err("the disk ends before its partition tables".into())
            }
            Found::NeedMore if self.head.len() > HEAD_LIMIT => {
                Err("the disk's partition tables are not where they belong".into())
            }
            Found::NeedMore => Ok(true),
            Found::System(spans, size) => {
                let end = spans.iter().map(|s| s.source + s.length).max();
                self.end = end.unwrap_or(0);
                self.spans = Some(spans);
                self.size = size;
                let head = std::mem::take(&mut self.head);
                self.route(&head)
            }
        }
    }

    fn route(&mut self, bytes: &[u8]) -> Result<bool, String> {
        let start = self.position;
        let end = start + bytes.len() as u64;
        let partition = self.partition;
        let io = |e: std::io::Error| format!("the {partition} partition cannot be written: {e}");
        for span in self.spans.iter().flatten() {
            let from = start.max(span.source);
            let to = end.min(span.source + span.length);
            if from >= to {
                continue;
            }
            let at = span.target + (from - span.source);
            self.out.seek(SeekFrom::Start(at)).map_err(io)?;
            let piece = &bytes[(from - start) as usize..(to - start) as usize];
            self.out.write_all(piece).map_err(io)?;
        }
        self.position = end;
        Ok(self.position < self.end)
    }

    /// The output once every byte of the partition was taken, with the partition's size.
    pub fn finish(mut self) -> Result<(W, u64), String> {
        if self.spans.is_none() {
            return Err("the disk ends before its partition tables".into());
        }
        if self.position < self.end {
            return Err(format!(
                "the disk ends inside its {} partition",
                self.partition
            ));
        }
        let partition = self.partition;
        let io = |e: std::io::Error| format!("the {partition} partition cannot be written: {e}");
        self.out.flush().map_err(io)?;
        Ok((self.out, self.size))
    }
}

/// Disks shaped as the emulator's, for the tests here and of the image reader.
#[cfg(any(test, feature = "conformance"))]
pub(crate) mod fixture {
    use super::*;

    fn put(disk: &mut [u8], at: u64, bytes: &[u8]) {
        let at = at as usize;
        disk[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// A GPT entry named `name` from sector `first` to `last`.
    fn gpt_entry(name: &str, first: u64, last: u64) -> Vec<u8> {
        let mut entry = vec![0u8; 128];
        entry[0] = 0xAF;
        entry[32..40].copy_from_slice(&first.to_le_bytes());
        entry[40..48].copy_from_slice(&last.to_le_bytes());
        for (i, unit) in name.encode_utf16().enumerate() {
            entry[56 + 2 * i..58 + 2 * i].copy_from_slice(&unit.to_le_bytes());
        }
        entry
    }

    /// The liblp metadata of a `super` holding `system` at `system_at` (bytes into `super`),
    /// `system_size` bytes long, after a `vendor` partition.
    fn lp_metadata(system_at: u64, system_size: u64) -> Vec<u8> {
        lp_metadata_of(&[
            ("vendor", 1u64 << 20, 8 * SECTOR),
            ("system", system_at, system_size),
        ])
    }

    /// The liblp metadata of a `super` holding each of `parts` (its name, where it starts in
    /// `super` and its size in bytes), each in one run.
    fn lp_metadata_of(parts: &[(&str, u64, u64)]) -> Vec<u8> {
        let mut partitions = Vec::new();
        for (extent, (name, _, _)) in parts.iter().enumerate() {
            let mut entry = vec![0u8; 52];
            entry[..name.len()].copy_from_slice(name.as_bytes());
            entry[40..44].copy_from_slice(&(extent as u32).to_le_bytes());
            entry[44..48].copy_from_slice(&1u32.to_le_bytes());
            partitions.extend_from_slice(&entry);
        }
        let mut extents = Vec::new();
        for (_, at, size) in parts {
            extents.extend_from_slice(&(size / SECTOR).to_le_bytes());
            extents.extend_from_slice(&LP_TARGET_LINEAR.to_le_bytes());
            extents.extend_from_slice(&(at / SECTOR).to_le_bytes());
            extents.extend_from_slice(&0u32.to_le_bytes());
        }
        let count = parts.len() as u32;
        let tables = [partitions, extents].concat();
        let mut header = vec![0u8; 128];
        header[0..4].copy_from_slice(&LP_HEADER_MAGIC.to_le_bytes());
        header[4..6].copy_from_slice(&10u16.to_le_bytes());
        header[8..12].copy_from_slice(&128u32.to_le_bytes());
        header[44..48].copy_from_slice(&(tables.len() as u32).to_le_bytes());
        header[48..80].copy_from_slice(&Sha256::digest(&tables));
        let descriptors = [(0u32, count, 52u32), (52 * count, count, 24)];
        for (i, (offset, count, size)) in descriptors.iter().enumerate() {
            let at = 80 + 12 * i;
            header[at..at + 4].copy_from_slice(&offset.to_le_bytes());
            header[at + 4..at + 8].copy_from_slice(&count.to_le_bytes());
            header[at + 8..at + 12].copy_from_slice(&size.to_le_bytes());
        }
        [header, tables].concat()
    }

    /// A GPT disk of `size` bytes whose `super` partition starts at 2 MiB and runs to the end.
    fn gpt_disk(size: u64) -> Vec<u8> {
        let super_at = 2u64 << 20;
        let mut disk = vec![0u8; size as usize];
        let mut header = vec![0u8; 92];
        header[..8].copy_from_slice(b"EFI PART");
        header[72..80].copy_from_slice(&2u64.to_le_bytes());
        header[80..84].copy_from_slice(&4u32.to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());
        put(&mut disk, SECTOR, &header);
        let vbmeta = gpt_entry("vbmeta", 2048, 4095);
        let last = size / SECTOR - 1;
        let entries = [vbmeta, gpt_entry("super", super_at / SECTOR, last)].concat();
        put(&mut disk, 2 * SECTOR, &entries);
        disk
    }

    /// A disk as the emulator's `system.img` is: a GPT whose `super` partition starts at
    /// 2 MiB and holds `system` (the bytes given, a whole number of sectors) 1 MiB in.
    pub fn disk(system: &[u8]) -> Vec<u8> {
        assert_eq!(system.len() as u64 % SECTOR, 0);
        let super_at = 2u64 << 20;
        let system_at = 1u64 << 20;
        let super_size = system_at + system.len() as u64 + (1 << 20);
        let mut disk = gpt_disk(super_at + super_size);
        let geometry = super_at + LP_GEOMETRY_OFFSET;
        put(&mut disk, geometry, &LP_GEOMETRY_MAGIC.to_le_bytes());
        let metadata = lp_metadata(system_at, system.len() as u64);
        put(&mut disk, geometry + 2 * LP_GEOMETRY_SIZE, &metadata);
        put(&mut disk, super_at + system_at, system);
        disk
    }

    /// A disk as the emulator's `system.img` is, whose `super` holds each of `parts` (its name
    /// and bytes, a whole number of sectors), one after another from 1 MiB in.
    pub fn disk_of(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let super_at = 2u64 << 20;
        let mut placed = Vec::new();
        let mut at = 1u64 << 20;
        for (name, bytes) in parts {
            assert_eq!(bytes.len() as u64 % SECTOR, 0);
            placed.push((*name, at, bytes.len() as u64));
            at += (bytes.len() as u64).div_ceil(4096) * 4096;
        }
        let super_size = at + (1 << 20);
        let mut disk = gpt_disk(super_at + super_size);
        let geometry = super_at + LP_GEOMETRY_OFFSET;
        put(&mut disk, geometry, &LP_GEOMETRY_MAGIC.to_le_bytes());
        put(
            &mut disk,
            geometry + 2 * LP_GEOMETRY_SIZE,
            &lp_metadata_of(&placed),
        );
        for ((_, at, _), (_, bytes)) in placed.iter().zip(parts) {
            put(&mut disk, super_at + at, bytes);
        }
        disk
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::disk;
    use super::*;
    use std::io::Cursor;

    fn extract(disk: &[u8], piece: usize) -> Result<Vec<u8>, String> {
        let mut extract = Extract::new(Cursor::new(Vec::new()), disk.len() as u64);
        for bytes in disk.chunks(piece) {
            if !extract.take(bytes)? {
                break;
            }
        }
        let (out, size) = extract.finish()?;
        let mut data = out.into_inner();
        data.resize(size as usize, 0);
        Ok(data)
    }

    #[test]
    fn fr25_the_system_partition_is_found_in_super_and_copied_alone() {
        let system: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        let image = disk(&system);
        for piece in [1000, 65_536, 3 << 20] {
            assert_eq!(extract(&image, piece).unwrap(), system, "{piece}");
        }
    }

    #[test]
    fn fr25_a_bare_ext4_image_is_taken_whole() {
        let mut image = vec![0u8; 8192];
        image[1080..1082].copy_from_slice(&0xEF53u16.to_le_bytes());
        image[5000] = 9;
        assert_eq!(extract(&image, 4096).unwrap(), image);
    }

    #[test]
    fn fr25_a_disk_without_a_system_partition_is_refused() {
        let system = vec![1u8; 4096];
        let mut image = disk(&system);
        // Rename super, so the table names neither super nor system.
        let name_at = 2 * SECTOR as usize + 128 + 56;
        image[name_at] = b'x';
        assert!(extract(&image, 1 << 20).is_err());
        assert!(extract(&[0u8; 4096], 4096).is_err(), "not a disk");
        let cut = disk(&system);
        let cut = &cut[..(2 << 20) + 4096 * 3];
        assert!(extract(cut, 4096).is_err(), "cut short");
        let sparse = 0xED26_FF3Au32.to_le_bytes();
        assert!(extract(&sparse, 4).is_err(), "sparse");
    }

    #[test]
    fn fr25_damaged_super_metadata_is_refused() {
        let system = vec![1u8; 4096];
        let mut image = disk(&system);
        // The first byte of the partition table, which the checksum covers.
        let tables = (2 << 20) + LP_GEOMETRY_OFFSET + 2 * LP_GEOMETRY_SIZE + 128;
        image[tables as usize] ^= 1;
        let refused = extract(&image, 1 << 20).unwrap_err();
        assert!(refused.contains("checksum"), "{refused}");
    }
}
