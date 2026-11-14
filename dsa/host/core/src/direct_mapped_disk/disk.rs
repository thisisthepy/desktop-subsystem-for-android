//! The system disk around the file systems: its GPT, the super partition's metadata (Android's
//! liblp format) and the logical partitions in it, and how the copy lays them out.
//!
//! The copy keeps the image's disk as it is up to the super partition: the same GPT, the same
//! small partitions (verified boot data, boot images) at the same places, so the kernel
//! command line's partition list stays right for them. The super partition starts where it
//! did and holds the same geometry; its metadata names the logical partitions again, now one
//! extent each, packed at the block device's alignment from its first logical sector, each
//! erofs one replaced by its uncompressed copy. The super partition then ends after the last
//! of them, and the disk a whole number of 2 MiB after that, with the GPT's backup at its end.
//!
//! ```text
//! 0               protective MBR, GPT header and entries (super's end and the disk's size
//!                 changed, checksums made again)
//! 34 sectors ..   the image's bytes up to the super partition, as they are
//! super           reserved 4 KiB and the two geometry copies, as they are; every metadata
//!                 slot, primary and backup, with the new extents; zeroes; the partitions
//! super's end ..  zeroes, then the backup GPT entries and header in the last 33 sectors
//! ```

use sha2::{Digest, Sha256};
use std::io::Write;

use super::erofs_read::Reader;
use super::erofs_write;
use super::ReadAt;
use crate::diagnostics::crc32;

const SECTOR: u64 = 512;
/// The disk is a whole number of these: Linux maps persistent memory in 2 MiB sections.
pub const DISK_ALIGNMENT: u64 = 2 << 20;
/// What the GPT keeps at the disk's end: 32 sectors of entries and the header.
const BACKUP_GPT_SECTORS: u64 = 33;

const LP_RESERVED: u64 = 4096;
const LP_GEOMETRY_SIZE: u64 = 4096;
const LP_GEOMETRY_MAGIC: u32 = 0x616c_4467;
const LP_HEADER_MAGIC: u32 = 0x414c_5030;
const LP_TARGET_LINEAR: u32 = 0;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("four bytes"))
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("eight bytes"))
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn read(dev: &mut dyn ReadAt, at: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; len];
    dev.read_exact_at(at, &mut buf)
        .map_err(|e| format!("reading the system disk at {at}: {e}"))?;
    Ok(buf)
}

/// A part of the disk made of extents (offset, length) one after the other: a logical
/// partition, read as one device.
pub struct Extents<'a> {
    dev: &'a mut dyn ReadAt,
    extents: Vec<(u64, u64)>,
    len: u64,
}

impl<'a> Extents<'a> {
    pub fn new(dev: &'a mut dyn ReadAt, extents: Vec<(u64, u64)>) -> Extents<'a> {
        let len = extents.iter().map(|e| e.1).sum();
        Extents { dev, extents, len }
    }
}

impl ReadAt for Extents<'_> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if offset + buf.len() as u64 > self.len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "a read past the end of the partition",
            ));
        }
        let mut done = 0usize;
        let mut base = 0u64;
        for &(start, len) in &self.extents {
            let at = offset + done as u64;
            if done == buf.len() {
                break;
            }
            if at < base + len {
                let within = at - base;
                let take = ((len - within) as usize).min(buf.len() - done);
                self.dev
                    .read_exact_at(start + within, &mut buf[done..done + take])?;
                done += take;
            }
            base += len;
        }
        Ok(())
    }

    fn len(&self) -> u64 {
        self.len
    }
}

/// One logical partition of the super partition, as the copy places it.
pub struct Partition {
    pub name: String,
    /// Where its data is on the image's disk.
    source: Vec<(u64, u64)>,
    /// Where the copy puts it, from the super partition's start, and how much it takes.
    pub offset: u64,
    pub len: u64,
    /// The layout of its uncompressed copy; `None` copies it as it is.
    copy: Option<erofs_write::Plan>,
}

/// How the copy of a system disk is laid out.
pub struct DiskPlan {
    pub disk_bytes: u64,
    pub super_offset: u64,
    pub super_bytes: u64,
    gpt_head: Vec<u8>,
    gpt_tail: Vec<u8>,
    super_head: Vec<u8>,
    pub partitions: Vec<Partition>,
}

/// A stretch of the copy written in one go, and where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub offset: u64,
    pub len: u64,
}

struct Gpt {
    header: Vec<u8>,
    entries: Vec<u8>,
    entry_size: usize,
    super_index: usize,
    super_first: u64,
}

fn read_gpt(dev: &mut dyn ReadAt) -> Result<Gpt, String> {
    let header = read(dev, SECTOR, SECTOR as usize)?;
    if &header[0..8] != b"EFI PART" {
        return Err("the system disk has no GPT".into());
    }
    let size = u32_at(&header, 12) as usize;
    if !(92..=512).contains(&size) {
        return Err("the system disk's GPT header is not plausible".into());
    }
    let mut zeroed = header[..size].to_vec();
    zeroed[16..20].fill(0);
    if crc32(&zeroed) != u32_at(&header, 16) {
        return Err("the system disk's GPT header fails its checksum".into());
    }
    let entries_lba = u64_at(&header, 72);
    let count = u32_at(&header, 80) as usize;
    let entry_size = u32_at(&header, 84) as usize;
    if entries_lba != 2 || entry_size < 128 || count * entry_size != 32 * SECTOR as usize {
        return Err("the system disk's GPT entries are not where the image puts them".into());
    }
    let entries = read(dev, entries_lba * SECTOR, count * entry_size)?;
    if crc32(&entries) != u32_at(&header, 88) {
        return Err("the system disk's GPT entries fail their checksum".into());
    }
    let mut super_index = None;
    let mut last_end = 0u64;
    for i in 0..count {
        let entry = &entries[i * entry_size..(i + 1) * entry_size];
        if entry[0..16].iter().all(|&b| b == 0) {
            continue;
        }
        let name: Vec<u16> = (0..36)
            .map(|c| u16_at(entry, 56 + c * 2))
            .take_while(|&c| c != 0)
            .collect();
        last_end = last_end.max(u64_at(entry, 40));
        if String::from_utf16_lossy(&name) == "super" {
            super_index = Some(i);
        }
    }
    let super_index = super_index.ok_or("the system disk has no super partition")?;
    let entry = &entries[super_index * entry_size..];
    if u64_at(entry, 40) != last_end {
        return Err("the super partition is not the system disk's last".into());
    }
    let super_first = u64_at(entry, 32);
    Ok(Gpt {
        header: header[..size].to_vec(),
        entries,
        entry_size,
        super_index,
        super_first,
    })
}

/// The protective MBR, the GPT header and its entries for a disk of `sectors`, with the super
/// partition ending at `super_last`: the head (34 sectors) and the backup (33 sectors).
fn new_gpt(
    mbr: &[u8],
    gpt: &Gpt,
    sectors: u64,
    super_last: u64,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut entries = gpt.entries.clone();
    put64(
        &mut entries,
        gpt.super_index * gpt.entry_size + 40,
        super_last,
    );
    let entries_crc = crc32(&entries);
    let header = |mine: u64, other: u64, entries_lba: u64| {
        let mut h = gpt.header.clone();
        put64(&mut h, 24, mine);
        put64(&mut h, 32, other);
        put64(&mut h, 48, sectors - BACKUP_GPT_SECTORS - 1);
        put64(&mut h, 72, entries_lba);
        put32(&mut h, 88, entries_crc);
        put32(&mut h, 16, 0);
        let crc = crc32(&h);
        put32(&mut h, 16, crc);
        h.resize(SECTOR as usize, 0);
        h
    };
    let mut head = mbr.to_vec();
    if head[446 + 4] == 0xee {
        put32(
            &mut head,
            446 + 12,
            (sectors - 1).min(u64::from(u32::MAX)) as u32,
        );
    }
    head.extend_from_slice(&header(1, sectors - 1, 2));
    head.extend_from_slice(&entries);
    let mut tail = entries;
    tail.extend_from_slice(&header(sectors - 1, 1, sectors - BACKUP_GPT_SECTORS));
    Ok((head, tail))
}

struct Metadata {
    geometry: Vec<u8>,
    max_size: u64,
    slots: u64,
    header: Vec<u8>,
    tables: Vec<u8>,
}

struct Table {
    offset: usize,
    count: usize,
    size: usize,
}

fn table(header: &[u8], at: usize) -> Table {
    Table {
        offset: u32_at(header, at) as usize,
        count: u32_at(header, at + 4) as usize,
        size: u32_at(header, at + 8) as usize,
    }
}

fn read_metadata(super_dev: &mut dyn ReadAt) -> Result<Metadata, String> {
    let geometry = read(super_dev, LP_RESERVED, LP_GEOMETRY_SIZE as usize)?;
    if u32_at(&geometry, 0) != LP_GEOMETRY_MAGIC {
        return Err("the super partition has no metadata geometry".into());
    }
    let struct_size = u32_at(&geometry, 4) as usize;
    if !(52..=4096).contains(&struct_size) {
        return Err("the super partition's geometry is not plausible".into());
    }
    let mut zeroed = geometry[..struct_size].to_vec();
    zeroed[8..40].fill(0);
    if Sha256::digest(&zeroed)[..] != geometry[8..40] {
        return Err("the super partition's geometry fails its checksum".into());
    }
    let max_size = u64::from(u32_at(&geometry, 40));
    let slots = u64::from(u32_at(&geometry, 44));
    if max_size == 0 || max_size > 1 << 20 || slots == 0 || slots > 4 || max_size % SECTOR != 0 {
        return Err("the super partition's metadata size is not plausible".into());
    }
    let primary = LP_RESERVED + 2 * LP_GEOMETRY_SIZE;
    let mut last = String::new();
    for at in [primary, primary + max_size * slots] {
        let bytes = read(super_dev, at, max_size as usize)?;
        match parse_header(&bytes) {
            Ok((header, tables)) => {
                return Ok(Metadata {
                    geometry,
                    max_size,
                    slots,
                    header,
                    tables,
                })
            }
            Err(why) => last = why,
        }
    }
    Err(format!(
        "the super partition's metadata cannot be read: {last}"
    ))
}

fn parse_header(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    if u32_at(bytes, 0) != LP_HEADER_MAGIC || u16_at(bytes, 4) != 10 {
        return Err("no metadata header of version 10".into());
    }
    let header_size = u32_at(bytes, 8) as usize;
    if !(128..=1024).contains(&header_size) {
        return Err("a metadata header of a size this host does not know".into());
    }
    let mut header = bytes[..header_size].to_vec();
    let mut zeroed = header.clone();
    zeroed[12..44].fill(0);
    if Sha256::digest(&zeroed)[..] != header[12..44] {
        return Err("the metadata header fails its checksum".into());
    }
    let tables_size = u32_at(&header, 44) as usize;
    let tables = bytes
        .get(header_size..header_size + tables_size)
        .ok_or("the metadata tables run past their slot")?
        .to_vec();
    if Sha256::digest(&tables)[..] != header[48..80] {
        return Err("the metadata tables fail their checksum".into());
    }
    header[12..44].fill(0);
    Ok((header, tables))
}

fn name_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Whether the logical partition `name` (with its slot suffix) is one of `keep`.
fn kept(name: &str, keep: &[String]) -> bool {
    keep.iter().any(|k| {
        name == k
            || name
                .strip_prefix(k.as_str())
                .is_some_and(|s| s == "_a" || s == "_b")
    })
}

/// Lays out the copy of the system disk `dev`: every erofs logical partition uncompressed,
/// but those named in `keep` (without their slot suffix), which stay as they are with their
/// verity data, so the guest can still verify them.
pub fn plan(dev: &mut dyn ReadAt, keep: &[String]) -> Result<DiskPlan, String> {
    let mbr = read(dev, 0, SECTOR as usize)?;
    let gpt = read_gpt(dev)?;
    let super_offset = gpt.super_first * SECTOR;
    let entry = &gpt.entries[gpt.super_index * gpt.entry_size..];
    let super_len = (u64_at(entry, 40) + 1 - gpt.super_first) * SECTOR;
    let meta = {
        let mut view = Extents::new(dev, vec![(super_offset, super_len)]);
        read_metadata(&mut view)?
    };
    let header = &meta.header;
    let parts = table(header, 80);
    let extents = table(header, 92);
    let devices = table(header, 116);
    if parts.size < 52 || extents.size < 24 || devices.size < 64 || devices.count != 1 {
        return Err("the super partition's metadata tables are not what Android writes".into());
    }
    let device = &meta.tables[devices.offset..devices.offset + devices.size];
    let first_logical = u64_at(device, 0) * SECTOR;
    let alignment = u64::from(u32_at(device, 8)).max(1 << 20);
    if u32_at(device, 12) != 0 {
        return Err("the super partition has an alignment offset".into());
    }
    let reserved_end = LP_RESERVED + 2 * LP_GEOMETRY_SIZE + 2 * meta.slots * meta.max_size;
    if reserved_end > first_logical {
        return Err("the super partition's metadata overlaps its partitions".into());
    }
    let mut partitions = Vec::new();
    for i in 0..parts.count {
        let p = &meta.tables[parts.offset + i * parts.size..parts.offset + (i + 1) * parts.size];
        let name = name_of(&p[0..36]);
        let first = u32_at(p, 40) as usize;
        let count = u32_at(p, 44) as usize;
        let mut source = Vec::new();
        for e in first..first + count {
            let x = meta
                .tables
                .get(extents.offset + e * extents.size..extents.offset + (e + 1) * extents.size)
                .ok_or("a partition names a missing extent")?;
            if u32_at(x, 8) != LP_TARGET_LINEAR || u32_at(x, 20) != 0 {
                return Err(format!(
                    "{name} has an extent that is not on the super partition"
                ));
            }
            let (start, len) = (u64_at(x, 12) * SECTOR, u64_at(x, 0) * SECTOR);
            if start + len > super_len {
                return Err(format!("{name} reaches past the super partition"));
            }
            source.push((super_offset + start, len));
        }
        let len: u64 = source.iter().map(|e| e.1).sum();
        let mut copy = None;
        if len > 0 && !kept(&name, keep) {
            let mut view = Extents::new(dev, source.clone());
            let magic = read(&mut view, 1024, 4).map(|m| u32_at(&m, 0));
            if magic == Ok(super::erofs_read::MAGIC) {
                // An erofs file system this host cannot copy exactly stops the copy: a disk
                // only partly mapped is no better than the image's own.
                let mut reader = Reader::new(&mut view).map_err(|e| format!("{name}: {e}"))?;
                copy = Some(erofs_write::plan(&mut reader).map_err(|e| format!("{name}: {e}"))?);
            }
        }
        let new_len = match &copy {
            Some(copy) => copy.bytes().div_ceil(alignment) * alignment,
            None => len,
        };
        partitions.push(Partition {
            name,
            source,
            offset: 0,
            len: new_len,
            copy,
        });
    }
    // In the order the image placed them, each at the alignment.
    let mut order: Vec<usize> = (0..partitions.len()).collect();
    order.sort_by_key(|&i| (partitions[i].source.first().map_or(u64::MAX, |e| e.0), i));
    let mut at = first_logical.div_ceil(alignment) * alignment;
    for &i in &order {
        if partitions[i].len == 0 {
            continue;
        }
        partitions[i].offset = at;
        at += partitions[i].len.div_ceil(alignment) * alignment;
    }
    let super_bytes = at;
    // The groups' size limits still hold.
    let groups = table(header, 104);
    for g in 0..groups.count {
        let group = &meta.tables[groups.offset + g * groups.size..];
        let limit = u64_at(group, 40);
        let used: u64 = (0..parts.count)
            .filter(|&i| u32_at(&meta.tables[parts.offset + i * parts.size..], 48) as usize == g)
            .map(|i| partitions[i].len)
            .sum();
        if limit != 0 && used > limit {
            return Err(format!(
                "the uncompressed partitions of group {} need {used} bytes, more than its {limit}",
                name_of(&group[0..36])
            ));
        }
    }
    let tables = new_tables(
        &meta,
        &parts,
        extents.size,
        &devices,
        &partitions,
        super_bytes,
    );
    let mut new_header = meta.header.clone();
    let tables_bytes = tables.0;
    put32(&mut new_header, 44, tables_bytes.len() as u32);
    new_header[48..80].copy_from_slice(&Sha256::digest(&tables_bytes));
    put32(&mut new_header, 80, 0);
    put32(&mut new_header, 92, tables.1 as u32);
    put32(&mut new_header, 96, tables.2 as u32);
    put32(&mut new_header, 104, tables.3 as u32);
    put32(&mut new_header, 116, tables.4 as u32);
    new_header[12..44].fill(0);
    let checksum = Sha256::digest(&new_header);
    new_header[12..44].copy_from_slice(&checksum);
    let mut slot = new_header;
    slot.extend_from_slice(&tables_bytes);
    if slot.len() as u64 > meta.max_size {
        return Err("the new metadata does not fit its slot".into());
    }
    slot.resize(meta.max_size as usize, 0);
    let mut super_head = read(
        dev,
        super_offset,
        (LP_RESERVED + 2 * LP_GEOMETRY_SIZE) as usize,
    )?;
    super_head[LP_RESERVED as usize..(LP_RESERVED + LP_GEOMETRY_SIZE) as usize]
        .copy_from_slice(&meta.geometry);
    for _ in 0..2 * meta.slots {
        super_head.extend_from_slice(&slot);
    }
    let first_partition = order
        .iter()
        .map(|&i| &partitions[i])
        .find(|p| p.len > 0)
        .map_or(super_bytes, |p| p.offset);
    super_head.resize(first_partition as usize, 0);
    let disk_bytes = (super_offset + super_bytes + BACKUP_GPT_SECTORS * SECTOR + SECTOR)
        .div_ceil(DISK_ALIGNMENT)
        * DISK_ALIGNMENT;
    let sectors = disk_bytes / SECTOR;
    let super_last = gpt.super_first + super_bytes / SECTOR - 1;
    let (gpt_head, gpt_tail) = new_gpt(&mbr, &gpt, sectors, super_last)?;
    // Partitions in the order they are written.
    let mut placed: Vec<Partition> = Vec::new();
    let mut rest: Vec<Option<Partition>> = partitions.into_iter().map(Some).collect();
    for &i in &order {
        if let Some(p) = rest[i].take() {
            if p.len > 0 {
                placed.push(p);
            }
        }
    }
    Ok(DiskPlan {
        disk_bytes,
        super_offset,
        super_bytes,
        gpt_head,
        gpt_tail,
        super_head,
        partitions: placed,
    })
}

/// The metadata tables with one extent per placed partition, and where each table starts:
/// (tables, extents offset, extents count, groups offset, block devices offset).
fn new_tables(
    meta: &Metadata,
    parts: &Table,
    extent_size: usize,
    devices: &Table,
    partitions: &[Partition],
    super_bytes: u64,
) -> (Vec<u8>, usize, usize, usize, usize) {
    let groups = table(&meta.header, 104);
    let mut part_table =
        meta.tables[parts.offset..parts.offset + parts.count * parts.size].to_vec();
    let mut extent_table = Vec::new();
    let mut extent_count = 0u32;
    for (i, p) in partitions.iter().enumerate() {
        let entry = &mut part_table[i * parts.size..(i + 1) * parts.size];
        if p.len == 0 {
            put32(entry, 40, extent_count);
            put32(entry, 44, 0);
            continue;
        }
        put32(entry, 40, extent_count);
        put32(entry, 44, 1);
        let mut e = vec![0u8; extent_size];
        put64(&mut e, 0, p.len / SECTOR);
        put32(&mut e, 8, LP_TARGET_LINEAR);
        put64(&mut e, 12, p.offset / SECTOR);
        put32(&mut e, 20, 0);
        extent_table.extend_from_slice(&e);
        extent_count += 1;
    }
    let group_table =
        meta.tables[groups.offset..groups.offset + groups.count * groups.size].to_vec();
    let mut device_table = meta.tables[devices.offset..devices.offset + devices.size].to_vec();
    put64(&mut device_table, 16, super_bytes);
    let mut out = part_table;
    let extents_at = out.len();
    out.extend_from_slice(&extent_table);
    let groups_at = out.len();
    out.extend_from_slice(&group_table);
    let devices_at = out.len();
    out.extend_from_slice(&device_table);
    (
        out,
        extents_at,
        extent_count as usize,
        groups_at,
        devices_at,
    )
}

impl DiskPlan {
    /// The partitions (indexes into [`partitions`](Self::partitions)) the copy holds an
    /// uncompressed file system for.
    pub fn copied(&self) -> Vec<usize> {
        (0..self.partitions.len())
            .filter(|&i| self.partitions[i].copy.is_some())
            .collect()
    }

    /// Writes partition `index` as the image has it, for a check of the copy.
    pub fn write_original(
        &self,
        dev: &mut dyn ReadAt,
        index: usize,
        out: &mut dyn Write,
    ) -> Result<(), String> {
        for &(start, len) in &self.partitions[index].source {
            copy(dev, start, len, out, &|| false)?;
        }
        Ok(())
    }

    /// Writes the uncompressed file system the copy holds for partition `index`, without the
    /// zeroes after it.
    pub fn write_copied(
        &self,
        dev: &mut dyn ReadAt,
        index: usize,
        out: &mut dyn Write,
    ) -> Result<(), String> {
        let p = &self.partitions[index];
        let plan = p.copy.as_ref().ok_or("the partition is not copied")?;
        let mut view = Extents::new(dev, p.source.clone());
        let mut reader = Reader::new(&mut view).map_err(|e| format!("{}: {e}", p.name))?;
        erofs_write::write(&mut reader, plan, out, &|| false)
            .map_err(|e| format!("{}: {e}", p.name))
    }

    /// The stretches the copy is written in, in order: the head (the GPT and the image's
    /// bytes up to super), the super partition's head, each partition, and the end.
    pub fn units(&self) -> Vec<Unit> {
        let mut units = vec![
            Unit {
                name: "disk head".into(),
                offset: 0,
                len: self.super_offset,
            },
            Unit {
                name: "super metadata".into(),
                offset: self.super_offset,
                len: self.super_head.len() as u64,
            },
        ];
        for (i, p) in self.partitions.iter().enumerate() {
            let end = self
                .partitions
                .get(i + 1)
                .map_or(self.super_bytes, |next| next.offset);
            units.push(Unit {
                name: p.name.clone(),
                offset: self.super_offset + p.offset,
                len: end - p.offset,
            });
        }
        let tail = self.super_offset + self.super_bytes;
        units.push(Unit {
            name: "disk end".into(),
            offset: tail,
            len: self.disk_bytes - tail,
        });
        units
    }

    /// Writes unit `index` of [`units`](Self::units) to `out`, reading from the image's disk
    /// `dev`. `stop` is asked as it goes.
    pub fn write_unit(
        &self,
        dev: &mut dyn ReadAt,
        index: usize,
        out: &mut dyn Write,
        stop: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let units = self.units();
        let unit = &units[index];
        let mut counted = Counted {
            inner: out,
            written: 0,
        };
        let io = |e: std::io::Error| format!("writing the copy: {e}");
        match index {
            0 => {
                counted.write_all(&self.gpt_head).map_err(io)?;
                let from = self.gpt_head.len() as u64;
                copy(dev, from, unit.len - from, &mut counted, stop)?;
            }
            1 => counted.write_all(&self.super_head).map_err(io)?,
            i if i == units.len() - 1 => {
                let zeros = unit.len - self.gpt_tail.len() as u64;
                write_zeros(&mut counted, zeros)?;
                counted.write_all(&self.gpt_tail).map_err(io)?;
            }
            i => {
                let p = &self.partitions[i - 2];
                match &p.copy {
                    Some(plan) => {
                        let mut view = Extents::new(dev, p.source.clone());
                        let mut reader =
                            Reader::new(&mut view).map_err(|e| format!("{}: {e}", p.name))?;
                        erofs_write::write(&mut reader, plan, &mut counted, stop)
                            .map_err(|e| format!("{}: {e}", p.name))?;
                    }
                    None => {
                        for &(start, len) in &p.source {
                            copy(dev, start, len, &mut counted, stop)?;
                        }
                    }
                }
                let pad = unit.len - counted.written;
                write_zeros(&mut counted, pad)?;
            }
        }
        if counted.written != unit.len {
            return Err(format!(
                "{} came to {} bytes, not the {} planned",
                unit.name, counted.written, unit.len
            ));
        }
        Ok(())
    }
}

struct Counted<'a> {
    inner: &'a mut dyn Write,
    written: u64,
}

impl Write for Counted<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_zeros(out: &mut dyn Write, mut len: u64) -> Result<(), String> {
    let zeros = vec![0u8; 1 << 20];
    while len > 0 {
        let take = len.min(zeros.len() as u64) as usize;
        out.write_all(&zeros[..take])
            .map_err(|e| format!("writing the copy: {e}"))?;
        len -= take as u64;
    }
    Ok(())
}

fn copy(
    dev: &mut dyn ReadAt,
    from: u64,
    len: u64,
    out: &mut dyn Write,
    stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0;
    while done < len {
        if stop() {
            return Err("stopped".into());
        }
        let take = (len - done).min(buf.len() as u64) as usize;
        dev.read_exact_at(from + done, &mut buf[..take])
            .map_err(|e| format!("reading the system disk at {}: {e}", from + done))?;
        out.write_all(&buf[..take])
            .map_err(|e| format!("writing the copy: {e}"))?;
        done += take as u64;
    }
    Ok(())
}
