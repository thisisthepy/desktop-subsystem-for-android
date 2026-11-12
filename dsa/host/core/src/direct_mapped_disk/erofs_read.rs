//! Reading an erofs file system as Android's build makes it: inodes, extended attributes,
//! folders, and file data in every layout the build uses, LZ4-compressed clusters included.
//!
//! The layouts follow the kernel's erofs (fs/erofs, Linux 6.18):
//!
//! - `FLAT_PLAIN`: the data in whole blocks from `raw_blkaddr`;
//! - `FLAT_INLINE`: the same, the last block's part kept right after the inode;
//! - `CHUNK_BASED`: a table after the inode names each chunk's first block;
//! - `COMPRESSED_FULL` and `COMPRESSED_COMPACT`: logical clusters of one block, each a head
//!   (where an extent starts, and the physical cluster it is decompressed from) or not, in
//!   8-byte entries or in packs of 4-byte and 2-byte entries. A physical cluster is LZ4, or
//!   stored as it is (`PLAIN`, possibly interlaced), one block or more (big clusters), and the
//!   file's last one may be kept inline after the indexes (tail packing).
//!
//! What the build does not use, and this reader cannot reproduce faithfully, is refused with
//! the reason: other block sizes, extra devices, long extended attribute name prefixes,
//! fragments, and algorithms other than LZ4.

use super::ReadAt;

pub const MAGIC: u32 = 0xE0F5_E1E2;
pub const SUPERBLOCK_OFFSET: u64 = 1024;
pub const BLOCK_BITS: u8 = 12;
pub const BLOCK: u64 = 1 << BLOCK_BITS;

const INCOMPAT_ZERO_PADDING: u32 = 0x1;
/// Every incompatible feature this reader knows: zero padding, compression configurations
/// and big clusters, chunked files, the second head type, tail packing, fragments and
/// deduplication, and long xattr name prefixes (which it then refuses when an image has any).
const INCOMPAT_KNOWN: u32 = 0x7f;

pub const LAYOUT_FLAT_PLAIN: u8 = 0;
pub const LAYOUT_COMPRESSED_FULL: u8 = 1;
pub const LAYOUT_FLAT_INLINE: u8 = 2;
pub const LAYOUT_COMPRESSED_COMPACT: u8 = 3;
pub const LAYOUT_CHUNK_BASED: u8 = 4;

const NULL_ADDR: u32 = u32::MAX;

pub const S_IFMT: u16 = 0o170_000;
pub const S_IFREG: u16 = 0o100_000;
pub const S_IFDIR: u16 = 0o040_000;
pub const S_IFLNK: u16 = 0o120_000;
pub const S_IFCHR: u16 = 0o020_000;
pub const S_IFBLK: u16 = 0o060_000;

const LCLUSTER_PLAIN: u8 = 0;
const LCLUSTER_HEAD1: u8 = 1;
const LCLUSTER_NONHEAD: u8 = 2;
const LCLUSTER_HEAD2: u8 = 3;
/// In a non-head cluster's first delta: the count of blocks of a big physical cluster.
const D0_CBLKCNT: u32 = 1 << 11;

const ADVISE_COMPACTED_2B: u16 = 0x1;
const ADVISE_BIG_PCLUSTER_1: u16 = 0x2;
const ADVISE_BIG_PCLUSTER_2: u16 = 0x4;
const ADVISE_INLINE_PCLUSTER: u16 = 0x8;
const ADVISE_INTERLACED_PCLUSTER: u16 = 0x10;
const ADVISE_FRAGMENT_PCLUSTER: u16 = 0x20;
const COMPRESSION_LZ4: u8 = 0;

/// How many blocks the reader keeps for the metadata it reads again and again.
const CACHE_BLOCKS: usize = 1024;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("four bytes"))
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("eight bytes"))
}

fn align(value: u64, to: u64) -> u64 {
    value.div_ceil(to) * to
}

/// The superblock's fields that matter to a copy of the file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Super {
    pub root_nid: u64,
    pub build_time: u64,
    pub build_time_nsec: u32,
    pub meta_blkaddr: u32,
    pub xattr_blkaddr: u32,
    pub uuid: [u8; 16],
    pub volume_name: [u8; 16],
    pub feature_incompat: u32,
}

/// One extended attribute: its name's prefix index (`security.` is 6) and the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xattr {
    pub index: u8,
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inode {
    pub nid: u64,
    pub mode: u16,
    pub nlink: u32,
    pub size: u64,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u64,
    pub mtime_nsec: u32,
    /// `i_u`: the first block, a device number, or the chunk format, by layout.
    pub raw: u32,
    pub layout: u8,
    pub xattrs: Vec<Xattr>,
    /// Where the inode is, and where what follows it (inline data, indexes) starts.
    iloc: u64,
    ibody_end: u64,
}

impl Inode {
    pub fn kind(&self) -> u16 {
        self.mode & S_IFMT
    }
}

/// A folder entry: its name, inode and file type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    pub nid: u64,
    pub file_type: u8,
}

/// An erofs file system on `dev`.
pub struct Reader<'a> {
    dev: &'a mut dyn ReadAt,
    pub sb: Super,
    cache: Vec<Option<(u64, Box<[u8]>)>>,
}

/// One logical cluster's index, as the kernel's map recorder holds it.
#[derive(Debug, Clone, Copy)]
struct Lcluster {
    kind: u8,
    clusterofs: u32,
    pblk: u64,
    /// The block count of a big physical cluster, on its head's next cluster.
    cblkcnt: Option<u32>,
    /// Where the indexes after this one start: the tail's inline cluster follows the last.
    next: u64,
}

/// A compressed file's map header and where its indexes are.
struct Zmap {
    advise: u16,
    algorithms: [u8; 2],
    idata_size: u16,
    /// The first index: after the map header.
    ebase: u64,
    compact: bool,
    total: u64,
    compacted_4b_initial: u64,
    compacted_2b: u64,
}

impl<'a> Reader<'a> {
    /// Reads the superblock on `dev` and checks that this reader can copy the file system.
    pub fn new(dev: &'a mut dyn ReadAt) -> Result<Reader<'a>, String> {
        let mut sb = [0u8; 128];
        dev.read_exact_at(SUPERBLOCK_OFFSET, &mut sb)
            .map_err(|e| format!("reading the erofs superblock: {e}"))?;
        if u32_at(&sb, 0) != MAGIC {
            return Err("it is not an erofs file system".into());
        }
        if sb[12] != BLOCK_BITS {
            return Err(format!(
                "its blocks are 2^{} bytes; only 4 KiB blocks are copied",
                sb[12]
            ));
        }
        let incompat = u32_at(&sb, 80);
        if incompat & !INCOMPAT_KNOWN != 0 {
            return Err(format!(
                "it uses erofs features {:#x} this host does not know",
                incompat & !INCOMPAT_KNOWN
            ));
        }
        if u16_at(&sb, 86) != 0 {
            return Err("it spans extra devices".into());
        }
        if sb[90] != 0 {
            return Err("its folder blocks are not file system blocks".into());
        }
        if sb[91] != 0 {
            return Err("it has long extended attribute name prefixes".into());
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&sb[48..64]);
        let mut volume_name = [0u8; 16];
        volume_name.copy_from_slice(&sb[64..80]);
        Ok(Reader {
            dev,
            sb: Super {
                root_nid: u64::from(u16_at(&sb, 14)),
                build_time: u64_at(&sb, 24),
                build_time_nsec: u32_at(&sb, 32),
                meta_blkaddr: u32_at(&sb, 40),
                xattr_blkaddr: u32_at(&sb, 44),
                uuid,
                volume_name,
                feature_incompat: incompat,
            },
            cache: vec![None; CACHE_BLOCKS],
        })
    }

    /// Reads `buf.len()` bytes at `offset`, small reads through the block cache.
    fn read(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        if buf.len() as u64 > 2 * BLOCK {
            return self
                .dev
                .read_exact_at(offset, buf)
                .map_err(|e| format!("reading the file system at {offset}: {e}"));
        }
        let mut done = 0usize;
        while done < buf.len() {
            let at = offset + done as u64;
            let block = at / BLOCK;
            let within = (at % BLOCK) as usize;
            let slot = (block as usize) % CACHE_BLOCKS;
            let hit = matches!(&self.cache[slot], Some((b, _)) if *b == block);
            if !hit {
                let mut data = vec![0u8; BLOCK as usize];
                let start = block * BLOCK;
                let len = (self.dev.len().saturating_sub(start)).min(BLOCK) as usize;
                self.dev
                    .read_exact_at(start, &mut data[..len])
                    .map_err(|e| format!("reading the file system at {start}: {e}"))?;
                self.cache[slot] = Some((block, data.into_boxed_slice()));
            }
            let (_, data) = self.cache[slot].as_ref().expect("filled above");
            let take = (BLOCK as usize - within).min(buf.len() - done);
            buf[done..done + take].copy_from_slice(&data[within..within + take]);
            done += take;
        }
        Ok(())
    }

    fn bytes(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len];
        self.read(offset, &mut buf)?;
        Ok(buf)
    }

    fn iloc(&self, nid: u64) -> u64 {
        u64::from(self.sb.meta_blkaddr) * BLOCK + nid * 32
    }

    /// The inode `nid`, with its extended attributes.
    pub fn inode(&mut self, nid: u64) -> Result<Inode, String> {
        let iloc = self.iloc(nid);
        let head = self.bytes(iloc, 64)?;
        let format = u16_at(&head, 0);
        let extended = format & 1 == 1;
        let layout = ((format >> 1) & 7) as u8;
        if format >> 4 != 0 {
            return Err(format!(
                "inode {nid} has format bits {format:#x} this host does not know"
            ));
        }
        let icount = u64::from(u16_at(&head, 2));
        let inode_size = if extended { 64 } else { 32 };
        let xattr_size = if icount == 0 {
            0
        } else {
            12 + 4 * (icount - 1)
        };
        let mut inode = if extended {
            Inode {
                nid,
                mode: u16_at(&head, 4),
                nlink: u32_at(&head, 44),
                size: u64_at(&head, 8),
                uid: u32_at(&head, 24),
                gid: u32_at(&head, 28),
                mtime: u64_at(&head, 32),
                mtime_nsec: u32_at(&head, 40),
                raw: u32_at(&head, 16),
                layout,
                xattrs: Vec::new(),
                iloc,
                ibody_end: iloc + inode_size + xattr_size,
            }
        } else {
            Inode {
                nid,
                mode: u16_at(&head, 4),
                nlink: u32::from(u16_at(&head, 6)),
                size: u64::from(u32_at(&head, 8)),
                uid: u32::from(u16_at(&head, 24)),
                gid: u32::from(u16_at(&head, 26)),
                // A compact inode keeps its time as an offset from the build time.
                mtime: self.sb.build_time + u64::from(u32_at(&head, 12)),
                mtime_nsec: self.sb.build_time_nsec,
                raw: u32_at(&head, 16),
                layout,
                xattrs: Vec::new(),
                iloc,
                ibody_end: iloc + inode_size + xattr_size,
            }
        };
        if layout > LAYOUT_CHUNK_BASED {
            return Err(format!("inode {nid} has the unknown data layout {layout}"));
        }
        if icount != 0 {
            inode.xattrs = self.xattrs(iloc + inode_size, xattr_size)?;
        }
        Ok(inode)
    }

    /// One extended attribute entry at `at`, and where the next one starts.
    fn xattr_entry(&mut self, at: u64) -> Result<(Xattr, u64), String> {
        let head = self.bytes(at, 4)?;
        let (name_len, index) = (u64::from(head[0]), head[1]);
        let value_len = u64::from(u16_at(&head, 2));
        if index & 0x80 != 0 {
            return Err("an extended attribute uses a long name prefix".into());
        }
        let name = self.bytes(at + 4, name_len as usize)?;
        let value = self.bytes(at + 4 + name_len, value_len as usize)?;
        Ok((
            Xattr { index, name, value },
            align(at + 4 + name_len + value_len, 4),
        ))
    }

    fn xattrs(&mut self, start: u64, size: u64) -> Result<Vec<Xattr>, String> {
        let header = self.bytes(start, 12)?;
        let shared = u64::from(header[4]);
        let mut out = Vec::new();
        for i in 0..shared {
            let id = u64::from(u32_at(&self.bytes(start + 12 + 4 * i, 4)?, 0));
            let at = u64::from(self.sb.xattr_blkaddr) * BLOCK + id * 4;
            out.push(self.xattr_entry(at)?.0);
        }
        let end = start + size;
        let mut at = start + 12 + 4 * shared;
        while at + 4 <= end {
            let (entry, next) = self.xattr_entry(at)?;
            if next > end {
                return Err("an extended attribute runs past its inode".into());
            }
            out.push(entry);
            at = next;
        }
        Ok(out)
    }

    /// The entries of the folder `inode`, `.` and `..` among them, in their order.
    pub fn entries(&mut self, inode: &Inode) -> Result<Vec<Entry>, String> {
        let mut data = Vec::new();
        self.data(inode, &mut |chunk| {
            data.extend_from_slice(chunk);
            Ok(())
        })?;
        let mut out = Vec::new();
        for block in data.chunks(BLOCK as usize) {
            if block.len() < 12 {
                return Err(format!("folder {} has a cut entry", inode.nid));
            }
            let first = usize::from(u16_at(block, 8));
            if first < 12 || first % 12 != 0 || first > block.len() {
                return Err(format!("folder {} has a bad entry block", inode.nid));
            }
            let count = first / 12;
            for i in 0..count {
                let at = i * 12;
                let start = usize::from(u16_at(block, at + 8));
                let end = if i + 1 < count {
                    usize::from(u16_at(block, at + 12 + 8))
                } else {
                    // The last name ends at the block's end or at the first NUL.
                    let rest = &block[start.min(block.len())..];
                    start + rest.iter().position(|&b| b == 0).unwrap_or(rest.len())
                };
                if start > end || end > block.len() || start == end {
                    return Err(format!("folder {} has a bad name", inode.nid));
                }
                out.push(Entry {
                    name: block[start..end].to_vec(),
                    nid: u64_at(block, at),
                    file_type: block[at + 10],
                });
            }
        }
        Ok(out)
    }

    /// Gives `sink` the file's data from its start to its end, in pieces.
    pub fn data(
        &mut self,
        inode: &Inode,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        match inode.layout {
            LAYOUT_FLAT_PLAIN => self.flat(inode, inode.size, sink),
            LAYOUT_FLAT_INLINE => {
                let full = inode.size.div_ceil(BLOCK).saturating_sub(1) * BLOCK;
                self.flat(inode, full, sink)?;
                let tail = inode.size - full;
                if (inode.ibody_end % BLOCK) + tail > BLOCK {
                    return Err(format!("inode {}'s inline data crosses a block", inode.nid));
                }
                let data = self.bytes(inode.ibody_end, tail as usize)?;
                sink(&data)
            }
            LAYOUT_CHUNK_BASED => self.chunked(inode, sink),
            LAYOUT_COMPRESSED_FULL | LAYOUT_COMPRESSED_COMPACT => self.compressed(inode, sink),
            other => Err(format!("inode {} has the data layout {other}", inode.nid)),
        }
    }

    /// `len` bytes of whole blocks from the inode's first block (none read for a hole).
    fn flat(
        &mut self,
        inode: &Inode,
        len: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.blocks_from(inode.raw, len, sink)
    }

    fn blocks_from(
        &mut self,
        blkaddr: u32,
        len: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        const PIECE: u64 = 1 << 20;
        let mut done = 0;
        let mut buf = vec![0u8; PIECE.min(len) as usize];
        while done < len {
            let take = (len - done).min(PIECE) as usize;
            if blkaddr == NULL_ADDR {
                buf[..take].fill(0);
            } else {
                let at = u64::from(blkaddr) * BLOCK + done;
                self.read(at, &mut buf[..take])?;
            }
            sink(&buf[..take])?;
            done += take as u64;
        }
        Ok(())
    }

    fn chunked(
        &mut self,
        inode: &Inode,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let format = inode.raw & 0xffff;
        if format & !0x3f != 0 {
            return Err(format!(
                "inode {} has the chunk format {format:#x}",
                inode.nid
            ));
        }
        let chunk_bits = u32::from(BLOCK_BITS) + (format & 0x1f);
        if chunk_bits > 40 {
            return Err(format!("inode {} has chunks of 2^{chunk_bits}", inode.nid));
        }
        let chunk = 1u64 << chunk_bits;
        let indexes = format & 0x20 != 0;
        let unit = if indexes { 8 } else { 4 };
        let table = align(inode.ibody_end, unit);
        let count = inode.size.div_ceil(chunk);
        for i in 0..count {
            let entry = self.bytes(table + i * unit, unit as usize)?;
            let blkaddr = if indexes {
                if u16_at(&entry, 0) != 0 || u16_at(&entry, 2) != 0 {
                    return Err(format!(
                        "inode {} has a chunk on another device or past 16 TiB",
                        inode.nid
                    ));
                }
                u32_at(&entry, 4)
            } else {
                u32_at(&entry, 0)
            };
            let len = (inode.size - i * chunk).min(chunk);
            self.blocks_from(blkaddr, len, sink)?;
        }
        Ok(())
    }

    fn zmap(&mut self, inode: &Inode) -> Result<Zmap, String> {
        let at = align(inode.ibody_end, 8);
        let header = self.bytes(at, 8)?;
        let clusterbits = header[7];
        if clusterbits & 0x80 != 0 {
            return Err(format!(
                "inode {} is kept whole in the packed inode (fragments)",
                inode.nid
            ));
        }
        let advise = u16_at(&header, 4);
        if advise & ADVISE_FRAGMENT_PCLUSTER != 0 {
            return Err(format!("inode {} keeps its tail as a fragment", inode.nid));
        }
        if clusterbits & 7 != 0 {
            return Err(format!(
                "inode {} has logical clusters larger than a block",
                inode.nid
            ));
        }
        let compact = inode.layout == LAYOUT_COMPRESSED_COMPACT;
        let big1 = advise & ADVISE_BIG_PCLUSTER_1 != 0;
        let big2 = advise & ADVISE_BIG_PCLUSTER_2 != 0;
        if compact && big1 != big2 {
            return Err(format!(
                "inode {} has compact indexes with one big cluster type only",
                inode.nid
            ));
        }
        let ebase = at + 8;
        let total = inode.size.div_ceil(BLOCK);
        // Up to the next 32-byte boundary, or none when the indexes start on one.
        let compacted_4b_initial = ((32 - ebase % 32) / 4) % 8;
        let compacted_2b = if advise & ADVISE_COMPACTED_2B != 0 && compacted_4b_initial < total {
            (total - compacted_4b_initial) / 16 * 16
        } else {
            0
        };
        Ok(Zmap {
            advise,
            algorithms: [header[6] & 15, header[6] >> 4],
            idata_size: u16_at(&header, 2),
            ebase,
            compact,
            total,
            compacted_4b_initial,
            compacted_2b,
        })
    }

    fn lcluster(&mut self, z: &Zmap, lcn: u64) -> Result<Lcluster, String> {
        if !z.compact {
            let pos = z.ebase + lcn * 8;
            let entry = self.bytes(pos, 8)?;
            let advise = u16_at(&entry, 0);
            let kind = (advise & 3) as u8;
            let mut out = Lcluster {
                kind,
                clusterofs: 0,
                pblk: 0,
                cblkcnt: None,
                next: pos + 8,
            };
            if kind == LCLUSTER_NONHEAD {
                let d0 = u32::from(u16_at(&entry, 4));
                if d0 & D0_CBLKCNT != 0 {
                    if z.advise & (ADVISE_BIG_PCLUSTER_1 | ADVISE_BIG_PCLUSTER_2) == 0 {
                        return Err("a block count without big clusters".into());
                    }
                    out.cblkcnt = Some(d0 & !D0_CBLKCNT);
                }
            } else {
                out.clusterofs = u32::from(u16_at(&entry, 2));
                if u64::from(out.clusterofs) >= BLOCK {
                    return Err("a cluster offset past its cluster".into());
                }
                out.pblk = u64::from(u32_at(&entry, 4));
            }
            return Ok(out);
        }
        // Compact indexes: 4-byte entries up to a 32-byte boundary, then 2-byte entries in
        // packs of 16 (when the file says so), then 4-byte entries for the rest.
        let mut pos = z.ebase;
        let mut lcn_left = lcn;
        let shift;
        if lcn_left < z.compacted_4b_initial {
            shift = 2;
        } else {
            pos += z.compacted_4b_initial * 4;
            lcn_left -= z.compacted_4b_initial;
            if lcn_left < z.compacted_2b {
                shift = 1;
            } else {
                pos += z.compacted_2b * 2;
                lcn_left -= z.compacted_2b;
                shift = 2;
            }
        }
        pos += lcn_left << shift;
        self.unpack(z, shift, pos)
    }

    /// The kernel's `unpack_compacted_index`.
    fn unpack(&mut self, z: &Zmap, shift: u32, pos: u64) -> Result<Lcluster, String> {
        let vcnt: u64 = if shift == 2 { 2 } else { 16 };
        let pack = vcnt << shift;
        let start = pos / pack * pack;
        let mut data = self.bytes(start, pack as usize)?;
        data.extend_from_slice(&[0; 4]);
        let lobits: u32 = 12;
        let encodebits = ((pack - 4) * 8 / vcnt) as u32;
        let i = ((pos - start) >> shift) as i64;
        let decode = |index: i64| -> (u32, u8) {
            let bit = encodebits * index as u32;
            let v = u32_at(&data, (bit / 8) as usize) >> (bit & 7);
            (v & ((1 << lobits) - 1), ((v >> lobits) & 3) as u8)
        };
        let big = z.advise & ADVISE_BIG_PCLUSTER_1 != 0;
        let (lo, kind) = decode(i);
        let mut out = Lcluster {
            kind,
            clusterofs: 0,
            pblk: 0,
            cblkcnt: None,
            next: start + pack,
        };
        if kind == LCLUSTER_NONHEAD {
            if lo & D0_CBLKCNT != 0 {
                if !big {
                    return Err("a block count without big clusters".into());
                }
                out.cblkcnt = Some(lo & !D0_CBLKCNT);
            }
            return Ok(out);
        }
        out.clusterofs = lo;
        let mut nblk: u64;
        let mut j = i;
        if !big {
            nblk = 1;
            while j > 0 {
                j -= 1;
                let (lo, kind) = decode(j);
                if kind == LCLUSTER_NONHEAD {
                    j -= i64::from(lo);
                }
                if j >= 0 {
                    nblk += 1;
                }
            }
        } else {
            nblk = 0;
            while j > 0 {
                j -= 1;
                let (lo, kind) = decode(j);
                if kind == LCLUSTER_NONHEAD {
                    if lo & D0_CBLKCNT != 0 {
                        j -= 1;
                        nblk += u64::from(lo & !D0_CBLKCNT);
                        continue;
                    }
                    if lo <= 1 {
                        return Err("a big cluster's index is corrupt".into());
                    }
                    j -= i64::from(lo) - 2;
                    continue;
                }
                nblk += 1;
            }
        }
        out.pblk = u64::from(u32_at(&data, (pack - 4) as usize)) + nblk;
        Ok(out)
    }

    fn compressed(
        &mut self,
        inode: &Inode,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        if inode.size == 0 {
            return Ok(());
        }
        let z = self.zmap(inode)?;
        let mut clusters = Vec::with_capacity(z.total as usize);
        for lcn in 0..z.total {
            clusters.push(self.lcluster(&z, lcn)?);
        }
        let tail_data = clusters.last().expect("at least one cluster").next;
        let heads: Vec<u64> = (0..z.total)
            .filter(|&lcn| clusters[lcn as usize].kind != LCLUSTER_NONHEAD)
            .collect();
        let start_of = |lcn: u64| lcn * BLOCK + u64::from(clusters[lcn as usize].clusterofs);
        if heads.first() != Some(&0) || start_of(0) != 0 {
            return Err(format!(
                "inode {}'s first cluster does not start its data",
                inode.nid
            ));
        }
        let big1 = z.advise & ADVISE_BIG_PCLUSTER_1 != 0;
        let big2 = z.advise & ADVISE_BIG_PCLUSTER_2 != 0;
        let zero_padding = self.sb.feature_incompat & INCOMPAT_ZERO_PADDING != 0;
        let mut written = 0u64;
        for (k, &lcn) in heads.iter().enumerate() {
            let head = clusters[lcn as usize];
            let start = start_of(lcn);
            let end = heads.get(k + 1).map_or(inode.size, |&next| start_of(next));
            if start != written || end <= start || end > inode.size {
                return Err(format!(
                    "inode {}'s extents do not follow each other",
                    inode.nid
                ));
            }
            let last = k + 1 == heads.len();
            let (input_at, input) = if last && z.advise & ADVISE_INLINE_PCLUSTER != 0 {
                let len = u64::from(z.idata_size);
                if len == 0 || tail_data % BLOCK + len > BLOCK {
                    return Err(format!("inode {}'s inline tail is corrupt", inode.nid));
                }
                (tail_data, self.bytes(tail_data, len as usize)?)
            } else {
                let big = if head.kind == LCLUSTER_HEAD1 {
                    big1
                } else {
                    big2
                };
                let blocks = if !big || (lcn + 1) * BLOCK >= inode.size {
                    1
                } else {
                    match clusters.get(lcn as usize + 1) {
                        Some(next) if next.kind != LCLUSTER_NONHEAD => 1,
                        Some(next) => match next.cblkcnt {
                            Some(count) if count > 0 => u64::from(count),
                            _ => {
                                return Err(format!(
                                    "inode {}'s big cluster has no block count",
                                    inode.nid
                                ))
                            }
                        },
                        None => 1,
                    }
                };
                let at = head.pblk * BLOCK;
                (at, self.bytes(at, (blocks * BLOCK) as usize)?)
            };
            let mut out = vec![0u8; (end - start) as usize];
            match head.kind {
                LCLUSTER_PLAIN => {
                    if out.len() > input.len() {
                        return Err(format!(
                            "inode {}'s stored cluster is shorter than its extent",
                            inode.nid
                        ));
                    }
                    if z.advise & ADVISE_INTERLACED_PCLUSTER != 0 {
                        // The first part, to the end of its block, is at the input's end.
                        let first = ((BLOCK - start % BLOCK) as usize).min(out.len());
                        let from = input.len() - first;
                        out[..first].copy_from_slice(&input[from..]);
                        let rest = out.len() - first;
                        out[first..].copy_from_slice(&input[..rest]);
                    } else {
                        let len = out.len();
                        out.copy_from_slice(&input[..len]);
                    }
                }
                LCLUSTER_HEAD1 | LCLUSTER_HEAD2 => {
                    let algorithm = z.algorithms[usize::from(head.kind == LCLUSTER_HEAD2)];
                    if algorithm != COMPRESSION_LZ4 {
                        return Err(format!(
                            "inode {} is compressed with algorithm {algorithm}, not LZ4",
                            inode.nid
                        ));
                    }
                    let mut from = 0usize;
                    if zero_padding {
                        let window = input.len().min((BLOCK - input_at % BLOCK) as usize);
                        from = input[..window]
                            .iter()
                            .position(|&b| b != 0)
                            .ok_or_else(|| format!("inode {} has an empty cluster", inode.nid))?;
                    }
                    super::lz4::decode_into(&input[from..], &mut out)
                        .map_err(|e| format!("inode {}: {e}", inode.nid))?;
                }
                other => return Err(format!("inode {} has the cluster type {other}", inode.nid)),
            }
            sink(&out)?;
            written = end;
        }
        Ok(())
    }
}
