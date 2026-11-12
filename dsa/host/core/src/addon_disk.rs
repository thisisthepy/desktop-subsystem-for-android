//! The user's add-on disk: the images of the add-ons a user installed (the Google apps, an ARM
//! translation package), on a disk the host attaches to their VM read only.
//!
//! Each add-on is an erofs image the guest mounts at boot. The images used to live on the
//! user's Android disk, in `/data`, which is encrypted: ext4 gives an encrypted file no direct
//! I/O without inline encryption, so every page the guest read from an image there was cached
//! twice, once for the image file and once by erofs, a few hundred MiB for the Google apps. The
//! images are public packages with nothing to keep secret, so they go on a disk of their own
//! that is not encrypted, where erofs reads each one straight into its own cache.
//!
//! The disk is one per user and only the host writes it, when the user installs or removes an
//! add-on. It is a GPT disk with one partition, [`PARTITION_NAME`], whose by-name link the
//! guest finds wherever the disk sits. The partition is an erofs file system that holds each
//! add-on's image as one plain, uncompressed file, named for the add-on ([`Addon::file_name`]):
//!
//! ```text
//! sector 0          protective MBR
//! sectors 1-33      GPT header and partition entries
//! 1 MiB             the partition: erofs, with gapps.img and arm-translation.img at its root
//! the last 1 MiB    the backup partition entries and GPT header
//! [Vhd only]        a fixed VHD footer, so Hyper-V attaches the file as a disk
//! ```
//!
//! erofs keeps each file whole and block aligned, so the guest mounts each image from it
//! file-backed with `directio` and reads it once. The partition table also names each image's
//! own bytes as a partition of their own ([`Addon::partition_name`]), inside the erofs
//! partition: the host's direct-mapped mode attaches the disk as persistent memory, and the
//! guest mounts each image from its partition with DAX, which a file-backed mount cannot have,
//! so its files are mapped from the host and not cached. Every other boot ignores them. The disk
//! is a whole number of 2 MiB, the sections Linux maps persistent memory in. Installing or removing an add-on writes a
//! whole new disk beside the attached one (the pending disk), with the images of the other
//! add-ons copied over, because the running VM keeps the attached disk open. The next start of
//! Android puts the pending disk in its place before the VM opens it
//! ([`AddonDisk::prepare_for_boot`]), which is when every change takes effect. A disk with no
//! add-on on it is removed, and the VM gets none.
//!
//! Each disk file has its images' SHA-256 digests beside it (`<disk>.sha256`), recorded when
//! each image was installed: the digest the package's signed list gave, or for a package
//! without one the digest of what the host checked and wrote. Before a boot the images on the
//! disk are checked against them ([`AddonDisk::prepare_for_boot`]); an image that differs is
//! taken off the disk, said so in the log, and Android starts without it.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use sha2::Digest;

use crate::diagnostics::crc32;
use crate::erofs::Stream;

/// The partition's name in the partition table, which the guest finds it by
/// (`/dev/block/by-name/dsa_addons`). Android makes a by-name link for the partitions of every
/// disk, so the name is DSA's own and never one of Android's.
pub const PARTITION_NAME: &str = "dsa_addons";
/// The partition's UUID.
pub const PARTITION_UUID: &str = "8d1c4e7a-3b52-4f19-a6e0-5c2b9d7f4e13";
/// The UUIDs of the partitions that name each add-on's image ([`Addon::partition_name`]).
const GAPPS_PARTITION_UUID: &str = "4a7e2c19-8b3d-4f60-9c15-6e0d3b8a2f71";
const ARM_TRANSLATION_PARTITION_UUID: &str = "c25f8e03-7a14-4b9d-8e62-1f4a9c3d5b08";
/// The disk's GUID, in its partition table and its VHD footer.
const DISK_GUID: &str = "b3f0a9d4-6e21-4c8b-9f57-2a4d1e8c6b90";
/// GPT's type for Linux file system data, the generic Linux type.
const LINUX_DATA_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// The bootconfig line that has the guest mount the add-on disk, for a boot the host attaches
/// one to.
pub const BOOTCONFIG_ON: &str = "androidboot.dsa.addon_disk = \"on\"\n";

const SECTOR: u64 = 512;
const MIB: u64 = 1 << 20;
/// The disk is a whole number of these: Linux maps persistent memory in 2 MiB sections, and a
/// device of another size gets no page structs, so no DAX.
const SECTION: u64 = 2 * MIB;
/// The partition starts at 1 MiB and its size is a whole number of MiB, as partitioning tools
/// align them.
const PARTITION_FIRST_SECTOR: u64 = MIB / SECTOR;
const ENTRIES: u64 = 128;
const ENTRY_BYTES: u64 = 128;
const ENTRY_SECTORS: u64 = ENTRIES * ENTRY_BYTES / SECTOR;
const FOOTER_BYTES: u64 = 512;
/// erofs's block size, superblock and inode size, as [`crate::erofs`] writes them.
const BLOCK: u64 = 4096;
const SUPERBLOCK: u64 = 1024;
const INODE: u64 = 32;
const EROFS_MAGIC: u32 = 0xE0F5_E1E2;
/// How much is copied at a time.
const PIECE: usize = 1 << 20;

/// Every change to any add-on disk goes one at a time, in this process.
static CHANGES: Mutex<()> = Mutex::new(());
/// The disk files whose images this process has checked against their digests, with the
/// length and time of change each had then, so a disk is read whole once and not at every
/// boot. Every write to a disk file takes it off.
static CHECKED: Mutex<BTreeMap<PathBuf, (u64, SystemTime)>> = Mutex::new(BTreeMap::new());

/// An image's SHA-256 digest.
type Sha256 = [u8; 32];

fn stamp_of(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn forget_checked(path: &Path) {
    CHECKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(path);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Sha256> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Where the digests of the disk file `disk` are kept: `<disk>.sha256`.
fn records_path(disk: &Path) -> PathBuf {
    let mut name = disk
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".sha256");
    disk.with_file_name(name)
}

/// The digests recorded for the disk file `disk`: `None` when it has no record, as a disk an
/// earlier DSA wrote has not. One line per image, `<file name> <hex digest>`.
fn read_records(disk: &Path) -> io::Result<Option<BTreeMap<Addon, Sha256>>> {
    let text = match fs::read_to_string(records_path(disk)) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut records = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (name, digest) = line
            .split_once(' ')
            .ok_or_else(|| invalid("its digest record is damaged"))?;
        let addon = Addon::of_file_name(name.as_bytes())
            .ok_or_else(|| invalid("its digest record is damaged"))?;
        let digest = unhex(digest.trim()).ok_or_else(|| invalid("its digest record is damaged"))?;
        records.insert(addon, digest);
    }
    Ok(Some(records))
}

/// Records `records` as the digests of the disk file `disk`, whole or not at all.
fn write_records(disk: &Path, records: &BTreeMap<Addon, Sha256>) -> io::Result<()> {
    let text: String = records
        .iter()
        .map(|(addon, digest)| format!("{} {}\n", addon.file_name(), hex(digest)))
        .collect();
    let path = records_path(disk);
    let mut partial = path.clone().into_os_string();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    {
        let mut file = File::create(&partial)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&partial, &path).inspect_err(|_| {
        let _ = fs::remove_file(&partial);
    })
}

/// The SHA-256 digest of `entry`'s image in `file`.
fn digest_of(file: &mut File, entry: &Entry) -> io::Result<Sha256> {
    file.seek(SeekFrom::Start(entry.offset))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; PIECE];
    let mut left = entry.size;
    while left > 0 {
        let length = left.min(PIECE as u64) as usize;
        file.read_exact(&mut buffer[..length])?;
        hasher.update(&buffer[..length]);
        left -= length as u64;
    }
    Ok(hasher.finalize().into())
}

/// An add-on whose image goes on the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Addon {
    /// The Google apps, which the guest lays over `/product` and `/system_ext`.
    Gapps,
    /// A translation package the user installed (their own or Google's translator), which the
    /// guest binds into `/system`.
    ArmTranslation,
}

impl Addon {
    /// Every add-on, in the order the disk holds them.
    pub const ALL: [Addon; 2] = [Addon::Gapps, Addon::ArmTranslation];

    /// The image's name on the disk, where the guest's init scripts mount it from
    /// (`/mnt/dsa-addons/<name>`).
    pub fn file_name(self) -> &'static str {
        match self {
            Addon::Gapps => "gapps.img",
            Addon::ArmTranslation => "arm-translation.img",
        }
    }

    /// The partition that names the image's bytes on the disk, which the guest mounts the
    /// image from in the host's direct-mapped mode (`/dev/block/by-name/<name>`).
    pub fn partition_name(self) -> &'static str {
        match self {
            Addon::Gapps => "dsa_gapps",
            Addon::ArmTranslation => "dsa_arm_translation",
        }
    }

    fn partition_uuid(self) -> &'static str {
        match self {
            Addon::Gapps => GAPPS_PARTITION_UUID,
            Addon::ArmTranslation => ARM_TRANSLATION_PARTITION_UUID,
        }
    }

    /// What the image is called in what the user reads.
    pub fn what(self) -> &'static str {
        match self {
            Addon::Gapps => "the Google apps image",
            Addon::ArmTranslation => "the ARM translation image",
        }
    }

    fn of_file_name(name: &[u8]) -> Option<Addon> {
        Addon::ALL
            .into_iter()
            .find(|addon| addon.file_name().as_bytes() == name)
    }
}

/// How the disk is kept as a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The disk's bytes alone, as crosvm and Apple's Virtualization framework take a disk.
    Raw,
    /// The disk's bytes followed by a fixed VHD footer, as Hyper-V takes a disk.
    Vhd,
}

/// The `androidboot.dsa.<key>` line that says where an add-on's image is for one boot: on the
/// add-on disk (`disk`), or, until the host has moved it there, where a DSA from before the
/// add-on disk kept it in `/data` (`data`).
pub fn image_bootconfig(key: &str, on_disk: bool) -> String {
    let place = if on_disk { "disk" } else { "data" };
    format!("androidboot.dsa.{key} = \"{place}\"\n")
}

/// What a boot's bootconfig carries for the add-on disk: [`BOOTCONFIG_ON`] when the host
/// attaches one, which it does only while it holds an add-on.
pub fn bootconfig(attached: &[Addon]) -> &'static str {
    if attached.is_empty() {
        ""
    } else {
        BOOTCONFIG_ON
    }
}

/// The bootconfig line that tells the guest its read-only disks are persistent memory this
/// boot (the host's direct-mapped mode).
pub const DIRECT_MAPPED_BOOTCONFIG: &str = "androidboot.dsa.direct_mapped = \"on\"\n";

/// A boot's bootconfig, `text`, for the host's direct-mapped mode: each add-on image the boot
/// takes from the disk (`"disk"`) is mounted from its own partition on the persistent memory
/// instead (`"dax"`), and the guest is told the mode is on. An image still in `/data` stays
/// where it is.
pub fn direct_mapped_bootconfig(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + DIRECT_MAPPED_BOOTCONFIG.len());
    for line in text.split_inclusive('\n') {
        let mapped = line
            .strip_prefix("androidboot.dsa.")
            .and_then(|rest| rest.split_once(" = "))
            .filter(|(key, value)| key.ends_with("_image") && value.trim_end() == "\"disk\"");
        match mapped {
            Some((key, _)) => out.push_str(&format!("androidboot.dsa.{key} = \"dax\"\n")),
            None => out.push_str(line),
        }
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(DIRECT_MAPPED_BOOTCONFIG);
    out
}

/// One add-on's image on a disk: where its bytes start in the disk file, and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    addon: Addon,
    offset: u64,
    size: u64,
}

/// A user's add-on disk: the one the VM attaches, at `path`, and the pending one beside it that
/// the next start puts in its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonDisk {
    path: PathBuf,
    format: Format,
}

impl AddonDisk {
    /// The disk kept at `path` in `format`.
    pub fn new(path: impl Into<PathBuf>, format: Format) -> AddonDisk {
        AddonDisk {
            path: path.into(),
            format,
        }
    }

    /// Where the disk the VM attaches is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The format the disk is kept in.
    pub fn format(&self) -> Format {
        self.format
    }

    /// `path` with `suffix` after its file name.
    fn beside(&self, suffix: &str) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        name.push(suffix);
        self.path.with_file_name(name)
    }

    /// Where the disk the next start puts in place is, until then.
    pub fn pending_path(&self) -> PathBuf {
        self.beside(".next")
    }

    /// Where a disk is written until it is whole.
    fn partial_path(&self) -> PathBuf {
        self.beside(".part")
    }

    /// The disk the next start of Android attaches: the pending one if there is one, else the
    /// one in place.
    fn next_disk(&self) -> PathBuf {
        let pending = self.pending_path();
        if pending.is_file() {
            pending
        } else {
            self.path.clone()
        }
    }

    /// What the disk at `path` holds; a disk that is not there holds nothing, and one that
    /// cannot be read is logged and counts as holding nothing.
    fn entries_of(path: &Path) -> Vec<Entry> {
        match read_entries(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                eprintln!(
                    "add-ons: the add-on disk {} cannot be read ({e}); it counts as empty",
                    path.display()
                );
                Vec::new()
            }
        }
    }

    /// The add-ons the next start of Android finds on the disk.
    pub fn contents(&self) -> Vec<Addon> {
        let _change = lock();
        Self::entries_of(&self.next_disk())
            .into_iter()
            .map(|entry| entry.addon)
            .collect()
    }

    /// Whether the next start of Android finds `addon` on the disk.
    pub fn holds(&self, addon: Addon) -> bool {
        self.contents().contains(&addon)
    }

    /// `addon`'s image as the next start of Android finds it on the disk, for the tests of what
    /// puts it there.
    #[cfg(any(test, feature = "conformance"))]
    pub fn image(&self, addon: Addon) -> Option<Vec<u8>> {
        let _change = lock();
        let path = self.next_disk();
        let entry = Self::entries_of(&path)
            .into_iter()
            .find(|entry| entry.addon == addon)?;
        let mut file = File::open(&path).ok()?;
        read_at(&mut file, entry.offset, entry.size as usize).ok()
    }

    /// The add-ons on the disk in place, which a running VM has attached.
    pub fn attached(&self) -> Vec<Addon> {
        let _change = lock();
        Self::entries_of(&self.path)
            .into_iter()
            .map(|entry| entry.addon)
            .collect()
    }

    /// Puts `addon`'s image, `size` bytes read from `content`, on the disk for the next start of
    /// Android, in place of any image of it there was. With `sha256`, the image must have that
    /// SHA-256 digest. The image must be an erofs image. The disk the running VM has attached
    /// is not touched.
    pub fn install(
        &self,
        addon: Addon,
        size: u64,
        sha256: Option<&[u8]>,
        content: &mut dyn Read,
    ) -> Result<(), String> {
        let _change = lock();
        let base = self.next_disk();
        let records = read_records(&base).unwrap_or(Some(BTreeMap::new()));
        let kept: Vec<Entry> = Self::entries_of(&base)
            .into_iter()
            .filter(|entry| entry.addon != addon)
            .collect();
        let mut order: Vec<Addon> = kept.iter().map(|entry| entry.addon).collect();
        order.push(addon);
        self.write_pending(&order, &mut |stream, which| match kept
            .iter()
            .find(|entry| entry.addon == which)
        {
            Some(entry) => copy_kept(stream, entry, &base, records.as_ref()),
            None => copy_image(stream, addon, size, sha256, content),
        })?;
        eprintln!(
            "add-ons: {} ({size} bytes) is on the add-on disk {} for the next start of Android",
            addon.what(),
            self.path.display()
        );
        Ok(())
    }

    /// Takes `addon`'s image off the disk for the next start of Android. Says whether there
    /// was one.
    pub fn remove(&self, addon: Addon) -> Result<bool, String> {
        let _change = lock();
        let base = self.next_disk();
        let records = read_records(&base).unwrap_or(Some(BTreeMap::new()));
        let entries = Self::entries_of(&base);
        if !entries.iter().any(|entry| entry.addon == addon) {
            return Ok(false);
        }
        let kept: Vec<Entry> = entries
            .into_iter()
            .filter(|entry| entry.addon != addon)
            .collect();
        let order: Vec<Addon> = kept.iter().map(|entry| entry.addon).collect();
        self.write_pending(&order, &mut |stream, which| {
            let entry = kept
                .iter()
                .find(|entry| entry.addon == which)
                .expect("every add-on written is kept");
            copy_kept(stream, entry, &base, records.as_ref())
        })?;
        eprintln!(
            "add-ons: {} is off the add-on disk {} from the next start of Android",
            addon.what(),
            self.path.display()
        );
        Ok(true)
    }

    /// Writes a disk holding the images of `addons`, which `fill` writes, and makes it the
    /// pending one.
    fn write_pending(&self, addons: &[Addon], fill: &mut Fill<'_>) -> Result<(), String> {
        self.write_disk_at(&self.pending_path(), addons, fill)
    }

    /// Writes a disk holding the images of `addons`, which `fill` writes, with their digests,
    /// and puts it at `target`.
    fn write_disk_at(
        &self,
        target: &Path,
        addons: &[Addon],
        fill: &mut Fill<'_>,
    ) -> Result<(), String> {
        let partial = self.partial_path();
        if let Some(dir) = partial.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(dir)
                .map_err(|e| format!("{} cannot be made: {e}", dir.display()))?;
        }
        let mut records = BTreeMap::new();
        let written = write_disk(&partial, self.format, addons, &mut |stream, addon| {
            let digest = fill(stream, addon)?;
            records.insert(addon, digest);
            Ok(digest)
        });
        let placed = written.and_then(|()| self.place(&partial, target, &records));
        if placed.is_err() {
            let _ = fs::remove_file(&partial);
        }
        placed
    }

    /// Puts the whole disk `partial` at `target`, with `records` as its digests: the digests
    /// first, so a disk is never in place with the digests of another.
    fn place(
        &self,
        partial: &Path,
        target: &Path,
        records: &BTreeMap<Addon, Sha256>,
    ) -> Result<(), String> {
        forget_checked(target);
        write_records(target, records).map_err(|e| {
            format!(
                "the digests of the new add-on disk cannot be kept beside {}: {e}",
                target.display()
            )
        })?;
        fs::rename(partial, target).map_err(|e| {
            format!(
                "the new add-on disk cannot be put at {}: {e}",
                target.display()
            )
        })
    }

    /// Takes over a disk an earlier DSA kept elsewhere, read from `source`, as this disk
    /// (`pending` false) or as the pending one. The copy is checked as every disk is read, so
    /// one that is not an add-on disk this host would write is refused, and nothing is put in
    /// place. An earlier DSA kept no digests, so the digests of the images as they are now are
    /// recorded. A disk already at the place is never replaced. Returns the add-ons it holds.
    pub fn take_over(&self, source: &mut dyn Read, pending: bool) -> Result<Vec<Addon>, String> {
        let _change = lock();
        let target = if pending {
            self.pending_path()
        } else {
            self.path.clone()
        };
        if target.exists() {
            return Err(format!("{} is there already", target.display()));
        }
        let partial = self.partial_path();
        if let Some(dir) = partial.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(dir)
                .map_err(|e| format!("{} cannot be made: {e}", dir.display()))?;
        }
        let copied = (|| -> Result<Vec<Addon>, String> {
            let mut file = File::create(&partial).map_err(io_text(&partial))?;
            io::copy(source, &mut file)
                .map_err(|e| format!("the earlier disk cannot be read: {e}"))?;
            file.sync_all().map_err(io_text(&partial))?;
            drop(file);
            let mut file = File::open(&partial).map_err(io_text(&partial))?;
            let length = file.metadata().map_err(io_text(&partial))?.len();
            let entries = read_entries_of(&mut file, length)
                .map_err(|e| format!("the earlier disk is refused: {e}"))?;
            let mut records = BTreeMap::new();
            for entry in &entries {
                let digest = digest_of(&mut file, entry)
                    .map_err(|e| format!("the earlier disk cannot be read: {e}"))?;
                records.insert(entry.addon, digest);
            }
            drop(file);
            self.place(&partial, &target, &records)?;
            Ok(entries.into_iter().map(|entry| entry.addon).collect())
        })();
        if copied.is_err() {
            let _ = fs::remove_file(&partial);
        }
        copied
    }

    /// Checks the images on the disk in place against their digests, once for each version of
    /// the file. A disk an earlier DSA wrote has no digests: those of its images as they are
    /// now are recorded. Returns the add-ons whose images are intact, and those that are not,
    /// with why.
    fn check(&self, entries: &[Entry]) -> (Vec<Addon>, Vec<(Addon, String)>) {
        let all = || entries.iter().map(|entry| entry.addon).collect::<Vec<_>>();
        let stamp = stamp_of(&self.path);
        if stamp.is_some()
            && CHECKED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&self.path)
                == stamp.as_ref()
        {
            return (all(), Vec::new());
        }
        let records = match read_records(&self.path) {
            Ok(records) => records,
            Err(e) => {
                let why = format!("its digest record cannot be read ({e})");
                return (
                    Vec::new(),
                    entries
                        .iter()
                        .map(|entry| (entry.addon, why.clone()))
                        .collect(),
                );
            }
        };
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) => {
                let why = format!("the disk cannot be read ({e})");
                return (
                    Vec::new(),
                    entries
                        .iter()
                        .map(|entry| (entry.addon, why.clone()))
                        .collect(),
                );
            }
        };
        let mut good = Vec::new();
        let mut bad = Vec::new();
        let mut found = BTreeMap::new();
        for entry in entries {
            match digest_of(&mut file, entry) {
                Ok(digest) => {
                    found.insert(entry.addon, digest);
                    match records.as_ref().map(|records| records.get(&entry.addon)) {
                        None => good.push(entry.addon),
                        Some(Some(wanted)) if *wanted == digest => good.push(entry.addon),
                        Some(Some(_)) => bad.push((
                            entry.addon,
                            "its SHA-256 differs from the one recorded when it was installed"
                                .to_string(),
                        )),
                        Some(None) => bad.push((
                            entry.addon,
                            "no digest was recorded for it when it was installed".to_string(),
                        )),
                    }
                }
                Err(e) => bad.push((entry.addon, format!("it cannot be read ({e})"))),
            }
        }
        drop(file);
        if records.is_none() {
            match write_records(&self.path, &found) {
                Ok(()) => eprintln!(
                    "add-ons: the add-on disk {} came from an earlier DSA without digests; the \
                     digests of its images are recorded now",
                    self.path.display()
                ),
                Err(e) => eprintln!(
                    "add-ons: the digests of the add-on disk {} cannot be recorded ({e}); they \
                     are checked again at the next start",
                    self.path.display()
                ),
            }
        }
        if bad.is_empty() {
            if let Some(stamp) = stamp {
                CHECKED
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(self.path.clone(), stamp);
            }
        }
        (good, bad)
    }

    /// Checks the images of the disk in place against their digests now, as the next start
    /// would, so that start need not read them. A service calls it when it starts, at low
    /// priority. Returns the add-ons whose images are intact.
    pub fn verify(&self) -> Vec<Addon> {
        let _change = lock();
        let entries = Self::entries_of(&self.path);
        self.check(&entries).0
    }

    /// Readies the disk for a start of the VM, which must not be running: the pending disk, if
    /// there is one, takes the place of the one before, and a disk that holds no add-on is
    /// removed. Returns the add-ons on the disk the VM is to attach; none means the VM gets no
    /// add-on disk. A disk that cannot be read is logged and not attached, so Android still
    /// starts.
    pub fn prepare_for_boot(&self) -> Vec<Addon> {
        let _change = lock();
        let pending = self.pending_path();
        if pending.is_file() {
            forget_checked(&self.path);
            // The digests first, as the pending disk was written: a disk is never in place
            // with the digests of another for longer than this step.
            let records = records_path(&pending);
            if records.is_file() {
                if let Err(e) = fs::rename(&records, records_path(&self.path)) {
                    eprintln!(
                        "add-ons: the digests of the new add-on disk {} cannot be put in place \
                         ({e})",
                        pending.display()
                    );
                }
            }
            if let Err(e) = fs::rename(&pending, &self.path) {
                eprintln!(
                    "add-ons: the new add-on disk {} cannot take the place of {} ({e}); Android \
                     starts with the add-ons it had",
                    pending.display(),
                    self.path.display()
                );
            }
        }
        let _ = fs::remove_file(self.partial_path());
        let entries = match read_entries(&self.path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                eprintln!(
                    "add-ons: the add-on disk {} cannot be read ({e}); Android starts without its \
                     add-ons",
                    self.path.display()
                );
                return Vec::new();
            }
        };
        if entries.is_empty() {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(records_path(&self.path));
            return Vec::new();
        }
        let (good, bad) = self.check(&entries);
        if bad.is_empty() {
            return good;
        }
        for (addon, why) in &bad {
            eprintln!(
                "add-ons: {} on the add-on disk {} is refused: {why}. Android starts without \
                 it; install it again to use it",
                addon.what(),
                self.path.display()
            );
        }
        // The disk again with the intact images alone, so Android starts with those.
        let records = read_records(&self.path).ok().flatten();
        let kept: Vec<Entry> = entries
            .into_iter()
            .filter(|entry| good.contains(&entry.addon))
            .collect();
        if kept.is_empty() {
            forget_checked(&self.path);
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(records_path(&self.path));
            return Vec::new();
        }
        let base = self.path.clone();
        let rewritten = self.write_disk_at(&base, &good, &mut |stream, which| {
            let entry = kept
                .iter()
                .find(|entry| entry.addon == which)
                .expect("every add-on written is kept");
            copy_kept(stream, entry, &base, records.as_ref())
        });
        match rewritten {
            Ok(()) => good,
            Err(why) => {
                eprintln!(
                    "add-ons: the add-on disk {} cannot be written again without the refused \
                     images ({why}); Android starts without its add-ons",
                    self.path.display()
                );
                Vec::new()
            }
        }
    }

    /// Whether the disk in place can be persistent memory in the host's direct-mapped mode: a
    /// whole number of 2 MiB, with a partition over each image it holds. A disk written by a
    /// DSA from before the mode has neither.
    pub fn is_mappable(&self) -> bool {
        let _change = lock();
        is_mappable(&self.path, self.format)
    }

    /// Writes the disk in place again in the layout the direct-mapped mode maps, with the same
    /// images, when it is not in it already. For a start of the VM, which must not be running,
    /// after [`prepare_for_boot`](Self::prepare_for_boot). A disk that cannot be written again
    /// is left as it was, and the error says why.
    pub fn bring_to_mappable_layout(&self) -> Result<(), String> {
        let _change = lock();
        if !self.path.is_file() || is_mappable(&self.path, self.format) {
            return Ok(());
        }
        let entries = Self::entries_of(&self.path);
        if entries.is_empty() {
            return Ok(());
        }
        let records = read_records(&self.path).ok().flatten();
        let base = self.path.clone();
        let order: Vec<Addon> = entries.iter().map(|entry| entry.addon).collect();
        self.write_disk_at(&base, &order, &mut |stream, which| {
            let entry = entries
                .iter()
                .find(|entry| entry.addon == which)
                .expect("every add-on written is on the disk");
            copy_kept(stream, entry, &base, records.as_ref())
        })
    }

    /// Removes the disk, the pending one and any half-written one, with their digests, as
    /// removing DSA does.
    pub fn delete(&self) {
        let _change = lock();
        for path in [self.path.clone(), self.pending_path(), self.partial_path()] {
            forget_checked(&path);
            let _ = fs::remove_file(records_path(&path));
            let _ = fs::remove_file(path);
        }
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    CHANGES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Writes one add-on's image into the erofs file system of a new disk, and gives its digest.
type Fill<'a> = dyn FnMut(&mut Stream<At<File>>, Addon) -> Result<Sha256, String> + 'a;

/// A file seen from `base` on: what the erofs writer writes at 0 lands at `base`.
struct At<W> {
    inner: W,
    base: u64,
}

impl<W: Write> Write for At<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.inner.write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for At<W> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let at = match to {
            SeekFrom::Start(at) => self.inner.seek(SeekFrom::Start(self.base + at))?,
            other => self.inner.seek(other)?,
        };
        at.checked_sub(self.base)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "before the partition"))
    }
}

fn io_text(what: &Path) -> impl Fn(io::Error) -> String + '_ {
    move |e| format!("{} cannot be written: {e}", what.display())
}

/// Writes a whole disk holding the images of `addons` to `path`, in `format`: `fill` writes
/// each image into the disk's erofs file system, in the order [`Addon::ALL`] gives them.
fn write_disk(
    path: &Path,
    format: Format,
    addons: &[Addon],
    fill: &mut Fill<'_>,
) -> Result<(), String> {
    let mut addons = addons.to_vec();
    addons.sort();
    addons.dedup();
    let file = File::create(path).map_err(io_text(path))?;
    let mut stream = Stream::new(At {
        inner: file,
        base: PARTITION_FIRST_SECTOR * SECTOR,
    })?;
    for addon in addons {
        fill(&mut stream, addon)?;
    }
    let mut out = stream.finish()?;
    let fs_bytes = out.stream_position().map_err(io_text(path))?;
    let mut file = out.inner;
    // 1 MiB before the partition and 1 MiB after it, so the disk is a whole number of sections
    // when the partition is.
    let partition_sectors = fs_bytes.div_ceil(SECTION) * SECTION / SECTOR;
    let sectors = PARTITION_FIRST_SECTOR + partition_sectors + MIB / SECTOR;
    let disk_bytes = sectors * SECTOR;
    file.set_len(disk_bytes).map_err(io_text(path))?;
    let write_table = |file: &mut File, images: &[Entry]| -> Result<(), String> {
        for (at, data) in partition_table(sectors, partition_sectors, images) {
            file.seek(SeekFrom::Start(at)).map_err(io_text(path))?;
            file.write_all(&data).map_err(io_text(path))?;
        }
        file.flush().map_err(io_text(path))
    };
    // Each image's own partition comes from where the file system put the image, which the
    // disk is read back for once its erofs partition is in the table.
    write_table(&mut file, &[])?;
    let images = read_entries(path).map_err(|e| {
        format!(
            "the new add-on disk {} cannot be read back: {e}",
            path.display()
        )
    })?;
    write_table(&mut file, &images)?;
    if format == Format::Vhd {
        file.seek(SeekFrom::Start(disk_bytes))
            .map_err(io_text(path))?;
        file.write_all(&vhd_footer(disk_bytes))
            .map_err(io_text(path))?;
    }
    file.sync_all().map_err(io_text(path))
}

/// Copies the image `entry` from the disk at `from` into `stream`. With `records`, the digests
/// recorded for that disk, the image must have the one recorded for it, so an image that
/// changed since it was installed is not carried onto a new disk; a disk an earlier DSA wrote
/// has no record.
fn copy_kept<W: Write + Seek>(
    stream: &mut Stream<W>,
    entry: &Entry,
    from: &Path,
    records: Option<&BTreeMap<Addon, Sha256>>,
) -> Result<Sha256, String> {
    let wanted = match records {
        Some(records) => Some(records.get(&entry.addon).ok_or_else(|| {
            format!(
                "{} on {} has no digest recorded; install it again",
                entry.addon.what(),
                from.display()
            )
        })?),
        None => None,
    };
    let mut source =
        File::open(from).map_err(|e| format!("{} cannot be read: {e}", from.display()))?;
    source
        .seek(SeekFrom::Start(entry.offset))
        .map_err(|e| format!("{} cannot be read: {e}", from.display()))?;
    copy_image(
        stream,
        entry.addon,
        entry.size,
        wanted.map(|digest| digest.as_slice()),
        &mut source.take(entry.size),
    )
}

/// Copies `addon`'s image, `size` bytes of `content`, into `stream` as its file, checking that
/// it is an erofs image and, with `sha256`, that it has that digest. Gives its digest.
fn copy_image<W: Write + Seek>(
    stream: &mut Stream<W>,
    addon: Addon,
    size: u64,
    sha256: Option<&[u8]>,
    content: &mut dyn Read,
) -> Result<Sha256, String> {
    let what = addon.what();
    if size < SUPERBLOCK + 4 {
        return Err(format!(
            "{what} is {size} bytes, too small to be an erofs image"
        ));
    }
    stream.begin_file(addon.file_name(), size)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; PIECE];
    let mut done = 0u64;
    while done < size {
        let length = (size - done).min(PIECE as u64) as usize;
        content
            .read_exact(&mut buffer[..length])
            .map_err(|e| format!("{what} did not arrive whole ({done} of {size} bytes): {e}"))?;
        let piece = &buffer[..length];
        if done == 0 && !crate::arm_translation::is_erofs(piece) {
            return Err(format!("{what} is not an erofs image"));
        }
        hasher.update(piece);
        stream.write_data(piece)?;
        done += length as u64;
    }
    let digest: Sha256 = hasher.finalize().into();
    if let Some(wanted) = sha256 {
        if digest.as_slice() != wanted {
            return Err(format!(
                "{what} did not arrive intact (its SHA-256 differs); install it again"
            ));
        }
    }
    stream.end_file()?;
    Ok(digest)
}

/// A GUID's 16 bytes as GPT and VHD store them: the first three fields little endian.
fn guid_bytes(text: &str) -> [u8; 16] {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let mut raw = [0u8; 16];
    for (i, byte) in raw.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("a constant GUID");
    }
    let mut out = raw;
    out[0..4].copy_from_slice(&[raw[3], raw[2], raw[1], raw[0]]);
    out[4..6].copy_from_slice(&[raw[5], raw[4]]);
    out[6..8].copy_from_slice(&[raw[7], raw[6]]);
    out
}

fn gpt_header(sectors: u64, current: u64, backup: u64, entries_lba: u64, crc: u32) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&current.to_le_bytes());
    h[32..40].copy_from_slice(&backup.to_le_bytes());
    h[40..48].copy_from_slice(&(2 + ENTRY_SECTORS).to_le_bytes());
    h[48..56].copy_from_slice(&(sectors - 2 - ENTRY_SECTORS).to_le_bytes());
    h[56..72].copy_from_slice(&guid_bytes(DISK_GUID));
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(ENTRY_BYTES as u32).to_le_bytes());
    h[88..92].copy_from_slice(&crc.to_le_bytes());
    let header_crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&header_crc.to_le_bytes());
    h
}

/// Writes one partition entry: its type, UUID, first and last sector, and name.
fn put_entry(entry: &mut [u8], uuid: &str, first: u64, last: u64, name: &str) {
    entry[0..16].copy_from_slice(&guid_bytes(LINUX_DATA_TYPE));
    entry[16..32].copy_from_slice(&guid_bytes(uuid));
    entry[32..40].copy_from_slice(&first.to_le_bytes());
    entry[40..48].copy_from_slice(&last.to_le_bytes());
    for (i, unit) in name.encode_utf16().enumerate() {
        entry[56 + i * 2..58 + i * 2].copy_from_slice(&unit.to_le_bytes());
    }
}

/// The partition table of a disk of `sectors` whose erofs partition has `partition_sectors`,
/// then a partition for each image in `images` over the image's bytes inside the erofs
/// partition, in whole blocks: each piece with the byte it goes at.
fn partition_table(sectors: u64, partition_sectors: u64, images: &[Entry]) -> Vec<(u64, Vec<u8>)> {
    let mut mbr = vec![0u8; 512];
    mbr[446 + 1..446 + 4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr[446 + 4] = 0xEE;
    mbr[446 + 5..446 + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    let size = u32::try_from(sectors - 1).unwrap_or(u32::MAX);
    mbr[446 + 12..446 + 16].copy_from_slice(&size.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    let mut entries = vec![0u8; (ENTRIES * ENTRY_BYTES) as usize];
    let last = PARTITION_FIRST_SECTOR + partition_sectors - 1;
    let mut slots = entries.chunks_mut(ENTRY_BYTES as usize);
    if let Some(slot) = slots.next() {
        put_entry(
            slot,
            PARTITION_UUID,
            PARTITION_FIRST_SECTOR,
            last,
            PARTITION_NAME,
        );
    }
    for (image, slot) in images.iter().zip(slots) {
        let end = (image.offset + image.size.div_ceil(BLOCK) * BLOCK) / SECTOR;
        put_entry(
            slot,
            image.addon.partition_uuid(),
            image.offset / SECTOR,
            end.min(last + 1) - 1,
            image.addon.partition_name(),
        );
    }
    let crc = crc32(&entries);
    let backup_entries = sectors - 1 - ENTRY_SECTORS;
    vec![
        (0, mbr),
        (SECTOR, gpt_header(sectors, 1, sectors - 1, 2, crc).to_vec()),
        (2 * SECTOR, entries.clone()),
        (backup_entries * SECTOR, entries),
        (
            (sectors - 1) * SECTOR,
            gpt_header(sectors, sectors - 1, 1, backup_entries, crc).to_vec(),
        ),
    ]
}

/// The VHD disk geometry for `sectors`, by the algorithm of the VHD format.
pub(crate) fn geometry(sectors: u64) -> (u16, u8, u8) {
    let total = sectors.min(65535 * 16 * 255);
    let (mut spt, mut heads, mut cth);
    if total >= 65535 * 16 * 63 {
        spt = 255;
        heads = 16;
        cth = total / spt;
    } else {
        spt = 17;
        cth = total / spt;
        heads = cth.div_ceil(1024).max(4);
        if cth >= heads * 1024 || heads > 16 {
            spt = 31;
            heads = 16;
            cth = total / spt;
        }
        if cth >= heads * 1024 {
            spt = 63;
            heads = 16;
            cth = total / spt;
        }
    }
    ((cth / heads) as u16, heads as u8, spt as u8)
}

/// The fixed VHD footer for a disk of `disk_bytes`.
fn vhd_footer(disk_bytes: u64) -> [u8; FOOTER_BYTES as usize] {
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
    let (cylinders, heads, spt) = geometry(disk_bytes / SECTOR);
    f[56..58].copy_from_slice(&cylinders.to_be_bytes());
    f[58] = heads;
    f[59] = spt;
    // Disk type 2: fixed.
    f[60..64].copy_from_slice(&2u32.to_be_bytes());
    f[68..84].copy_from_slice(&guid_bytes(DISK_GUID));
    let sum: u32 = f.iter().map(|&b| u32::from(b)).sum();
    f[64..68].copy_from_slice(&(!sum).to_be_bytes());
    f
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

fn read_at(file: &mut File, at: u64, length: usize) -> io::Result<Vec<u8>> {
    let mut data = vec![0u8; length];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut data)?;
    Ok(data)
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().expect("eight bytes"))
}

/// Whether the disk at `path`, kept in `format`, is in the layout the direct-mapped mode maps:
/// a whole number of 2 MiB, and each image on it covered from its first byte by a partition of
/// its own, named for it, in whole blocks.
fn is_mappable(path: &Path, format: Format) -> bool {
    let check = || -> io::Result<bool> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let footer = if format == Format::Vhd {
            FOOTER_BYTES
        } else {
            0
        };
        if length < footer || (length - footer) % SECTION != 0 {
            return Ok(false);
        }
        let header = read_at(&mut file, SECTOR, 92)?;
        let entries_lba = u64_at(&header, 72);
        let count = u64::from(u32_at(&header, 80)).min(ENTRIES * 4);
        let entry_bytes = u64::from(u32_at(&header, 84));
        if entry_bytes < 128 {
            return Ok(false);
        }
        let table = read_at(
            &mut file,
            entries_lba * SECTOR,
            (count * entry_bytes) as usize,
        )?;
        let images = read_entries(path)?;
        Ok(!images.is_empty()
            && images.iter().all(|image| {
                table.chunks(entry_bytes as usize).any(|entry| {
                    let first = u64_at(entry, 32);
                    let last = u64_at(entry, 40);
                    entry_named(&entry[56..128], image.addon.partition_name())
                        && first.checked_mul(SECTOR) == Some(image.offset)
                        && last
                            .checked_sub(first)
                            .and_then(|span| span.checked_add(1))
                            .and_then(|sectors| sectors.checked_mul(SECTOR))
                            == Some(image.size.div_ceil(BLOCK) * BLOCK)
                })
            }))
    };
    check().unwrap_or(false)
}

/// The most partition entries a disk may list. This host writes [`ENTRIES`]; a table longer
/// than this is not one it made.
const MAX_ENTRIES: u64 = 512;
/// The largest partition entry accepted. The entry size must be a power of two from 128 bytes
/// on, and every partitioning tool writes 128.
const MAX_ENTRY_BYTES: u64 = 512;
/// The largest root folder accepted. The disk's root holds one name per add-on.
const MAX_ROOT_FOLDER: u64 = 64 * BLOCK;

fn too_big(what: &str) -> io::Error {
    invalid(format!("its {what} lies outside any size a disk can have"))
}

fn entry_named(raw: &[u8], expected: &str) -> bool {
    let wanted: Vec<u8> = expected.encode_utf16().flat_map(u16::to_le_bytes).collect();
    raw.starts_with(&wanted) && raw[wanted.len()..].iter().all(|&b| b == 0)
}

/// The add-on images on the disk at `path`, read the way the guest finds them: the partition
/// by its name in the partition table, then the files at the root of its erofs file system.
///
/// Every number is read from a file the service did not make itself at this moment, so each
/// is checked before it sizes a buffer or a read: a disk that does not fit is refused with a
/// reason, and the service goes on. Every partition in the table must be the add-on partition
/// or one of the add-on image partitions, since Android makes a by-name link for each
/// partition of every disk, and another partition could take the name of one of Android's own.
fn read_entries(path: &Path) -> io::Result<Vec<Entry>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    read_entries_of(&mut file, length)
}

fn read_entries_of(file: &mut File, length: u64) -> io::Result<Vec<Entry>> {
    let header = read_at(file, SECTOR, 92)?;
    if &header[0..8] != b"EFI PART" {
        return Err(invalid("it has no GPT partition table"));
    }
    let entries_lba = u64_at(&header, 72);
    let count = u64::from(u32_at(&header, 80));
    let entry_bytes = u64::from(u32_at(&header, 84));
    if !(128..=MAX_ENTRY_BYTES).contains(&entry_bytes) || !entry_bytes.is_power_of_two() {
        return Err(invalid(format!(
            "its partition entries are {entry_bytes} bytes each, where 128 is the size a \
             partition table has"
        )));
    }
    if count == 0 || count > MAX_ENTRIES {
        return Err(invalid(format!(
            "its partition table lists {count} entries, more than this host reads ({MAX_ENTRIES})"
        )));
    }
    let table_bytes = count
        .checked_mul(entry_bytes)
        .ok_or_else(|| too_big("partition table"))?;
    let table_at = entries_lba
        .checked_mul(SECTOR)
        .ok_or_else(|| too_big("partition table"))?;
    if entries_lba < 2
        || table_at
            .checked_add(table_bytes)
            .is_none_or(|end| end > length)
    {
        return Err(invalid("its partition table does not fit the disk"));
    }
    let table = read_at(file, table_at, table_bytes as usize)?;
    let mut partition = None;
    let mut image_partitions: Vec<(Addon, u64, u64)> = Vec::new();
    for entry in table.chunks(entry_bytes as usize) {
        // An entry whose type is all zeros is unused.
        if entry[0..16].iter().all(|&b| b == 0) {
            continue;
        }
        let name = &entry[56..128];
        if entry_named(name, PARTITION_NAME) {
            if partition.is_some() {
                return Err(invalid(format!(
                    "it has more than one {PARTITION_NAME} partition"
                )));
            }
            partition = Some(entry);
            continue;
        }
        let Some(addon) = Addon::ALL
            .into_iter()
            .find(|addon| entry_named(name, addon.partition_name()))
        else {
            return Err(invalid(format!(
                "it has a partition named {:?}, and an add-on disk holds only its \
                 {PARTITION_NAME} partition",
                partition_name(name)
            )));
        };
        if image_partitions.iter().any(|(seen, _, _)| *seen == addon) {
            return Err(invalid(format!(
                "it has more than one {} partition",
                addon.partition_name()
            )));
        }
        let first = u64_at(entry, 32)
            .checked_mul(SECTOR)
            .ok_or_else(|| too_big("partition"))?;
        let stop = u64_at(entry, 40)
            .checked_add(1)
            .and_then(|last| last.checked_mul(SECTOR))
            .ok_or_else(|| too_big("partition"))?;
        image_partitions.push((addon, first, stop));
    }
    let Some(partition) = partition else {
        return Err(invalid(format!("it has no {PARTITION_NAME} partition")));
    };
    let base = u64_at(partition, 32)
        .checked_mul(SECTOR)
        .ok_or_else(|| too_big("partition"))?;
    let end = u64_at(partition, 40)
        .checked_add(1)
        .and_then(|last| last.checked_mul(SECTOR))
        .ok_or_else(|| too_big("partition"))?;
    if end > length || base >= end || end - base < SUPERBLOCK + 128 {
        return Err(invalid("its partition does not fit the disk"));
    }
    for (_, first, stop) in &image_partitions {
        if *first < base || *stop > end || *first >= *stop {
            return Err(invalid("an add-on image partition does not fit the disk"));
        }
    }
    let superblock = read_at(file, base + SUPERBLOCK, 128)?;
    if u32_at(&superblock, 0) != EROFS_MAGIC || superblock[12] != 12 {
        return Err(invalid(
            "its partition is not an erofs file system of 4 KiB blocks",
        ));
    }
    let root = u64::from(u16_at(&superblock, 14));
    let meta = u64::from(u32_at(&superblock, 40)) * BLOCK;
    let inode = |file: &mut File, nid: u64| -> io::Result<(u16, u64, u64)> {
        let at = nid
            .checked_mul(INODE)
            .and_then(|offset| offset.checked_add(meta))
            .and_then(|offset| offset.checked_add(base))
            .filter(|at| at.checked_add(INODE).is_some_and(|stop| stop <= end))
            .ok_or_else(|| invalid("an inode lies outside its partition"))?;
        let raw = read_at(file, at, INODE as usize)?;
        let format = u16_at(&raw, 0);
        // A compact inode (bit 0 clear) whose data is stored whole (layout 0).
        if format & 1 != 0 || (format >> 1) & 7 != 0 {
            return Err(invalid(
                "its file system holds an inode this host does not write",
            ));
        }
        let mode = u16_at(&raw, 4);
        let size = u64::from(u32_at(&raw, 8));
        let start = base + u64::from(u32_at(&raw, 16)) * BLOCK;
        if start.checked_add(size).is_none_or(|stop| stop > end) {
            return Err(invalid("a file runs past the end of its partition"));
        }
        Ok((mode, size, start))
    };
    let (mode, size, start) = inode(file, root)?;
    if mode & 0o170_000 != 0o040_000 {
        return Err(invalid("its file system's root is not a folder"));
    }
    if size > MAX_ROOT_FOLDER {
        return Err(invalid(format!(
            "its root folder is {size} bytes, more than an add-on disk's root holds"
        )));
    }
    let folder = read_at(file, start, size as usize)?;
    let mut found = Vec::new();
    for block in folder.chunks(BLOCK as usize) {
        if block.len() < 12 {
            break;
        }
        let count = usize::from(u16_at(block, 8)) / 12;
        for k in 0..count {
            let at = k * 12;
            if at + 12 > block.len() {
                return Err(invalid("its root folder is damaged"));
            }
            let nid = u64_at(block, at);
            let name_start = usize::from(u16_at(block, at + 8));
            let name_end = if k + 1 < count {
                usize::from(u16_at(block, at + 12 + 8))
            } else {
                let tail = block.get(name_start..).unwrap_or(&[]);
                name_start + tail.iter().position(|&b| b == 0).unwrap_or(tail.len())
            };
            let Some(name) = block.get(name_start..name_end) else {
                return Err(invalid("its root folder is damaged"));
            };
            let Some(addon) = Addon::of_file_name(name) else {
                continue;
            };
            let (mode, size, offset) = inode(file, nid)?;
            if mode & 0o170_000 == 0o100_000 {
                if found.iter().any(|entry: &Entry| entry.addon == addon) {
                    return Err(invalid(format!(
                        "its root folder holds {} twice",
                        addon.file_name()
                    )));
                }
                found.push(Entry {
                    addon,
                    offset,
                    size,
                });
            }
        }
    }
    found.sort_by_key(|entry| entry.addon);
    Ok(found)
}

/// A partition's name from its entry, for a reason given to a person.
fn partition_name(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dsa-addon-disk-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An erofs image holding one file, `name`, with `data`.
    fn image(name: &str, data: &[u8]) -> Vec<u8> {
        let mut tree = crate::erofs::Builder::new();
        tree.file(name, 0o644, data.to_vec()).unwrap();
        let mut out = Vec::new();
        tree.write(&mut out).unwrap();
        out
    }

    fn install(disk: &AddonDisk, addon: Addon, image: &[u8]) -> Result<(), String> {
        disk.install(addon, image.len() as u64, None, &mut &image[..])
    }

    /// The image of `addon` on the disk at `path`, read back from the disk's own layout.
    fn read_back(path: &Path, addon: Addon) -> Option<Vec<u8>> {
        let entry = read_entries(path)
            .unwrap()
            .into_iter()
            .find(|entry| entry.addon == addon)?;
        let mut file = File::open(path).unwrap();
        Some(read_at(&mut file, entry.offset, entry.size as usize).unwrap())
    }

    #[test]
    fn fr32_the_addon_disk_holds_each_image_whole_in_an_erofs_partition_of_its_own() {
        let dir = scratch("layout");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        let gapps = image("product/app/a.apk", &[7u8; 10_000]);
        let translator = image("system/lib64/libdsa_native_bridge.so", b"bridge");
        install(&disk, Addon::Gapps, &gapps).unwrap();
        install(&disk, Addon::ArmTranslation, &translator).unwrap();
        assert!(
            !disk.path().exists(),
            "nothing is attached before the next start"
        );
        assert_eq!(disk.contents(), [Addon::Gapps, Addon::ArmTranslation]);
        assert_eq!(
            disk.prepare_for_boot(),
            [Addon::Gapps, Addon::ArmTranslation]
        );
        assert!(!disk.pending_path().exists());

        let bytes = fs::read(disk.path()).unwrap();
        assert_eq!(bytes.len() as u64 % MIB, 0, "a whole number of MiB");
        // The partition, by its name, from 1 MiB: an erofs file system holding each image as a
        // file at its root, byte for byte, as the guest mounts it.
        let partition = &bytes[MIB as usize..bytes.len() - MIB as usize];
        let files = crate::erofs::read::files(&partition[..erofs_length(partition)]);
        assert_eq!(files["gapps.img"], gapps);
        assert_eq!(files["arm-translation.img"], translator);
        assert_eq!(files.len(), 2);
        // Each image starts on a block of its own, so erofs reads it with direct I/O.
        for entry in read_entries(disk.path()).unwrap() {
            assert_eq!((entry.offset - MIB) % BLOCK, 0, "{:?}", entry.addon);
        }
        // The GPT names the partition, and both of its headers are sound.
        let header = &bytes[512..604];
        assert_eq!(&header[0..8], b"EFI PART");
        let mut zeroed = header.to_vec();
        zeroed[16..20].fill(0);
        assert_eq!(crc32(&zeroed), u32_at(header, 16));
        let backup = &bytes[bytes.len() - 512..bytes.len() - 420];
        assert_eq!(&backup[0..8], b"EFI PART");
        assert_eq!(u64_at(backup, 24), bytes.len() as u64 / 512 - 1);
        let name: Vec<u16> = bytes[1024 + 56..1024 + 128]
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        assert_eq!(String::from_utf16(&name).unwrap(), PARTITION_NAME);
        assert_eq!(bytes[510..512], [0x55, 0xAA]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// How long the erofs file system at the start of `partition` is, by its superblock.
    fn erofs_length(partition: &[u8]) -> usize {
        u32_at(partition, SUPERBLOCK as usize + 36) as usize * BLOCK as usize
    }

    #[test]
    fn fr32_a_change_takes_effect_at_the_next_start_and_keeps_the_other_addons() {
        let dir = scratch("change");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        let gapps = image("a", b"google");
        let translator = image("b", b"translator");
        install(&disk, Addon::Gapps, &gapps).unwrap();
        install(&disk, Addon::ArmTranslation, &translator).unwrap();
        disk.prepare_for_boot();
        assert_eq!(disk.attached(), [Addon::Gapps, Addon::ArmTranslation]);

        // While the VM runs, a change goes to the pending disk only.
        let newer = image("a", b"google, newer");
        install(&disk, Addon::Gapps, &newer).unwrap();
        assert!(disk.remove(Addon::ArmTranslation).unwrap());
        assert!(!disk.remove(Addon::ArmTranslation).unwrap(), "already off");
        assert_eq!(read_back(disk.path(), Addon::Gapps).unwrap(), gapps);
        assert_eq!(disk.attached(), [Addon::Gapps, Addon::ArmTranslation]);
        assert_eq!(disk.contents(), [Addon::Gapps]);
        assert!(disk.holds(Addon::Gapps));
        assert!(!disk.holds(Addon::ArmTranslation));

        assert_eq!(disk.prepare_for_boot(), [Addon::Gapps]);
        assert_eq!(read_back(disk.path(), Addon::Gapps).unwrap(), newer);
        assert_eq!(read_back(disk.path(), Addon::ArmTranslation), None);

        // The last add-on off: the next start has no disk at all.
        assert!(disk.remove(Addon::Gapps).unwrap());
        assert_eq!(disk.attached(), [Addon::Gapps]);
        assert_eq!(disk.prepare_for_boot(), Vec::<Addon>::new());
        assert!(!disk.path().exists());
        assert!(!disk.pending_path().exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_an_image_that_is_not_whole_erofs_or_intact_leaves_the_disk_as_it_was() {
        let dir = scratch("refuse");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        let gapps = image("a", b"google");
        install(&disk, Addon::Gapps, &gapps).unwrap();
        let translator = image("b", b"translator");

        let short = disk
            .install(
                Addon::ArmTranslation,
                translator.len() as u64 + 10,
                None,
                &mut &translator[..],
            )
            .unwrap_err();
        assert!(short.contains("did not arrive whole"), "{short}");
        let not_erofs = vec![1u8; 8192];
        let why = install(&disk, Addon::ArmTranslation, &not_erofs).unwrap_err();
        assert!(why.contains("not an erofs image"), "{why}");
        let digest = sha2::Sha256::digest(b"something else");
        let why = disk
            .install(
                Addon::ArmTranslation,
                translator.len() as u64,
                Some(digest.as_slice()),
                &mut &translator[..],
            )
            .unwrap_err();
        assert!(why.contains("SHA-256"), "{why}");
        let digest = sha2::Sha256::digest(&translator);
        disk.install(
            Addon::ArmTranslation,
            translator.len() as u64,
            Some(digest.as_slice()),
            &mut &translator[..],
        )
        .unwrap();
        assert!(!disk.partial_path().exists());
        assert_eq!(disk.contents(), [Addon::Gapps, Addon::ArmTranslation]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_a_disk_that_cannot_be_read_is_not_attached() {
        let dir = scratch("broken");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        assert_eq!(disk.prepare_for_boot(), Vec::<Addon>::new(), "no disk yet");
        fs::write(disk.path(), vec![0u8; 4 << 20]).unwrap();
        assert_eq!(disk.prepare_for_boot(), Vec::<Addon>::new());
        assert!(disk.contents().is_empty());
        // Installing over it starts a new disk.
        install(&disk, Addon::Gapps, &image("a", b"google")).unwrap();
        assert_eq!(disk.prepare_for_boot(), [Addon::Gapps]);
        disk.delete();
        assert!(!disk.path().exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_a_hyper_v_disk_is_the_same_disk_with_a_fixed_vhd_footer() {
        let dir = scratch("vhd");
        let raw = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        let vhd = AddonDisk::new(dir.join("addons.vhd"), Format::Vhd);
        let gapps = image("a", b"google");
        install(&raw, Addon::Gapps, &gapps).unwrap();
        install(&vhd, Addon::Gapps, &gapps).unwrap();
        raw.prepare_for_boot();
        assert_eq!(vhd.prepare_for_boot(), [Addon::Gapps]);
        let raw = fs::read(raw.path()).unwrap();
        let vhd = fs::read(vhd.path()).unwrap();
        assert_eq!(vhd.len(), raw.len() + 512);
        assert_eq!(vhd[..raw.len()], raw[..]);
        let footer = &vhd[raw.len()..];
        assert_eq!(&footer[0..8], b"conectix");
        assert_eq!(
            u64::from_be_bytes(footer[48..56].try_into().unwrap()),
            raw.len() as u64
        );
        assert_eq!(
            u32::from_be_bytes(footer[60..64].try_into().unwrap()),
            2,
            "fixed"
        );
        let mut zeroed = footer.to_vec();
        zeroed[64..68].fill(0);
        let sum: u32 = zeroed.iter().map(|&b| u32::from(b)).sum();
        assert_eq!(u32::from_be_bytes(footer[64..68].try_into().unwrap()), !sum);
        assert_eq!(
            read_back(&dir.join("addons.vhd"), Addon::Gapps).unwrap(),
            gapps
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The partition entry `index` of the disk `bytes`: its first and last sector and name.
    fn partition(bytes: &[u8], index: usize) -> (u64, u64, String) {
        let entry = &bytes[1024 + index * 128..1024 + (index + 1) * 128];
        let name: Vec<u16> = entry[56..128]
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        (
            u64_at(entry, 32),
            u64_at(entry, 40),
            String::from_utf16(&name).unwrap(),
        )
    }

    #[test]
    fn nfr1_each_image_has_a_partition_of_its_own_for_the_direct_mapped_mode() {
        // In the host's direct-mapped mode the disk is persistent memory, and the guest mounts
        // each image from a partition over its own bytes with DAX, which a file-backed mount
        // cannot have.
        let dir = scratch("mapped");
        let disk = AddonDisk::new(dir.join("addons.vhd"), Format::Vhd);
        let gapps = image("product/app/a.apk", &[7u8; 10_000]);
        let translator = image("system/lib64/libdsa_native_bridge.so", b"bridge");
        install(&disk, Addon::Gapps, &gapps).unwrap();
        install(&disk, Addon::ArmTranslation, &translator).unwrap();
        disk.prepare_for_boot();
        let bytes = fs::read(disk.path()).unwrap();
        let disk_bytes = bytes.len() as u64 - FOOTER_BYTES;
        assert_eq!(disk_bytes % SECTION, 0, "a whole number of 2 MiB sections");
        let (first, last, name) = partition(&bytes, 0);
        assert_eq!(name, PARTITION_NAME);
        assert_eq!(first, PARTITION_FIRST_SECTOR);
        for (index, (addon, data)) in [(Addon::Gapps, &gapps), (Addon::ArmTranslation, &translator)]
            .into_iter()
            .enumerate()
        {
            let (start, end, name) = partition(&bytes, index + 1);
            assert_eq!(name, addon.partition_name());
            // Inside the erofs partition, on a page, a whole number of pages, and holding the
            // image byte for byte from its first sector.
            assert!(start >= first && end <= last, "{name}");
            assert_eq!(start * SECTOR % BLOCK, 0, "{name}");
            assert_eq!((end + 1 - start) * SECTOR % BLOCK, 0, "{name}");
            assert_eq!(
                (end + 1 - start) * SECTOR,
                (data.len() as u64).div_ceil(BLOCK) * BLOCK,
                "{name}"
            );
            let at = (start * SECTOR) as usize;
            assert_eq!(&bytes[at..at + data.len()], &data[..], "{name}");
        }
        assert_eq!(partition(&bytes, 3).2, "", "nothing else");
        // Both partition tables carry them, under one checksum.
        let header = &bytes[512..604];
        let crc = u32_at(header, 88);
        assert_eq!(crc32(&bytes[1024..1024 + 128 * 128]), crc);
        let backup_entries = (disk_bytes / SECTOR - 1 - ENTRY_SECTORS) * SECTOR;
        assert_eq!(
            bytes[backup_entries as usize..backup_entries as usize + 128 * 128],
            bytes[1024..1024 + 128 * 128]
        );
        // The disk still reads as before: the images by name in the erofs partition.
        assert_eq!(disk.attached(), [Addon::Gapps, Addon::ArmTranslation]);
        assert_eq!(read_back(disk.path(), Addon::Gapps).unwrap(), gapps);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Changes the disk file at `path` with `change`, and moves its time of change on, as any
    /// later write would.
    fn alter(path: &Path, change: impl FnOnce(&mut Vec<u8>)) {
        let mut bytes = fs::read(path).unwrap();
        change(&mut bytes);
        fs::write(path, &bytes).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }

    #[test]
    fn fr32_a_gpt_with_a_huge_entry_size_or_count_is_refused_without_aborting() {
        let dir = scratch("huge-gpt");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        install(&disk, Addon::Gapps, &image("a", b"google")).unwrap();
        assert_eq!(disk.prepare_for_boot(), [Addon::Gapps]);
        for (at, value) in [
            (84usize, u32::MAX),
            (84, 0x8000_0000),
            (84, 1024),
            (84, 192),
            (80, u32::MAX),
            (80, 0),
            (72, u32::MAX),
        ] {
            let path = disk.path().to_path_buf();
            let before = fs::read(&path).unwrap();
            alter(&path, |bytes| {
                let field = SECTOR as usize + at;
                if at == 72 {
                    bytes[field..field + 8].copy_from_slice(&u64::MAX.to_le_bytes());
                } else {
                    bytes[field..field + 4].copy_from_slice(&value.to_le_bytes());
                }
            });
            let why = read_entries(&path).unwrap_err();
            assert_eq!(why.kind(), io::ErrorKind::InvalidData, "{at}: {why}");
            assert_eq!(disk.prepare_for_boot(), Vec::<Addon>::new(), "{at}");
            fs::write(&path, before).unwrap();
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_a_partition_named_other_than_the_addon_partition_is_refused() {
        let dir = scratch("other-partition");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        install(&disk, Addon::Gapps, &image("a", b"google")).unwrap();
        assert_eq!(disk.prepare_for_boot(), [Addon::Gapps]);
        alter(disk.path(), |bytes| {
            // An extra entry in the table, after the add-on and image partitions.
            let entry = 2 * SECTOR as usize + 2 * ENTRY_BYTES as usize;
            bytes[entry..entry + 16].copy_from_slice(&guid_bytes(LINUX_DATA_TYPE));
            bytes[entry + 32..entry + 40].copy_from_slice(&PARTITION_FIRST_SECTOR.to_le_bytes());
            bytes[entry + 40..entry + 48]
                .copy_from_slice(&(PARTITION_FIRST_SECTOR + 7).to_le_bytes());
            for (i, unit) in "vbmeta_a".encode_utf16().enumerate() {
                bytes[entry + 56 + i * 2..entry + 58 + i * 2].copy_from_slice(&unit.to_le_bytes());
            }
        });
        let why = read_entries(disk.path()).unwrap_err().to_string();
        assert!(why.contains("vbmeta_a"), "{why}");
        assert_eq!(disk.prepare_for_boot(), Vec::<Addon>::new());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_a_tampered_image_is_refused_and_android_starts_without_it() {
        let dir = scratch("tampered");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        let gapps = image("a", &[3u8; 20_000]);
        let translator = image("b", b"translator");
        install(&disk, Addon::Gapps, &gapps).unwrap();
        install(&disk, Addon::ArmTranslation, &translator).unwrap();
        assert_eq!(
            disk.prepare_for_boot(),
            [Addon::Gapps, Addon::ArmTranslation]
        );
        assert!(records_path(disk.path()).is_file(), "digests recorded");
        let gapps_at = read_entries(disk.path())
            .unwrap()
            .into_iter()
            .find(|entry| entry.addon == Addon::Gapps)
            .unwrap();
        alter(disk.path(), |bytes| {
            let at = (gapps_at.offset + gapps_at.size - 1) as usize;
            bytes[at] ^= 0xFF;
        });
        assert_eq!(disk.prepare_for_boot(), [Addon::ArmTranslation]);
        // The disk now holds the intact image alone, as it was installed.
        assert_eq!(read_back(disk.path(), Addon::Gapps), None);
        assert_eq!(
            read_back(disk.path(), Addon::ArmTranslation).unwrap(),
            translator
        );
        assert_eq!(disk.prepare_for_boot(), [Addon::ArmTranslation]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr1_a_disk_from_before_the_direct_mapped_mode_is_written_again_in_its_layout() {
        let dir = scratch("older");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        let gapps = image("product/app/a.apk", &[9u8; 30_000]);
        install(&disk, Addon::Gapps, &gapps).unwrap();
        disk.prepare_for_boot();
        assert!(disk.is_mappable());
        // As an older DSA wrote it: no partition over the image.
        let mut bytes = fs::read(disk.path()).unwrap();
        let sectors = bytes.len() as u64 / SECTOR;
        let (first, last, _) = partition(&bytes, 0);
        for (at, data) in partition_table(sectors, last + 1 - first, &[]) {
            bytes[at as usize..at as usize + data.len()].copy_from_slice(&data);
        }
        fs::write(disk.path(), &bytes).unwrap();
        assert!(!disk.is_mappable());
        assert_eq!(disk.attached(), [Addon::Gapps], "it still reads as before");
        disk.bring_to_mappable_layout().unwrap();
        assert!(disk.is_mappable());
        assert_eq!(disk.attached(), [Addon::Gapps]);
        assert_eq!(read_back(disk.path(), Addon::Gapps).unwrap(), gapps);
        assert!(!disk.partial_path().exists());
        // A disk in the layout already is left alone, and no disk at all is nothing to do.
        let before = fs::metadata(disk.path()).unwrap().modified().unwrap();
        disk.bring_to_mappable_layout().unwrap();
        assert_eq!(
            fs::metadata(disk.path()).unwrap().modified().unwrap(),
            before
        );
        disk.delete();
        disk.bring_to_mappable_layout().unwrap();
        assert!(!disk.is_mappable());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_an_image_changed_after_install_is_not_carried_onto_a_new_disk() {
        let dir = scratch("tampered-kept");
        let disk = AddonDisk::new(dir.join("addons.img"), Format::Raw);
        install(&disk, Addon::Gapps, &image("a", &[5u8; 9_000])).unwrap();
        let entry = read_entries(&disk.pending_path()).unwrap()[0];
        alter(&disk.pending_path(), |bytes| {
            bytes[(entry.offset + entry.size - 1) as usize] ^= 1;
        });
        let why = install(&disk, Addon::ArmTranslation, &image("b", b"t")).unwrap_err();
        assert!(why.contains("SHA-256"), "{why}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nfr1_a_direct_mapped_boot_mounts_each_image_from_its_partition() {
        let normal = format!(
            "{BOOTCONFIG_ON}androidboot.dsa.gapps = \"on\"\n{}{}",
            image_bootconfig("gapps_image", true),
            image_bootconfig("arm_translation_image", false)
        );
        assert_eq!(
            direct_mapped_bootconfig(&normal),
            "androidboot.dsa.addon_disk = \"on\"\nandroidboot.dsa.gapps = \"on\"\n\
             androidboot.dsa.gapps_image = \"dax\"\n\
             androidboot.dsa.arm_translation_image = \"data\"\n\
             androidboot.dsa.direct_mapped = \"on\"\n"
        );
        assert_eq!(direct_mapped_bootconfig(""), DIRECT_MAPPED_BOOTCONFIG);
    }

    #[test]
    fn fr32_a_disk_taken_over_from_an_earlier_place_is_checked_and_its_digests_recorded() {
        let dir = scratch("take-over");
        let old = AddonDisk::new(dir.join("old").join("addons.img"), Format::Raw);
        let gapps = image("a", b"google");
        install(&old, Addon::Gapps, &gapps).unwrap();
        old.prepare_for_boot();
        let disk = AddonDisk::new(dir.join("new").join("addons.img"), Format::Raw);
        let mut source = File::open(old.path()).unwrap();
        assert_eq!(disk.take_over(&mut source, false).unwrap(), [Addon::Gapps]);
        assert!(records_path(disk.path()).is_file());
        assert_eq!(disk.prepare_for_boot(), [Addon::Gapps]);
        assert_eq!(read_back(disk.path(), Addon::Gapps).unwrap(), gapps);
        // Never over a disk that is there, and never a file that is not an add-on disk.
        let mut again = File::open(old.path()).unwrap();
        assert!(disk.take_over(&mut again, false).is_err());
        let junk = vec![0u8; 4 << 20];
        let why = disk.take_over(&mut &junk[..], true).unwrap_err();
        assert!(why.contains("refused"), "{why}");
        assert!(!disk.pending_path().exists());
        assert!(!disk.partial_path().exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_the_bootconfig_says_whether_the_disk_is_there_and_where_each_image_is() {
        assert_eq!(bootconfig(&[]), "");
        assert_eq!(
            bootconfig(&[Addon::Gapps]),
            "androidboot.dsa.addon_disk = \"on\"\n"
        );
        assert_eq!(
            image_bootconfig("gapps_image", true),
            "androidboot.dsa.gapps_image = \"disk\"\n"
        );
        assert_eq!(
            image_bootconfig("arm_translation_image", false),
            "androidboot.dsa.arm_translation_image = \"data\"\n"
        );
    }
}
