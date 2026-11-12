//! The system disk of the host's direct-mapped mode, made on the host from the image's.
//!
//! The published image compresses its read-only partitions (erofs with LZ4), which keeps its
//! download under GitHub's 2 GiB limit for one file, and a compressed file cannot be mapped
//! with DAX. So the host makes the disk the mode maps itself, from the image's verified
//! `system.vhdx`: the same disk with every erofs partition copied uncompressed
//! ([`erofs_write`]) and the super partition laid out again around them ([`disk`]), as a
//! fixed VHD (the disk's bytes and a 512-byte footer), which Hyper-V's virtual persistent
//! memory maps as it is.
//!
//! The copy is made the same way everywhere, so the image build makes it once too and
//! records its SHA-256 in `image.json` (`direct_mapped.system_disk`), which the signed release
//! list covers with its own hash. The host checks what it made against that hash before it
//! uses it: the copy is checked, not trusted. A change to how the copy is made changes the
//! bytes, so it comes with a new [`FORMAT`], and an image made for another format is booted
//! as before.
//!
//! [`Store`] keeps the copies in a folder of the host's: making one in the background,
//! resuming where an interrupted run stopped, checking it, and removing the copy of an image
//! that is no longer the current one.

pub mod disk;
pub mod erofs_read;
pub mod erofs_write;
pub mod lz4;
pub mod worker;

pub use worker::DirectMappedImages;

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::vhdx::ReadAt;

/// How the copy is made. The image build records the format it made its hash with, and a
/// host makes only copies of its own format.
pub const FORMAT: u32 = 1;

/// The VHD footer after the disk's bytes.
pub const FOOTER_BYTES: u64 = 512;

/// What `image.json` says of the copy (`direct_mapped.system_disk`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemDisk {
    /// The [`FORMAT`] the build made it in.
    pub format: u32,
    /// The disk's size, without the VHD footer: a whole number of 2 MiB.
    pub bytes: u64,
    /// The SHA-256 of the disk's bytes, without the footer, in hexadecimal.
    pub sha256: String,
    /// The size of its super partition, which the kernel command line's partition list
    /// gives for it.
    pub super_bytes: u64,
}

impl SystemDisk {
    /// Whether this host can make the copy: its format, and a plausible description.
    pub fn is_usable(&self) -> bool {
        self.format == FORMAT
            && self.bytes > 0
            && self.bytes % disk::DISK_ALIGNMENT == 0
            && self.super_bytes > 0
            && self.super_bytes < self.bytes
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

/// Describes the copy of the system disk `source` with its partitions in `keep` left as they
/// are: what the image build records in `image.json`. Nothing is written; the copy's bytes go
/// through the hash only.
pub fn describe(source: &mut dyn ReadAt, keep: &[String]) -> Result<SystemDisk, String> {
    let plan = disk::plan(source, keep)?;
    let mut hash = HashingSink(Sha256::new());
    for index in 0..plan.units().len() {
        plan.write_unit(source, index, &mut hash, &|| false)?;
    }
    Ok(SystemDisk {
        format: FORMAT,
        bytes: plan.disk_bytes,
        sha256: hex(&hash.0.finalize()),
        super_bytes: plan.super_bytes,
    })
}

struct HashingSink(Sha256);

impl Write for HashingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The size of the file at `path` and the SHA-256 of all of it, in hexadecimal.
pub fn file_sha256(path: &Path) -> io::Result<(u64, String)> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 4 << 20];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        len += n as u64;
    }
    Ok((len, hex(&hash.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The fixed VHD footer of a copy of `disk_bytes`, whose identifier comes from its hash so
/// the same copy always has the same footer.
pub fn footer(disk_bytes: u64, sha256: &str) -> [u8; FOOTER_BYTES as usize] {
    let mut f = [0u8; FOOTER_BYTES as usize];
    f[0..8].copy_from_slice(b"conectix");
    f[8..12].copy_from_slice(&2u32.to_be_bytes());
    f[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    f[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    f[28..32].copy_from_slice(b"dsa ");
    f[32..36].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    f[36..40].copy_from_slice(b"Wi2k");
    f[40..48].copy_from_slice(&disk_bytes.to_be_bytes());
    f[48..56].copy_from_slice(&disk_bytes.to_be_bytes());
    let (cylinders, heads, spt) = crate::addon_disk::geometry(disk_bytes / 512);
    f[56..58].copy_from_slice(&cylinders.to_be_bytes());
    f[58] = heads;
    f[59] = spt;
    f[60..64].copy_from_slice(&2u32.to_be_bytes());
    let id = Sha256::digest(format!("dsa direct-mapped system disk {sha256}"));
    f[68..84].copy_from_slice(&id[..16]);
    let sum: u32 = f.iter().map(|&b| u32::from(b)).sum();
    f[64..68].copy_from_slice(&(!sum).to_be_bytes());
    f
}

/// A copy that is ready to boot from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    /// The fixed VHD.
    pub path: PathBuf,
    /// The disk's size without the footer.
    pub disk_bytes: u64,
    pub super_bytes: u64,
}

/// How far a run is, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// What it is doing: "making" or "checking".
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
}

/// What the store records of a checked copy, beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    disk: SystemDisk,
    /// The file as it was when it was checked: a copy changed since is not used.
    file_bytes: u64,
    modified_ns: u128,
}

/// What a run in progress records, so the next one resumes after the last whole stretch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Resume {
    disk: SystemDisk,
    units_done: usize,
}

/// Only one run at a time touches a store's folder in this process, whoever starts it.
static RUNS: Mutex<()> = Mutex::new(());

/// The folder that keeps the copies, one per image version: `<version>.vhd` and its record
/// `<version>.vhd.json` once checked, `<version>.part.vhd` and `<version>.part.json` while
/// it is being made. A store for a host that attaches raw disks keeps `.img` files, the disk's
/// bytes without the VHD footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    folder: PathBuf,
    footer: bool,
}

fn modified_ns(meta: &fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temporary = path.with_extension("json.new");
    let text = serde_json::to_vec_pretty(value).expect("serialises");
    let mut file = fs::File::create(&temporary)
        .map_err(|e| format!("writing {}: {e}", temporary.display()))?;
    file.write_all(&text)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("writing {}: {e}", temporary.display()))?;
    drop(file);
    fs::rename(&temporary, path).map_err(|e| format!("placing {}: {e}", path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Why a run failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// It was asked to stop; the next run resumes.
    Stopped,
    /// What it made does not have the hash the image names; it was removed.
    Mismatch(String),
    /// The copy cannot be made: the image, the disk, or this host's format.
    Failed(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Stopped => write!(f, "stopped before it was done"),
            Failure::Mismatch(why) | Failure::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl Store {
    /// A store of fixed VHDs, which Hyper-V's virtual persistent memory maps.
    pub fn new(folder: impl Into<PathBuf>) -> Store {
        Store {
            folder: folder.into(),
            footer: true,
        }
    }

    /// A store of raw disks, which virtio-pmem maps.
    pub fn raw(folder: impl Into<PathBuf>) -> Store {
        Store {
            folder: folder.into(),
            footer: false,
        }
    }

    fn extension(&self) -> &'static str {
        if self.footer {
            "vhd"
        } else {
            "img"
        }
    }

    /// The size of the copy's file for a disk of `bytes`.
    fn file_bytes(&self, bytes: u64) -> u64 {
        if self.footer {
            bytes + FOOTER_BYTES
        } else {
            bytes
        }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// The copy of the image `version`.
    pub fn path(&self, version: &str) -> PathBuf {
        self.folder.join(format!("{version}.{}", self.extension()))
    }

    fn record_path(&self, version: &str) -> PathBuf {
        self.folder
            .join(format!("{version}.{}.json", self.extension()))
    }

    fn part_path(&self, version: &str) -> PathBuf {
        self.folder
            .join(format!("{version}.part.{}", self.extension()))
    }

    fn resume_path(&self, version: &str) -> PathBuf {
        self.folder.join(format!("{version}.part.json"))
    }

    /// The copy of `version` when it was checked against `expected` and has not changed
    /// since: what a boot uses. Anything else boots the image's own disk.
    pub fn ready(&self, version: &str, expected: &SystemDisk) -> Option<Ready> {
        let record: Record = read_json(&self.record_path(version))?;
        let meta = fs::metadata(self.path(version)).ok()?;
        (record.disk == *expected
            && meta.len() == self.file_bytes(expected.bytes)
            && record.file_bytes == meta.len()
            && record.modified_ns == modified_ns(&meta))
        .then(|| Ready {
            path: self.path(version),
            disk_bytes: expected.bytes,
            super_bytes: expected.super_bytes,
        })
    }

    /// Removes every copy, finished or not, of an image other than `version`: an image that
    /// was replaced takes its copy with it.
    pub fn remove_others(&self, version: &str) -> Vec<PathBuf> {
        let _run = RUNS.lock().unwrap_or_else(|e| e.into_inner());
        self.remove_others_locked(version)
    }

    fn remove_others_locked(&self, version: &str) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return Vec::new();
        };
        let mine = [
            self.path(version),
            self.record_path(version),
            self.part_path(version),
            self.resume_path(version),
        ]
        .map(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()));
        let mut removed = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !mine.contains(&Some(name)) && fs::remove_file(entry.path()).is_ok() {
                removed.push(entry.path());
            }
        }
        removed
    }

    fn remove(&self, version: &str) {
        for path in [
            self.path(version),
            self.record_path(version),
            self.part_path(version),
            self.resume_path(version),
        ] {
            let _ = fs::remove_file(path);
        }
    }

    /// Hashes the disk bytes of the file at `path` (its first `bytes`).
    fn hash_file(
        path: &Path,
        bytes: u64,
        progress: &mut dyn FnMut(&Progress),
        stop: &dyn Fn() -> bool,
    ) -> Result<String, Failure> {
        let mut file = fs::File::open(path)
            .map_err(|e| Failure::Failed(format!("opening {}: {e}", path.display())))?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 4 << 20];
        let mut done = 0u64;
        while done < bytes {
            if stop() {
                return Err(Failure::Stopped);
            }
            let take = (bytes - done).min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..take])
                .map_err(|e| Failure::Mismatch(format!("reading {}: {e}", path.display())))?;
            hash.update(&buf[..take]);
            done += take as u64;
            progress(&Progress {
                phase: "checking",
                done,
                total: bytes,
            });
        }
        Ok(hex(&hash.finalize()))
    }

    /// Checks the copy of `version` byte by byte against `expected`, as the service does when
    /// it starts: a copy that differs is removed (and made again by the next
    /// [`produce`](Self::produce)), one that matches is recorded again as it is now. Returns
    /// whether a checked copy is there.
    pub fn verify(
        &self,
        version: &str,
        expected: &SystemDisk,
        progress: &mut dyn FnMut(&Progress),
        stop: &dyn Fn() -> bool,
    ) -> Result<bool, Failure> {
        let _run = RUNS.lock().unwrap_or_else(|e| e.into_inner());
        self.verify_locked(version, expected, progress, stop)
    }

    fn verify_locked(
        &self,
        version: &str,
        expected: &SystemDisk,
        progress: &mut dyn FnMut(&Progress),
        stop: &dyn Fn() -> bool,
    ) -> Result<bool, Failure> {
        let path = self.path(version);
        let Ok(meta) = fs::metadata(&path) else {
            let _ = fs::remove_file(self.record_path(version));
            return Ok(false);
        };
        let footer_ok = meta.len() == self.file_bytes(expected.bytes)
            && (!self.footer
                || read_footer(&path, expected.bytes)
                    .is_some_and(|f| f == footer(expected.bytes, &expected.sha256)));
        let matches = footer_ok
            && match Self::hash_file(&path, expected.bytes, progress, stop) {
                Ok(actual) => actual.eq_ignore_ascii_case(&expected.sha256),
                Err(Failure::Stopped) => return Err(Failure::Stopped),
                Err(_) => false,
            };
        if !matches {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(self.record_path(version));
            return Ok(false);
        }
        let meta = fs::metadata(&path)
            .map_err(|e| Failure::Failed(format!("reading {}: {e}", path.display())))?;
        write_json(
            &self.record_path(version),
            &Record {
                disk: expected.clone(),
                file_bytes: meta.len(),
                modified_ns: modified_ns(&meta),
            },
        )
        .map_err(Failure::Failed)?;
        Ok(true)
    }

    /// Makes the copy of `version` from its system disk `source` (the image's `system.vhdx`,
    /// already checked against the signed list), leaving the partitions in `keep` as they
    /// are, unless a checked one is there; resumes a run that stopped. The copy is synced and
    /// then checked against `expected` before it takes its name; one that does not match is
    /// removed and made once more from the start. Copies of other images are removed first.
    pub fn produce(
        &self,
        version: &str,
        source: &Path,
        expected: &SystemDisk,
        keep: &[String],
        progress: &mut dyn FnMut(&Progress),
        stop: &dyn Fn() -> bool,
    ) -> Result<Ready, Failure> {
        let _run = RUNS.lock().unwrap_or_else(|e| e.into_inner());
        if !expected.is_usable() {
            return Err(Failure::Failed(format!(
                "the image describes its direct-mapped disk in format {}, and this host makes \
                 format {FORMAT}",
                expected.format
            )));
        }
        fs::create_dir_all(&self.folder)
            .map_err(|e| Failure::Failed(format!("creating {}: {e}", self.folder.display())))?;
        self.remove_others_locked(version);
        if let Some(ready) = self.ready(version, expected) {
            return Ok(ready);
        }
        let mut last = None;
        for _ in 0..2 {
            match self.attempt(version, source, expected, keep, progress, stop) {
                Ok(ready) => return Ok(ready),
                Err(Failure::Mismatch(why)) => {
                    self.remove(version);
                    last = Some(why);
                }
                Err(other) => return Err(other),
            }
        }
        Err(Failure::Mismatch(last.unwrap_or_default()))
    }

    fn attempt(
        &self,
        version: &str,
        source: &Path,
        expected: &SystemDisk,
        keep: &[String],
        progress: &mut dyn FnMut(&Progress),
        stop: &dyn Fn() -> bool,
    ) -> Result<Ready, Failure> {
        let mut dev = crate::vhdx::open_disk(source)
            .map_err(|e| Failure::Failed(format!("opening {}: {e}", source.display())))?;
        let plan = disk::plan(dev.as_mut(), keep).map_err(Failure::Failed)?;
        if plan.disk_bytes != expected.bytes || plan.super_bytes != expected.super_bytes {
            return Err(Failure::Failed(format!(
                "the copy would be {} bytes with a super partition of {}, and the image says {} \
                 and {}; it was made another way",
                plan.disk_bytes, plan.super_bytes, expected.bytes, expected.super_bytes
            )));
        }
        let units = plan.units();
        let part = self.part_path(version);
        let resume_path = self.resume_path(version);
        let resumed: Option<Resume> = read_json(&resume_path);
        let mut done = match resumed {
            Some(r) if r.disk == *expected && part.exists() && r.units_done <= units.len() => {
                r.units_done
            }
            _ => {
                let _ = fs::remove_file(&part);
                0
            }
        };
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&part)
            .map_err(|e| Failure::Failed(format!("opening {}: {e}", part.display())))?;
        let total = plan.disk_bytes;
        while done < units.len() {
            if stop() {
                return Err(Failure::Stopped);
            }
            let unit = &units[done];
            file.seek(SeekFrom::Start(unit.offset))
                .map_err(|e| Failure::Failed(format!("writing {}: {e}", part.display())))?;
            {
                let mut out = io::BufWriter::with_capacity(4 << 20, &mut file);
                let mut reporting = Reporting {
                    inner: &mut out,
                    at: unit.offset,
                    total,
                    progress: &mut *progress,
                    last: 0,
                };
                plan.write_unit(dev.as_mut(), done, &mut reporting, stop)
                    .map_err(|e| {
                        if e == "stopped" || e.ends_with(": stopped") {
                            Failure::Stopped
                        } else {
                            Failure::Failed(e)
                        }
                    })?;
                out.flush()
                    .map_err(|e| Failure::Failed(format!("writing {}: {e}", part.display())))?;
            }
            file.sync_data()
                .map_err(|e| Failure::Failed(format!("writing {}: {e}", part.display())))?;
            done += 1;
            write_json(
                &resume_path,
                &Resume {
                    disk: expected.clone(),
                    units_done: done,
                },
            )
            .map_err(Failure::Failed)?;
        }
        let footer_bytes = footer(plan.disk_bytes, &expected.sha256);
        let tail: &[u8] = if self.footer { &footer_bytes } else { &[] };
        file.seek(SeekFrom::Start(plan.disk_bytes))
            .and_then(|_| file.write_all(tail))
            .and_then(|()| file.set_len(self.file_bytes(plan.disk_bytes)))
            .and_then(|()| file.sync_all())
            .map_err(|e| Failure::Failed(format!("writing {}: {e}", part.display())))?;
        drop(file);
        let actual = Self::hash_file(&part, plan.disk_bytes, progress, stop)?;
        if !actual.eq_ignore_ascii_case(&expected.sha256) {
            return Err(Failure::Mismatch(format!(
                "the disk made from {} has SHA-256 {actual}; the image says {}",
                source.display(),
                expected.sha256
            )));
        }
        let path = self.path(version);
        let _ = fs::remove_file(&path);
        fs::rename(&part, &path)
            .map_err(|e| Failure::Failed(format!("placing {}: {e}", path.display())))?;
        let meta = fs::metadata(&path)
            .map_err(|e| Failure::Failed(format!("reading {}: {e}", path.display())))?;
        write_json(
            &self.record_path(version),
            &Record {
                disk: expected.clone(),
                file_bytes: meta.len(),
                modified_ns: modified_ns(&meta),
            },
        )
        .map_err(Failure::Failed)?;
        let _ = fs::remove_file(&resume_path);
        Ok(Ready {
            path,
            disk_bytes: plan.disk_bytes,
            super_bytes: plan.super_bytes,
        })
    }
}

fn read_footer(path: &Path, at: u64) -> Option<[u8; FOOTER_BYTES as usize]> {
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut f = [0u8; FOOTER_BYTES as usize];
    file.read_exact(&mut f).ok()?;
    Some(f)
}

/// Passes writes on and tells `progress` where the copy is, every 64 MiB.
struct Reporting<'a, W: Write> {
    inner: &'a mut W,
    at: u64,
    total: u64,
    progress: &'a mut dyn FnMut(&Progress),
    last: u64,
}

impl<W: Write> Write for Reporting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.at += n as u64;
        if self.at - self.last >= 64 << 20 || self.at == self.total {
            self.last = self.at;
            (self.progress)(&Progress {
                phase: "making",
                done: self.at,
                total: self.total,
            });
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests;
