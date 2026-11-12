//! Writing an uncompressed copy of an erofs file system, which the guest maps with DAX.
//!
//! The copy has the same tree, modes, owners, times, link counts and extended attributes as
//! the original (the SELinux labels and file capabilities among them), and every file's data
//! stored whole: `FLAT_PLAIN`, the layout erofs maps directly, for every inode. It is laid out
//! in one pass over the original's metadata ([`plan`]), and written in order ([`write`]), so
//! the same original always gives the same bytes:
//!
//! ```text
//! block 0              the superblock, at byte 1024
//! blocks 1 ..          the inodes (64 bytes each, extended) with their attributes inline,
//!                      in the order a walk of the tree first meets them; nid 0 is unused
//! after them           each inode's data in the same order, block aligned
//! ```

use std::collections::HashMap;
use std::io::Write;

use super::erofs_read::{
    Entry, Inode, Reader, Xattr, BLOCK, BLOCK_BITS, MAGIC, SUPERBLOCK_OFFSET, S_IFBLK, S_IFCHR,
    S_IFDIR, S_IFLNK, S_IFMT, S_IFREG,
};

const EXTENDED_INODE: u64 = 64;
const SLOT: u64 = 32;
const TYPE_FOLDER: u8 = 2;

struct Node {
    inode: Inode,
    /// The folder's entries with the copy's inode numbers, `.` and `..` among them.
    entries: Option<Vec<(Vec<u8>, u64, u8)>>,
    nid: u64,
    /// The data's size in the copy and its first block (0 for none).
    size: u64,
    block: u32,
}

/// The layout of the copy of one file system.
pub struct Plan {
    nodes: Vec<Node>,
    root: u64,
    meta_blocks: u64,
    total_blocks: u64,
    build_time: u64,
    build_time_nsec: u32,
    uuid: [u8; 16],
    volume_name: [u8; 16],
}

impl Plan {
    /// The copy's size in bytes.
    pub fn bytes(&self) -> u64 {
        self.total_blocks * BLOCK
    }
}

fn xattr_body(xattrs: &[Xattr]) -> Vec<u8> {
    if xattrs.is_empty() {
        return Vec::new();
    }
    let mut body = vec![0u8; 12];
    for x in xattrs {
        body.push(x.name.len() as u8);
        body.push(x.index);
        body.extend_from_slice(&(x.value.len() as u16).to_le_bytes());
        body.extend_from_slice(&x.name);
        body.extend_from_slice(&x.value);
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }
    body
}

/// Lays out the copy of the file system `reader` reads.
pub fn plan(reader: &mut Reader) -> Result<Plan, String> {
    let root_nid = reader.sb.root_nid;
    let mut index_of: HashMap<u64, usize> = HashMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    // Each folder's entries in the original's inode numbers, kept until all are numbered.
    let mut listings: Vec<Option<Vec<Entry>>> = Vec::new();
    let mut parents: Vec<usize> = Vec::new();
    let mut stack = vec![(root_nid, 0usize)];
    while let Some((nid, parent)) = stack.pop() {
        if index_of.contains_key(&nid) {
            continue;
        }
        let inode = reader.inode(nid)?;
        let index = nodes.len();
        index_of.insert(nid, index);
        let listing = if inode.kind() == S_IFDIR {
            let entries = reader.entries(&inode)?;
            // In name order, so the walk meets the children as the folder lists them.
            for entry in entries.iter().rev() {
                if entry.name != b"." && entry.name != b".." {
                    stack.push((entry.nid, index));
                }
            }
            Some(entries)
        } else {
            match inode.kind() {
                S_IFREG | S_IFLNK | S_IFCHR | S_IFBLK | 0o010_000 | 0o140_000 => {}
                other => return Err(format!("inode {nid} has the file type {other:#o}")),
            }
            None
        };
        nodes.push(Node {
            inode,
            entries: None,
            nid: 0,
            size: 0,
            block: 0,
        });
        listings.push(listing);
        parents.push(if nid == root_nid { index } else { parent });
    }
    if nodes.len() > u32::MAX as usize {
        return Err("the file system has more inodes than a copy can number".into());
    }
    // Inode numbers: 32-byte slots from slot 1.
    let mut offset = SLOT;
    for node in &mut nodes {
        node.nid = offset / SLOT;
        let body = xattr_body(&node.inode.xattrs).len() as u64;
        if body > 0 && (body - 12) / 4 + 1 > u64::from(u16::MAX) {
            return Err(format!(
                "inode {} has more attributes than an inode holds",
                node.inode.nid
            ));
        }
        offset += (EXTENDED_INODE + body).div_ceil(SLOT) * SLOT;
    }
    let root = nodes[0].nid;
    if root > u64::from(u16::MAX) {
        return Err("the root folder's inode number does not fit the superblock".into());
    }
    let meta_blocks = offset.div_ceil(BLOCK);
    for index in 0..nodes.len() {
        if let Some(listing) = &listings[index] {
            let mut entries = Vec::with_capacity(listing.len());
            for entry in listing {
                let nid = if entry.name == b"." {
                    nodes[index].nid
                } else if entry.name == b".." {
                    nodes[parents[index]].nid
                } else {
                    let target = index_of.get(&entry.nid).ok_or_else(|| {
                        format!("folder {} names a missing inode", nodes[index].inode.nid)
                    })?;
                    nodes[*target].nid
                };
                entries.push((entry.name.clone(), nid, entry.file_type));
            }
            if !entries.iter().any(|e| e.0 == b".") {
                entries.push((b".".to_vec(), nodes[index].nid, TYPE_FOLDER));
            }
            if !entries.iter().any(|e| e.0 == b"..") {
                entries.push((b"..".to_vec(), nodes[parents[index]].nid, TYPE_FOLDER));
            }
            let data = folder_data(&entries);
            nodes[index].size = data.len() as u64;
            nodes[index].entries = Some(entries);
        } else {
            let kind = nodes[index].inode.kind();
            nodes[index].size = if kind == S_IFREG || kind == S_IFLNK {
                nodes[index].inode.size
            } else {
                0
            };
        }
    }
    let mut block = 1 + meta_blocks;
    for node in &mut nodes {
        let blocks = node.size.div_ceil(BLOCK);
        node.block = if blocks == 0 { 0 } else { block as u32 };
        block += blocks;
        if block > u64::from(u32::MAX) {
            return Err("the copy would be larger than erofs addresses".into());
        }
    }
    Ok(Plan {
        nodes,
        root,
        meta_blocks,
        total_blocks: block,
        build_time: reader.sb.build_time,
        build_time_nsec: reader.sb.build_time_nsec,
        uuid: reader.sb.uuid,
        volume_name: reader.sb.volume_name,
    })
}

fn folder_data(entries: &[(Vec<u8>, u64, u8)]) -> Vec<u8> {
    crate::erofs::folder_blocks(
        entries
            .iter()
            .map(|(name, nid, kind)| (name.as_slice(), *nid, *kind))
            .collect(),
    )
}

fn superblock(plan: &Plan) -> [u8; 128] {
    let mut sb = [0u8; 128];
    sb[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    sb[12] = BLOCK_BITS;
    sb[14..16].copy_from_slice(&(plan.root as u16).to_le_bytes());
    sb[16..24].copy_from_slice(&(plan.nodes.len() as u64).to_le_bytes());
    sb[24..32].copy_from_slice(&plan.build_time.to_le_bytes());
    sb[32..36].copy_from_slice(&plan.build_time_nsec.to_le_bytes());
    sb[36..40].copy_from_slice(&(plan.total_blocks as u32).to_le_bytes());
    // The inodes start at block 1; there are no shared attributes.
    sb[40..44].copy_from_slice(&1u32.to_le_bytes());
    sb[48..64].copy_from_slice(&plan.uuid);
    sb[64..80].copy_from_slice(&plan.volume_name);
    sb
}

fn inode_bytes(node: &Node, ino: u32) -> Vec<u8> {
    let body = xattr_body(&node.inode.xattrs);
    let mut out = vec![0u8; EXTENDED_INODE as usize];
    // Extended (bit 0), FLAT_PLAIN (layout 0).
    out[0..2].copy_from_slice(&1u16.to_le_bytes());
    let icount: u16 = if body.is_empty() {
        0
    } else {
        ((body.len() - 12) / 4 + 1) as u16
    };
    out[2..4].copy_from_slice(&icount.to_le_bytes());
    out[4..6].copy_from_slice(&node.inode.mode.to_le_bytes());
    out[8..16].copy_from_slice(&node.size.to_le_bytes());
    let kind = node.inode.mode & S_IFMT;
    let raw = if kind == S_IFCHR || kind == S_IFBLK {
        node.inode.raw
    } else {
        node.block
    };
    out[16..20].copy_from_slice(&raw.to_le_bytes());
    out[20..24].copy_from_slice(&ino.to_le_bytes());
    out[24..28].copy_from_slice(&node.inode.uid.to_le_bytes());
    out[28..32].copy_from_slice(&node.inode.gid.to_le_bytes());
    out[32..40].copy_from_slice(&node.inode.mtime.to_le_bytes());
    out[40..44].copy_from_slice(&node.inode.mtime_nsec.to_le_bytes());
    out[44..48].copy_from_slice(&node.inode.nlink.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Writes the copy `plan` lays out to `out`, reading each file's data from `reader`.
/// `stop` is asked between files; when it says so, the write ends with an error.
pub fn write(
    reader: &mut Reader,
    plan: &Plan,
    out: &mut dyn Write,
    stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    let io = |e: std::io::Error| format!("writing the copy: {e}");
    let mut head = vec![0u8; BLOCK as usize];
    head[SUPERBLOCK_OFFSET as usize..SUPERBLOCK_OFFSET as usize + 128]
        .copy_from_slice(&superblock(plan));
    out.write_all(&head).map_err(io)?;
    let mut meta = vec![0u8; (plan.meta_blocks * BLOCK) as usize];
    for (index, node) in plan.nodes.iter().enumerate() {
        let bytes = inode_bytes(node, index as u32 + 1);
        let at = (node.nid * SLOT) as usize;
        meta[at..at + bytes.len()].copy_from_slice(&bytes);
    }
    out.write_all(&meta).map_err(io)?;
    for node in &plan.nodes {
        if stop() {
            return Err("stopped".into());
        }
        let mut written = 0u64;
        if let Some(entries) = &node.entries {
            let data = folder_data(entries);
            out.write_all(&data).map_err(io)?;
            written = data.len() as u64;
        } else if node.size > 0 {
            let size = node.size;
            reader.data(&node.inode, &mut |chunk| {
                if written + chunk.len() as u64 > size {
                    return Err(format!(
                        "inode {} has more data than its size",
                        node.inode.nid
                    ));
                }
                written += chunk.len() as u64;
                out.write_all(chunk).map_err(io)
            })?;
        }
        if written != node.size {
            return Err(format!(
                "inode {} gave {written} bytes of its {}",
                node.inode.nid, node.size
            ));
        }
        let pad = node.size.div_ceil(BLOCK) * BLOCK - node.size;
        out.write_all(&vec![0u8; pad as usize]).map_err(io)?;
    }
    Ok(())
}
