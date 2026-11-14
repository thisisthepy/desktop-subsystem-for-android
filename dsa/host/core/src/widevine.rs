//! Widevine, the DRM module streaming apps play protected video with, for a user who asks for
//! it.
//!
//! DSA never ships or redistributes Google's Widevine. Google distributes it only as one APEX,
//! `com.google.android.widevine`, inside the vendor partition of its emulator system images.
//! When the user asks for it, their own computer reads Google's list of system images,
//! downloads the Google APIs image of the Android version and the processor architecture their
//! Android runs ([`find_image`]), checks it against the SHA-1 and size the list gives, and
//! takes the APEX out of the image's vendor partition byte for byte ([`take_apex`]). The APEX
//! is never changed: `apexd` checks its signature against Google's key at every boot, and
//! a changed APEX would not activate. An image of another architecture or another Android
//! version, and an APEX built for another architecture, are refused. The download is deleted
//! once the APEX is out, whether or not that worked. No other source is taken: the origin and
//! the licence of a copy found elsewhere cannot be checked.
//!
//! The client sends the APEX to the service, which checks it again ([`check_apex`]) and puts
//! it on the user's add-on disk ([`crate::addon_disk`], `widevine.img`) as the one file of an
//! erofs image ([`image`]). While one is recorded and on the disk, each boot's bootconfig
//! carries [`BOOTCONFIG_ON`] and where the image is, and the guest's init lays the image's
//! folder over `/vendor/apex` before `apexd` reads it, so `apexd` activates the APEX as one of
//! the vendor's own. The image carries the SELinux policy the module's service runs under. A
//! boot without Widevine carries [`BOOTCONFIG_OFF`], and the guest then removes the device
//! certificates Widevine kept in `/data/vendor/mediadrm`, so removing the module removes what
//! it provisioned too. Every change applies from the next start of Android, since `apexd`
//! activates APEXes once, at boot.
//!
//! The security level is L3, the software one: streaming services send an L3 device standard
//! definition at most, so the settings app and the command line say to watch in high
//! definition in the host's own browser or app.

use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::apk_info::zip::Archive;
use crate::arm_translation::google::{self, ext4, Progress, Scratch, SdkImage};
use crate::direct_mapped_disk::erofs_read;
use crate::settings::SettingsStore;
use crate::update::store::Fetch;
use crate::vhdx::ReadAt;

/// The APEX's module name, which its `apex_manifest.pb` gives.
pub const MODULE: &str = "com.google.android.widevine";
/// The bootconfig line that has the guest activate the module for one boot.
pub const BOOTCONFIG_ON: &str = "androidboot.dsa.widevine = \"on\"\n";
/// The bootconfig line of a boot without the module: the guest removes the device
/// certificates a removed module left.
pub const BOOTCONFIG_OFF: &str = "androidboot.dsa.widevine = \"off\"\n";
/// The bootconfig key that says where the image is for one boot
/// ([`crate::addon_disk::image_bootconfig`]).
pub const IMAGE_KEY: &str = "widevine_image";
/// The most an APEX may be. Widevine's is a few MiB.
pub const MAX_APEX_BYTES: u64 = 128 << 20;
/// The security level the module gives.
pub const SECURITY_LEVEL: &str = "L3";
/// Google's terms for what the SDK repository gives.
pub const TERMS: &str = google::TERMS;
/// The repository the images come from: Google's, as for its ARM translator.
pub const REPOSITORY: &str = google::REPOSITORY;
/// The environment variable that names a mirror of Google's repository instead, a URL or a
/// folder, as for Google's ARM translator.
pub const REPOSITORY_ENV: &str = "DSA_GOOGLE_REPOSITORY";

/// ELF's machine numbers for the two architectures DSA runs.
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;

/// The ABI of this host's Android: arm64-v8a on an ARM64 computer, x86_64 on any other.
pub fn guest_abi() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64-v8a"
    } else {
        "x86_64"
    }
}

/// The Widevine module a user installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidevinePackage {
    /// The APEX's file name in `/vendor/apex`: [`MODULE`] with `.apex` (or `.capex`, as Google
    /// may compress it).
    pub apex: String,
    /// The image it came from: its build, API level and revision.
    pub version: String,
    /// The Android major version and the ABI of that image.
    pub android_major: u32,
    pub abi: String,
    /// The image archive on Google's download server.
    pub source: String,
    /// The APEX's size in bytes and its SHA-256, in lower-case hexadecimal.
    pub size: u64,
    pub sha256: String,
    /// The licence Google's list offers the image under.
    pub license: String,
}

fn plain_text(what: &str, value: &str, limit: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(format!(
            "the module's {what} is {value:?}, which is not a usable {what}"
        ));
    }
    Ok(())
}

/// The SHA-256 of `data` in lower-case hexadecimal.
pub fn sha256_hex(data: &[u8]) -> String {
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl WidevinePackage {
    /// Whether the description is one the service takes, for a guest of `android_major` (0
    /// when the service does not know it) running `abi`.
    pub fn check(&self, android_major: u32, abi: &str) -> Result<(), String> {
        if self.apex != format!("{MODULE}.apex") && self.apex != format!("{MODULE}.capex") {
            return Err(format!(
                "the module's file is called {:?}; Widevine's is {MODULE}.apex",
                self.apex
            ));
        }
        plain_text("version", &self.version, 128)?;
        plain_text("source", &self.source, 1024)?;
        plain_text("licence", &self.license, 256)?;
        if self.size == 0 || self.size > MAX_APEX_BYTES {
            return Err(format!(
                "the module is {} bytes; it must have from 1 byte to {} MiB",
                self.size,
                MAX_APEX_BYTES >> 20
            ));
        }
        let hex = self.sha256.len() == 64 && self.sha256.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex || self.sha256.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err("the request does not carry the module's SHA-256".into());
        }
        if self.abi != abi {
            return Err(format!(
                "the module is for {}, and Android here runs on {abi}; install the one for {abi}",
                self.abi
            ));
        }
        if google::api_level(self.android_major).is_none() {
            return Err(format!(
                "the module is for Android {}, which DSA does not know",
                self.android_major
            ));
        }
        if android_major != 0 && self.android_major != android_major {
            return Err(format!(
                "the module is for Android {}, and Android here is {android_major}; install the \
                 one for Android {android_major}",
                self.android_major
            ));
        }
        Ok(())
    }
}

/// Where Widevine stands for a user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WidevineStatus {
    /// The module the user installed.
    pub package: Option<WidevinePackage>,
    /// Their add-on disk holds it for Android's next start.
    pub on_disk: bool,
    /// Their Android runs now; the two fields below hold only then.
    pub running: bool,
    /// The running Android was started with Widevine.
    pub active: bool,
    /// What is installed differs from what the running Android was started with.
    pub restart_needed: bool,
}

impl WidevineStatus {
    /// The status from what is recorded (`package`), what the next start's disk holds
    /// (`on_disk`), whether Android runs and whether the disk it attached held the module.
    pub fn new(
        package: Option<WidevinePackage>,
        on_disk: bool,
        running: bool,
        attached: bool,
    ) -> WidevineStatus {
        let active = running && attached;
        let wanted = package.is_some() && on_disk;
        WidevineStatus {
            package,
            on_disk,
            running,
            active,
            restart_needed: running && active != wanted,
        }
    }
}

/// What a boot's bootconfig carries for Widevine: [`BOOTCONFIG_ON`] and where the image is
/// while a module is installed and on the add-on disk the boot attaches, [`BOOTCONFIG_OFF`]
/// while none is installed. Nothing while one is recorded but not on the disk, so a disk that
/// could not be read does not cost the user their device certificates.
pub fn bootconfig(installed: Option<&WidevinePackage>, on_disk: bool) -> String {
    match (installed, on_disk) {
        (Some(_), true) => format!(
            "{BOOTCONFIG_ON}{}",
            crate::addon_disk::image_bootconfig(IMAGE_KEY, true)
        ),
        (Some(_), false) => String::new(),
        (None, _) => BOOTCONFIG_OFF.to_string(),
    }
}

/// The bootconfig for `user`'s next boot, from what their settings record and whether the
/// add-on disk the boot attaches holds the image (`attached`). A record that cannot be read is
/// logged and counts as unknown, so Android starts and nothing is removed.
pub fn bootconfig_for(
    store: &dyn SettingsStore,
    user: &str,
    attached: &[crate::addon_disk::Addon],
) -> String {
    let on_disk = attached.contains(&crate::addon_disk::Addon::Widevine);
    match store.widevine(user) {
        Ok(Some(package)) => {
            if on_disk {
                eprintln!(
                    "widevine: {user}'s Android starts with Widevine from {}",
                    package.version
                );
            } else {
                eprintln!(
                    "widevine: {user}'s Widevine module is not on their add-on disk; Android \
                     starts without it (install it again)"
                );
            }
            bootconfig(Some(&package), on_disk)
        }
        Ok(None) => bootconfig(None, on_disk),
        Err(why) => {
            eprintln!("widevine: {why}; starting {user}'s Android without Widevine");
            String::new()
        }
    }
}

/// Reads Google's list in `repository` ([`REPOSITORY`], or a mirror of it) and picks the
/// Google APIs image of Android `android_major` for `abi`, the one the module is taken from.
pub fn find_image(
    fetch: &dyn Fetch,
    repository: &str,
    android_major: u32,
    abi: &str,
) -> Result<SdkImage, String> {
    google::find_image_for_abi(fetch, repository, android_major, abi)
}

/// The value of `key` in a properties file (`key=value` lines).
fn property(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.trim().split_once('='))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The major part of an API level as a list or a properties file writes it (`36`, `36.1`).
fn api_major(text: &str) -> Option<u32> {
    text.split('.').next()?.parse().ok()
}

/// The name a protobuf message gives in field 1 (`apex_manifest.pb`'s `name`).
fn manifest_name(data: &[u8]) -> Option<String> {
    let mut at = 0;
    let varint = |at: &mut usize| -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *data.get(*at)?;
            *at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    };
    while at < data.len() {
        let key = varint(&mut at)?;
        let (field, wire) = (key >> 3, key & 7);
        match wire {
            0 => {
                varint(&mut at)?;
            }
            1 => at = at.checked_add(8)?,
            2 => {
                let length = usize::try_from(varint(&mut at)?).ok()?;
                let end = at.checked_add(length)?;
                let bytes = data.get(at..end)?;
                if field == 1 {
                    return String::from_utf8(bytes.to_vec()).ok();
                }
                at = end;
            }
            5 => at = at.checked_add(4)?,
            _ => return None,
        }
    }
    None
}

/// An in-memory file system image, read where a reader asks.
struct Bytes<'a>(&'a [u8]);

impl ReadAt for Bytes<'_> {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let start = usize::try_from(offset).map_err(std::io::Error::other)?;
        let bytes = start
            .checked_add(buf.len())
            .and_then(|end| self.0.get(start..end))
            .ok_or_else(|| std::io::Error::other("a read past the end of the image"))?;
        buf.copy_from_slice(bytes);
        Ok(())
    }

    fn len(&self) -> u64 {
        self.0.len() as u64
    }
}

/// A file system the module or its payload is in: ext4 or erofs.
enum Fs<'a, R: Read + Seek> {
    Ext4(ext4::Ext4<R>),
    Erofs(erofs_read::Reader<'a>),
}

const EROFS_MAGIC: u32 = 0xE0F5_E1E2;

fn is_erofs(head: &[u8]) -> bool {
    head.get(1024..1028)
        .is_some_and(|m| u32::from_le_bytes(m.try_into().unwrap()) == EROFS_MAGIC)
}

impl<R: Read + Seek> Fs<'_, R> {
    /// The regular file at `path` from the root, if there is one.
    fn file(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
        match self {
            Fs::Ext4(fs) => match fs.find(path)? {
                Some(inode) if inode.kind() == ext4::Kind::File => Ok(Some(fs.read(&inode)?)),
                _ => Ok(None),
            },
            Fs::Erofs(fs) => {
                let Some(inode) = erofs_find(fs, path)? else {
                    return Ok(None);
                };
                if inode.kind() != erofs_read::S_IFREG {
                    return Ok(None);
                }
                if inode.size > ext4::MAX_FILE {
                    return Err(format!("a file of {} bytes is too large", inode.size));
                }
                let mut data = Vec::with_capacity(inode.size as usize);
                fs.data(&inode, &mut |piece| {
                    data.extend_from_slice(piece);
                    Ok(())
                })?;
                Ok(Some(data))
            }
        }
    }

    /// The names of the entries of the folder at `path`, with whether each is a folder;
    /// none when there is no folder there.
    fn list(&mut self, path: &str) -> Result<Vec<(String, bool)>, String> {
        match self {
            Fs::Ext4(fs) => match fs.find(path)? {
                Some(inode) if inode.kind() == ext4::Kind::Folder => {
                    let mut out = Vec::new();
                    for (name, number) in fs.list(&inode)? {
                        if name == "." || name == ".." {
                            continue;
                        }
                        let child = fs.inode(number)?;
                        out.push((name, child.kind() == ext4::Kind::Folder));
                    }
                    Ok(out)
                }
                _ => Ok(Vec::new()),
            },
            Fs::Erofs(fs) => match erofs_find(fs, path)? {
                Some(inode) if inode.kind() == erofs_read::S_IFDIR => {
                    let mut out = Vec::new();
                    for entry in fs.entries(&inode)? {
                        if entry.name == b"." || entry.name == b".." {
                            continue;
                        }
                        let child = fs.inode(entry.nid)?;
                        let name = String::from_utf8_lossy(&entry.name).into_owned();
                        out.push((name, child.kind() == erofs_read::S_IFDIR));
                    }
                    Ok(out)
                }
                _ => Ok(Vec::new()),
            },
        }
    }
}

fn erofs_find(
    fs: &mut erofs_read::Reader<'_>,
    path: &str,
) -> Result<Option<erofs_read::Inode>, String> {
    let mut inode = fs.inode(fs.sb.root_nid)?;
    for name in path.split('/').filter(|name| !name.is_empty()) {
        if inode.kind() != erofs_read::S_IFDIR {
            return Ok(None);
        }
        let entries = fs.entries(&inode)?;
        match entries
            .into_iter()
            .find(|entry| entry.name == name.as_bytes())
        {
            Some(entry) => inode = fs.inode(entry.nid)?,
            None => return Ok(None),
        }
    }
    Ok(Some(inode))
}

/// The ELF machine of the first program or library under `bin` or `lib64` in a payload, by
/// depth-first search.
fn first_machine<R: Read + Seek>(fs: &mut Fs<'_, R>) -> Result<Option<u16>, String> {
    let mut stack: Vec<String> = vec!["lib64".into(), "bin".into()];
    let mut seen = 0;
    while let Some(folder) = stack.pop() {
        let mut entries = fs.list(&folder)?;
        entries.sort();
        for (name, is_folder) in entries.into_iter().rev() {
            seen += 1;
            if seen > 4096 {
                return Ok(None);
            }
            let path = format!("{folder}/{name}");
            if is_folder {
                stack.push(path);
            } else if let Some(data) = fs.file(&path)? {
                if data.len() >= 20 && data.starts_with(b"\x7fELF") {
                    return Ok(Some(u16::from_le_bytes([data[18], data[19]])));
                }
            }
        }
    }
    Ok(None)
}

fn machine_abi(machine: u16) -> String {
    match machine {
        EM_X86_64 => "x86_64".into(),
        EM_AARCH64 => "arm64-v8a".into(),
        other => format!("ELF machine {other}"),
    }
}

/// The module name and the processor of an APEX (or a compressed APEX, whose original APEX
/// is inside): from its `apex_manifest.pb`, and the ELF machine of its payload's programs.
pub fn apex_facts(apex: &[u8]) -> Result<(String, String), String> {
    let mut archive = Archive::open(Cursor::new(apex))
        .map_err(|why| format!("the module is not an APEX ({why})"))?;
    let manifest = archive
        .entry("apex_manifest.pb")
        .cloned()
        .ok_or("the module is not an APEX: it has no apex_manifest.pb")?;
    let manifest = archive.read(&manifest, 64 << 10)?;
    let name = manifest_name(&manifest).ok_or("the module's apex_manifest.pb is damaged")?;
    if let Some(original) = archive.entry("original_apex").cloned() {
        let original = archive.read(&original, MAX_APEX_BYTES as usize)?;
        let (_, abi) = apex_facts(&original)?;
        return Ok((name, abi));
    }
    let payload = archive
        .entry("apex_payload.img")
        .cloned()
        .ok_or("the module is not an APEX: it has no apex_payload.img")?;
    let payload = archive.read(&payload, ext4::MAX_FILE as usize)?;
    let machine = if is_erofs(&payload) {
        let mut source = Bytes(&payload);
        let reader = erofs_read::Reader::new(&mut source)?;
        first_machine::<Cursor<&[u8]>>(&mut Fs::Erofs(reader))?
    } else {
        let fs = ext4::Ext4::open(Cursor::new(&payload[..]))
            .map_err(|why| format!("the module's payload cannot be read ({why})"))?;
        first_machine(&mut Fs::Ext4(fs))?
    };
    let machine =
        machine.ok_or("the module's payload holds no program to tell its processor by")?;
    Ok((name, machine_abi(machine)))
}

/// Checks the APEX the service received against the description it was sent with (`package`,
/// itself checked first): its size and SHA-256, that it is Widevine's APEX, and that it was
/// built for the package's processor.
pub fn check_apex(apex: &[u8], package: &WidevinePackage) -> Result<(), String> {
    if apex.len() as u64 != package.size {
        return Err(format!(
            "the module arrived as {} bytes, where {} were announced",
            apex.len(),
            package.size
        ));
    }
    if sha256_hex(apex) != package.sha256 {
        return Err(
            "the module did not arrive intact (its SHA-256 differs); install it again".into(),
        );
    }
    let (name, abi) = apex_facts(apex)?;
    if name != MODULE {
        return Err(format!(
            "the APEX is {name}, not Widevine's ({MODULE}); nothing was changed"
        ));
    }
    if abi != package.abi {
        return Err(format!(
            "the module was built for {abi}, and Android here runs on {}; nothing was changed",
            package.abi
        ));
    }
    Ok(())
}

/// The erofs image the add-on disk holds for the module: the APEX, unchanged, as its one file,
/// named as the guest's `/vendor/apex` names it.
pub fn image(apex: Vec<u8>, package: &WidevinePackage) -> Result<Vec<u8>, String> {
    let mut tree = crate::erofs::Builder::new();
    tree.file(&package.apex, 0o644, apex)?;
    let mut out = Vec::new();
    tree.write(&mut out)?;
    Ok(out)
}

/// An APEX's file name and bytes.
type NamedApex = (String, Vec<u8>);

/// The APEX in a vendor partition at `partition`, as its file name and bytes, with the
/// partition's `build.prop`.
fn vendor_apex(partition: &Path) -> Result<(Option<NamedApex>, Option<String>), String> {
    let file = File::open(partition).map_err(google::io(partition))?;
    let mut head = vec![0u8; 2048];
    let mut reader = BufReader::new(file);
    reader
        .read_exact(&mut head)
        .map_err(|e| format!("the image's vendor partition cannot be read: {e}"))?;
    let erofs = is_erofs(&head);
    let mut raw;
    let mut fs = if erofs {
        raw = crate::vhdx::RawDisk::new(File::open(partition).map_err(google::io(partition))?)
            .map_err(google::io(partition))?;
        Fs::<BufReader<File>>::Erofs(erofs_read::Reader::new(&mut raw)?)
    } else {
        reader.rewind().map_err(google::io(partition))?;
        Fs::Ext4(ext4::Ext4::open(reader)?)
    };
    // The partition's root is the vendor folder itself, or `/` with the vendor folder in it.
    let root = if fs.list("vendor/apex")?.is_empty() {
        ""
    } else {
        "vendor/"
    };
    // Android 16's images name the file after the module; Android 17's add the module's
    // version (com.google.android.widevine-15027108-cp2a.apex). The manifest inside says which
    // module it is, and is checked once the file is out; the file is kept under the plain name.
    let mut apex = None;
    let mut names: Vec<String> = fs
        .list(&format!("{root}apex"))?
        .into_iter()
        .filter(|(name, folder)| !folder && apex_file_kind(name).is_some())
        .map(|(name, _)| name)
        .collect();
    names.sort_by_key(|name| (apex_file_kind(name) != Some(ApexFile::Plain), name.clone()));
    if let Some(found) = names.into_iter().next() {
        if let Some(data) = fs.file(&format!("{root}apex/{found}"))? {
            let extension = if found.ends_with(".capex") {
                "capex"
            } else {
                "apex"
            };
            apex = Some((format!("{MODULE}.{extension}"), data));
        }
    }
    let build = fs
        .file(&format!("{root}build.prop"))?
        .map(|data| String::from_utf8_lossy(&data).into_owned());
    Ok((apex, build))
}

/// How a file in a vendor image's `apex` folder names Widevine's module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApexFile {
    /// `com.google.android.widevine.apex` (or `.capex`).
    Plain,
    /// The module's name, a dash and its version: `com.google.android.widevine-15027108-cp2a.apex`.
    Versioned,
}

/// Whether `name` is the file of Widevine's module in a vendor image, and how it is named.
/// Another module whose name starts the same (`com.google.android.widevinex.apex`) is not.
pub fn apex_file_kind(name: &str) -> Option<ApexFile> {
    let stem = name
        .strip_suffix(".apex")
        .or_else(|| name.strip_suffix(".capex"))?;
    if stem == MODULE {
        return Some(ApexFile::Plain);
    }
    let version = stem.strip_prefix(MODULE)?.strip_prefix('-')?;
    let plain = !version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    plain.then_some(ApexFile::Versioned)
}

/// Downloads `image` into the new folder `work`, checks it against Google's list and against
/// the Android (`android_major`, `abi`) it is for, and writes Widevine's APEX out of its vendor
/// partition, unchanged, to `out`. `work` is removed afterwards whatever happens, and so is
/// `out` when the APEX could not be taken.
pub fn take_apex(
    fetch: &dyn Fetch,
    image: &SdkImage,
    android_major: u32,
    abi: &str,
    work: &Path,
    out: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<WidevinePackage, String> {
    if out.exists() {
        return Err(format!("{} is in the way of the module", out.display()));
    }
    fs::create_dir_all(work).map_err(google::io(work))?;
    let scratch = Scratch(work.to_path_buf());
    let taken = take(fetch, image, android_major, abi, &scratch.0, out, progress);
    drop(scratch);
    if taken.is_err() {
        let _ = fs::remove_file(out);
    }
    taken
}

fn take(
    fetch: &dyn Fetch,
    image: &SdkImage,
    android_major: u32,
    abi: &str,
    work: &Path,
    out: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<WidevinePackage, String> {
    let api = google::api_level(android_major)
        .ok_or_else(|| format!("Android {android_major} is not known to DSA"))?;
    let refuse = |what: String| {
        Err(format!(
            "Google's image {} is {what}, and Android here is Android {android_major} \
             (API {api}) on {abi}; nothing was changed",
            image.url
        ))
    };
    let zip = work.join("image.zip");
    google::download(fetch, image, &zip, progress)?;
    if let Some(properties) = google::source_properties(&zip)? {
        if let Some(level) = property(&properties, "AndroidVersion.ApiLevel") {
            if api_major(&level) != Some(api) {
                return refuse(format!("of API level {level}"));
            }
        }
        if let Some(image_abi) = property(&properties, "SystemImage.Abi") {
            if image_abi != abi {
                return refuse(format!("for {image_abi}"));
            }
        }
    }
    progress(Progress::Extracting);
    let partition = work.join("vendor.part");
    google::extract_partition(&zip, &partition, "vendor")?;
    let _ = fs::remove_file(&zip);
    progress(Progress::Building);
    let (apex, build_prop) = vendor_apex(&partition)?;
    let _ = fs::remove_file(&partition);
    let build_prop = build_prop.unwrap_or_default();
    if let Some(level) = property(&build_prop, "ro.vendor.build.version.sdk") {
        if api_major(&level) != Some(api) {
            return refuse(format!("of API level {level}"));
        }
    }
    for key in ["ro.vendor.product.cpu.abi", "ro.product.vendor.cpu.abi"] {
        if let Some(image_abi) = property(&build_prop, key) {
            if image_abi != abi {
                return refuse(format!("for {image_abi}"));
            }
        }
    }
    let Some((name, apex)) = apex else {
        return Err(format!(
            "Google's image for Android {android_major} on {abi} ({}) holds no Widevine \
             module; nothing was changed",
            image.url
        ));
    };
    let (module, built_for) = apex_facts(&apex)?;
    if module != MODULE {
        return Err(format!(
            "the image's {name} is {module}, not Widevine's; nothing was changed"
        ));
    }
    if built_for != abi {
        return refuse(format!("carrying a Widevine module built for {built_for}"));
    }
    if apex.len() as u64 > MAX_APEX_BYTES {
        return Err(format!(
            "the image's Widevine module is {} bytes, more than {} MiB; nothing was changed",
            apex.len(),
            MAX_APEX_BYTES >> 20
        ));
    }
    let build = property(&build_prop, "ro.vendor.build.id")
        .or_else(|| google::build_id(build_prop.as_bytes()));
    let package = WidevinePackage {
        apex: name,
        version: google::version(image, build.as_deref()),
        android_major,
        abi: abi.to_string(),
        source: image.url.clone(),
        size: apex.len() as u64,
        sha256: sha256_hex(&apex),
        license: image.license.clone(),
    };
    package.check(android_major, abi)?;
    if let Some(dir) = out.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(google::io(dir))?;
    }
    fs::write(out, &apex).map_err(google::io(out))?;
    Ok(package)
}

/// Google's repository and Widevine's APEX as fakes, for the tests here and of the programs
/// that fetch from it.
#[cfg(any(test, feature = "conformance"))]
pub mod fixture {
    use super::*;
    use crate::apk_info::fixture::archive;
    use crate::arm_translation::google::disk::fixture::disk_of;
    use crate::arm_translation::google::ext4::fixture::{image as ext4_image, Item};
    use crate::arm_translation::google::fixture::{list, offer, Offer};

    /// The program Widevine's APEX runs, as its payload holds it.
    pub const SERVICE: &str = "bin/hw/android.hardware.drm-service.widevine";
    /// The vendor build the fake images carry.
    pub const BUILD_ID: &str = "BE2A.250530.026.F3";

    fn manifest(name: &str) -> Vec<u8> {
        let mut out = vec![0x0a, name.len() as u8];
        out.extend_from_slice(name.as_bytes());
        // version: 1
        out.extend_from_slice(&[0x10, 0x01]);
        out
    }

    fn elf(machine: u16) -> Vec<u8> {
        let mut data = vec![0u8; 64];
        data[..4].copy_from_slice(b"\x7fELF");
        data[4] = 2;
        data[18..20].copy_from_slice(&machine.to_le_bytes());
        data
    }

    /// The ELF machine of `abi`.
    pub fn machine_of(abi: &str) -> u16 {
        if abi == "arm64-v8a" {
            EM_AARCH64
        } else {
            EM_X86_64
        }
    }

    /// An APEX named `name` whose payload's service is built for `abi`.
    pub fn apex(name: &str, abi: &str) -> Vec<u8> {
        let program = elf(machine_of(abi));
        let payload = ext4_image(&[
            Item::File("etc/init.rc", 0o644, b"service x /apex/x\n"),
            Item::File(SERVICE, 0o755, &program),
        ]);
        let manifest = manifest(name);
        archive(
            &[
                ("apex_manifest.pb", &manifest[..]),
                ("apex_payload.img", &payload[..]),
                ("apex_pubkey", &b"key"[..]),
            ],
            b"",
        )
    }

    /// An emulator image archive as Google's for API level `api` and `abi`: its disk, whose
    /// `super` holds a system partition and a vendor partition with `apex` (when given) in
    /// its `apex` folder.
    pub fn image_archive(api: &str, abi: &str, apex: Option<&[u8]>) -> Vec<u8> {
        let build_prop = format!(
            "ro.vendor.build.id={BUILD_ID}\nro.vendor.build.version.sdk={api}\n\
             ro.vendor.product.cpu.abi={abi}\n"
        );
        let system = ext4_image(&[Item::File("system/build.prop", 0o644, b"ro.build.id=x\n")]);
        let name = format!("apex/{MODULE}.apex");
        let mut items = vec![
            Item::File("build.prop", 0o644, build_prop.as_bytes()),
            Item::File("apex/com.google.cf.wifi.apex", 0o644, b"other"),
        ];
        if let Some(apex) = apex {
            items.push(Item::File(&name, 0o644, apex));
        }
        let vendor = ext4_image(&items);
        let disk = disk_of(&[("system", &system), ("vendor", &vendor)]);
        let properties = format!(
            "AndroidVersion.ApiLevel={api}\nSystemImage.Abi={abi}\nSystemImage.TagId=google_apis\n"
        );
        let properties_name = format!("{abi}/source.properties");
        let disk_name = format!("{abi}/system.img");
        archive(
            &[
                (properties_name.as_str(), properties.as_bytes()),
                (disk_name.as_str(), &disk[..]),
            ],
            b"",
        )
    }

    /// Writes a repository into `folder`: Google's list offering, for each `(api, abi, zip)`,
    /// `zip` as the Google APIs image of that API level and ABI, at a revision above every
    /// other one listed.
    pub fn write_repository(folder: &Path, images: &[(&str, &str, &[u8])]) -> std::io::Result<()> {
        fs::create_dir_all(folder)?;
        let names: Vec<String> = images
            .iter()
            .map(|(api, abi, _)| format!("{abi}-{api}.zip"))
            .collect();
        let sha1s: Vec<String> = images
            .iter()
            .map(|(_, _, zip)| google::fixture::sha1_of(zip))
            .collect();
        let mut offers: Vec<Offer<'_>> = google::fixture::decoys();
        for (((api, abi, zip), name), sha1) in images.iter().zip(&names).zip(&sha1s) {
            offers.push(Offer {
                abi,
                size: zip.len() as u64,
                sha1: sha1.as_str(),
                ..offer(api, google::fixture::NEWEST_REVISION, name)
            });
            fs::write(folder.join(name), zip)?;
        }
        fs::write(folder.join(google::LISTS[0]), list(&offers))
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use crate::erofs::read::files;

    fn folders(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("dsa-widevine-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        (base.join("work"), base.join("module.apex"))
    }

    fn repository(name: &str, images: &[(&str, &str, &[u8])]) -> String {
        let folder =
            std::env::temp_dir().join(format!("dsa-widevine-repo-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        write_repository(&folder, images).unwrap();
        folder.display().to_string()
    }

    #[test]
    fn fr37_android_17s_versioned_apex_file_is_widevines() {
        // Google's Android 17 x86_64 image (x86_64-37.0_r08) keeps the module as
        // vendor/apex/com.google.android.widevine-15027108-cp2a.apex.
        let versioned = format!("{MODULE}-15027108-cp2a.apex");
        assert_eq!(apex_file_kind(&versioned), Some(ApexFile::Versioned));
        assert_eq!(
            apex_file_kind(&format!("{MODULE}.apex")),
            Some(ApexFile::Plain)
        );
        assert_eq!(
            apex_file_kind(&format!("{MODULE}.capex")),
            Some(ApexFile::Plain)
        );
        assert_eq!(
            apex_file_kind(&format!("{MODULE}-1.capex")),
            Some(ApexFile::Versioned)
        );
        assert_eq!(apex_file_kind("com.google.android.widevinex.apex"), None);
        assert_eq!(apex_file_kind(&format!("{MODULE}-.apex")), None);
        assert_eq!(apex_file_kind(&format!("{MODULE}-1/../x.apex")), None);
        assert_eq!(apex_file_kind("com.android.hardware.cas.apex"), None);
    }

    #[test]
    fn fr37_widevine_apex_is_taken_unchanged() {
        let abi = guest_abi();
        let module = apex(MODULE, abi);
        let zip = image_archive("36", abi, Some(&module));
        let repo = repository("taken", &[("36", abi, &zip)]);
        let fetch = crate::update::store::FileFetch;
        let image = find_image(&fetch, &repo, 16, abi).unwrap();
        let (work, out) = folders("taken");
        let mut steps = Vec::new();
        let package =
            take_apex(&fetch, &image, 16, abi, &work, &out, &mut |s| steps.push(s)).unwrap();
        // Byte for byte what the image holds, and nothing else of it.
        assert_eq!(fs::read(&out).unwrap(), module);
        assert!(!work.exists(), "the download is deleted");
        assert!(steps.contains(&Progress::Extracting));
        assert_eq!(package.apex, format!("{MODULE}.apex"));
        assert_eq!(package.android_major, 16);
        assert_eq!(package.abi, abi);
        assert_eq!(package.size, module.len() as u64);
        assert_eq!(package.sha256, sha256_hex(&module));
        assert_eq!(package.version, format!("{BUILD_ID} (API 36, r20)"));
        assert_eq!(package.license, "android-sdk-license");
        assert!(package.source.ends_with(&format!("{abi}-36.zip")));
        package.check(16, abi).unwrap();
        check_apex(&module, &package).unwrap();
        // The add-on image holds the APEX alone, unchanged, under its own name.
        let made = super::image(module.clone(), &package).unwrap();
        let held = files(&made);
        assert_eq!(held.len(), 1);
        assert_eq!(held[&package.apex], module);
        let _ = fs::remove_dir_all(out.parent().unwrap());
    }

    #[test]
    fn fr37_an_image_or_apex_of_another_architecture_or_android_version_is_refused() {
        let abi = guest_abi();
        let other_abi = if abi == "x86_64" {
            "arm64-v8a"
        } else {
            "x86_64"
        };
        let fetch = crate::update::store::FileFetch;
        // The image Google lists for this Android, but whose contents say otherwise.
        let cases: [(&str, Vec<u8>, &str); 4] = [
            (
                "version",
                image_archive("37", abi, Some(&apex(MODULE, abi))),
                "API level 37",
            ),
            (
                "abi",
                image_archive("36", other_abi, Some(&apex(MODULE, other_abi))),
                other_abi,
            ),
            (
                "apex-abi",
                image_archive("36", abi, Some(&apex(MODULE, other_abi))),
                "built for",
            ),
            (
                "missing",
                image_archive("36", abi, None),
                "holds no Widevine",
            ),
        ];
        for (name, zip, said) in cases {
            let repo = repository(name, &[("36", abi, &zip)]);
            let image = find_image(&fetch, &repo, 16, abi).unwrap();
            let (work, out) = folders(name);
            let refused = take_apex(&fetch, &image, 16, abi, &work, &out, &mut |_| {}).unwrap_err();
            assert!(refused.contains(said), "{name}: {refused}");
            assert!(refused.contains("nothing was changed"), "{name}: {refused}");
            assert!(!out.exists(), "{name}: nothing is kept");
            assert!(!work.exists(), "{name}: the download is deleted");
            let _ = fs::remove_dir_all(out.parent().unwrap());
        }
        // Google's list without an image for this Android: refused before any download.
        let zip = image_archive("36", abi, Some(&apex(MODULE, abi)));
        let repo = repository("unlisted", &[("36", abi, &zip)]);
        let refused = find_image(&fetch, &repo, 15, abi).unwrap_err();
        assert!(refused.contains("nothing was changed"), "{refused}");
    }

    #[test]
    fn fr37_the_service_takes_only_widevines_apex_as_described() {
        let abi = guest_abi();
        let other_abi = if abi == "x86_64" {
            "arm64-v8a"
        } else {
            "x86_64"
        };
        let module = apex(MODULE, abi);
        let package = WidevinePackage {
            apex: format!("{MODULE}.apex"),
            version: "BE2A (API 36, r7)".into(),
            android_major: 16,
            abi: abi.into(),
            source: "https://dl.google.com/android/repository/sys-img/google_apis/x.zip".into(),
            size: module.len() as u64,
            sha256: sha256_hex(&module),
            license: "android-sdk-license".into(),
        };
        package.check(16, abi).unwrap();
        package.check(0, abi).unwrap();
        assert!(package.check(17, abi).unwrap_err().contains("Android 17"));
        assert!(package.check(16, other_abi).is_err());
        for bad in [
            WidevinePackage {
                apex: "../x.apex".into(),
                ..package.clone()
            },
            WidevinePackage {
                apex: "com.android.other.apex".into(),
                ..package.clone()
            },
            WidevinePackage {
                size: 0,
                ..package.clone()
            },
            WidevinePackage {
                size: MAX_APEX_BYTES + 1,
                ..package.clone()
            },
            WidevinePackage {
                sha256: "00".into(),
                ..package.clone()
            },
            WidevinePackage {
                version: "a\nb".into(),
                ..package.clone()
            },
        ] {
            assert!(bad.check(16, abi).is_err(), "{bad:?}");
        }
        check_apex(&module, &package).unwrap();
        let mut changed = module.clone();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        assert!(check_apex(&changed, &package)
            .unwrap_err()
            .contains("SHA-256"));
        let other = apex("com.android.other", abi);
        let described = WidevinePackage {
            size: other.len() as u64,
            sha256: sha256_hex(&other),
            ..package.clone()
        };
        assert!(check_apex(&other, &described)
            .unwrap_err()
            .contains("not Widevine"));
        let foreign = apex(MODULE, other_abi);
        let described = WidevinePackage {
            size: foreign.len() as u64,
            sha256: sha256_hex(&foreign),
            ..package.clone()
        };
        assert!(check_apex(&foreign, &described)
            .unwrap_err()
            .contains("built for"));
        assert!(check_apex(
            b"not a zip",
            &WidevinePackage {
                size: 9,
                sha256: sha256_hex(b"not a zip"),
                ..package
            }
        )
        .is_err());
    }

    #[test]
    fn fr37_the_bootconfig_activates_widevine_only_while_it_is_installed_and_on_the_disk() {
        let package = WidevinePackage {
            apex: format!("{MODULE}.apex"),
            version: "v".into(),
            android_major: 16,
            abi: guest_abi().into(),
            source: "s".into(),
            size: 1,
            sha256: "0".repeat(64),
            license: "l".into(),
        };
        assert_eq!(
            bootconfig(Some(&package), true),
            "androidboot.dsa.widevine = \"on\"\nandroidboot.dsa.widevine_image = \"disk\"\n"
        );
        assert_eq!(bootconfig(None, false), BOOTCONFIG_OFF);
        assert_eq!(bootconfig(None, true), BOOTCONFIG_OFF);
        assert_eq!(bootconfig(Some(&package), false), "");
        // The direct-mapped mode takes the image from its own partition.
        let mapped = crate::addon_disk::direct_mapped_bootconfig(&bootconfig(Some(&package), true));
        assert!(
            mapped.contains("androidboot.dsa.widevine_image = \"dax\"\n"),
            "{mapped}"
        );
        let store = crate::settings::MemorySettings::default();
        use crate::addon_disk::Addon;
        assert_eq!(bootconfig_for(&store, "u", &[]), BOOTCONFIG_OFF);
        store.set_widevine("u", Some(&package)).unwrap();
        assert_eq!(bootconfig_for(&store, "u", &[]), "");
        assert!(bootconfig_for(&store, "u", &[Addon::Widevine]).starts_with(BOOTCONFIG_ON));
    }

    #[test]
    fn fr37_the_status_says_when_a_restart_applies_a_change() {
        let package = || {
            Some(WidevinePackage {
                apex: format!("{MODULE}.apex"),
                version: "v".into(),
                android_major: 16,
                abi: guest_abi().into(),
                source: "s".into(),
                size: 1,
                sha256: "0".repeat(64),
                license: "l".into(),
            })
        };
        let stopped = WidevineStatus::new(package(), true, false, false);
        assert!(!stopped.active && !stopped.restart_needed);
        let installed_now = WidevineStatus::new(package(), true, true, false);
        assert!(!installed_now.active && installed_now.restart_needed);
        let booted_with = WidevineStatus::new(package(), true, true, true);
        assert!(booted_with.active && !booted_with.restart_needed);
        let removed_now = WidevineStatus::new(None, false, true, true);
        assert!(removed_now.active && removed_now.restart_needed);
        let never = WidevineStatus::new(None, false, true, false);
        assert!(!never.active && !never.restart_needed);
    }

    #[test]
    fn fr37_a_protobuf_manifest_name_is_read_whatever_comes_before_it() {
        assert_eq!(manifest_name(&[0x0a, 1, b'a']), Some("a".into()));
        assert_eq!(manifest_name(&[0x10, 5, 0x0a, 1, b'b']), Some("b".into()));
        assert_eq!(manifest_name(&[0x0a, 9, b'a']), None);
        assert_eq!(manifest_name(&[0xff]), None);
    }
}
