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

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
            Some(entry) => copy_kept(stream, entry, &base),
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
            copy_kept(stream, entry, &base)
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
        let partial = self.partial_path();
        if let Some(dir) = partial.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(dir)
                .map_err(|e| format!("{} cannot be made: {e}", dir.display()))?;
        }
        let written = write_disk(&partial, self.format, addons, fill);
        let placed = written.and_then(|()| {
            fs::rename(&partial, self.pending_path()).map_err(|e| {
                format!(
                    "the new add-on disk cannot be put at {}: {e}",
                    self.pending_path().display()
                )
            })
        });
        if placed.is_err() {
            let _ = fs::remove_file(&partial);
        }
        placed
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
        }
        entries.into_iter().map(|entry| entry.addon).collect()
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
        let order: Vec<Addon> = entries.iter().map(|entry| entry.addon).collect();
        let partial = self.partial_path();
        let written = write_disk(&partial, self.format, &order, &mut |stream, which| {
            let entry = entries
                .iter()
                .find(|entry| entry.addon == which)
                .expect("every add-on written is on the disk");
            copy_kept(stream, entry, &self.path)
        })
        .and_then(|()| {
            fs::rename(&partial, &self.path).map_err(|e| {
                format!(
                    "the add-on disk {} cannot be replaced: {e}",
                    self.path.display()
                )
            })
        });
        if written.is_err() {
            let _ = fs::remove_file(&partial);
        }
        written
    }

    /// Removes the disk, the pending one and any half-written one, as removing DSA does.
    pub fn delete(&self) {
        let _change = lock();
        for path in [self.path.clone(), self.pending_path(), self.partial_path()] {
            let _ = fs::remove_file(path);
        }
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    CHANGES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Writes one add-on's image into the erofs file system of a new disk.
type Fill<'a> = dyn FnMut(&mut Stream<At<File>>, Addon) -> Result<(), String> + 'a;

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

/// Copies the image `entry` from the disk at `from` into `stream`.
fn copy_kept<W: Write + Seek>(
    stream: &mut Stream<W>,
    entry: &Entry,
    from: &Path,
) -> Result<(), String> {
    let mut source =
        File::open(from).map_err(|e| format!("{} cannot be read: {e}", from.display()))?;
    source
        .seek(SeekFrom::Start(entry.offset))
        .map_err(|e| format!("{} cannot be read: {e}", from.display()))?;
    copy_image(
        stream,
        entry.addon,
        entry.size,
        None,
        &mut source.take(entry.size),
    )
}

/// Copies `addon`'s image, `size` bytes of `content`, into `stream` as its file, checking that
/// it is an erofs image and, with `sha256`, that it has that digest.
fn copy_image<W: Write + Seek>(
    stream: &mut Stream<W>,
    addon: Addon,
    size: u64,
    sha256: Option<&[u8]>,
    content: &mut dyn Read,
) -> Result<(), String> {
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
    if let Some(wanted) = sha256 {
        if hasher.finalize().as_slice() != wanted {
            return Err(format!(
                "{what} did not arrive intact (its SHA-256 differs); install it again"
            ));
        }
    }
    stream.end_file()
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
fn geometry(sectors: u64) -> (u16, u8, u8) {
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
                let wanted: Vec<u8> = image
                    .addon
                    .partition_name()
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect();
                table.chunks(entry_bytes as usize).any(|entry| {
                    let name = &entry[56..128];
                    let first = u64_at(entry, 32);
                    let last = u64_at(entry, 40);
                    name.starts_with(&wanted)
                        && name[wanted.len()..].iter().all(|&b| b == 0)
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

/// The add-on images on the disk at `path`, read the way the guest finds them: the partition
/// by its name in the partition table, then the files at the root of its erofs file system.
fn read_entries(path: &Path) -> io::Result<Vec<Entry>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let header = read_at(&mut file, SECTOR, 92)?;
    if &header[0..8] != b"EFI PART" {
        return Err(invalid("it has no GPT partition table"));
    }
    let entries_lba = u64_at(&header, 72);
    let count = u64::from(u32_at(&header, 80)).min(ENTRIES * 4);
    let entry_bytes = u64::from(u32_at(&header, 84));
    if entry_bytes < 128 {
        return Err(invalid("its partition entries are too small"));
    }
    let table = read_at(
        &mut file,
        entries_lba * SECTOR,
        (count * entry_bytes) as usize,
    )?;
    let wanted: Vec<u8> = PARTITION_NAME
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let partition = table.chunks(entry_bytes as usize).find(|entry| {
        let name = &entry[56..128];
        name.starts_with(&wanted) && name[wanted.len()..].iter().all(|&b| b == 0)
    });
    let Some(partition) = partition else {
        return Err(invalid(format!("it has no {PARTITION_NAME} partition")));
    };
    let base = u64_at(partition, 32) * SECTOR;
    let end = (u64_at(partition, 40) + 1) * SECTOR;
    if end > length || base >= end {
        return Err(invalid("its partition does not fit the disk"));
    }
    let superblock = read_at(&mut file, base + SUPERBLOCK, 128)?;
    if u32_at(&superblock, 0) != EROFS_MAGIC || superblock[12] != 12 {
        return Err(invalid(
            "its partition is not an erofs file system of 4 KiB blocks",
        ));
    }
    let root = u64::from(u16_at(&superblock, 14));
    let meta = u64::from(u32_at(&superblock, 40)) * BLOCK;
    let inode = |file: &mut File, nid: u64| -> io::Result<(u16, u64, u64)> {
        let raw = read_at(file, base + meta + nid * INODE, INODE as usize)?;
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
        if start + size > end {
            return Err(invalid("a file runs past the end of its partition"));
        }
        Ok((mode, size, start))
    };
    let (mode, size, start) = inode(&mut file, root)?;
    if mode & 0o170_000 != 0o040_000 {
        return Err(invalid("its file system's root is not a folder"));
    }
    let folder = read_at(&mut file, start, size as usize)?;
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
            let (mode, size, offset) = inode(&mut file, nid)?;
            if mode & 0o170_000 == 0o100_000 {
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
