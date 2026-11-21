//! The raw copy of an image's system disk, and how it is trusted.
//!
//! A host whose VMM attaches only raw disks (crosvm, Virtualization framework) reads the
//! image's VHDX system disk into a raw copy once. The copy is checked, not trusted: against the
//! hash `image.json` gives for the disk as it is (`system_disk_raw`, which the image build
//! records) when it gives one, and its hash is recorded beside it (`<copy>.json`) with the
//! file's size and time. A copy that changed is made again from the VHDX, and a host checks it
//! byte by byte when it starts ([`verify_raw_copy`]).
//!
//! Making the copy (reading VHDX, punching holes) and the file's permissions are the host's:
//! it hands [`checked_raw_copy`] a function that writes the copy.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::direct_mapped_disk::file_sha256;

/// What `image.json` says of the system disk as it is (`system_disk_raw`): its size and
/// SHA-256. An image from before it says nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawDisk {
    pub bytes: u64,
    pub sha256: String,
}

/// What is recorded beside a checked raw copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RawRecord {
    disk: RawDisk,
    modified_ns: u128,
}

/// The file the check of `raw` is recorded in.
pub fn raw_record(raw: &Path) -> PathBuf {
    let mut name = raw.as_os_str().to_owned();
    name.push(".json");
    PathBuf::from(name)
}

/// What the image in `image_dir` says of the raw copy `raw`, when `raw` is its system disk.
/// Other disks of the image (the user's disk template) are not described.
pub fn expected_raw(image_dir: &Path, raw: &Path) -> Option<RawDisk> {
    if raw.file_stem()? != "system" {
        return None;
    }
    let text = std::fs::read(image_dir.join("image.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_slice(&text).ok()?;
    serde_json::from_value(manifest.get("system_disk_raw")?.clone()).ok()
}

fn modified_ns(path: &Path) -> Option<u128> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos(),
    )
}

fn read_record(raw: &Path) -> Option<RawRecord> {
    serde_json::from_slice(&std::fs::read(raw_record(raw)).ok()?).ok()
}

fn write_record(raw: &Path, disk: RawDisk) -> Result<(), String> {
    let record = RawRecord {
        disk,
        modified_ns: modified_ns(raw).unwrap_or(0),
    };
    std::fs::write(
        raw_record(raw),
        serde_json::to_vec_pretty(&record).expect("serialises"),
    )
    .map_err(|e| format!("recording {}: {e}", raw.display()))
}

fn forget(raw: &Path) {
    let _ = std::fs::remove_file(raw);
    let _ = std::fs::remove_file(raw_record(raw));
}

/// Whether the raw copy `raw` is the one checked last: its record matches the file as it is,
/// and the hash `expected` names, when there is one.
pub fn raw_copy_is_checked(raw: &Path, expected: Option<&RawDisk>) -> bool {
    let Some(record) = read_record(raw) else {
        return false;
    };
    let size = std::fs::metadata(raw).map(|m| m.len()).ok();
    size == Some(record.disk.bytes)
        && modified_ns(raw) == Some(record.modified_ns)
        && expected.is_none_or(|e| e.sha256.eq_ignore_ascii_case(&record.disk.sha256))
}

/// Hashes the raw copy `raw`, refuses it when `expected` names another hash, and records it.
fn check_raw(raw: &Path, expected: Option<&RawDisk>) -> Result<(), String> {
    let (bytes, sha256) =
        file_sha256(raw).map_err(|e| format!("reading {}: {e}", raw.display()))?;
    if let Some(e) = expected {
        if e.bytes != bytes || !e.sha256.eq_ignore_ascii_case(&sha256) {
            let _ = std::fs::remove_file(raw_record(raw));
            return Err(format!(
                "{} is {bytes} bytes with SHA-256 {sha256}; the image says {} bytes with {}, so \
                 it is not the image's system disk",
                raw.display(),
                e.bytes,
                e.sha256
            ));
        }
    }
    write_record(raw, RawDisk { bytes, sha256 })
}

/// Makes sure `raw` is a checked raw copy of the disk `source` of the image in `image_dir`.
/// A copy whose record matches is kept; one without a record, or that changed since, is made
/// again by `write(source, raw)` and checked against the hash the image names. A copy with
/// no `source` to make it again from is checked as it is, and refused when it is not the
/// image's. Returns whether the copy was made now, for the host to set its permissions.
pub fn checked_raw_copy(
    raw: &Path,
    source: Option<&Path>,
    image_dir: &Path,
    write: &dyn Fn(&Path, &Path) -> Result<(), String>,
) -> Result<bool, String> {
    let expected = expected_raw(image_dir, raw);
    if raw.exists() && raw_copy_is_checked(raw, expected.as_ref()) {
        return Ok(false);
    }
    let Some(source) = source else {
        // A raw disk a package put there, with no VHDX to make it again from: checked now.
        check_raw(raw, expected.as_ref())?;
        return Ok(false);
    };
    forget(raw);
    write(source, raw)?;
    if let Err(why) = check_raw(raw, expected.as_ref()) {
        let _ = std::fs::remove_file(raw);
        return Err(why);
    }
    Ok(true)
}

/// Checks the raw copy `raw` byte by byte, as a host does when it starts: a copy that is not
/// the one recorded, or not the one the image in `image_dir` names, is removed (with its
/// record), and the next boot makes it again from `source`. Returns whether a checked copy is
/// there. A copy with no `source` is left alone, since nothing can make it again.
pub fn verify_raw_copy(raw: &Path, source: &Path, image_dir: &Path) -> Result<bool, String> {
    if !raw.exists() || !source.is_file() {
        return Ok(false);
    }
    let wanted = expected_raw(image_dir, raw).or(read_record(raw).map(|r| r.disk));
    let (bytes, sha256) =
        file_sha256(raw).map_err(|e| format!("reading {}: {e}", raw.display()))?;
    let matches = wanted
        .as_ref()
        .is_some_and(|w| w.bytes == bytes && w.sha256.eq_ignore_ascii_case(&sha256));
    if !matches {
        forget(raw);
        return Ok(false);
    }
    write_record(raw, RawDisk { bytes, sha256 })?;
    Ok(true)
}

/// [`verify_raw_copy`], with what it found written to the service's log as `build`.
pub fn verify_raw_copy_logged(raw: &Path, source: &Path, image_dir: &Path, build: &str) {
    let had_copy = raw.exists();
    match verify_raw_copy(raw, source, image_dir) {
        Ok(true) => eprintln!(
            "{build}: the raw copy {} matches its record",
            raw.display()
        ),
        Ok(false) if had_copy => eprintln!(
            "{build}: the raw copy {} did not match the image; it was removed, and the next boot makes it again",
            raw.display()
        ),
        Ok(false) => {}
        Err(why) => eprintln!("{build}: checking the raw system disk: {why}"),
    }
}
