//! Reading the disks of an image's dynamic VHDX files.
//!
//! The image ships its disks as dynamic VHDX files, which Hyper-V attaches. A host that needs
//! the disk's own bytes (a raw copy for crosvm or the Virtualization framework, or the
//! direct-mapped system disk made from it) reads them through [`layout`] and [`VirtualDisk`].
//!
//! The reader follows the VHDX format: the file identifier, the two headers (the one with
//! the higher sequence number wins), the region table that locates the block allocation
//! table and the metadata, the metadata items that give the block size, the logical sector
//! size and the virtual size, and the allocation table's payload entries. A file with a log
//! to replay, or one that is a differencing disk, is refused: the image makes neither.

use std::io::{self, Read, Seek, SeekFrom};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

const FILE_SIGNATURE: &[u8; 8] = b"vhdxfile";
const HEADER_OFFSETS: [u64; 2] = [64 * KIB, 128 * KIB];
const REGION_TABLE_OFFSETS: [u64; 2] = [192 * KIB, 256 * KIB];
/// The metadata table at the start of the metadata region, and the most entries it holds.
const METADATA_TABLE_SIZE: u64 = 64 * KIB;
const METADATA_MAX_ENTRIES: usize = 2047;

/// The region table's entries for the block allocation table and the metadata.
pub const BAT_REGION: &str = "2DC27766-F623-4200-9D64-115E9BFD4A08";
pub const METADATA_REGION: &str = "8B7CA206-4790-4B9A-B8FE-575F050F886E";
/// The metadata items the reader needs.
pub const FILE_PARAMETERS: &str = "CAA16737-FA36-4D43-B3B6-33F0AA44E76B";
pub const VIRTUAL_DISK_SIZE: &str = "2FA54224-CD1B-4876-B211-5DBED83BF4B8";
pub const LOGICAL_SECTOR_SIZE: &str = "8141BF1D-A96F-4709-BA47-F233A8FAAB5F";

/// A payload block whose data is in the file.
const PAYLOAD_FULLY_PRESENT: u64 = 6;
/// A payload block of a differencing disk, partly in its parent.
const PAYLOAD_PARTIALLY_PRESENT: u64 = 7;

/// A GUID's 16 bytes as VHDX stores them: the first three fields little endian.
pub fn guid_bytes(text: &str) -> [u8; 16] {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let mut raw = [0u8; 16];
    for (i, byte) in raw.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("a constant GUID");
    }
    let mut out = raw;
    out[0..4].copy_from_slice(&[raw[3], raw[2], raw[1], raw[0]]);
    out[4..6].copy_from_slice(&[raw[5], raw[4]]);
    out[6..8].copy_from_slice(&[raw[7], raw[6]]);
    out
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn read_at(file: &mut (impl Read + Seek), at: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// What the reader learned of a VHDX: its geometry and where each block's data is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub virtual_size: u64,
    pub block_size: u64,
    pub logical_sector_size: u64,
    /// The file offset of each payload block's data, or `None` for a block that reads as
    /// zeroes.
    pub blocks: Vec<Option<u64>>,
}

/// The bytes of the metadata item `id` in the metadata region `meta`, which must hold at
/// least `size` bytes: 8 for the file parameters and the virtual disk size, 4 for the
/// logical sector size.
///
/// An item's offset counts from the start of the region and its data lies after the 64 KiB
/// table, wholly inside the region. An item that starts inside the table, runs past the
/// region's end or is too short for its value is refused.
fn metadata_item<'a>(meta: &'a [u8], id: &str, size: u64) -> io::Result<&'a [u8]> {
    let entries = u16_at(meta, 10) as usize;
    for i in 0..entries.min(METADATA_MAX_ENTRIES) {
        let entry = &meta[32 + i * 32..32 + (i + 1) * 32];
        if entry[0..16] != guid_bytes(id) {
            continue;
        }
        let (at, len) = (u64::from(u32_at(entry, 16)), u64::from(u32_at(entry, 20)));
        if len < size {
            return Err(invalid(format!(
                "the VHDX metadata item {id} is {len} bytes, too short for its {size}-byte value"
            )));
        }
        let end = at.checked_add(len).filter(|end| *end <= meta.len() as u64);
        return match end {
            Some(end) if at >= METADATA_TABLE_SIZE => Ok(&meta[at as usize..end as usize]),
            _ => Err(invalid(format!(
                "the VHDX metadata item {id} lies outside its region (offset {at}, {len} bytes, \
                 in a region of {} bytes whose first {METADATA_TABLE_SIZE} are the table)",
                meta.len()
            ))),
        };
    }
    Err(invalid(format!("the VHDX lacks the metadata item {id}")))
}

/// Reads the layout of the VHDX in `file`.
pub fn layout(file: &mut (impl Read + Seek)) -> io::Result<Layout> {
    if read_at(file, 0, 8)? != FILE_SIGNATURE {
        return Err(invalid("the file is not a VHDX (no vhdxfile identifier)"));
    }
    // The current header is the valid one with the higher sequence number.
    let mut current: Option<(u64, Vec<u8>)> = None;
    for at in HEADER_OFFSETS {
        let header = read_at(file, at, 4096)?;
        if &header[0..4] != b"head" {
            continue;
        }
        let sequence = u64_at(&header, 8);
        if current.as_ref().is_none_or(|(seen, _)| sequence > *seen) {
            current = Some((sequence, header));
        }
    }
    let (_, header) = current.ok_or_else(|| invalid("the VHDX has no valid header"))?;
    if header[48..64].iter().any(|&b| b != 0) {
        return Err(invalid(
            "the VHDX has a log to replay (it was not closed cleanly); open it once in \
             Hyper-V or qemu-img to finish it",
        ));
    }
    let table = REGION_TABLE_OFFSETS
        .iter()
        .map(|&at| read_at(file, at, 64 * KIB as usize))
        .find(|table| table.as_ref().is_ok_and(|t| &t[0..4] == b"regi"))
        .ok_or_else(|| invalid("the VHDX has no region table"))??;
    let count = u32_at(&table, 8) as usize;
    if count > 2047 {
        return Err(invalid("the VHDX region table is not plausible"));
    }
    let (mut bat, mut metadata) = (None, None);
    for i in 0..count {
        let entry = &table[16 + i * 32..16 + (i + 1) * 32];
        let region = (u64_at(entry, 16), u32_at(entry, 24) as u64);
        if entry[0..16] == guid_bytes(BAT_REGION) {
            bat = Some(region);
        } else if entry[0..16] == guid_bytes(METADATA_REGION) {
            metadata = Some(region);
        }
    }
    let (bat_at, bat_len) = bat.ok_or_else(|| invalid("the VHDX has no allocation table"))?;
    let (meta_at, meta_len) = metadata.ok_or_else(|| invalid("the VHDX has no metadata"))?;
    // The metadata region starts with its 64 KiB table, so a shorter one holds no items.
    if !(METADATA_TABLE_SIZE..=64 * MIB).contains(&meta_len) || bat_len > 1024 * MIB {
        return Err(invalid("the VHDX regions are not plausible"));
    }
    let meta = read_at(file, meta_at, meta_len as usize)?;
    if &meta[0..8] != b"metadata" {
        return Err(invalid("the VHDX metadata table has no signature"));
    }
    let parameters = metadata_item(&meta, FILE_PARAMETERS, 8)?;
    let block_size = u64::from(u32_at(parameters, 0));
    if u32_at(parameters, 4) & 2 != 0 {
        return Err(invalid(
            "the VHDX is a differencing disk, which needs its parent",
        ));
    }
    let virtual_size = u64_at(metadata_item(&meta, VIRTUAL_DISK_SIZE, 8)?, 0);
    let logical_sector_size = u64::from(u32_at(metadata_item(&meta, LOGICAL_SECTOR_SIZE, 4)?, 0));
    if !(MIB..=256 * MIB).contains(&block_size)
        || !block_size.is_power_of_two()
        || ![512, 4096].contains(&logical_sector_size)
    {
        return Err(invalid("the VHDX block or sector size is not plausible"));
    }
    // One sector bitmap entry follows each chunk of payload entries.
    let chunk_ratio = ((1u64 << 23) * logical_sector_size) / block_size;
    let block_count = virtual_size.div_ceil(block_size);
    let table = read_at(file, bat_at, bat_len as usize)?;
    let mut blocks = Vec::with_capacity(block_count as usize);
    for block in 0..block_count {
        let index = (block + block / chunk_ratio) as usize;
        let entry = table
            .get(index * 8..index * 8 + 8)
            .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
            .ok_or_else(|| invalid("the VHDX allocation table is too short"))?;
        let state = entry & 7;
        if state == PAYLOAD_PARTIALLY_PRESENT {
            return Err(invalid(
                "the VHDX has a block that is partly in a parent disk",
            ));
        }
        blocks.push((state == PAYLOAD_FULLY_PRESENT).then_some((entry >> 20) * MIB));
    }
    Ok(Layout {
        virtual_size,
        block_size,
        logical_sector_size,
        blocks,
    })
}

/// Something whose bytes can be read at any offset: a disk, or a part of one.
pub trait ReadAt {
    /// Fills `buf` with the bytes at `offset`. Reading past the end is an error.
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    /// How many bytes there are.
    fn len(&self) -> u64;
    /// Whether there are none.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The virtual disk inside a VHDX, read through its block allocation table: what the guest
/// sees when Hyper-V attaches the file.
pub struct VirtualDisk<F> {
    file: F,
    layout: Layout,
}

impl<F: Read + Seek> VirtualDisk<F> {
    /// Opens the virtual disk of the VHDX `file`.
    pub fn new(mut file: F) -> io::Result<VirtualDisk<F>> {
        let layout = layout(&mut file)?;
        Ok(VirtualDisk { file, layout })
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }
}

impl<F: Read + Seek> ReadAt for VirtualDisk<F> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|end| *end <= self.layout.virtual_size)
            .ok_or_else(|| invalid("a read past the end of the VHDX's disk"))?;
        let mut at = offset;
        let mut done = 0usize;
        while at < end {
            let block = at / self.layout.block_size;
            let within = at % self.layout.block_size;
            let len = ((self.layout.block_size - within).min(end - at)) as usize;
            let part = &mut buf[done..done + len];
            match self.layout.blocks[block as usize] {
                Some(file_offset) => {
                    self.file.seek(SeekFrom::Start(file_offset + within))?;
                    self.file.read_exact(part)?;
                }
                None => part.fill(0),
            }
            at += len as u64;
            done += len;
        }
        Ok(())
    }

    fn len(&self) -> u64 {
        self.layout.virtual_size
    }
}

/// A raw disk image in a file or in memory.
pub struct RawDisk<F> {
    file: F,
    len: u64,
}

impl<F: Read + Seek> RawDisk<F> {
    pub fn new(mut file: F) -> io::Result<RawDisk<F>> {
        let len = file.seek(SeekFrom::End(0))?;
        Ok(RawDisk { file, len })
    }
}

impl<F: Read + Seek> ReadAt for RawDisk<F> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if offset + buf.len() as u64 > self.len {
            return Err(invalid("a read past the end of the disk"));
        }
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)
    }

    fn len(&self) -> u64 {
        self.len
    }
}

/// Opens a disk file as the disk it holds: a VHDX's virtual disk, or a raw image as it is.
pub fn open_disk(path: &std::path::Path) -> io::Result<Box<dyn ReadAt + Send>> {
    let mut file = std::fs::File::open(path)?;
    let mut signature = [0u8; 8];
    let is_vhdx = file.read_exact(&mut signature).is_ok() && &signature == FILE_SIGNATURE;
    file.seek(SeekFrom::Start(0))?;
    // No read buffer: the readers above read whole blocks and keep their own cache, and a
    // buffer would be thrown away at every seek.
    Ok(if is_vhdx {
        Box::new(VirtualDisk::new(file)?)
    } else {
        Box::new(RawDisk::new(file)?)
    })
}

/// Dynamic VHDX files laid out as the image build's `qemu-img convert -O vhdx` writes them.
#[cfg(test)]
pub(crate) mod test_image {
    use super::*;

    /// The Page83 data and physical sector size items qemu-img also writes.
    const PAGE_83_DATA: &str = "BECA12AB-B2E6-4523-93EF-C309E000C746";
    const PHYSICAL_SECTOR_SIZE: &str = "CDA348C7-445D-4471-9CC9-E9885251C556";

    pub(crate) const METADATA_AT: usize = 3 * MIB as usize;
    pub(crate) const METADATA_LEN: usize = MIB as usize;
    const BAT_AT: usize = 2 * MIB as usize;
    const PAYLOAD_AT: usize = 4 * MIB as usize;

    fn put(file: &mut [u8], at: usize, bytes: &[u8]) {
        file[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Sets metadata table entry `index`: the item, its offset from the region's start and
    /// its length.
    pub(crate) fn set_item(file: &mut [u8], index: usize, id: &str, at: u32, len: u32) {
        let entry = METADATA_AT + 32 + index * 32;
        put(file, entry, &guid_bytes(id));
        put(file, entry + 16, &at.to_le_bytes());
        put(file, entry + 20, &len.to_le_bytes());
    }

    /// `disk` in a dynamic VHDX of `block_size` blocks, as qemu-img writes it: two headers
    /// (the second one current), two region tables naming a 1 MiB allocation table at 2 MiB
    /// and a 1 MiB metadata region at 3 MiB, and five metadata items packed from the end of
    /// the table: file parameters (8 bytes), virtual disk size (8), Page83 data (16), logical
    /// sector size (4) and physical sector size (4). Blocks of zeroes are left out.
    pub(crate) fn vhdx_like_qemu_img(disk: &[u8], block_size: usize) -> Vec<u8> {
        let blocks = disk.len().div_ceil(block_size);
        let mut file = vec![0u8; PAYLOAD_AT];
        put(&mut file, 0, FILE_SIGNATURE);
        for (i, at) in HEADER_OFFSETS.into_iter().enumerate() {
            let at = at as usize;
            put(&mut file, at, b"head");
            put(
                &mut file,
                at + 8,
                &(0x85b1_b0cd_u64 + i as u64).to_le_bytes(),
            );
        }
        for at in REGION_TABLE_OFFSETS {
            let at = at as usize;
            put(&mut file, at, b"regi");
            put(&mut file, at + 8, &2u32.to_le_bytes());
            for (i, (id, region)) in [(BAT_REGION, BAT_AT), (METADATA_REGION, METADATA_AT)]
                .into_iter()
                .enumerate()
            {
                let entry = at + 16 + i * 32;
                put(&mut file, entry, &guid_bytes(id));
                put(&mut file, entry + 16, &(region as u64).to_le_bytes());
                put(&mut file, entry + 24, &(MIB as u32).to_le_bytes());
            }
        }
        put(&mut file, METADATA_AT, b"metadata");
        put(&mut file, METADATA_AT + 10, &5u16.to_le_bytes());
        let items: [(&str, Vec<u8>, u32); 5] = [
            (
                FILE_PARAMETERS,
                [(block_size as u32).to_le_bytes(), 0u32.to_le_bytes()].concat(),
                4,
            ),
            (
                VIRTUAL_DISK_SIZE,
                (disk.len() as u64).to_le_bytes().to_vec(),
                6,
            ),
            (PAGE_83_DATA, [0x5au8; 16].to_vec(), 6),
            (LOGICAL_SECTOR_SIZE, 512u32.to_le_bytes().to_vec(), 6),
            (PHYSICAL_SECTOR_SIZE, 512u32.to_le_bytes().to_vec(), 6),
        ];
        let mut offset = METADATA_TABLE_SIZE as usize;
        for (i, (id, value, flags)) in items.iter().enumerate() {
            set_item(&mut file, i, id, offset as u32, value.len() as u32);
            put(
                &mut file,
                METADATA_AT + 32 + i * 32 + 24,
                &flags.to_le_bytes(),
            );
            put(&mut file, METADATA_AT + offset, value);
            offset += value.len();
        }
        for block in 0..blocks {
            let data = &disk[block * block_size..((block + 1) * block_size).min(disk.len())];
            if data.iter().all(|&b| b == 0) {
                continue;
            }
            let at = file.len();
            file.resize(at + block_size, 0);
            put(&mut file, at, data);
            let entry = PAYLOAD_FULLY_PRESENT | ((at as u64 / MIB) << 20);
            put(&mut file, BAT_AT + block * 8, &entry.to_le_bytes());
        }
        file
    }
}

#[cfg(test)]
mod tests {
    use super::test_image::*;
    use super::*;
    use std::io::Cursor;

    /// A 3 MiB disk whose first and last blocks hold data and whose middle block is zeroes.
    fn disk() -> Vec<u8> {
        let mut disk = vec![0u8; 3 * MIB as usize];
        disk[..MIB as usize].fill(0xa5);
        disk[2 * MIB as usize + 100] = 0x5a;
        disk
    }

    fn opens(file: Vec<u8>) -> io::Result<Layout> {
        layout(&mut Cursor::new(file))
    }

    #[test]
    fn nfr1_a_vhdx_laid_out_like_the_image_build_writes_it_opens() {
        let disk = disk();
        let file = vhdx_like_qemu_img(&disk, MIB as usize);
        let found = opens(file.clone()).unwrap();
        assert_eq!(found.virtual_size, disk.len() as u64);
        assert_eq!(found.block_size, MIB);
        assert_eq!(found.logical_sector_size, 512);
        assert_eq!(found.blocks, vec![Some(4 * MIB), None, Some(5 * MIB)]);
        let mut virtual_disk = VirtualDisk::new(Cursor::new(file)).unwrap();
        let mut read = vec![0u8; disk.len()];
        virtual_disk.read_exact_at(0, &mut read).unwrap();
        assert_eq!(read, disk);
    }

    #[test]
    fn nfr1_metadata_items_at_the_edges_of_their_region_are_read() {
        // The logical sector size right after the table, and the file parameters in the
        // region's last 8 bytes.
        let mut file = vhdx_like_qemu_img(&disk(), MIB as usize);
        let first = METADATA_TABLE_SIZE as u32;
        let last = (METADATA_LEN - 8) as u32;
        set_item(&mut file, 3, LOGICAL_SECTOR_SIZE, first, 4);
        file[METADATA_AT + first as usize..][..4].copy_from_slice(&4096u32.to_le_bytes());
        set_item(&mut file, 0, FILE_PARAMETERS, last, 8);
        file[METADATA_AT + last as usize..][..8]
            .copy_from_slice(&[(2 * MIB as u32).to_le_bytes(), 0u32.to_le_bytes()].concat());
        let found = opens(file).unwrap();
        assert_eq!(found.logical_sector_size, 4096);
        assert_eq!(found.block_size, 2 * MIB);
        assert_eq!(found.virtual_size, 3 * MIB);
    }

    #[test]
    fn nfr5_a_metadata_item_outside_its_region_is_refused() {
        let outside = |at: u32, len: u32| {
            let mut file = vhdx_like_qemu_img(&disk(), MIB as usize);
            set_item(&mut file, 1, VIRTUAL_DISK_SIZE, at, len);
            opens(file).unwrap_err().to_string()
        };
        // One byte past the region's end, and wholly past it.
        assert!(outside((METADATA_LEN - 7) as u32, 8).contains("outside its region"));
        assert!(outside(METADATA_LEN as u32, 8).contains("outside its region"));
        // Inside the table, before the first byte an item may use.
        assert!(outside(METADATA_TABLE_SIZE as u32 - 4, 8).contains("outside its region"));
        assert!(outside(0, 8).contains("outside its region"));
        // An offset and a length whose sum does not fit in 32 bits.
        assert!(outside(u32::MAX, 8).contains("outside its region"));
        assert!(outside(METADATA_TABLE_SIZE as u32, u32::MAX).contains("outside its region"));
    }

    #[test]
    fn nfr5_a_metadata_item_too_short_for_its_value_is_refused() {
        let mut file = vhdx_like_qemu_img(&disk(), MIB as usize);
        set_item(
            &mut file,
            3,
            LOGICAL_SECTOR_SIZE,
            METADATA_TABLE_SIZE as u32,
            2,
        );
        assert!(opens(file).unwrap_err().to_string().contains("too short"));
        let mut empty = vhdx_like_qemu_img(&disk(), MIB as usize);
        set_item(&mut empty, 0, FILE_PARAMETERS, 0, 0);
        assert!(opens(empty).unwrap_err().to_string().contains("too short"));
    }

    #[test]
    fn nfr5_a_metadata_region_too_small_for_its_table_is_refused() {
        for len in [0u32, 8, METADATA_TABLE_SIZE as u32 - 1] {
            let mut file = vhdx_like_qemu_img(&disk(), MIB as usize);
            for at in REGION_TABLE_OFFSETS {
                // The metadata region's entry is the second; its length is at byte 24.
                let entry = at as usize + 16 + 32 + 24;
                file[entry..entry + 4].copy_from_slice(&len.to_le_bytes());
            }
            assert!(opens(file)
                .unwrap_err()
                .to_string()
                .contains("not plausible"));
        }
    }
}
