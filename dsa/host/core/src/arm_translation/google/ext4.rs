//! Reading files from an ext4 file system image, as Android's build makes them.
//!
//! Only what such an image uses is read: block groups with 32- or 64-byte descriptors,
//! inodes whose data is mapped by extents (or kept inline, or a short link's target kept in
//! the inode), and folders as lists of entries (an indexed folder reads the same, its index
//! blocks look like empty entries). Nothing is ever written.

use std::io::{Read, Seek, SeekFrom};

/// The signature of the superblock, at byte 56 of it.
const MAGIC: u16 = 0xEF53;
const SUPERBLOCK: u64 = 1024;
/// The extent tree's node signature.
const EXTENT_MAGIC: u16 = 0xF30A;
/// Inode flags.
const FLAG_EXTENTS: u32 = 0x8_0000;
const FLAG_INLINE: u32 = 0x1000_0000;
/// Incompatible features: entries carry the file type; 64-bit descriptors; inline data.
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_META_BG: u32 = 0x10;
const INCOMPAT_64BIT: u32 = 0x80;
/// The most a file read here may hold; the translator's largest library is about 9 MiB.
pub const MAX_FILE: u64 = 256 << 20;
/// How deep an extent tree may go.
const MAX_DEPTH: u32 = 5;

const S_IFMT: u16 = 0o170_000;
const S_IFREG: u16 = 0o100_000;
const S_IFDIR: u16 = 0o040_000;
const S_IFLNK: u16 = 0o120_000;

/// What an inode is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    Link,
    Other,
}

/// An inode as read.
#[derive(Debug, Clone)]
pub struct Inode {
    pub mode: u16,
    pub size: u64,
    flags: u32,
    block: [u8; 60],
}

impl Inode {
    pub fn kind(&self) -> Kind {
        match self.mode & S_IFMT {
            S_IFREG => Kind::File,
            S_IFDIR => Kind::Folder,
            S_IFLNK => Kind::Link,
            _ => Kind::Other,
        }
    }

    /// The permission bits.
    pub fn permissions(&self) -> u16 {
        self.mode & 0o7777
    }
}

/// An ext4 file system in `reader`.
pub struct Ext4<R> {
    reader: R,
    block_size: u64,
    inodes_per_group: u32,
    inode_size: u64,
    descriptor_size: u64,
    descriptors: u64,
    groups: u64,
    name_length_is_u8: bool,
}

fn damaged(what: &str) -> String {
    format!("the system partition's {what} is damaged")
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

impl<R: Read + Seek> Ext4<R> {
    /// Opens the file system, checking its superblock.
    pub fn open(mut reader: R) -> Result<Ext4<R>, String> {
        let sb = read_at(&mut reader, SUPERBLOCK, 1024)?;
        if u16_at(&sb, 0x38) != MAGIC {
            return Err("the system partition is not an ext4 file system".into());
        }
        let log_block = u32_at(&sb, 0x18);
        if log_block > 6 {
            return Err(damaged("superblock"));
        }
        let block_size = 1024u64 << log_block;
        let mut blocks = u64::from(u32_at(&sb, 0x4));
        let first_data_block = u64::from(u32_at(&sb, 0x14));
        let blocks_per_group = u64::from(u32_at(&sb, 0x20));
        let inodes_per_group = u32_at(&sb, 0x28);
        let revision = u32_at(&sb, 0x4C);
        let inode_size = if revision == 0 {
            128
        } else {
            u64::from(u16_at(&sb, 0x58))
        };
        let incompat = u32_at(&sb, 0x60);
        let wide = incompat & INCOMPAT_64BIT != 0;
        let descriptor_size = if wide {
            blocks |= u64::from(u32_at(&sb, 0x150)) << 32;
            u64::from(u16_at(&sb, 0xFE))
        } else {
            32
        };
        if incompat & INCOMPAT_META_BG != 0 {
            return Err("the system partition's layout (meta_bg) is not read here".into());
        }
        let sane = blocks_per_group > 0
            && inodes_per_group > 0
            && (128..=block_size).contains(&inode_size)
            && (32..=1024).contains(&descriptor_size);
        if !sane {
            return Err(damaged("superblock"));
        }
        let groups = blocks.saturating_sub(first_data_block);
        let groups = groups.div_ceil(blocks_per_group);
        Ok(Ext4 {
            reader,
            block_size,
            inodes_per_group,
            inode_size,
            descriptor_size,
            descriptors: (first_data_block + 1) * block_size,
            groups,
            name_length_is_u8: incompat & INCOMPAT_FILETYPE != 0,
        })
    }

    /// The inode numbered `number`.
    pub fn inode(&mut self, number: u32) -> Result<Inode, String> {
        if number == 0 {
            return Err(damaged("folder entries"));
        }
        let group = u64::from((number - 1) / self.inodes_per_group);
        let index = u64::from((number - 1) % self.inodes_per_group);
        if group >= self.groups {
            return Err(damaged("folder entries"));
        }
        let at = self.descriptors + group * self.descriptor_size;
        let descriptor = read_at(&mut self.reader, at, self.descriptor_size as usize)?;
        let mut table = u64::from(u32_at(&descriptor, 0x8));
        if self.descriptor_size >= 64 {
            table |= u64::from(u32_at(&descriptor, 0x28)) << 32;
        }
        let at = table * self.block_size + index * self.inode_size;
        let raw = read_at(&mut self.reader, at, 128)?;
        let size_high = u64::from(u32_at(&raw, 0x6C));
        let mut block = [0u8; 60];
        block.copy_from_slice(&raw[0x28..0x28 + 60]);
        Ok(Inode {
            mode: u16_at(&raw, 0),
            size: u64::from(u32_at(&raw, 0x4)) | (size_high << 32),
            flags: u32_at(&raw, 0x20),
            block,
        })
    }

    /// The extents under the extent tree node `node`: (first block in the file, blocks,
    /// first block on the disk, whether the blocks are unwritten).
    fn extents(
        &mut self,
        node: &[u8],
        depth_left: u32,
        out: &mut Vec<(u64, u64, u64, bool)>,
    ) -> Result<(), String> {
        if node.len() < 12 || u16_at(node, 0) != EXTENT_MAGIC {
            return Err(damaged("extent tree"));
        }
        let entries = usize::from(u16_at(node, 2));
        let depth = u16_at(node, 6);
        if node.len() < 12 + 12 * entries || depth_left == 0 {
            return Err(damaged("extent tree"));
        }
        for k in 0..entries {
            let entry = &node[12 + 12 * k..24 + 12 * k];
            if depth == 0 {
                let logical = u64::from(u32_at(entry, 0));
                let mut length = u64::from(u16_at(entry, 4));
                let start_high = u64::from(u16_at(entry, 6));
                let start = (start_high << 32) | u64::from(u32_at(entry, 8));
                let unwritten = length > 32_768;
                if unwritten {
                    length -= 32_768;
                }
                out.push((logical, length, start, unwritten));
            } else {
                let leaf_high = u64::from(u16_at(entry, 8));
                let leaf = (leaf_high << 32) | u64::from(u32_at(entry, 4));
                let at = leaf * self.block_size;
                let child = read_at(&mut self.reader, at, self.block_size as usize)?;
                self.extents(&child, depth_left - 1, out)?;
            }
        }
        Ok(())
    }

    /// The whole content of `inode`: a file's bytes, a link's target, a folder's entries.
    pub fn read(&mut self, inode: &Inode) -> Result<Vec<u8>, String> {
        if inode.size > MAX_FILE {
            return Err(format!("a file of {} bytes is too large", inode.size));
        }
        let size = inode.size as usize;
        if inode.flags & FLAG_INLINE != 0 {
            if size > inode.block.len() {
                return Err("a file kept partly in its attributes is not read here".into());
            }
            return Ok(inode.block[..size].to_vec());
        }
        if inode.flags & FLAG_EXTENTS == 0 {
            if inode.kind() == Kind::Link && size < inode.block.len() {
                return Ok(inode.block[..size].to_vec());
            }
            if size == 0 {
                return Ok(Vec::new());
            }
            return Err("a file mapped without extents is not read here".into());
        }
        let mut extents = Vec::new();
        self.extents(&inode.block, MAX_DEPTH, &mut extents)?;
        let mut data = vec![0u8; size];
        for (logical, length, start, unwritten) in extents {
            let from = logical.saturating_mul(self.block_size);
            if unwritten || from >= inode.size {
                continue;
            }
            let bytes = length.saturating_mul(self.block_size);
            let bytes = bytes.min(inode.size - from);
            let disk = start.saturating_mul(self.block_size);
            let piece = read_at(&mut self.reader, disk, bytes as usize)?;
            let from = from as usize;
            data[from..from + piece.len()].copy_from_slice(&piece);
        }
        Ok(data)
    }

    /// The entries of the folder `inode`, without `.` and `..`: each name with its inode
    /// number.
    pub fn list(&mut self, inode: &Inode) -> Result<Vec<(String, u32)>, String> {
        if inode.kind() != Kind::Folder {
            return Err(damaged("folder"));
        }
        let data = self.read(inode)?;
        let mut entries = Vec::new();
        let mut at = 0;
        while at + 8 <= data.len() {
            let number = u32_at(&data, at);
            let length = usize::from(u16_at(&data, at + 4));
            let name_length = if self.name_length_is_u8 {
                usize::from(data[at + 6])
            } else {
                usize::from(u16_at(&data, at + 6))
            };
            if length < 8 || at + length > data.len() || 8 + name_length > length {
                return Err(damaged("folder"));
            }
            let name = &data[at + 8..at + 8 + name_length];
            if number != 0 && name != b"." && name != b".." && !name.is_empty() {
                let name = String::from_utf8_lossy(name).into_owned();
                entries.push((name, number));
            }
            at += length;
        }
        Ok(entries)
    }

    /// The inode at `path` (names separated by `/`, from the root), if there is one.
    pub fn find(&mut self, path: &str) -> Result<Option<Inode>, String> {
        let mut inode = self.inode(2)?;
        for name in path.split('/').filter(|name| !name.is_empty()) {
            if inode.kind() != Kind::Folder {
                return Ok(None);
            }
            let entries = self.list(&inode)?;
            match entries.into_iter().find(|(entry, _)| entry == name) {
                Some((_, number)) => inode = self.inode(number)?,
                None => return Ok(None),
            }
        }
        Ok(Some(inode))
    }
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, length: usize) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| format!("the system partition cannot be read: {e}");
    reader.seek(SeekFrom::Start(at)).map_err(io)?;
    let mut data = vec![0u8; length];
    reader.read_exact(&mut data).map_err(io)?;
    Ok(data)
}

/// ext4 images shaped as Android's build makes them, for the tests here and of the image
/// reader.
#[cfg(any(test, feature = "conformance"))]
pub(crate) mod fixture {
    /// What to put in an image: a file with its permissions, a link, or a folder.
    pub enum Item<'a> {
        File(&'a str, u16, &'a [u8]),
        Link(&'a str, &'a str),
        Folder(&'a str),
    }

    const BLOCK: usize = 4096;
    const INODES: usize = 256;
    const INODE_SIZE: usize = 256;
    /// Blocks 0 (superblock), 1 (descriptors), then the inode table.
    const TABLE: usize = 2;
    const FIRST_DATA: usize = TABLE + INODES * INODE_SIZE / BLOCK;

    /// A folder entry: its name, its inode and its file type.
    type Entry = (String, u32, u8);

    struct Builder {
        image: Vec<u8>,
        next_inode: u32,
        next_block: usize,
        /// Each folder: its path, its inode and its entries.
        folders: Vec<(String, u32, Vec<Entry>)>,
    }

    impl Builder {
        fn inode(&mut self, number: u32, mode: u16, size: u64, flags: u32, block: &[u8]) {
            let at = TABLE * BLOCK + (number as usize - 1) * INODE_SIZE;
            let inode = &mut self.image[at..at + INODE_SIZE];
            inode[0..2].copy_from_slice(&mode.to_le_bytes());
            inode[4..8].copy_from_slice(&(size as u32).to_le_bytes());
            inode[0x6C..0x70].copy_from_slice(&((size >> 32) as u32).to_le_bytes());
            inode[0x20..0x24].copy_from_slice(&flags.to_le_bytes());
            inode[0x28..0x28 + block.len()].copy_from_slice(block);
        }

        /// Stores `data` in blocks of its own, mapped by one extent in the inode.
        fn data(&mut self, number: u32, mode: u16, data: &[u8]) {
            let blocks = data.len().div_ceil(BLOCK).max(1);
            let start = self.next_block;
            self.next_block += blocks;
            self.image.resize(self.next_block * BLOCK, 0);
            let at = start * BLOCK;
            self.image[at..at + data.len()].copy_from_slice(data);
            let mut tree = vec![0u8; 24];
            tree[0..2].copy_from_slice(&0xF30Au16.to_le_bytes());
            tree[2..4].copy_from_slice(&1u16.to_le_bytes());
            tree[4..6].copy_from_slice(&4u16.to_le_bytes());
            tree[16..18].copy_from_slice(&(blocks as u16).to_le_bytes());
            tree[20..24].copy_from_slice(&(start as u32).to_le_bytes());
            self.inode(number, mode, data.len() as u64, 0x8_0000, &tree);
        }

        fn folder(&mut self, path: &str) -> u32 {
            if let Some(found) = self.folders.iter().find(|f| f.0 == path) {
                return found.1;
            }
            let (parent, name) = match path.rsplit_once('/') {
                Some((parent, name)) => (self.folder(parent), name),
                None => (2, path),
            };
            let number = self.next_inode;
            self.next_inode += 1;
            self.entry(parent, name, number, 2);
            self.folders.push((path.to_string(), number, Vec::new()));
            number
        }

        fn entry(&mut self, folder: u32, name: &str, number: u32, kind: u8) {
            let found = self.folders.iter_mut().find(|f| f.1 == folder).unwrap();
            found.2.push((name.to_string(), number, kind));
        }

        fn add(&mut self, path: &str, kind: u8) -> u32 {
            let (parent, name) = match path.rsplit_once('/') {
                Some((parent, name)) => (self.folder(parent), name),
                None => (2, path),
            };
            let number = self.next_inode;
            self.next_inode += 1;
            self.entry(parent, name, number, kind);
            number
        }
    }

    /// An ext4 image of 4 KiB blocks holding `items`, its folders made as needed.
    pub fn image(items: &[Item<'_>]) -> Vec<u8> {
        let mut builder = Builder {
            image: vec![0u8; FIRST_DATA * BLOCK],
            next_inode: 12,
            next_block: FIRST_DATA,
            folders: vec![(String::new(), 2, Vec::new())],
        };
        for item in items {
            match item {
                Item::File(path, permissions, data) => {
                    let number = builder.add(path, 1);
                    builder.data(number, 0o100_000 | permissions, data);
                }
                Item::Link(path, target) => {
                    let number = builder.add(path, 7);
                    let size = target.len() as u64;
                    builder.inode(number, 0o120_777, size, 0, target.as_bytes());
                }
                Item::Folder(path) => {
                    builder.folder(path);
                }
            }
        }
        let folders = std::mem::take(&mut builder.folders);
        for (path, number, entries) in &folders {
            let parent = match path.rsplit_once('/') {
                Some((parent, _)) => folders.iter().find(|f| f.0 == parent).unwrap().1,
                None => 2,
            };
            let mut all = vec![(".".to_string(), *number, 2u8)];
            all.push(("..".to_string(), parent, 2));
            all.extend(entries.iter().cloned());
            let mut data = Vec::new();
            for (i, (name, child, kind)) in all.iter().enumerate() {
                let length = if i + 1 == all.len() {
                    BLOCK - data.len()
                } else {
                    (8 + name.len()).div_ceil(4) * 4
                };
                data.extend_from_slice(&child.to_le_bytes());
                data.extend_from_slice(&(length as u16).to_le_bytes());
                data.push(name.len() as u8);
                data.push(*kind);
                data.extend_from_slice(name.as_bytes());
                data.resize(data.len() + length - 8 - name.len(), 0);
            }
            assert_eq!(data.len(), BLOCK, "one block per folder");
            builder.data(*number, 0o040_755, &data);
        }
        let blocks = builder.next_block as u32;
        let sb = &mut builder.image[1024..2048];
        sb[0x4..0x8].copy_from_slice(&blocks.to_le_bytes());
        sb[0x18..0x1C].copy_from_slice(&2u32.to_le_bytes());
        sb[0x20..0x24].copy_from_slice(&32_768u32.to_le_bytes());
        sb[0x28..0x2C].copy_from_slice(&(INODES as u32).to_le_bytes());
        sb[0x38..0x3A].copy_from_slice(&0xEF53u16.to_le_bytes());
        sb[0x4C..0x50].copy_from_slice(&1u32.to_le_bytes());
        sb[0x58..0x5A].copy_from_slice(&(INODE_SIZE as u16).to_le_bytes());
        sb[0x60..0x64].copy_from_slice(&0x42u32.to_le_bytes());
        // The one group's descriptor, in block 1, names the inode table.
        let table = (TABLE as u32).to_le_bytes();
        let mut image = builder.image;
        image[BLOCK + 8..BLOCK + 12].copy_from_slice(&table);
        image
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{image, Item};
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fr25_files_links_and_folders_are_read_from_an_ext4_image() {
        let library = vec![5u8; 10_000];
        let built = image(&[
            Item::File("system/lib64/libndk_translation.so", 0o644, &library),
            Item::Link("system/lib64/liblink.so", "libndk_translation.so"),
            Item::File("system/bin/runner", 0o755, b"#!"),
            Item::Folder("system/etc/empty"),
        ]);
        let mut fs = Ext4::open(Cursor::new(built)).unwrap();
        let path = "system/lib64/libndk_translation.so";
        let file = fs.find(path).unwrap().unwrap();
        assert_eq!(file.kind(), Kind::File);
        assert_eq!(file.permissions(), 0o644);
        assert_eq!(fs.read(&file).unwrap(), library);
        let link = fs.find("system/lib64/liblink.so").unwrap().unwrap();
        assert_eq!(link.kind(), Kind::Link);
        assert_eq!(fs.read(&link).unwrap(), b"libndk_translation.so");
        let runner = fs.find("system/bin/runner").unwrap().unwrap();
        assert_eq!(runner.permissions(), 0o755);
        let lib64 = fs.find("system/lib64").unwrap().unwrap();
        let entries = fs.list(&lib64).unwrap();
        let mut names: Vec<String> = entries.into_iter().map(|e| e.0).collect();
        names.sort();
        assert_eq!(names, ["liblink.so", "libndk_translation.so"]);
        let empty = fs.find("system/etc/empty").unwrap().unwrap();
        assert!(fs.list(&empty).unwrap().is_empty());
        assert!(fs.find("system/missing").unwrap().is_none());
        assert!(fs.find("system/bin/runner/x").unwrap().is_none());
    }

    #[test]
    fn fr25_what_is_not_ext4_is_refused() {
        assert!(Ext4::open(Cursor::new(vec![0u8; 8192])).is_err());
        assert!(Ext4::open(Cursor::new(vec![0u8; 100])).is_err());
    }
}
