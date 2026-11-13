//! The direct-mapped system disk made from a small system disk built here the way Android's
//! build lays one out: a GPT with a boot partition and the super partition, liblp metadata,
//! an erofs `system_a` with LZ4 clusters (full and compact indexes), inline data, a symbolic
//! link, a folder and extended attributes, an `odm_a` kept as it is, and an empty `system_b`.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar};
use std::time::Duration;

use super::erofs_read::{Reader, BLOCK, LAYOUT_FLAT_PLAIN, S_IFDIR, S_IFLNK};
use super::*;
use crate::diagnostics::crc32;
use crate::vhdx::RawDisk;

const MIB: usize = 1 << 20;

fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

fn align(v: usize, to: usize) -> usize {
    v.div_ceil(to) * to
}

/// An LZ4 block of `data` as literals only.
fn lz4_literals(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let n = data.len();
    if n < 15 {
        out.push((n as u8) << 4);
    } else {
        out.push(0xf0);
        let mut rest = n - 15;
        while rest >= 255 {
            out.push(255);
            rest -= 255;
        }
        out.push(rest as u8);
    }
    out.extend_from_slice(data);
    out
}

/// An LZ4 block of `pattern` (16 bytes) repeated to `len`: the pattern, then one match.
fn lz4_repeat(pattern: &[u8; 16], len: usize) -> Vec<u8> {
    let matched = len - 16;
    let code = matched - 4;
    let mut out = vec![0xf0 | (code.min(15) as u8), 1];
    out.extend_from_slice(pattern);
    out.extend_from_slice(&16u16.to_le_bytes());
    if code >= 15 {
        let mut rest = code - 15;
        while rest >= 255 {
            out.push(255);
            rest -= 255;
        }
        out.push(rest as u8);
    }
    out
}

fn repeat(pattern: &[u8; 16], len: usize) -> Vec<u8> {
    pattern.iter().copied().cycle().take(len).collect()
}

/// A block whose LZ4 data sits at its end, after zero padding.
fn padded_block(lz4: &[u8]) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK as usize];
    let at = block.len() - lz4.len();
    put(&mut block, at, lz4);
    block
}

fn compact_inode(layout: u8, mode: u16, nlink: u16, size: u32, raw: u32, icount: u16) -> Vec<u8> {
    let mut i = vec![0u8; 32];
    put(&mut i, 0, &(u16::from(layout) << 1).to_le_bytes());
    put(&mut i, 2, &icount.to_le_bytes());
    put(&mut i, 4, &mode.to_le_bytes());
    put(&mut i, 6, &nlink.to_le_bytes());
    put(&mut i, 8, &size.to_le_bytes());
    put(&mut i, 16, &raw.to_le_bytes());
    i
}

const PATTERN_A: &[u8; 16] = b"the system part.";
const PATTERN_B: &[u8; 16] = b"second extent!!!";
const PATTERN_C: &[u8; 16] = b"compact indexes.";
const BUILD_TIME: u64 = 1_700_000_000;

/// A folder's blocks from its entries (name, inode, file type).
fn names(entries: &[(&str, u64, u8)]) -> Vec<u8> {
    crate::erofs::folder_blocks(
        entries
            .iter()
            .map(|(name, nid, kind)| (name.as_bytes(), *nid, *kind))
            .collect(),
    )
}

/// What a file system holds, by path: mode, owner, time, attributes and data.
type Tree = BTreeMap<String, (u16, u32, u32, u64, Vec<(u8, Vec<u8>, Vec<u8>)>, Vec<u8>)>;

/// The source erofs and the tree it holds.
fn source_erofs() -> Vec<u8> {
    let f2 = repeat(PATTERN_C, 3000);
    let f3: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
    // Blocks: 0 superblock, 1 inodes, 2 shared attributes, 3 root folder, 4 and 5 f1's
    // clusters, 6 f2's, 7 f3's whole block, 8 the folder d, 9 f4.
    let mut image = vec![0u8; 10 * BLOCK as usize];
    let meta = BLOCK as usize;
    let mut at = 32; // nid 0 unused
    let place = |image: &mut Vec<u8>, bytes: &[u8], at: &mut usize| -> u64 {
        *at = align(*at, 32);
        put(image, meta + *at, bytes);
        let nid = (*at / 32) as u64;
        *at += bytes.len();
        nid
    };
    let root_len = names(&[
        (".", 0, 2),
        ("..", 0, 2),
        ("d", 0, 2),
        ("f1", 0, 1),
        ("f2", 0, 1),
        ("f3", 0, 1),
        ("link", 0, 7),
    ])
    .len();
    let d_len = names(&[(".", 0, 2), ("..", 0, 2), ("f4", 0, 1)]).len();
    let root = place(
        &mut image,
        &compact_inode(0, S_IFDIR | 0o755, 3, root_len as u32, 3, 0),
        &mut at,
    );
    // f1: full indexes, two extents, the second starting 1000 bytes into cluster 1.
    let mut f1_inode = compact_inode(1, 0o100_644, 1, 6000, 0, 0);
    f1_inode.extend_from_slice(&[0u8; 8]); // map header: LZ4, no advice
    let index = |kind: u16, ofs: u16, blk: u32| {
        let mut e = Vec::new();
        e.extend_from_slice(&kind.to_le_bytes());
        e.extend_from_slice(&ofs.to_le_bytes());
        e.extend_from_slice(&blk.to_le_bytes());
        e
    };
    f1_inode.extend_from_slice(&index(1, 0, 4));
    f1_inode.extend_from_slice(&index(1, 1000, 5));
    let f1_nid = place(&mut image, &f1_inode, &mut at);
    // f2: compact 4-byte indexes, one head, whose pack names the block before its cluster.
    let mut f2_inode = compact_inode(3, 0o100_755, 1, 3000, 0, 0);
    f2_inode.extend_from_slice(&[0u8; 8]);
    f2_inode.extend_from_slice(&[0x00, 0x10, 0, 0]);
    f2_inode.extend_from_slice(&5u32.to_le_bytes());
    let f2_nid = place(&mut image, &f2_inode, &mut at);
    // f3: one whole block and an inline tail.
    let mut f3_inode = compact_inode(2, 0o100_600, 1, 5000, 7, 0);
    f3_inode.extend_from_slice(&f3[4096..]);
    let f3_nid = place(&mut image, &f3_inode, &mut at);
    let mut link_inode = compact_inode(2, S_IFLNK | 0o777, 1, 6, 0, 0);
    link_inode.extend_from_slice(b"f1/../");
    let link_nid = place(&mut image, &link_inode, &mut at);
    let d_nid = place(
        &mut image,
        &compact_inode(0, S_IFDIR | 0o750, 2, d_len as u32, 8, 0),
        &mut at,
    );
    // f4: an extended inode with a shared attribute and an inline one.
    let mut f4_inode = vec![0u8; 64];
    let mut body = vec![0u8; 12];
    body[4] = 1; // one shared attribute, id 0
    body.extend_from_slice(&0u32.to_le_bytes());
    let label = b"u:object_r:system_file:s0\0";
    body.extend_from_slice(&[7, 6]);
    body.extend_from_slice(&(label.len() as u16).to_le_bytes());
    body.extend_from_slice(b"selinux");
    body.extend_from_slice(label);
    while body.len() % 4 != 0 {
        body.push(0);
    }
    let icount = ((body.len() - 12) / 4 + 1) as u16;
    put(&mut f4_inode, 0, &1u16.to_le_bytes());
    put(&mut f4_inode, 2, &icount.to_le_bytes());
    put(&mut f4_inode, 4, &0o100_640u16.to_le_bytes());
    put(&mut f4_inode, 8, &10u64.to_le_bytes());
    put(&mut f4_inode, 16, &9u32.to_le_bytes());
    put(&mut f4_inode, 24, &1000u32.to_le_bytes());
    put(&mut f4_inode, 28, &1001u32.to_le_bytes());
    put(&mut f4_inode, 32, &12_345u64.to_le_bytes());
    put(&mut f4_inode, 40, &678u32.to_le_bytes());
    put(&mut f4_inode, 44, &1u32.to_le_bytes());
    f4_inode.extend_from_slice(&body);
    let f4_nid = place(&mut image, &f4_inode, &mut at);
    assert!(at <= BLOCK as usize, "the fixture's inodes fit their block");
    // The shared attribute: security.capability.
    let shared = 2 * BLOCK as usize;
    put(&mut image, shared, &[10, 6, 4, 0]);
    put(&mut image, shared + 4, b"capability");
    put(&mut image, shared + 14, &[1, 2, 3, 4]);
    let root_data = names(&[
        (".", root, 2),
        ("..", root, 2),
        ("d", d_nid, 2),
        ("f1", f1_nid, 1),
        ("f2", f2_nid, 1),
        ("f3", f3_nid, 1),
        ("link", link_nid, 7),
    ]);
    put(&mut image, 3 * BLOCK as usize, &root_data);
    put(
        &mut image,
        4 * BLOCK as usize,
        &padded_block(&lz4_repeat(PATTERN_A, 5096)),
    );
    put(
        &mut image,
        5 * BLOCK as usize,
        &padded_block(&lz4_repeat(PATTERN_B, 904)),
    );
    put(
        &mut image,
        6 * BLOCK as usize,
        &padded_block(&lz4_literals(&f2)),
    );
    put(&mut image, 7 * BLOCK as usize, &f3[..4096]);
    let d_data = names(&[(".", d_nid, 2), ("..", root, 2), ("f4", f4_nid, 1)]);
    put(&mut image, 8 * BLOCK as usize, &d_data);
    put(&mut image, 9 * BLOCK as usize, b"f4's data.");
    let mut sb = [0u8; 128];
    put(&mut sb, 0, &erofs_read::MAGIC.to_le_bytes());
    sb[12] = 12;
    put(&mut sb, 14, &(root as u16).to_le_bytes());
    put(&mut sb, 16, &7u64.to_le_bytes());
    put(&mut sb, 24, &BUILD_TIME.to_le_bytes());
    put(&mut sb, 36, &10u32.to_le_bytes());
    put(&mut sb, 40, &1u32.to_le_bytes());
    put(&mut sb, 44, &2u32.to_le_bytes());
    put(&mut sb, 48, b"a uuid of 16 by.");
    put(&mut sb, 80, &1u32.to_le_bytes()); // zero padding
    put(&mut image, 1024, &sb);
    image
}

fn tree_of(dev: &mut dyn ReadAt) -> (Tree, Vec<u8>) {
    let mut reader = Reader::new(dev).unwrap();
    let mut tree = Tree::new();
    let mut layouts = Vec::new();
    let root = reader.sb.root_nid;
    let mut stack = vec![(root, String::new())];
    while let Some((nid, path)) = stack.pop() {
        let inode = reader.inode(nid).unwrap();
        layouts.push(inode.layout);
        let mut data = Vec::new();
        if inode.kind() == S_IFDIR {
            for entry in reader.entries(&inode).unwrap() {
                if entry.name != b"." && entry.name != b".." {
                    let name = String::from_utf8(entry.name).unwrap();
                    stack.push((entry.nid, format!("{path}/{name}")));
                }
            }
        } else {
            reader
                .data(&inode, &mut |chunk| {
                    data.extend_from_slice(chunk);
                    Ok(())
                })
                .unwrap();
        }
        let xattrs = inode
            .xattrs
            .iter()
            .map(|x| (x.index, x.name.clone(), x.value.clone()))
            .collect();
        tree.insert(
            path,
            (inode.mode, inode.uid, inode.gid, inode.mtime, xattrs, data),
        );
    }
    (tree, layouts)
}

#[test]
fn nfr1_compressed_and_inline_erofs_files_read_as_their_data() {
    let mut image = source_erofs();
    let mut dev = RawDisk::new(Cursor::new(&mut image)).unwrap();
    let (tree, _) = tree_of(&mut dev);
    let f1: Vec<u8> = [repeat(PATTERN_A, 5096), repeat(PATTERN_B, 904)].concat();
    assert_eq!(tree["/f1"].5, f1);
    assert_eq!(tree["/f2"].5, repeat(PATTERN_C, 3000));
    assert_eq!(tree["/f3"].5.len(), 5000);
    assert_eq!(tree["/f3"].5[4999], (4999u32 * 7 % 251) as u8);
    assert_eq!(tree["/link"].5, b"f1/../");
    assert_eq!(tree["/d/f4"].5, b"f4's data.");
    let f4 = &tree["/d/f4"];
    assert_eq!((f4.1, f4.2, f4.3), (1000, 1001, 12_345));
    assert_eq!(f4.4[0], (6, b"capability".to_vec(), vec![1, 2, 3, 4]));
    assert_eq!(f4.4[1].1, b"selinux");
    assert_eq!(
        tree["/f1"].3, BUILD_TIME,
        "a compact inode's time is the build's"
    );
}

/// The liblp metadata of a 4 MiB super partition: system_a at 1 MiB (1 MiB), odm_a at 2 MiB
/// (4 KiB), system_b empty.
fn super_partition(system: &[u8], odm: &[u8]) -> Vec<u8> {
    let mut sup = vec![0u8; 4 * MIB];
    let mut geometry = vec![0u8; 52];
    put(&mut geometry, 0, &0x616c_4467u32.to_le_bytes());
    put(&mut geometry, 4, &52u32.to_le_bytes());
    put(&mut geometry, 40, &4096u32.to_le_bytes());
    put(&mut geometry, 44, &2u32.to_le_bytes());
    put(&mut geometry, 48, &4096u32.to_le_bytes());
    let sum = Sha256::digest(&geometry);
    put(&mut geometry, 8, &sum);
    put(&mut sup, 4096, &geometry);
    put(&mut sup, 8192, &geometry);
    let partition = |name: &str, first: u32, count: u32| {
        let mut p = vec![0u8; 52];
        put(&mut p, 0, name.as_bytes());
        put(&mut p, 36, &1u32.to_le_bytes()); // read only
        put(&mut p, 40, &first.to_le_bytes());
        put(&mut p, 44, &count.to_le_bytes());
        p
    };
    let extent = |sectors: u64, start: u64| {
        let mut e = vec![0u8; 24];
        put(&mut e, 0, &sectors.to_le_bytes());
        put(&mut e, 12, &start.to_le_bytes());
        e
    };
    let mut tables = Vec::new();
    tables.extend(partition("system_a", 0, 1));
    tables.extend(partition("odm_a", 1, 1));
    tables.extend(partition("system_b", 2, 0));
    let extents_at = tables.len();
    tables.extend(extent(2048, 2048));
    tables.extend(extent(8, 4096));
    let groups_at = tables.len();
    let mut group = vec![0u8; 48];
    put(&mut group, 0, b"default");
    tables.extend(group);
    let devices_at = tables.len();
    let mut device = vec![0u8; 64];
    put(&mut device, 0, &2048u64.to_le_bytes());
    put(&mut device, 8, &(1u32 << 20).to_le_bytes());
    put(&mut device, 16, &(4 * MIB as u64).to_le_bytes());
    put(&mut device, 24, b"super");
    tables.extend(device);
    let mut header = vec![0u8; 128];
    put(&mut header, 0, &0x414c_5030u32.to_le_bytes());
    put(&mut header, 4, &10u16.to_le_bytes());
    put(&mut header, 8, &128u32.to_le_bytes());
    put(&mut header, 44, &(tables.len() as u32).to_le_bytes());
    put(&mut header, 48, &Sha256::digest(&tables));
    for (at, offset, count, size) in [
        (80, 0, 3, 52),
        (92, extents_at, 2, 24),
        (104, groups_at, 1, 48),
        (116, devices_at, 1, 64),
    ] {
        put(&mut header, at, &(offset as u32).to_le_bytes());
        put(&mut header, at + 4, &(count as u32).to_le_bytes());
        put(&mut header, at + 8, &(size as u32).to_le_bytes());
    }
    let sum = Sha256::digest(&header);
    put(&mut header, 12, &sum);
    for slot in 0..4 {
        let at = 12288 + slot * 4096;
        put(&mut sup, at, &header);
        put(&mut sup, at + 128, &tables);
    }
    put(&mut sup, MIB, system);
    put(&mut sup, 2 * MIB, odm);
    sup
}

/// A 7 MiB disk: GPT, boot_a at 1 MiB, super at 2 MiB (4 MiB), and the backup GPT.
fn system_disk() -> Vec<u8> {
    let sectors: u64 = 7 * MIB as u64 / 512;
    let mut disk = vec![0u8; 7 * MIB];
    disk[446 + 4] = 0xee;
    put(&mut disk, 446 + 8, &1u32.to_le_bytes());
    put(&mut disk, 446 + 12, &((sectors - 1) as u32).to_le_bytes());
    put(&mut disk, 510, &[0x55, 0xaa]);
    let mut entries = vec![0u8; 128 * 128];
    let entry = |entries: &mut Vec<u8>, i: usize, first: u64, last: u64, name: &str| {
        let e = &mut entries[i * 128..(i + 1) * 128];
        e[0] = 0xaf;
        e[16] = i as u8 + 1;
        put(e, 32, &first.to_le_bytes());
        put(e, 40, &last.to_le_bytes());
        for (k, c) in name.encode_utf16().enumerate() {
            put(e, 56 + k * 2, &c.to_le_bytes());
        }
    };
    entry(&mut entries, 0, 2048, 4095, "boot_a");
    entry(&mut entries, 1, 4096, 4096 + 8192 - 1, "super");
    let header = |mine: u64, other: u64, entries_lba: u64| {
        let mut h = vec![0u8; 92];
        put(&mut h, 0, b"EFI PART");
        put(&mut h, 8, &0x0001_0000u32.to_le_bytes());
        put(&mut h, 12, &92u32.to_le_bytes());
        put(&mut h, 24, &mine.to_le_bytes());
        put(&mut h, 32, &other.to_le_bytes());
        put(&mut h, 40, &34u64.to_le_bytes());
        put(&mut h, 48, &(sectors - 34).to_le_bytes());
        put(&mut h, 56, b"disk guid 16 byt");
        put(&mut h, 72, &entries_lba.to_le_bytes());
        put(&mut h, 80, &128u32.to_le_bytes());
        put(&mut h, 84, &128u32.to_le_bytes());
        put(&mut h, 88, &crc32(&entries).to_le_bytes());
        let crc = crc32(&h);
        put(&mut h, 16, &crc.to_le_bytes());
        h
    };
    put(&mut disk, 512, &header(1, sectors - 1, 2));
    put(&mut disk, 1024, &entries);
    let end = disk.len();
    put(&mut disk, end - 33 * 512, &entries);
    put(&mut disk, end - 512, &header(sectors - 1, 1, sectors - 33));
    let boot: Vec<u8> = (0..MIB).map(|i| (i % 253) as u8).collect();
    put(&mut disk, MIB, &boot);
    let odm: Vec<u8> = (0..4096).map(|i| (i % 7) as u8 + 1).collect();
    let sup = super_partition(&source_erofs(), &odm);
    put(&mut disk, 2 * MIB, &sup);
    disk
}

fn keep() -> Vec<String> {
    vec!["odm".to_string()]
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dsa-direct-mapped-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The image's disk in a file, and what the build would record of its copy.
fn image(dir: &Path) -> (PathBuf, SystemDisk) {
    let path = dir.join("system.img");
    let disk = system_disk();
    fs::write(&path, &disk).unwrap();
    let mut dev = RawDisk::new(Cursor::new(disk)).unwrap();
    let described = describe(&mut dev, &keep()).unwrap();
    (path, described)
}

fn sha256_of(path: &Path, bytes: u64) -> String {
    let data = fs::read(path).unwrap();
    hex(&Sha256::digest(&data[..bytes as usize]))
}

fn nothing(_: &Progress) {}

#[test]
fn nfr1_the_produced_system_disk_matches_the_hash_the_image_names() {
    let dir = scratch("hash");
    let (source, expected) = image(&dir);
    assert!(expected.is_usable());
    assert_eq!(expected.bytes % (2 << 20), 0, "a whole number of 2 MiB");
    let store = Store::new(dir.join("copies"));
    assert_eq!(store.ready("1", &expected), None);
    let ready = store
        .produce("1", &source, &expected, &keep(), &mut nothing, &|| false)
        .unwrap();
    assert_eq!(ready.path, store.path("1"));
    assert_eq!(sha256_of(&ready.path, expected.bytes), expected.sha256);
    let file = fs::read(&ready.path).unwrap();
    assert_eq!(file.len() as u64, expected.bytes + FOOTER_BYTES);
    assert_eq!(
        &file[expected.bytes as usize..],
        &footer(expected.bytes, &expected.sha256)[..]
    );
    assert_eq!(store.ready("1", &expected), Some(ready.clone()));
    // Another hash than the build's is not this copy.
    let mut other = expected.clone();
    other.sha256 = "0".repeat(64);
    assert_eq!(store.ready("1", &other), None);

    // The copy's system_a holds the same tree, every inode mapped whole; odm_a is as it was.
    let disk = file[..expected.bytes as usize].to_vec();
    let mut dev = RawDisk::new(Cursor::new(disk.clone())).unwrap();
    let plan = disk::plan(&mut dev, &["system".into(), "odm".into()]).unwrap();
    assert_eq!(plan.super_bytes, expected.super_bytes);
    let system = &plan.partitions[0];
    assert_eq!(system.name, "system_a");
    let start = (plan.super_offset + system.offset) as usize;
    let mut copied = RawDisk::new(Cursor::new(
        disk[start..start + system.len as usize].to_vec(),
    ))
    .unwrap();
    let (copy, layouts) = tree_of(&mut copied);
    let mut original = RawDisk::new(Cursor::new(source_erofs())).unwrap();
    let (tree, _) = tree_of(&mut original);
    assert_eq!(copy, tree);
    assert!(layouts.iter().all(|&l| l == LAYOUT_FLAT_PLAIN));
    let odm = &plan.partitions[1];
    let at = (plan.super_offset + odm.offset) as usize;
    let want: Vec<u8> = (0..4096).map(|i| (i % 7) as u8 + 1).collect();
    assert_eq!(&disk[at..at + 4096], &want[..]);
    // The disk before super is the image's (past the GPT).
    let image = fs::read(&source).unwrap();
    assert_eq!(&disk[34 * 512..2 * MIB], &image[34 * 512..2 * MIB]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nfr1_the_system_disk_is_described_and_made_from_a_vhdx_like_the_images() {
    // The image ships system.vhdx as qemu-img writes it, with a 4-byte logical sector size
    // item among its metadata; the release tool describes the copy from it and the host
    // makes the copy from it.
    let dir = scratch("vhdx");
    let disk = system_disk();
    let vhdx = crate::vhdx::test_image::vhdx_like_qemu_img(&disk, MIB);
    let path = dir.join("system.vhdx");
    fs::write(&path, &vhdx).unwrap();
    let mut from_vhdx = crate::vhdx::open_disk(&path).unwrap();
    assert_eq!(from_vhdx.len(), disk.len() as u64);
    let described = describe(from_vhdx.as_mut(), &keep()).unwrap();
    let mut raw = RawDisk::new(Cursor::new(disk)).unwrap();
    assert_eq!(described, describe(&mut raw, &keep()).unwrap());
    assert!(described.is_usable());
    let store = Store::new(dir.join("copies"));
    let ready = store
        .produce("1", &path, &described, &keep(), &mut nothing, &|| false)
        .unwrap();
    assert_eq!(sha256_of(&ready.path, described.bytes), described.sha256);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nfr1_a_tampered_system_disk_copy_is_rejected_and_made_again() {
    let dir = scratch("tampered");
    let (source, expected) = image(&dir);
    let store = Store::new(dir.join("copies"));
    let ready = store
        .produce("1", &source, &expected, &keep(), &mut nothing, &|| false)
        .unwrap();
    // A byte changed in place, the file's time put back as it was.
    let before = fs::metadata(&ready.path).unwrap().modified().unwrap();
    let mut data = fs::read(&ready.path).unwrap();
    data[3 * MIB] ^= 0xff;
    fs::write(&ready.path, &data).unwrap();
    fs::File::options()
        .write(true)
        .open(&ready.path)
        .unwrap()
        .set_modified(before)
        .unwrap();
    // The full check finds it, and removes it, so a boot does not use it.
    assert_eq!(
        store.verify("1", &expected, &mut nothing, &|| false),
        Ok(false)
    );
    assert!(!ready.path.exists());
    assert_eq!(store.ready("1", &expected), None);
    let again = store
        .produce("1", &source, &expected, &keep(), &mut nothing, &|| false)
        .unwrap();
    assert_eq!(sha256_of(&again.path, expected.bytes), expected.sha256);
    assert_eq!(
        store.verify("1", &expected, &mut nothing, &|| false),
        Ok(true)
    );
    // A change that moves the file's time is not used even before the full check.
    fs::write(&again.path, &data).unwrap();
    assert_eq!(store.ready("1", &expected), None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nfr1_a_partial_system_disk_copy_resumes_and_a_corrupt_one_is_made_again() {
    let dir = scratch("partial");
    let (source, expected) = image(&dir);
    let store = Store::new(dir.join("copies"));
    // Stopped after its first stretches.
    let asked = AtomicUsize::new(0);
    let stop = || asked.fetch_add(1, Ordering::Relaxed) > 3;
    assert_eq!(
        store.produce("1", &source, &expected, &keep(), &mut nothing, &stop),
        Err(Failure::Stopped)
    );
    assert!(store.part_path("1").exists());
    assert_eq!(
        store.ready("1", &expected),
        None,
        "a partial copy is not booted"
    );
    // It resumes.
    let ready = store
        .produce("1", &source, &expected, &keep(), &mut nothing, &|| false)
        .unwrap();
    assert_eq!(sha256_of(&ready.path, expected.bytes), expected.sha256);
    assert!(!store.part_path("1").exists());
    // A partial copy whose finished part was changed is found by the check at its end, and
    // the copy is made again from the start.
    fs::remove_file(&ready.path).unwrap();
    let asked = AtomicUsize::new(0);
    let stop = || asked.fetch_add(1, Ordering::Relaxed) > 3;
    assert_eq!(
        store.produce("1", &source, &expected, &keep(), &mut nothing, &stop),
        Err(Failure::Stopped)
    );
    let mut part = fs::read(store.part_path("1")).unwrap();
    part[100 * 1024] ^= 1;
    fs::write(store.part_path("1"), &part).unwrap();
    let ready = store
        .produce("1", &source, &expected, &keep(), &mut nothing, &|| false)
        .unwrap();
    assert_eq!(sha256_of(&ready.path, expected.bytes), expected.sha256);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nfr1_a_copy_with_another_hash_than_the_images_is_never_used() {
    let dir = scratch("mismatch");
    let (source, mut expected) = image(&dir);
    expected.sha256 = "ab".repeat(32);
    let store = Store::new(dir.join("copies"));
    let result = store.produce("1", &source, &expected, &keep(), &mut nothing, &|| false);
    assert!(matches!(result, Err(Failure::Mismatch(_))), "{result:?}");
    assert!(!store.path("1").exists());
    assert!(!store.part_path("1").exists());
    // Another format is not made at all.
    let (_, mut other) = image(&dir);
    other.format = FORMAT + 1;
    assert!(matches!(
        store.produce("1", &source, &other, &keep(), &mut nothing, &|| false),
        Err(Failure::Failed(_))
    ));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nfr1_a_replaced_image_takes_its_system_disk_copy_with_it() {
    let dir = scratch("replaced");
    let (source, expected) = image(&dir);
    let store = Store::new(dir.join("copies"));
    let old = store
        .produce(
            "17.0.0-1-1",
            &source,
            &expected,
            &keep(),
            &mut nothing,
            &|| false,
        )
        .unwrap();
    assert!(old.path.exists());
    let new = store
        .produce(
            "17.0.0-1-2",
            &source,
            &expected,
            &keep(),
            &mut nothing,
            &|| false,
        )
        .unwrap();
    assert!(new.path.exists());
    assert!(!old.path.exists());
    assert_eq!(store.ready("17.0.0-1-1", &expected), None);
    let left: Vec<String> = fs::read_dir(store.folder())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        left.iter().all(|name| name.starts_with("17.0.0-1-2.")),
        "{left:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nfr1_the_copys_super_partition_ends_after_its_last_partition() {
    let disk = system_disk();
    let mut dev = RawDisk::new(Cursor::new(disk)).unwrap();
    let plan = disk::plan(&mut dev, &keep()).unwrap();
    // Metadata to 1 MiB, system_a (10 blocks, aligned to 1 MiB), odm_a (4 KiB, one more).
    assert_eq!(plan.super_bytes, 3 << 20);
    assert_eq!(plan.super_offset, 2 << 20);
    assert_eq!(plan.disk_bytes, 6 << 20);
    let names: Vec<&str> = plan.partitions.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["system_a", "odm_a"]);
}

#[test]
fn nfr1_a_raw_store_keeps_the_disk_without_a_footer() {
    let dir = scratch("raw");
    let (source, expected) = image(&dir);
    let store = Store::raw(dir.join("copies"));
    let ready = store
        .produce("1", &source, &expected, &keep(), &mut nothing, &|| false)
        .unwrap();
    assert_eq!(ready.path.extension().unwrap(), "img");
    assert_eq!(fs::metadata(&ready.path).unwrap().len(), expected.bytes);
    assert_eq!(sha256_of(&ready.path, expected.bytes), expected.sha256);
    assert_eq!(
        store.verify("1", &expected, &mut nothing, &|| false),
        Ok(true)
    );
    let _ = fs::remove_dir_all(&dir);
}

/// An image folder as the store keeps it: its `image.json`, naming the copy of its system disk,
/// and the disk.
fn image_folder(root: &Path, version: &str) -> PathBuf {
    let dir = root.join(version);
    fs::create_dir_all(&dir).unwrap();
    let (source, expected) = image(&dir);
    fs::rename(&source, dir.join("system.img")).unwrap();
    let manifest = serde_json::json!({
        "version": version,
        "direct_mapped": {
            "fstab_suffix": "dsa_dax",
            "uncompressed_erofs": false,
            "verified_partitions": ["odm"],
            "system_disk": expected,
        },
    });
    fs::write(dir.join("image.json"), manifest.to_string()).unwrap();
    dir
}

#[test]
fn nfr1_a_boot_uses_the_image_disk_until_the_copy_is_made_and_checked() {
    let root = scratch("worker");
    let first = image_folder(&root.join("images"), "17.0.0-1-1");
    let current = Arc::new(Mutex::new((String::from("17.0.0-1-1"), first.clone())));
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let (gate, gate_open) = (Arc::new(Mutex::new(false)), Arc::new(Condvar::new()));
    let images = {
        let current = Arc::clone(&current);
        let lines = Arc::clone(&lines);
        let (gate, gate_open) = (Arc::clone(&gate), Arc::clone(&gate_open));
        DirectMappedImages::start(
            Store::new(root.join("direct-mapped")),
            Box::new(move || Some(current.lock().unwrap().clone())),
            // The thread waits until the test has seen a boot fall back.
            Box::new(move || {
                let mut open = gate.lock().unwrap();
                while !*open {
                    open = gate_open.wait(open).unwrap();
                }
                Ok(())
            }),
            Arc::new(move |line: &str| lines.lock().unwrap().push(line.to_string())),
        )
    };
    // While the copy is being made, a boot starts with the image's own disk.
    let why = images.boot_disk("17.0.0-1-1", &first).unwrap_err();
    assert!(why.contains("not made and checked yet"), "{why}");
    *gate.lock().unwrap() = true;
    gate_open.notify_all();
    assert!(images.wait_checked("17.0.0-1-1", Duration::from_secs(60)));
    let (ready, support) = images.boot_disk("17.0.0-1-1", &first).unwrap();
    assert_eq!(ready.super_bytes, support.system_disk.super_bytes);
    assert!(lines
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("making the uncompressed")));
    // A replaced image: the new one's copy is made and the old one's removed.
    let second = image_folder(&root.join("images"), "17.0.0-1-2");
    *current.lock().unwrap() = (String::from("17.0.0-1-2"), second.clone());
    images.kick();
    assert!(images.wait_checked("17.0.0-1-2", Duration::from_secs(60)));
    assert!(!ready.path.exists());
    assert!(images.boot_disk("17.0.0-1-1", &first).is_err());
    images.stop();
    // A service started again checks the copy before a boot uses it.
    let again = DirectMappedImages::start(
        Store::new(root.join("direct-mapped")),
        Box::new(move || Some((String::from("17.0.0-1-2"), second.clone()))),
        Box::new(|| Ok(())),
        Arc::new(|_: &str| {}),
    );
    assert!(again.wait_checked("17.0.0-1-2", Duration::from_secs(60)));
    again.stop();
    let _ = fs::remove_dir_all(&root);
}
