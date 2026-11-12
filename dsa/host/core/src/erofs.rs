//! Writing an erofs image: a read-only file system the guest's kernel mounts through a loop
//! device, so the host can hand the guest a tree of files as one checked file.
//!
//! The image is uncompressed and as plain as erofs allows: 4 KiB blocks, compact (32-byte)
//! inodes, and every file, folder and link stored whole in blocks of its own
//! (`EROFS_INODE_FLAT_PLAIN`). Nothing has extended attributes, and everything belongs to
//! root; the guest mounts the image with one SELinux context for all of it. That is enough for
//! the trees the host builds (an ARM translator's libraries and settings), and it keeps the
//! writer small and exact:
//!
//! ```text
//! block 0         the superblock, at byte 1024
//! blocks 1..=M    the inodes, 32 bytes each, inode N at byte 32 * N (N >= 1)
//! after them      each inode's data, in inode order, each starting on a block
//! ```
//!
//! A folder's data is its entries in name order, split into blocks: each block holds the
//! fixed-size entries first (inode number, where the name starts, the file type) and then
//! their names, without terminators.
//!
//! [`Builder`] holds the tree in memory and writes it in one go. [`Stream`] is for trees too
//! large for that (the Google apps' image is a few hundred MiB): it writes each file's data as
//! the file arrives, and the inodes and folders, which need every name, after the data once the
//! last file is in, so no file is held in memory:
//!
//! ```text
//! block 0              the superblock, at byte 1024
//! blocks 1 ..          each file's data, block aligned, in the order the files came
//! block M ..           the inodes (the superblock's meta_blkaddr), inode N at byte 32 * N
//! after them           each folder's entries
//! ```

use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom, Write};

/// The signature the superblock starts with.
pub const MAGIC: u32 = 0xE0F5_E1E2;
/// Where the superblock is.
pub const SUPERBLOCK_OFFSET: usize = 1024;
/// The block size, as a power of two.
const BLOCK_BITS: u8 = 12;
const BLOCK: usize = 1 << BLOCK_BITS;
/// The size of a compact inode, which is also the unit inode numbers count in.
const INODE: usize = 32;
/// The size of a folder entry before the names.
const DIRENT: usize = 12;
/// The longest name a file may have.
const MAX_NAME: usize = 255;

const TYPE_FILE: u8 = 1;
const TYPE_FOLDER: u8 = 2;
const TYPE_LINK: u8 = 7;

const S_IFREG: u16 = 0o100_000;
const S_IFDIR: u16 = 0o040_000;
const S_IFLNK: u16 = 0o120_000;

#[derive(Debug, Clone)]
enum Kind {
    File(Vec<u8>),
    Link(String),
    Folder(BTreeMap<Vec<u8>, usize>),
}

#[derive(Debug, Clone)]
struct Node {
    kind: Kind,
    /// The permission bits.
    permissions: u16,
    parent: usize,
}

/// A tree of files to write as an erofs image.
#[derive(Debug, Clone)]
pub struct Builder {
    nodes: Vec<Node>,
}

impl Default for Builder {
    fn default() -> Self {
        Builder::new()
    }
}

/// Splits a relative path into its names, refusing what could leave the tree.
fn names(path: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err(format!("{path:?} names no file"));
    }
    for part in &parts {
        let plain = *part != "." && *part != ".." && !part.contains('\0');
        if !plain || part.len() > MAX_NAME {
            return Err(format!("{path:?} is not a plain relative path"));
        }
    }
    Ok(parts)
}

fn type_of(kind: &Kind) -> u8 {
    match kind {
        Kind::File(_) => TYPE_FILE,
        Kind::Link(_) => TYPE_LINK,
        Kind::Folder(_) => TYPE_FOLDER,
    }
}

/// The inode number of the node at `index`: inode 0 is left unused.
fn nid(index: usize) -> u64 {
    index as u64 + 1
}

/// A folder's data from its entries (name, inode number, file type), `.` and `..` among them:
/// sorted by name and split into blocks, the last block cut where it ends.
pub(crate) fn folder_blocks(mut entries: Vec<(&[u8], u64, u8)>) -> Vec<u8> {
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut data = Vec::new();
    let mut rest = &entries[..];
    while !rest.is_empty() {
        // As many entries as fit in one block with their names.
        let mut count = 0;
        let mut used = 0;
        while count < rest.len() && used + DIRENT + rest[count].0.len() <= BLOCK {
            used += DIRENT + rest[count].0.len();
            count += 1;
        }
        let (block, after) = rest.split_at(count);
        let start = data.len();
        let mut name_offset = DIRENT * block.len();
        for (name, inode, kind) in block {
            data.extend_from_slice(&inode.to_le_bytes());
            data.extend_from_slice(&(name_offset as u16).to_le_bytes());
            data.push(*kind);
            data.push(0);
            name_offset += name.len();
        }
        for (name, _, _) in block {
            data.extend_from_slice(name);
        }
        rest = after;
        if !rest.is_empty() {
            data.resize(start + BLOCK, 0);
        }
    }
    data
}

/// A compact inode whose data is in blocks of its own (`i_format` 0, FLAT_PLAIN), with no
/// extended attributes.
fn inode_bytes(nid: u64, mode: u16, links: u16, size: u32, start: u32) -> [u8; INODE] {
    let mut inode = [0u8; INODE];
    inode[4..6].copy_from_slice(&mode.to_le_bytes());
    inode[6..8].copy_from_slice(&links.to_le_bytes());
    inode[8..12].copy_from_slice(&size.to_le_bytes());
    inode[16..20].copy_from_slice(&start.to_le_bytes());
    inode[20..24].copy_from_slice(&(nid as u32).to_le_bytes());
    inode
}

/// The superblock: no optional features, no checksum.
fn superblock(root: u64, inodes: u64, blocks: u32, meta_block: u32) -> [u8; 128] {
    let mut superblock = [0u8; 128];
    superblock[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    superblock[12] = BLOCK_BITS;
    superblock[14..16].copy_from_slice(&(root as u16).to_le_bytes());
    superblock[16..24].copy_from_slice(&inodes.to_le_bytes());
    superblock[36..40].copy_from_slice(&blocks.to_le_bytes());
    superblock[40..44].copy_from_slice(&meta_block.to_le_bytes());
    superblock
}

impl Builder {
    /// An empty tree: only the root folder.
    pub fn new() -> Builder {
        let root = Node {
            kind: Kind::Folder(BTreeMap::new()),
            permissions: 0o755,
            parent: 0,
        };
        Builder { nodes: vec![root] }
    }

    /// The child of the folder `parent` called `name`, if there is one.
    fn child(&self, parent: usize, name: &str) -> Result<Option<usize>, String> {
        match &self.nodes[parent].kind {
            Kind::Folder(children) => Ok(children.get(name.as_bytes()).copied()),
            _ => Err(format!("{name:?} is inside a file")),
        }
    }

    /// The folder at `parts`, made (with mode 0755) where it is missing.
    fn folder_at(&mut self, parts: &[&str]) -> Result<usize, String> {
        let mut at = 0;
        for part in parts {
            at = match self.child(at, part)? {
                Some(child) => match self.nodes[child].kind {
                    Kind::Folder(_) => child,
                    _ => return Err(format!("{part:?} is a file, not a folder")),
                },
                None => {
                    let folder = Kind::Folder(BTreeMap::new());
                    self.insert(at, part, folder, 0o755)?
                }
            };
        }
        Ok(at)
    }

    fn insert(
        &mut self,
        parent: usize,
        name: &str,
        kind: Kind,
        permissions: u16,
    ) -> Result<usize, String> {
        if self.child(parent, name)?.is_some() {
            return Err(format!("{name:?} is added twice"));
        }
        let index = self.nodes.len();
        if let Kind::Folder(children) = &mut self.nodes[parent].kind {
            children.insert(name.as_bytes().to_vec(), index);
        }
        self.nodes.push(Node {
            kind,
            permissions: permissions & 0o7777,
            parent,
        });
        Ok(index)
    }

    fn add(&mut self, path: &str, kind: Kind, permissions: u16) -> Result<(), String> {
        let parts = names(path)?;
        let (name, folders) = parts.split_last().expect("at least one name");
        let parent = self.folder_at(folders)?;
        self.insert(parent, name, kind, permissions)?;
        Ok(())
    }

    /// Adds a regular file with these permission bits, making its folders as needed.
    pub fn file(&mut self, path: &str, permissions: u16, data: Vec<u8>) -> Result<(), String> {
        if data.len() > u32::MAX as usize {
            return Err(format!("{path} is larger than 4 GiB"));
        }
        self.add(path, Kind::File(data), permissions)
    }

    /// Adds a symbolic link to `target`, making its folders as needed.
    pub fn link(&mut self, path: &str, target: &str) -> Result<(), String> {
        if target.is_empty() || target.len() >= BLOCK || target.contains('\0') {
            return Err(format!("{path} links to {target:?}, which is not usable"));
        }
        self.add(path, Kind::Link(target.to_string()), 0o777)
    }

    /// Adds a folder (mode 0755) and its parents, where they are missing.
    pub fn folder(&mut self, path: &str) -> Result<(), String> {
        let parts = names(path)?;
        self.folder_at(&parts)?;
        Ok(())
    }

    /// The paths of the regular files in the tree, in path order.
    pub fn files(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect(0, "", &mut out);
        out
    }

    fn collect(&self, index: usize, prefix: &str, out: &mut Vec<String>) {
        if let Kind::Folder(children) = &self.nodes[index].kind {
            for (name, &child) in children {
                let name = String::from_utf8_lossy(name);
                let path = format!("{prefix}{name}");
                match self.nodes[child].kind {
                    Kind::Folder(_) => self.collect(child, &format!("{path}/"), out),
                    Kind::File(_) => out.push(path),
                    Kind::Link(_) => {}
                }
            }
        }
    }

    /// A folder's data: its entries, block by block, the last block cut where it ends.
    fn folder_data(&self, index: usize, children: &BTreeMap<Vec<u8>, usize>) -> Vec<u8> {
        let parent = self.nodes[index].parent;
        let mut entries: Vec<(&[u8], u64, u8)> = vec![
            (b".", nid(index), TYPE_FOLDER),
            (b"..", nid(parent), TYPE_FOLDER),
        ];
        for (name, &child) in children {
            let kind = type_of(&self.nodes[child].kind);
            entries.push((name.as_slice(), nid(child), kind));
        }
        folder_blocks(entries)
    }

    /// The data each node stores: a file's content, a link's target, a folder's entries.
    fn node_data(&self, index: usize) -> Vec<u8> {
        match &self.nodes[index].kind {
            Kind::File(data) => data.clone(),
            Kind::Link(target) => target.as_bytes().to_vec(),
            Kind::Folder(children) => self.folder_data(index, children),
        }
    }

    /// How many links a node has: a folder has one from its parent, its own `.`, and the
    /// `..` of each folder in it.
    fn link_count(&self, index: usize) -> u16 {
        let Kind::Folder(children) = &self.nodes[index].kind else {
            return 1;
        };
        let is_folder = |child: &usize| matches!(self.nodes[*child].kind, Kind::Folder(_));
        let folders = children.values().filter(|c| is_folder(c)).count();
        (2 + folders).min(usize::from(u16::MAX)) as u16
    }

    /// Writes the image to `out`, returning its length in bytes.
    pub fn write(&self, out: &mut dyn Write) -> Result<u64, String> {
        let count = self.nodes.len();
        if count >= usize::from(u16::MAX) {
            return Err(format!("{count} files are more than the image takes"));
        }
        let meta_blocks = ((count + 1) * INODE).div_ceil(BLOCK);
        let datas: Vec<Vec<u8>> = (0..count).map(|i| self.node_data(i)).collect();
        let mut block = 1 + meta_blocks as u64;
        let mut meta = vec![0u8; meta_blocks * BLOCK];
        for (index, node) in self.nodes.iter().enumerate() {
            let size = datas[index].len();
            let blocks = size.div_ceil(BLOCK) as u64;
            let start = if blocks == 0 { 0 } else { block };
            block += blocks;
            if block > u64::from(u32::MAX) {
                return Err("the image would be larger than erofs takes".into());
            }
            let format = match node.kind {
                Kind::File(_) => S_IFREG,
                Kind::Link(_) => S_IFLNK,
                Kind::Folder(_) => S_IFDIR,
            };
            let mode = format | node.permissions;
            let at = nid(index) as usize * INODE;
            let links = self.link_count(index);
            let inode = inode_bytes(nid(index), mode, links, size as u32, start as u32);
            meta[at..at + INODE].copy_from_slice(&inode);
        }
        let total_blocks = block;
        let mut head = vec![0u8; BLOCK];
        // The inodes start at block 1.
        let superblock = superblock(nid(0), count as u64, total_blocks as u32, 1);
        head[SUPERBLOCK_OFFSET..SUPERBLOCK_OFFSET + 128].copy_from_slice(&superblock);
        let io = |e: std::io::Error| format!("the image cannot be written: {e}");
        out.write_all(&head).map_err(io)?;
        out.write_all(&meta).map_err(io)?;
        for data in &datas {
            out.write_all(data).map_err(io)?;
            let pad = data.len().div_ceil(BLOCK) * BLOCK - data.len();
            out.write_all(&vec![0u8; pad]).map_err(io)?;
        }
        Ok(total_blocks * BLOCK as u64)
    }
}

/// Splits `path` (`a/b/c`) into its names, refusing an empty name, `.`, `..`, a name with a
/// NUL or a backslash, and a name longer than erofs takes.
pub fn split_path(path: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = path.split('/').map(str::to_string).collect();
    for name in &names {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.len() > MAX_NAME
            || name.contains('\0')
            || name.contains('\\')
        {
            return Err(format!("{path:?} is not a usable path in an image"));
        }
    }
    Ok(names)
}

fn io_error(error: std::io::Error) -> String {
    format!("the image cannot be written: {error}")
}

#[derive(Debug, Default)]
struct StreamFolder {
    entries: BTreeMap<Vec<u8>, StreamNode>,
}

#[derive(Debug)]
enum StreamNode {
    Folder(StreamFolder),
    File { block: u32, size: u32 },
}

/// The file a [`Stream`] is writing: where its data started, and how much is still to come.
struct Open {
    path: Vec<String>,
    block: u32,
    size: u32,
    left: u64,
}

/// An erofs image written to `out` as its files arrive: regular files (0644) and folders
/// (0755), everything owned by root.
pub struct Stream<W: Write + Seek> {
    out: W,
    /// The block the next file's data starts at.
    next_block: u32,
    root: StreamFolder,
    open: Option<Open>,
}

/// One inode of a [`Stream`], once every file is in.
struct StreamInode {
    nid: u64,
    mode: u16,
    links: u16,
    size: u32,
    block: u32,
    /// A folder's entries, `..` among them; the layout adds `.`.
    entries: Vec<(Vec<u8>, u64, u8)>,
}

impl StreamInode {
    fn folder(nid: u64, parent: u64, folder: &StreamFolder) -> StreamInode {
        let folders = folder
            .entries
            .values()
            .filter(|node| matches!(node, StreamNode::Folder(_)))
            .count();
        StreamInode {
            nid,
            mode: S_IFDIR | 0o755,
            links: (2 + folders).min(usize::from(u16::MAX)) as u16,
            size: 0,
            block: 0,
            entries: vec![(b"..".to_vec(), parent, TYPE_FOLDER)],
        }
    }
}

impl<W: Write + Seek> Stream<W> {
    /// Starts an image at the start of `out`.
    pub fn new(mut out: W) -> Result<Stream<W>, String> {
        out.seek(SeekFrom::Start(0)).map_err(io_error)?;
        out.write_all(&[0u8; BLOCK]).map_err(io_error)?;
        Ok(Stream {
            out,
            next_block: 1,
            root: StreamFolder::default(),
            open: None,
        })
    }

    /// The folder at `names`, made with every folder on the way.
    fn folder_mut(&mut self, names: &[String]) -> Result<&mut StreamFolder, String> {
        let mut folder = &mut self.root;
        for name in names {
            let key = name.as_bytes().to_vec();
            if !folder.entries.contains_key(&key) {
                let made = StreamNode::Folder(StreamFolder::default());
                folder.entries.insert(key.clone(), made);
            }
            folder = match folder.entries.get_mut(&key) {
                Some(StreamNode::Folder(next)) => next,
                _ => {
                    return Err(format!(
                        "{} is a file in the image, not a folder",
                        names.join("/")
                    ))
                }
            };
        }
        Ok(folder)
    }

    /// Makes the folder `path`, and every folder on the way, where they are missing.
    pub fn add_dir(&mut self, path: &str) -> Result<(), String> {
        let names = split_path(path)?;
        self.folder_mut(&names).map(|_| ())
    }

    /// Starts the file `path`, of `size` bytes, which [`write_data`](Self::write_data) then
    /// writes. A file already at `path` is replaced; its data stays in the image unused.
    pub fn begin_file(&mut self, path: &str, size: u64) -> Result<(), String> {
        if self.open.is_some() {
            return Err("a file of the image is still being written".into());
        }
        let size = u32::try_from(size)
            .map_err(|_| format!("{path} is {size} bytes, more than an image file can hold"))?;
        let names = split_path(path)?;
        let (name, parent) = names.split_last().expect("at least one name");
        if let Some(StreamNode::Folder(_)) = self.folder_mut(parent)?.entries.get(name.as_bytes()) {
            return Err(format!("{path} is a folder in the image, not a file"));
        }
        self.open = Some(Open {
            path: names,
            block: self.next_block,
            size,
            left: u64::from(size),
        });
        Ok(())
    }

    /// Writes the next `data` of the file being written.
    pub fn write_data(&mut self, data: &[u8]) -> Result<(), String> {
        let Some(open) = self.open.as_mut() else {
            return Err("no file of the image is being written".into());
        };
        if data.len() as u64 > open.left {
            return Err(format!(
                "{} has more data than the {} bytes it announced",
                open.path.join("/"),
                open.size
            ));
        }
        open.left -= data.len() as u64;
        self.out.write_all(data).map_err(io_error)
    }

    /// Ends the file being written, which must have had all its bytes.
    pub fn end_file(&mut self) -> Result<(), String> {
        let Some(open) = self.open.take() else {
            return Err("no file of the image is being written".into());
        };
        if open.left != 0 {
            return Err(format!(
                "{} ended {} bytes short of the {} it announced",
                open.path.join("/"),
                open.left,
                open.size
            ));
        }
        let size = open.size as usize;
        let blocks = size.div_ceil(BLOCK);
        self.out
            .write_all(&vec![0u8; blocks * BLOCK - size])
            .map_err(io_error)?;
        self.next_block = u32::try_from(u64::from(open.block) + blocks as u64)
            .map_err(|_| "the image is larger than erofs takes".to_string())?;
        let (name, parent) = open.path.split_last().expect("at least one name");
        let file = StreamNode::File {
            block: if blocks == 0 { 0 } else { open.block },
            size: open.size,
        };
        self.folder_mut(parent)?
            .entries
            .insert(name.as_bytes().to_vec(), file);
        Ok(())
    }

    /// Adds the file `path` holding `data`.
    pub fn add_file(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        self.begin_file(path, data.len() as u64)?;
        self.write_data(data)?;
        self.end_file()
    }

    /// Writes the inodes, the folders and the superblock, and returns the output, which then
    /// holds the whole image.
    pub fn finish(mut self) -> Result<W, String> {
        if self.open.is_some() {
            return Err("a file of the image is still being written".into());
        }
        // Number every inode, the root first, breadth first.
        let mut inodes = vec![StreamInode::folder(1, 1, &self.root)];
        let mut queue: Vec<(&StreamFolder, u64)> = vec![(&self.root, 1)];
        let mut at = 0;
        while at < queue.len() {
            let (folder, nid) = queue[at];
            at += 1;
            for (name, node) in &folder.entries {
                let child = inodes.len() as u64 + 1;
                let kind = match node {
                    StreamNode::Folder(sub) => {
                        inodes.push(StreamInode::folder(child, nid, sub));
                        queue.push((sub, child));
                        TYPE_FOLDER
                    }
                    StreamNode::File { block, size } => {
                        inodes.push(StreamInode {
                            nid: child,
                            mode: S_IFREG | 0o644,
                            links: 1,
                            size: *size,
                            block: *block,
                            entries: Vec::new(),
                        });
                        TYPE_FILE
                    }
                };
                inodes[(nid - 1) as usize]
                    .entries
                    .push((name.clone(), child, kind));
            }
        }
        if inodes.len() >= usize::from(u16::MAX) {
            return Err(format!(
                "{} files are more than the image takes",
                inodes.len()
            ));
        }
        let meta_block = self.next_block;
        let meta_blocks = ((inodes.len() + 1) * INODE).div_ceil(BLOCK);
        let mut next_block = u64::from(meta_block) + meta_blocks as u64;
        let mut folders = Vec::new();
        for inode in inodes
            .iter_mut()
            .filter(|inode| inode.mode & 0o170_000 == S_IFDIR)
        {
            let mut entries: Vec<(&[u8], u64, u8)> = inode
                .entries
                .iter()
                .map(|(name, child, kind)| (name.as_slice(), *child, *kind))
                .collect();
            entries.push((b".", inode.nid, TYPE_FOLDER));
            let data = folder_blocks(entries);
            inode.size = data.len() as u32;
            inode.block = u32::try_from(next_block)
                .map_err(|_| "the image is larger than erofs takes".to_string())?;
            next_block += data.len().div_ceil(BLOCK) as u64;
            folders.push(data);
        }
        let blocks = u32::try_from(next_block)
            .map_err(|_| "the image is larger than erofs takes".to_string())?;
        let mut meta = vec![0u8; meta_blocks * BLOCK];
        for inode in &inodes {
            let at = inode.nid as usize * INODE;
            let bytes = inode_bytes(inode.nid, inode.mode, inode.links, inode.size, inode.block);
            meta[at..at + INODE].copy_from_slice(&bytes);
        }
        let start = u64::from(meta_block) * BLOCK as u64;
        self.out.seek(SeekFrom::Start(start)).map_err(io_error)?;
        self.out.write_all(&meta).map_err(io_error)?;
        for data in &folders {
            self.out.write_all(data).map_err(io_error)?;
            let pad = data.len().div_ceil(BLOCK) * BLOCK - data.len();
            self.out.write_all(&vec![0u8; pad]).map_err(io_error)?;
        }
        let superblock = superblock(1, inodes.len() as u64, blocks, meta_block);
        self.out
            .seek(SeekFrom::Start(SUPERBLOCK_OFFSET as u64))
            .map_err(io_error)?;
        self.out.write_all(&superblock).map_err(io_error)?;
        self.out
            .seek(SeekFrom::Start(u64::from(blocks) * BLOCK as u64))
            .map_err(io_error)?;
        self.out.flush().map_err(io_error)?;
        Ok(self.out)
    }
}

/// What an image holds, read back the way the kernel reads it, for the tests of the writer and
/// of what is built with it.
#[cfg(any(test, feature = "conformance"))]
pub mod read {
    use super::*;

    /// One thing in an image: its path, its mode (type and permissions), and its data (a
    /// link's target, a file's content, nothing for a folder).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Item {
        pub path: String,
        pub mode: u16,
        pub data: Vec<u8>,
    }

    fn u16_at(image: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([image[at], image[at + 1]])
    }

    fn u32_at(image: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(image[at..at + 4].try_into().unwrap())
    }

    fn inode(image: &[u8], number: u64) -> (u16, Vec<u8>) {
        let meta = u32_at(image, SUPERBLOCK_OFFSET + 40) as usize * BLOCK;
        let at = meta + number as usize * INODE;
        assert_eq!((u16_at(image, at) >> 1) & 7, 0, "flat plain");
        let mode = u16_at(image, at + 4);
        let size = u32_at(image, at + 8) as usize;
        let start = u32_at(image, at + 16) as usize * BLOCK;
        (mode, image[start..start + size].to_vec())
    }

    fn entries(data: &[u8]) -> Vec<(String, u64)> {
        let mut names = Vec::new();
        for block in data.chunks(BLOCK) {
            let count = u16_at(block, 8) as usize / DIRENT;
            for k in 0..count {
                let at = k * DIRENT;
                let child = u64::from_le_bytes(block[at..at + 8].try_into().unwrap());
                let start = u16_at(block, at + 8) as usize;
                let end = if k + 1 < count {
                    u16_at(block, at + DIRENT + 8) as usize
                } else {
                    let tail = &block[start..];
                    start + tail.iter().position(|&b| b == 0).unwrap_or(tail.len())
                };
                let name = String::from_utf8(block[start..end].to_vec()).unwrap();
                names.push((name, child));
            }
        }
        names
    }

    fn walk(image: &[u8], folder: u64, prefix: &str, out: &mut Vec<Item>) {
        let (_, data) = inode(image, folder);
        let names = entries(&data);
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "entries are in name order");
        for (name, child) in names {
            if name == "." || name == ".." {
                continue;
            }
            let path = format!("{prefix}{name}");
            let (mode, data) = inode(image, child);
            let is_folder = mode & 0o170_000 == S_IFDIR;
            let data = if is_folder { Vec::new() } else { data };
            out.push(Item {
                path: path.clone(),
                mode,
                data,
            });
            if is_folder {
                walk(image, child, &format!("{path}/"), out);
            }
        }
    }

    /// Every regular file in `image`, by path, with its content.
    pub fn files(image: &[u8]) -> BTreeMap<String, Vec<u8>> {
        items(image)
            .into_iter()
            .filter(|item| item.mode & 0o170_000 == S_IFREG)
            .map(|item| (item.path, item.data))
            .collect()
    }

    /// Everything in `image`, in path order.
    pub fn items(image: &[u8]) -> Vec<Item> {
        assert_eq!(u32_at(image, SUPERBLOCK_OFFSET), MAGIC);
        assert_eq!(image[SUPERBLOCK_OFFSET + 12], BLOCK_BITS);
        let blocks = u32_at(image, SUPERBLOCK_OFFSET + 36) as usize;
        assert_eq!(blocks * BLOCK, image.len(), "every block is counted");
        let root = u64::from(u16_at(image, SUPERBLOCK_OFFSET + 14));
        let mut out = Vec::new();
        walk(image, root, "", &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::read::items;
    use super::*;
    use std::io::Cursor;

    fn put(tree: &mut Builder, path: &str, mode: u16, data: &[u8]) {
        tree.file(path, mode, data.to_vec()).unwrap();
    }

    #[test]
    fn fr25_an_erofs_image_holds_the_files_links_and_folders_it_was_given() {
        let mut tree = Builder::new();
        put(&mut tree, "system/lib64/libbridge.so", 0o644, b"bridge");
        let link = "system/lib64/libdsa_native_bridge.so";
        tree.link(link, "libbridge.so").unwrap();
        put(&mut tree, "system/bin/runner", 0o755, &[7; 9000]);
        put(&mut tree, "system/etc/empty", 0o644, b"");
        tree.folder("system/lib64/arm64").unwrap();
        let mut image = Vec::new();
        let length = tree.write(&mut image).unwrap();
        assert_eq!(length as usize, image.len());
        assert!(crate::arm_translation::is_erofs(&image));
        let found = items(&image);
        let paths: Vec<&str> = found.iter().map(|item| item.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "system",
                "system/bin",
                "system/bin/runner",
                "system/etc",
                "system/etc/empty",
                "system/lib64",
                "system/lib64/arm64",
                "system/lib64/libbridge.so",
                "system/lib64/libdsa_native_bridge.so",
            ]
        );
        let at = |path: &str| found.iter().find(|item| item.path == path).unwrap();
        assert_eq!(at("system/bin/runner").mode, 0o100_755);
        assert_eq!(at("system/bin/runner").data, vec![7u8; 9000]);
        assert_eq!(at("system/lib64/libbridge.so").mode, 0o100_644);
        assert_eq!(at(link).mode, 0o120_777);
        assert_eq!(at(link).data, b"libbridge.so");
        assert_eq!(at("system/etc/empty").data, b"");
        assert_eq!(at("system/lib64/arm64").mode, 0o040_755);
        let files = tree.files();
        assert_eq!(files.len(), 3, "{files:?}");
    }

    #[test]
    fn fr25_a_folder_of_many_files_spans_several_blocks_in_name_order() {
        let mut tree = Builder::new();
        for i in 0..400u32 {
            let path = format!("lib64/arm64/lib{i:04}_with_a_long_name.so");
            put(&mut tree, &path, 0o644, &i.to_le_bytes());
        }
        let mut image = Vec::new();
        tree.write(&mut image).unwrap();
        let found = items(&image);
        assert_eq!(found.len(), 402);
        assert_eq!(found[2].path, "lib64/arm64/lib0000_with_a_long_name.so");
        assert_eq!(found[401].data, 399u32.to_le_bytes());
    }

    fn build(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut builder = Stream::new(Cursor::new(Vec::new())).unwrap();
        for (path, data) in files {
            builder.add_file(path, data).unwrap();
        }
        builder.finish().unwrap().into_inner()
    }

    #[test]
    fn fr32_an_image_holds_exactly_the_files_added() {
        let big: Vec<u8> = (0..10_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let image = build(&[
            ("product/priv-app/GmsCore/GmsCore.apk", big.clone()),
            ("product/etc/permissions/a.xml", b"<permissions/>".to_vec()),
            ("system_ext/etc/permissions/b.xml", Vec::new()),
            ("product/app/Calendar/Calendar.apk", vec![1; 4096]),
        ]);
        assert_eq!(image.len() % BLOCK, 0);
        let files = super::read::files(&image);
        assert_eq!(files.len(), 4);
        assert_eq!(files["product/priv-app/GmsCore/GmsCore.apk"], big);
        assert_eq!(files["product/etc/permissions/a.xml"], b"<permissions/>");
        assert_eq!(files["system_ext/etc/permissions/b.xml"], b"");
        assert_eq!(files["product/app/Calendar/Calendar.apk"], vec![1; 4096]);
        assert!(crate::arm_translation::is_erofs(&image));
    }

    #[test]
    fn fr32_a_directory_with_many_entries_spans_blocks() {
        let files: Vec<(String, Vec<u8>)> = (0..600)
            .map(|i| {
                (
                    format!("product/etc/sysconfig/config-{i:04}.xml"),
                    vec![i as u8],
                )
            })
            .collect();
        let mut builder = Stream::new(Cursor::new(Vec::new())).unwrap();
        for (path, data) in &files {
            builder.add_file(path, data).unwrap();
        }
        builder.add_dir("system_ext").unwrap();
        let image = builder.finish().unwrap().into_inner();
        let read = super::read::files(&image);
        assert_eq!(read.len(), 600);
        for (path, data) in &files {
            assert_eq!(&read[path], data, "{path}");
        }
    }

    #[test]
    fn fr32_a_later_file_replaces_an_earlier_one_at_the_same_path() {
        let image = build(&[
            ("product/etc/permissions/a.xml", b"old".to_vec()),
            ("product/etc/permissions/a.xml", b"new".to_vec()),
        ]);
        let files = super::read::files(&image);
        assert_eq!(files.len(), 1);
        assert_eq!(files["product/etc/permissions/a.xml"], b"new");
    }

    #[test]
    fn fr32_unusable_paths_and_short_files_are_refused() {
        let mut builder = Stream::new(Cursor::new(Vec::new())).unwrap();
        for path in ["", "/abs", "a/../b", "a/./b", "a//b", "a\\b"] {
            assert!(builder.begin_file(path, 1).is_err(), "{path:?} is taken");
        }
        builder.add_file("a/b", b"x").unwrap();
        assert!(
            builder.add_file("a/b/c", b"x").is_err(),
            "a file is not a directory"
        );
        assert!(
            builder.add_file("a", b"x").is_err(),
            "a directory is not a file"
        );
        builder.begin_file("c", 4).unwrap();
        builder.write_data(b"ab").unwrap();
        assert!(builder.write_data(b"cde").is_err(), "more than announced");
        assert!(builder.end_file().is_err(), "less than announced");
    }

    /// The reference implementation reads the image the way the guest's kernel does. CI's
    /// Linux runner has it; elsewhere this checks nothing.
    #[test]
    fn fr32_erofs_utils_read_the_image_back_when_they_are_installed() {
        let installed = std::process::Command::new("fsck.erofs")
            .arg("--help")
            .output()
            .is_ok();
        if !installed {
            eprintln!("fsck.erofs is not installed; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("dsa-erofs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut files: Vec<(String, Vec<u8>)> = (0..300)
            .map(|i| {
                (
                    format!("product/etc/sysconfig/config-{i:04}.xml"),
                    vec![i as u8; i],
                )
            })
            .collect();
        files.push((
            "product/priv-app/GmsCore/GmsCore.apk".into(),
            (0..100_000u32).map(|i| (i % 13) as u8).collect(),
        ));
        let mut builder = Stream::new(Cursor::new(Vec::new())).unwrap();
        for (path, data) in &files {
            builder.add_file(path, data).unwrap();
        }
        let image_path = dir.join("system.img");
        std::fs::write(&image_path, builder.finish().unwrap().into_inner()).unwrap();
        let extracted = dir.join("out");
        let output = std::process::Command::new("fsck.erofs")
            .arg(format!("--extract={}", extracted.display()))
            .arg(&image_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fsck.erofs: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for (path, data) in &files {
            assert_eq!(
                &std::fs::read(extracted.join(path)).unwrap(),
                data,
                "{path}"
            );
        }
        // The tree written in one go, with a link, reads back the same way.
        let mut tree = Builder::new();
        put(&mut tree, "system/lib64/libbridge.so", 0o644, b"bridge");
        tree.link("system/lib64/libdsa_native_bridge.so", "libbridge.so")
            .unwrap();
        let mut image = Vec::new();
        tree.write(&mut image).unwrap();
        std::fs::write(&image_path, image).unwrap();
        let extracted = dir.join("tree");
        let output = std::process::Command::new("fsck.erofs")
            .arg(format!("--extract={}", extracted.display()))
            .arg(&image_path)
            .output()
            .unwrap();
        assert!(output.status.success(), "fsck.erofs: {output:?}");
        assert_eq!(
            std::fs::read(extracted.join("system/lib64/libbridge.so")).unwrap(),
            b"bridge"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr25_a_path_that_could_leave_the_tree_is_refused() {
        let mut tree = Builder::new();
        for path in ["", "/", "../etc/passwd", "system/../../x", "a/./b"] {
            let added = tree.file(path, 0o644, Vec::new());
            assert!(added.is_err(), "{path:?}");
        }
        put(&mut tree, "a/b", 0o644, b"");
        let twice = tree.file("a/b", 0o644, Vec::new());
        assert!(twice.is_err());
        let inside_a_file = tree.file("a/b/c", 0o644, Vec::new());
        assert!(inside_a_file.is_err());
        assert!(tree.link("a/c", "").is_err());
    }
}
