//! What a VM boots from an image folder, and each user's Android disk.
//!
//! An image folder holds what the image build publishes for every host: `image.json`,
//! `initrd.img`, the kernel (`bzImage`, or `Image` for arm64), `system.vhdx` and `data.vhdx`,
//! the template of a user's disk (an older image has one `disk.vhdx` for both). crosvm attaches
//! raw and qcow2 disks, not VHDX, so the service reads the system disk into a raw copy beside
//! it (`system.img`, sparse) the first time it boots it ([`raw_disk`]); the published VHDX and
//! its checksum in `image.json` stay as they were. A raw or qcow2 file beside the VHDX wins
//! over it. The raw copy is checked, not trusted: against the hash `image.json` gives for the
//! disk as it is (`system_disk_raw`, which the image build records) when it gives one, and its
//! hash is recorded beside it (`system.img.json`) with the file's size and time; a copy that
//! changed is made again, and the service checks it byte by byte when it starts
//! ([`verify_raw_disk`]).
//!
//! Each user's disk is a sparse raw file (`data.img`), made from the template: the blocks the
//! template holds, and holes for the rest. The guest's discards punch holes in it again, so it
//! takes only what Android has written. A user whose disk an earlier DSA made as qcow2
//! (`data.qcow2`) keeps it; crosvm reads qcow2 too. The image version it was made from or
//! brought to is recorded beside it (`data.version`).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// What to boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFiles {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub kernel_cmdline: String,
    /// The read-only system disk; `None` for an image with one disk.
    pub system_image: Option<PathBuf>,
    /// The template of a new user's disk.
    pub disk_template: Option<PathBuf>,
}

/// The format of a disk file, from its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskFormat {
    Raw,
    Qcow2,
    Vhdx,
}

impl DiskFormat {
    pub fn of(path: &Path) -> DiskFormat {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("qcow2") => DiskFormat::Qcow2,
            Some("vhdx") => DiskFormat::Vhdx,
            _ => DiskFormat::Raw,
        }
    }
}

/// How much is copied at a time; a run of zeroes this long stays a hole.
const CHUNK: usize = 1 << 20;

/// Copies the raw disk `from` to `to`, leaving the runs of zeroes as holes.
pub fn copy_sparse(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = File::open(from)?;
    let size = source.metadata()?.len();
    let mut target = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(to)?;
    target.set_len(size)?;
    let mut buf = vec![0u8; CHUNK];
    let mut at = 0u64;
    while at < size {
        let len = ((size - at) as usize).min(CHUNK);
        source.read_exact(&mut buf[..len])?;
        if buf[..len].iter().any(|&b| b != 0) {
            target.seek(SeekFrom::Start(at))?;
            target.write_all(&buf[..len])?;
        } else {
            leave_hole(&target, at, len as u64)?;
        }
        at += len as u64;
    }
    target.sync_all()
}

/// Makes sure the range `at..at + len` of `file`, which nothing was written to, takes no
/// blocks. On Linux a file grown with `set_len` is a hole already. On APFS a copy made that
/// way was found to take a block for every byte, so on macOS the range is punched out.
#[cfg(not(target_os = "macos"))]
fn leave_hole(_file: &File, _at: u64, _len: u64) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn leave_hole(file: &File, at: u64, len: u64) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;

    /// `F_PUNCHHOLE` in `<sys/fcntl.h>`.
    const F_PUNCHHOLE: libc::c_int = 99;
    /// `struct fpunchhole` in `<sys/fcntl.h>`.
    #[repr(C)]
    struct FPunchhole {
        fp_flags: libc::c_uint,
        reserved: libc::c_uint,
        fp_offset: libc::off_t,
        fp_length: libc::off_t,
    }
    // The range must be whole file system blocks; a tail shorter than one stays allocated.
    const BLOCK: u64 = 4096;
    let len = len / BLOCK * BLOCK;
    if len == 0 || at % BLOCK != 0 {
        return Ok(());
    }
    let hole = FPunchhole {
        fp_flags: 0,
        reserved: 0,
        fp_offset: at as libc::off_t,
        fp_length: len as libc::off_t,
    };
    // SAFETY: `hole` is a valid `struct fpunchhole` that outlives the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), F_PUNCHHOLE, &hole) } == -1 {
        let error = io::Error::last_os_error();
        // A file system without holes (HFS+, exFAT) keeps the zeroes: the disk reads the
        // same, it only takes more room.
        if error.raw_os_error() != Some(libc::ENOTSUP) {
            return Err(error);
        }
    }
    Ok(())
}

/// Writes the disk `from` holds (VHDX or raw) to `to` as a sparse raw disk, through a
/// `.partial` file, so `to` is either whole or absent.
pub fn write_raw_disk(from: &Path, to: &Path) -> Result<(), String> {
    let mut partial = to.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let written = match DiskFormat::of(from) {
        DiskFormat::Vhdx => dsa_host_unix::vhdx::convert_to_raw(from, &partial),
        DiskFormat::Raw => copy_sparse(from, &partial),
        DiskFormat::Qcow2 => {
            return Err(format!(
                "{} is a qcow2 disk, which DSA does not make disks from; the image should carry \
                 a VHDX or raw disk",
                from.display()
            ))
        }
    };
    if let Err(error) = written.and_then(|()| std::fs::rename(&partial, to)) {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "could not read {} into {}: {error}",
            from.display(),
            to.display()
        ));
    }
    Ok(())
}

/// The disk crosvm attaches for `disk`: `disk` itself unless it is VHDX, else its raw copy
/// beside it, made now if it is not there yet and readable by every user, as the system disk
/// is shared by all of them. The raw copy of a system disk (`system.img`, whether the service
/// or a package made it) is used only while it is the copy that was checked: a copy without a
/// record, or one that changed since, is made again from the VHDX, and checked against the hash
/// `image.json` names.
pub fn raw_disk(disk: &Path) -> Result<PathBuf, String> {
    let is_copy = disk.extension().is_some_and(|e| e == "img");
    let (vhdx, raw) = match DiskFormat::of(disk) {
        DiskFormat::Vhdx => (Some(disk.to_path_buf()), disk.with_extension("img")),
        DiskFormat::Raw if is_copy => {
            let vhdx = disk.with_extension("vhdx");
            (vhdx.is_file().then_some(vhdx), disk.to_path_buf())
        }
        _ => return Ok(disk.to_path_buf()),
    };
    let expected = expected_raw(&raw);
    if raw.exists() && raw_copy_is_checked(&raw, expected.as_ref()) {
        return Ok(raw);
    }
    let Some(vhdx) = vhdx else {
        // A raw disk a package put there, with no VHDX to make it again from: checked now.
        check_raw(&raw, expected.as_ref())?;
        return Ok(raw);
    };
    let _ = std::fs::remove_file(&raw);
    let _ = std::fs::remove_file(raw_record(&raw));
    write_raw_disk(&vhdx, &raw)?;
    if let Err(why) = check_raw(&raw, expected.as_ref()) {
        let _ = std::fs::remove_file(&raw);
        return Err(why);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("making {} readable: {e}", raw.display()))?;
    }
    Ok(raw)
}

/// What `image.json` says of the system disk as it is (`system_disk_raw`): its size and
/// SHA-256. An image from before it says nothing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RawDisk {
    pub bytes: u64,
    pub sha256: String,
}

/// What is recorded beside a checked raw copy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RawRecord {
    disk: RawDisk,
    modified_ns: u128,
}

fn raw_record(raw: &Path) -> PathBuf {
    let mut name = raw.as_os_str().to_owned();
    name.push(".json");
    PathBuf::from(name)
}

fn expected_raw(raw: &Path) -> Option<RawDisk> {
    let text = std::fs::read(raw.with_file_name("image.json")).ok()?;
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

/// Whether the raw copy `raw` is the one checked last: its record matches the file as it is,
/// and the hash `expected` names, when there is one.
fn raw_copy_is_checked(raw: &Path, expected: Option<&RawDisk>) -> bool {
    let Some(record) = std::fs::read(raw_record(raw))
        .ok()
        .and_then(|text| serde_json::from_slice::<RawRecord>(&text).ok())
    else {
        return false;
    };
    let size = std::fs::metadata(raw).map(|m| m.len()).ok();
    size == Some(record.disk.bytes)
        && modified_ns(raw) == Some(record.modified_ns)
        && expected.is_none_or(|e| e.sha256.eq_ignore_ascii_case(&record.disk.sha256))
}

/// Hashes the raw copy `raw`, refuses it when `expected` names another hash, and records it.
fn check_raw(raw: &Path, expected: Option<&RawDisk>) -> Result<(), String> {
    let (bytes, sha256) = dsa_host_core::direct_mapped_disk::file_sha256(raw)
        .map_err(|e| format!("reading {}: {e}", raw.display()))?;
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
    let record = RawRecord {
        disk: RawDisk { bytes, sha256 },
        modified_ns: modified_ns(raw).unwrap_or(0),
    };
    std::fs::write(
        raw_record(raw),
        serde_json::to_vec_pretty(&record).expect("serialises"),
    )
    .map_err(|e| format!("recording {}: {e}", raw.display()))
}

/// Checks the raw copy of the system disk of the image in `dir` byte by byte, as the service
/// does when it starts: a copy that is not the one recorded, or not the one the image names,
/// is removed (with its record), and the next boot makes it again. Returns whether a checked
/// copy is there.
pub fn verify_raw_disk(dir: &Path) -> Result<bool, String> {
    let raw = dir.join("system.img");
    if !raw.exists() || !dir.join("system.vhdx").is_file() {
        return Ok(false);
    }
    let expected = expected_raw(&raw);
    let record = std::fs::read(raw_record(&raw))
        .ok()
        .and_then(|text| serde_json::from_slice::<RawRecord>(&text).ok());
    let (bytes, sha256) = dsa_host_core::direct_mapped_disk::file_sha256(&raw)
        .map_err(|e| format!("reading {}: {e}", raw.display()))?;
    let wanted = expected.or(record.map(|r| r.disk));
    let matches = wanted
        .as_ref()
        .is_some_and(|w| w.bytes == bytes && w.sha256.eq_ignore_ascii_case(&sha256));
    if !matches {
        let _ = std::fs::remove_file(&raw);
        let _ = std::fs::remove_file(raw_record(&raw));
        return Ok(false);
    }
    let record = RawRecord {
        disk: RawDisk { bytes, sha256 },
        modified_ns: modified_ns(&raw).unwrap_or(0),
    };
    std::fs::write(
        raw_record(&raw),
        serde_json::to_vec_pretty(&record).expect("serialises"),
    )
    .map_err(|e| format!("recording {}: {e}", raw.display()))?;
    Ok(true)
}

/// The disk file named `stem`, preferring a raw copy, then qcow2, then the published VHDX.
pub fn disk_file(dir: &Path, stem: &str) -> Option<PathBuf> {
    ["img", "raw", "qcow2", "vhdx"]
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|path| path.exists())
}

/// What to boot from the image folder `dir`, with `extra_cmdline` after the image's own
/// kernel command line, and `kernel` when the image carries none of its own.
pub fn image_files(
    dir: &Path,
    kernel: Option<&Path>,
    extra_cmdline: &str,
) -> Result<ImageFiles, String> {
    let kernel = ["bzImage", "Image"]
        .iter()
        .map(|name| dir.join(name))
        .find(|own| own.exists())
        .or_else(|| kernel.map(Path::to_path_buf))
        .ok_or_else(|| format!("the image in {} has no kernel", dir.display()))?;
    let manifest = std::fs::read_to_string(dir.join("image.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let image_cmdline = manifest
        .as_ref()
        .and_then(|m| m["kernel_cmdline"].as_str())
        .unwrap_or_default();
    let kernel_cmdline = format!("{image_cmdline} {extra_cmdline}")
        .trim()
        .to_string();
    let initrd = Some(dir.join("initrd.img")).filter(|p| p.exists());
    let (system_image, disk_template) = match disk_file(dir, "system") {
        Some(system) => (Some(system), disk_file(dir, "data")),
        None => (None, disk_file(dir, "disk")),
    };
    if system_image.is_none() && disk_template.is_none() {
        return Err(format!(
            "the image in {} has neither system.vhdx nor disk.vhdx",
            dir.display()
        ));
    }
    Ok(ImageFiles {
        kernel,
        initrd,
        kernel_cmdline,
        system_image,
        disk_template,
    })
}

/// The file beside a user's disk that records the image version it holds.
pub fn version_file(disk: &Path) -> PathBuf {
    disk.with_extension("version")
}

/// The image version the user's disk holds, if recorded.
pub fn disk_version(disk: &Path) -> Option<String> {
    std::fs::read_to_string(version_file(disk))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

#[cfg(target_os = "linux")]
pub use making::{bring_disk_to, make_disk};

#[cfg(target_os = "linux")]
mod making {
    use super::*;
    use crate::users::Account;

    /// Makes a new disk at `disk` for `account`: a sparse raw copy of `template`, or without
    /// one an empty sparse disk of `size_gb`. Only the user may read or write it.
    pub fn make_disk(
        account: &Account,
        disk: &Path,
        template: Option<&Path>,
        size_gb: u32,
    ) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        if let Some(folder) = disk.parent() {
            account.make_dir(folder)?;
        }
        match template {
            Some(template) => write_raw_disk(template, disk)?,
            None => {
                let file =
                    File::create(disk).map_err(|e| format!("creating {}: {e}", disk.display()))?;
                file.set_len(u64::from(size_gb) << 30)
                    .map_err(|e| format!("sizing {}: {e}", disk.display()))?;
            }
        }
        std::fs::set_permissions(disk, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("making {} private: {e}", disk.display()))?;
        account.own(disk)
    }

    /// Makes sure `account`'s disk holds the image `version`: a missing disk is made from
    /// `template`, and the version is recorded.
    ///
    /// The images made by the image build keep Android on the separate system disk, so a
    /// user's disk holds only their data and needs nothing copied when the image changes. A
    /// one-disk image (`disk.vhdx`) keeps Android on the user's disk; such a disk made at an
    /// older version is refused rather than booted with mismatched partitions.
    pub fn bring_disk_to(
        account: &Account,
        disk: &Path,
        template: &Path,
        two_disks: bool,
        version: &str,
    ) -> Result<(), String> {
        if !disk.exists() {
            make_disk(account, disk, Some(template), 0)?;
        } else if !two_disks {
            if let Some(held) = disk_version(disk).filter(|held| held != version) {
                return Err(format!(
                    "{} holds Android {held}, and the image {version} keeps Android on the \
                     same disk; reset Android's data (dsa reset) to start from {version}",
                    disk.display()
                ));
            }
        }
        account.write_private(&version_file(disk), version.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-image-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        dir
    }

    fn raw_described(dir: &Path, data: &[u8]) {
        let (bytes, sha256) = {
            let path = dir.join("described");
            std::fs::write(&path, data).unwrap();
            let found = dsa_host_core::direct_mapped_disk::file_sha256(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            found
        };
        let manifest = serde_json::json!({"system_disk_raw": {"bytes": bytes, "sha256": sha256}});
        std::fs::write(dir.join("image.json"), manifest.to_string()).unwrap();
    }

    #[test]
    fn nfr1_a_raw_system_disk_is_used_only_as_the_image_describes_it() {
        let dir = folder("raw-checked", &[]);
        let raw = dir.join("system.img");
        std::fs::write(&raw, b"the system disk").unwrap();
        raw_described(&dir, b"the system disk");
        assert_eq!(raw_disk(&raw).unwrap(), raw);
        assert!(raw_copy_is_checked(&raw, expected_raw(&raw).as_ref()));
        // Changed after it was checked: refused, since nothing can make it again.
        std::fs::write(&raw, b"the system disk, changed").unwrap();
        let why = raw_disk(&raw).unwrap_err();
        assert!(why.contains("not the image's system disk"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr1_a_tampered_raw_copy_is_removed_when_the_service_checks_it() {
        let dir = folder("raw-verify", &["system.vhdx"]);
        let raw = dir.join("system.img");
        std::fs::write(&raw, b"the system disk").unwrap();
        raw_described(&dir, b"the system disk");
        assert_eq!(verify_raw_disk(&dir), Ok(true));
        assert!(raw_copy_is_checked(&raw, expected_raw(&raw).as_ref()));
        // The same size, other bytes, and its time put back: the byte check finds it.
        let before = std::fs::metadata(&raw).unwrap().modified().unwrap();
        std::fs::write(&raw, b"the system disc").unwrap();
        File::options()
            .write(true)
            .open(&raw)
            .unwrap()
            .set_modified(before)
            .unwrap();
        assert_eq!(verify_raw_disk(&dir), Ok(false));
        assert!(!raw.exists());
        assert!(!raw_record(&raw).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr14_a_published_image_boots_its_own_kernel_and_disks() {
        let dir = folder(
            "two",
            &["bzImage", "initrd.img", "system.vhdx", "data.vhdx"],
        );
        std::fs::write(
            dir.join("image.json"),
            r#"{"kernel_cmdline": "console=ttyS0 androidboot.boot_devices=vmbus"}"#,
        )
        .unwrap();
        let files = image_files(&dir, None, "androidboot.dsa.x=1").unwrap();
        assert_eq!(files.kernel, dir.join("bzImage"));
        assert_eq!(files.initrd, Some(dir.join("initrd.img")));
        assert_eq!(files.system_image, Some(dir.join("system.vhdx")));
        assert_eq!(files.disk_template, Some(dir.join("data.vhdx")));
        assert_eq!(
            files.kernel_cmdline,
            "console=ttyS0 androidboot.boot_devices=vmbus androidboot.dsa.x=1"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr14_a_raw_copy_the_package_made_wins_over_the_vhdx() {
        let dir = folder("raw", &["Image", "system.vhdx", "system.img", "data.vhdx"]);
        let files = image_files(&dir, None, "").unwrap();
        assert_eq!(files.kernel, dir.join("Image"));
        assert_eq!(files.system_image, Some(dir.join("system.img")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr14_a_one_disk_image_is_the_template_of_the_users_disk() {
        let dir = folder("one", &["disk.vhdx"]);
        let files = image_files(&dir, Some(Path::new("/opt/dsa/bzImage")), "").unwrap();
        assert_eq!(files.kernel, PathBuf::from("/opt/dsa/bzImage"));
        assert_eq!(files.system_image, None);
        assert_eq!(files.disk_template, Some(dir.join("disk.vhdx")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr14_an_image_without_disks_is_refused() {
        let dir = folder("empty", &["bzImage"]);
        assert!(image_files(&dir, None, "").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr19_a_users_disk_is_a_sparse_raw_copy_of_the_template() {
        let dir = folder("sparse", &[]);
        let template = dir.join("data.img");
        let mut bytes = vec![0u8; 3 << 20];
        bytes[(2 << 20) + 5] = 7;
        std::fs::write(&template, &bytes).unwrap();
        let disk = dir.join("user.img");
        write_raw_disk(&template, &disk).unwrap();
        assert_eq!(std::fs::read(&disk).unwrap(), bytes);
        assert!(!dir.join("user.img.partial").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Only the megabyte with data takes blocks.
            assert!(std::fs::metadata(&disk).unwrap().blocks() * 512 <= 2 << 20);
        }
        assert!(write_raw_disk(&dir.join("old.qcow2"), &dir.join("x.img")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr14_crosvm_boots_a_raw_copy_of_a_vhdx_system_disk() {
        let dir = folder("rawcopy", &[]);
        let raw = dir.join("system.img");
        assert_eq!(raw_disk(&raw).unwrap(), raw);
        // A file that is not a VHDX is refused, and no partial copy is left behind.
        std::fs::write(dir.join("system.vhdx"), b"not a vhdx").unwrap();
        assert!(raw_disk(&dir.join("system.vhdx")).is_err());
        assert!(!raw.exists());
        assert!(!dir.join("system.img.partial").exists());
        // A raw copy made earlier is used as it is.
        std::fs::write(&raw, b"raw").unwrap();
        assert_eq!(raw_disk(&dir.join("system.vhdx")).unwrap(), raw);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr19_the_disks_version_is_kept_beside_it() {
        assert_eq!(
            version_file(Path::new("/h/.local/share/dsa/data.qcow2")),
            PathBuf::from("/h/.local/share/dsa/data.version")
        );
    }
}
