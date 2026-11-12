//! Reading the image's VHDX disks into raw disk images.
//!
//! The image ships its disks as dynamic VHDX files, which Hyper-V attaches. Apple's
//! Virtualization framework on macOS and crosvm on Linux attach raw images, so those hosts
//! convert each disk once. A dynamic VHDX holds only the blocks that were written; the raw
//! file gets those blocks and nothing else, and the rest stays a hole, so on APFS or ext4 the
//! raw disk takes about the space the VHDX did.
//!
//! The reader is the host core's (`dsa_host_core::vhdx`), which follows the VHDX format and
//! refuses a file with a log to replay or a differencing disk.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub use dsa_host_core::vhdx::{
    guid_bytes, layout, Layout, BAT_REGION, FILE_PARAMETERS, LOGICAL_SECTOR_SIZE,
    METADATA_REGION, VIRTUAL_DISK_SIZE,
};

const MIB: u64 = 1024 * 1024;
const FILE_SIGNATURE: &[u8; 8] = b"vhdxfile";

/// How much of a block is written at a time; a run of zeroes this long stays a hole.
const CHUNK: usize = MIB as usize;

/// Whether the file at `path` is a VHDX: it starts with the file identifier.
pub fn is_vhdx(path: &Path) -> io::Result<bool> {
    let mut signature = [0u8; 8];
    let mut file = File::open(path)?;
    Ok(file.read_exact(&mut signature).is_ok() && &signature == FILE_SIGNATURE)
}

/// Writes the disk the VHDX at `from` holds to `to` as a raw image of its virtual size,
/// writing only the data that is not zero.
pub fn convert_to_raw(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = File::open(from)?;
    let layout = layout(&mut source)?;
    let mut target = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(to)?;
    target.set_len(layout.virtual_size)?;
    let mut buf = vec![0u8; CHUNK];
    for (block, offset) in layout.blocks.iter().enumerate() {
        let Some(offset) = offset else { continue };
        let start = block as u64 * layout.block_size;
        let end = (start + layout.block_size).min(layout.virtual_size);
        let mut at = start;
        while at < end {
            let len = ((end - at) as usize).min(CHUNK);
            source.seek(SeekFrom::Start(offset + (at - start)))?;
            source.read_exact(&mut buf[..len])?;
            if buf[..len].iter().any(|&b| b != 0) {
                target.seek(SeekFrom::Start(at))?;
                target.write_all(&buf[..len])?;
            }
            at += len as u64;
        }
    }
    target.sync_all()
}
