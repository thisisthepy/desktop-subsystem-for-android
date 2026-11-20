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
//! (`dsa_host_core::disk::raw_copy`, the same checks on every host that attaches raw disks).
//!
//! Each user's disk is a sparse raw file (`data.img`), made from the template: the blocks the
//! template holds, and holes for the rest. The guest's discards punch holes in it again, so it
//! takes only what Android has written. A user whose disk an earlier DSA made as qcow2
//! (`data.qcow2`) keeps it; crosvm reads qcow2 too. The image version it was made from or
//! brought to is recorded beside it (`data.image-version`), and kept up to date by the host
//! core's algorithm ([`dsa_host_core::disk`]), the same as on every host.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use dsa_host_core::image_files::{GuestArch, ImageRulesOwned};

use dsa_host_core::disk::raw_copy::checked_raw_copy;
pub use dsa_host_core::disk::raw_copy::RawDisk;
pub use dsa_host_core::image_files::ImageFiles;

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
    let image_dir = raw.parent().unwrap_or(Path::new("."));
    let made = checked_raw_copy(&raw, vhdx.as_deref(), image_dir, &write_raw_disk)?;
    #[cfg(unix)]
    if made {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("making {} readable: {e}", raw.display()))?;
    }
    #[cfg(not(unix))]
    let _ = made;
    Ok(raw)
}

/// What to boot from the image folder `dir`, with `extra_cmdline` after the image's own
/// kernel command line, and `kernel` when the image carries none of its own. A raw or qcow2
/// disk beside the published VHDX wins over it.
pub fn image_files(
    dir: &Path,
    kernel: Option<&Path>,
    extra_cmdline: &str,
) -> Result<ImageFiles, String> {
    image_rules(kernel, extra_cmdline).read(dir)
}

/// The rules this host reads an image by: the kernel names it boots, the disk formats it
/// attaches (a raw or qcow2 disk beside the published VHDX wins over it), the architecture it
/// runs, and the `kernel` and `extra_cmdline` its own configuration supplies.
pub fn image_rules(kernel: Option<&Path>, extra_cmdline: &str) -> ImageRulesOwned {
    ImageRulesOwned {
        kernel_names: vec!["bzImage".into(), "Image".into()],
        disk_extensions: vec!["img".into(), "raw".into(), "qcow2".into(), "vhdx".into()],
        arch: Some(GuestArch::HOST),
        fallback_kernel: kernel.map(Path::to_path_buf),
        extra_cmdline: extra_cmdline.to_string(),
    }
}

/// The file beside a user's disk that earlier versions recorded the image version in.
fn legacy_version_file(disk: &Path) -> PathBuf {
    disk.with_extension("version")
}

/// Moves the image version an earlier version of DSA recorded beside `disk` (`data.version`)
/// to where the host core reads it (`data.image-version`), so the disk keeps its version.
pub fn adopt_legacy_version_record(disk: &Path) {
    let (old, new) = (
        legacy_version_file(disk),
        dsa_host_core::disk::version_file(disk),
    );
    if old.exists() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
}

#[cfg(target_os = "linux")]
pub use making::{make_disk, UserDisks};

#[cfg(target_os = "linux")]
mod making {
    use super::*;
    use crate::users::Account;
    use dsa_host_core::disk::DiskBackend;
    use dsa_host_unix::raw_disk::RawDiskBackend;

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

    /// The disks of `account`, as the host core's disk algorithm opens and makes them: raw
    /// files, made by [`make_disk`] and the version record written as the user. A qcow2 disk
    /// an earlier DSA made is kept as it is, since its partitions are not read here.
    pub struct UserDisks(pub Account);

    impl DiskBackend for UserDisks {
        type Device = <RawDiskBackend as DiskBackend>::Device;

        fn open(&self, disk: &Path, write: bool) -> Result<Self::Device, String> {
            RawDiskBackend.open(disk, write)
        }

        fn create(&self, disk: &Path, template: Option<&Path>, size_gb: u32) -> Result<(), String> {
            make_disk(&self.0, disk, template, size_gb)
        }

        fn write_beside(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
            self.0.write_private(path, bytes)
        }

        fn keeps_as_is(&self, disk: &Path) -> bool {
            DiskFormat::of(disk) == DiskFormat::Qcow2
        }

        /// crosvm attaches raw disks, so a VHDX system disk is read into one once.
        fn prepare_image_disk(&self, image_disk: &Path, _cache: &Path) -> Result<PathBuf, String> {
            raw_disk(image_disk)
        }

        /// The template is copied through the disk reader, VHDX or not, so it is not
        /// converted first.
        fn prepare_template_disk(&self, template: &Path, _cache: &Path) -> Result<PathBuf, String> {
            Ok(template.to_path_buf())
        }
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
    fn nfr1_a_raw_system_disk_is_used_only_as_the_image_describes_it() {
        let dir = folder("raw-checked", &[]);
        let raw = dir.join("system.img");
        std::fs::write(&raw, b"the system disk").unwrap();
        let (bytes, sha256) = dsa_host_core::direct_mapped_disk::file_sha256(&raw).unwrap();
        let manifest = serde_json::json!({"system_disk_raw": {"bytes": bytes, "sha256": sha256}});
        std::fs::write(dir.join("image.json"), manifest.to_string()).unwrap();
        assert_eq!(raw_disk(&raw).unwrap(), raw);
        // Changed after it was checked: refused, since nothing can make it again.
        std::fs::write(&raw, b"the system disk, changed").unwrap();
        let why = raw_disk(&raw).unwrap_err();
        assert!(why.contains("not the image's system disk"), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
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
        // A raw disk that is not there, with no VHDX to make it from, is refused.
        assert!(raw_disk(&raw).is_err());
        // A file that is not a VHDX is refused, and no partial copy is left behind.
        std::fs::write(dir.join("system.vhdx"), b"not a vhdx").unwrap();
        assert!(raw_disk(&dir.join("system.vhdx")).is_err());
        assert!(!raw.exists());
        assert!(!dir.join("system.img.partial").exists());
        // A raw copy with no VHDX beside it is checked and recorded, then used as it is.
        std::fs::remove_file(dir.join("system.vhdx")).unwrap();
        std::fs::write(&raw, b"raw").unwrap();
        assert_eq!(raw_disk(&raw).unwrap(), raw);
        // A checked copy made earlier is used as it is, whatever VHDX is beside it.
        std::fs::write(dir.join("system.vhdx"), b"not a vhdx").unwrap();
        assert_eq!(raw_disk(&dir.join("system.vhdx")).unwrap(), raw);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fr19_the_version_an_earlier_dsa_recorded_beside_the_disk_is_kept() {
        let dir = folder("legacy-version", &[]);
        let disk = dir.join("data.img");
        std::fs::write(dir.join("data.version"), "16.0.0-1").unwrap();
        adopt_legacy_version_record(&disk);
        assert_eq!(
            dsa_host_core::disk::disk_image_version(&disk).as_deref(),
            Some("16.0.0-1")
        );
        assert!(!dir.join("data.version").exists());
        // A record the host core wrote is not replaced by an old one.
        std::fs::write(dir.join("data.version"), "old").unwrap();
        adopt_legacy_version_record(&disk);
        assert_eq!(
            dsa_host_core::disk::disk_image_version(&disk).as_deref(),
            Some("16.0.0-1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
