//! The user's Android disk as a raw file: the disk core's backend on Linux and macOS.
//!
//! The VMMs of those hosts (crosvm, Apple's Virtualization framework) attach raw disks, so a
//! user's disk is a raw file in their own folder, and the image's VHDX disks are read through
//! [`dsa_host_core::vhdx`] where the host core needs their partitions. A new disk is a copy of
//! the template, which APFS clones and ext4 holds sparse when the copy leaves holes.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use dsa_host_core::disk::DiskBackend;
use dsa_host_core::vhdx::{ReadAt, VirtualDisk};

/// Raw files for the disks of a user's Android.
pub struct RawDiskBackend;

/// An open disk: a raw file, or the virtual disk of a VHDX (read only).
pub enum RawDevice {
    File(File),
    Vhdx {
        disk: VirtualDisk<File>,
        position: u64,
    },
}

impl Read for RawDevice {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            RawDevice::File(file) => file.read(buf),
            RawDevice::Vhdx { disk, position } => {
                let left = disk.len().saturating_sub(*position);
                let len = (buf.len() as u64).min(left) as usize;
                disk.read_exact_at(*position, &mut buf[..len])?;
                *position += len as u64;
                Ok(len)
            }
        }
    }
}

impl Write for RawDevice {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            RawDevice::File(file) => file.write(buf),
            RawDevice::Vhdx { .. } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a VHDX disk is read, not written",
            )),
        }
    }

    /// Flushing a raw disk makes its writes durable: the partitions of a new Android reach
    /// the disk before its version is recorded.
    fn flush(&mut self) -> io::Result<()> {
        match self {
            RawDevice::File(file) => file.sync_all(),
            RawDevice::Vhdx { .. } => Ok(()),
        }
    }
}

impl Seek for RawDevice {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match self {
            RawDevice::File(file) => file.seek(to),
            RawDevice::Vhdx { disk, position } => {
                let target = match to {
                    SeekFrom::Start(at) => Some(at),
                    SeekFrom::End(by) => disk.len().checked_add_signed(by),
                    SeekFrom::Current(by) => position.checked_add_signed(by),
                };
                *position = target.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a seek before the disk's start",
                    )
                })?;
                Ok(*position)
            }
        }
    }
}

/// The folder a disk is in, created when missing.
fn folder_of(disk: &Path) -> Result<&Path, String> {
    let folder = disk
        .parent()
        .ok_or_else(|| format!("{} has no folder", disk.display()))?;
    std::fs::create_dir_all(folder).map_err(|e| format!("creating {}: {e}", folder.display()))?;
    Ok(folder)
}

impl DiskBackend for RawDiskBackend {
    type Device = RawDevice;

    fn open(&self, disk: &Path, write: bool) -> Result<RawDevice, String> {
        let opened = |e: io::Error| format!("opening {}: {e}", disk.display());
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .open(disk)
            .map_err(opened)?;
        if write || !crate::vhdx::is_vhdx(disk).map_err(opened)? {
            return Ok(RawDevice::File(file));
        }
        Ok(RawDevice::Vhdx {
            disk: VirtualDisk::new(file).map_err(opened)?,
            position: 0,
        })
    }

    /// A copy goes through a temporary file, so one cut short never looks like a disk. A
    /// VHDX template is read into a raw disk; a raw one is copied. Without a template the
    /// disk is a raw file of `size_gb`, a hole until it is written; one already there is kept.
    fn create(&self, disk: &Path, template: Option<&Path>, size_gb: u32) -> Result<(), String> {
        folder_of(disk)?;
        let Some(template) = template else {
            if disk.exists() {
                return Ok(());
            }
            let file =
                File::create(disk).map_err(|e| format!("creating {}: {e}", disk.display()))?;
            return file
                .set_len(u64::from(size_gb) << 30)
                .map_err(|e| format!("sizing {}: {e}", disk.display()));
        };
        let partial = disk.with_extension("img.new");
        let is_vhdx = crate::vhdx::is_vhdx(template)
            .map_err(|e| format!("reading {}: {e}", template.display()))?;
        let copied = if is_vhdx {
            crate::vhdx::convert_to_raw(template, &partial)
        } else {
            std::fs::copy(template, &partial).map(|_| ())
        };
        copied.map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            format!(
                "copying {} to {}: {e}",
                template.display(),
                partial.display()
            )
        })?;
        std::fs::rename(&partial, disk).map_err(|e| format!("placing {}: {e}", disk.display()))
    }

    fn write_beside(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}
