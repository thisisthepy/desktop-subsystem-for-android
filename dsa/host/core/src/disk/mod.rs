//! Each user's Android disk and the pstore disk, on any host.
//!
//! A user's Android disk is made from the image's disk template and kept up to date with it:
//! - a new disk is a copy of the template;
//! - an image of two disks keeps Android on its shared system disk, so the user's disk keeps
//!   only the template's partitions (the data template's `misc`, `metadata` and `userdata`);
//! - an image of one disk carries Android in it, so a newer image's Android partitions are
//!   copied into the user's disk and the user's data partitions stay;
//! - a disk that Android never wrote to holds nothing and is replaced by a fresh copy.
//!
//! The image version the disk holds Android from is recorded beside it.
//!
//! What differs between hosts is how a disk is opened and made: a VHDX attached to the
//! computer on Windows, a raw file on the others. A host supplies that as a [`DiskBackend`];
//! the rules above are here, once.
//!
//! [`gpt`] reads and edits the partition tables, and [`pstore`] makes the small disk the
//! guest kernel keeps its last log on.

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

pub mod gpt;
pub mod pstore;
pub mod raw_copy;

/// The partitions of a single-disk image that hold Android itself. They come from the image;
/// every other partition holds the user's data.
pub const ANDROID_PARTITIONS: [&str; 8] = [
    "super",
    "vbmeta_a",
    "vbmeta_system_a",
    "vbmeta_system_dlkm_a",
    "vbmeta_vendor_dlkm_a",
    "boot_a",
    "init_boot_a",
    "vendor_boot_a",
];

/// How a host opens and makes the disks of a user's Android.
pub trait DiskBackend {
    /// An open disk. Dropping it lets go of the disk (a host that attaches the disk detaches
    /// it then), so the disk's file can be replaced afterwards.
    type Device: Read + Write + Seek;

    /// Opens `disk` for reading, and for writing when `write` is set.
    fn open(&self, disk: &Path, write: bool) -> Result<Self::Device, String>;

    /// Makes `disk` a copy of `template`, or an empty disk of `size_gb` when there is no
    /// template, creating its folder when it is missing. A copy that is cut short must never
    /// look like a disk.
    fn create(&self, disk: &Path, template: Option<&Path>, size_gb: u32) -> Result<(), String>;

    /// Writes a small file beside the disk with the rights the disk has.
    fn write_beside(&self, path: &Path, bytes: &[u8]) -> Result<(), String>;

    /// Whether `path` is a disk this backend reads a partition table from. A file that is not
    /// (a placeholder where an image carries no real disk) is left as it is when a disk is
    /// fitted to a data template.
    fn has_partitions(&self, _path: &Path) -> Result<bool, String> {
        Ok(true)
    }

    /// Whether the existing `disk` is in a format whose partitions this backend does not
    /// read (a disk an earlier version made in another format). It is kept as it is, and only
    /// the image version is recorded for it.
    fn keeps_as_is(&self, _disk: &Path) -> bool {
        false
    }

    /// The disk to use for `image_disk`, a disk the image carries: the disk itself when the
    /// host attaches it as it is, else a copy in a format it can, made in `cache` once. Used
    /// for the image's read-only system disk.
    fn prepare_image_disk(&self, image_disk: &Path, _cache: &Path) -> Result<PathBuf, String> {
        Ok(image_disk.to_path_buf())
    }

    /// The same for the image's disk template, which a new user's disk is made from. A host
    /// that reads the template as it is (through its own disk reader) leaves it alone.
    fn prepare_template_disk(&self, template: &Path, cache: &Path) -> Result<PathBuf, String> {
        self.prepare_image_disk(template, cache)
    }
}

/// Makes sure the user's `disk` exists and holds the image `key`'s Android: from the image's
/// disk `template` when it has one ([`bring_disk_to`]), else as a missing disk made empty at
/// `size_gb`.
pub fn prepare_user_disk<B: DiskBackend>(
    backend: &B,
    disk: &Path,
    template: Option<&Path>,
    key: &str,
    size_gb: u32,
) -> Result<(), String> {
    match template {
        Some(template) => bring_disk_to(backend, disk, template, key),
        None if !disk.exists() => backend.create(disk, None, size_gb),
        None => Ok(()),
    }
}

/// The record of the image version beside `disk`.
pub fn version_file(disk: &Path) -> PathBuf {
    disk.with_extension("image-version")
}

/// The image version the user's disk holds Android from, if known.
pub fn disk_image_version(disk: &Path) -> Option<String> {
    std::fs::read_to_string(version_file(disk))
        .ok()
        .map(|v| v.trim().to_string())
}

/// Records beside `disk` that it holds Android from image `version`.
pub fn record_disk_image_version<B: DiskBackend>(
    backend: &B,
    disk: &Path,
    version: &str,
) -> Result<(), String> {
    backend
        .write_beside(&version_file(disk), version.as_bytes())
        .map_err(|e| format!("recording the image version of {}: {e}", disk.display()))
}

/// Whether the disk's template is a data template, whose image keeps Android on a system
/// disk of its own.
fn is_data_template(template: &Path) -> bool {
    template
        .file_stem()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "data")
}

fn remove(disk: &Path, what: &str) -> Result<(), String> {
    std::fs::remove_file(disk).map_err(|e| format!("replacing the {what} {}: {e}", disk.display()))
}

fn is_blank_disk<B: DiskBackend>(backend: &B, disk: &Path) -> Result<bool, String> {
    let mut device = backend.open(disk, false)?;
    gpt::is_blank(&mut device).map_err(|e| format!("reading {}: {e}", disk.display()))
}

/// Brings the user's `disk` to `version`: a new disk is copied from `template`; a disk that
/// holds Android from another version gets the template's Android partitions, and keeps its
/// `misc`, `metadata` and `userdata` partitions, which hold the user's data. A disk that was
/// never given Android and was never written to holds nothing, and is replaced by a copy.
pub fn bring_disk_to<B: DiskBackend>(
    backend: &B,
    disk: &Path,
    template: &Path,
    version: &str,
) -> Result<(), String> {
    if disk.exists() && backend.keeps_as_is(disk) {
        return record_disk_image_version(backend, disk, version);
    }
    let held = disk_image_version(disk);
    if disk.exists() && held.is_none() && is_blank_disk(backend, disk)? {
        remove(disk, "empty")?;
    }
    if !disk.exists() {
        backend.create(disk, Some(template), 0)?;
        return record_disk_image_version(backend, disk, version);
    }
    if is_data_template(template) {
        // Even at the version it holds: a disk brought to a two-disk image before stale
        // partitions were removed is recorded at that image already.
        fit_to_data_template(backend, disk, template, version)?;
        if held.as_deref() == Some(version) {
            return Ok(());
        }
        return record_disk_image_version(backend, disk, version);
    }
    if held.as_deref() == Some(version) {
        return Ok(());
    }
    let replace = {
        let mut source = backend.open(template, false)?;
        let mut target = backend.open(disk, true)?;
        let have = gpt::partitions(&mut source)
            .map_err(|e| format!("reading {}: {e}", template.display()))?;
        let names: Vec<&str> = ANDROID_PARTITIONS
            .iter()
            .copied()
            .filter(|n| have.iter().any(|p| p.name == *n))
            .collect();
        let fits = gpt::fits(&mut source, &mut target, &names)
            .map_err(|e| format!("reading {}: {e}", disk.display()))?;
        if fits {
            gpt::copy_partitions(&mut source, &mut target, &names)
                .map_err(|e| format!("writing Android {version} into {}: {e}", disk.display()))?;
            None
        } else {
            // The image's disk has a partition this one lacks (a newer layout).
            Some(
                gpt::partition_is_blank(&mut target, "userdata")
                    .map_err(|e| format!("reading {}: {e}", disk.display()))?,
            )
        }
    };
    if let Some(unused) = replace {
        replace_if_unused(
            backend,
            disk,
            template,
            unused,
            &format!(
                "Android {version} needs a disk layout that {} does not have",
                disk.display()
            ),
        )?;
    }
    record_disk_image_version(backend, disk, version)
}

/// Replaces `disk`, whose layout the new image cannot use, by a copy of `template` when
/// `unused` says Android never wrote data to it. A disk with data cannot be relaid without
/// losing it, and is left as it is, with `need` as the reason. Both disks are closed.
fn replace_if_unused<B: DiskBackend>(
    backend: &B,
    disk: &Path,
    template: &Path,
    unused: bool,
    need: &str,
) -> Result<(), String> {
    if !unused {
        return Err(format!(
            "{need}, and the disk holds Android data, so it is left as it is"
        ));
    }
    remove(disk, "unused")?;
    backend.create(disk, Some(template), 0)
}

/// Gives the user's disk the partitions of the image's data template, and only those.
///
/// A disk made for an image that kept Android on it still holds Android's partitions, and
/// Android, which finds partitions by name, would boot from those instead of the image's
/// own disk: they are taken out, and the data partitions stay where they are, with the
/// user's data. A disk that lacks a partition the template has (made from an older
/// template) becomes a copy of the template if Android never wrote to it; one with data
/// cannot be relaid without losing it, and is left as it is.
fn fit_to_data_template<B: DiskBackend>(
    backend: &B,
    disk: &Path,
    template: &Path,
    version: &str,
) -> Result<(), String> {
    if !backend.has_partitions(disk)? || !backend.has_partitions(template)? {
        return Ok(());
    }
    let wanted: Vec<String> = {
        let mut source = backend.open(template, false)?;
        gpt::partitions(&mut source)
            .map_err(|e| format!("reading {}: {e}", template.display()))?
            .into_iter()
            .map(|p| p.name)
            .collect()
    };
    let (names, unused) = {
        let mut target = backend.open(disk, true)?;
        let have =
            gpt::partitions(&mut target).map_err(|e| format!("reading {}: {e}", disk.display()))?;
        let missing: Vec<&str> = wanted
            .iter()
            .filter(|name| !have.iter().any(|p| &p.name == *name))
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            if have.iter().all(|p| wanted.contains(&p.name)) {
                return Ok(());
            }
            let keep: Vec<&str> = wanted.iter().map(String::as_str).collect();
            gpt::keep_only(&mut target, &keep).map_err(|e| {
                format!(
                    "removing the old Android partitions from {}: {e}",
                    disk.display()
                )
            })?;
            return Ok(());
        }
        let unused = gpt::partition_is_blank(&mut target, "userdata")
            .map_err(|e| format!("reading {}: {e}", disk.display()))?;
        (missing.join(", "), unused)
    };
    replace_if_unused(
        backend,
        disk,
        template,
        unused,
        &format!(
            "Android {version} needs a disk with a {names} partition, which {} does not have",
            disk.display()
        ),
    )
}
