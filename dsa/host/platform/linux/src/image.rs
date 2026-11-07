//! What a VM boots from an image folder, and each user's Android disk.
//!
//! An image folder holds what the image build publishes for every host: `image.json`,
//! `initrd.img`, the kernel (`bzImage`, or `Image` for arm64), `system.vhdx` and `data.vhdx`,
//! the template of a user's disk (an older image has one `disk.vhdx` for both). crosvm attaches
//! raw and qcow2 disks, not VHDX, so the service reads the system disk into a raw copy beside
//! it (`system.img`, sparse) the first time it boots it ([`raw_disk`]); the published VHDX and
//! its checksum in `image.json` stay as they were. A raw or qcow2 file beside the VHDX wins
//! over it.
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
/// is shared by all of them.
pub fn raw_disk(disk: &Path) -> Result<PathBuf, String> {
    if DiskFormat::of(disk) != DiskFormat::Vhdx {
        return Ok(disk.to_path_buf());
    }
    let raw = disk.with_extension("img");
    if !raw.exists() {
        write_raw_disk(disk, &raw)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o644))
                .map_err(|e| format!("making {} readable: {e}", raw.display()))?;
        }
    }
    Ok(raw)
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
