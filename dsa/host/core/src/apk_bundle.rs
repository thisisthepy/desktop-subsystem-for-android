//! Split-APK bundles: an app that comes as a base APK and split APKs in one archive, as
//! APKPure (`.xapk`), bundletool and SAI (`.apks`) and APKMirror (`.apkm`) make them.
//!
//! What a file is comes from its content, not its name: an APK has `AndroidManifest.xml` at
//! its root, an `.xapk` a `manifest.json`, an `.apks` a `toc.pb` (or only APKs), an `.apkm` an
//! `info.json`. An Android App Bundle (`.aab`, with `BundleConfig.pb`) is refused with its
//! reason, as is a bundle whose APKs are encrypted, as some older `.apkm` files are.
//!
//! Of the APKs a bundle holds, a device gets only what it needs, in one install session:
//!
//! - the base APK, and every feature split;
//! - one ABI, chosen from the native libraries of all the APKs together as for a single APK
//!   ([`crate::native_abi::choose`]): `x86_64`, unless its libraries leave out some of the
//!   `arm64-v8a` ones and ARM translation is on. A device that runs only `arm64-v8a` takes
//!   that;
//! - for the base and each feature, the density split nearest the device's density (the
//!   higher one of two as near), and the language splits of the device's languages and
//!   English;
//! - any other configuration split, such as a texture format, since Android takes them all.
//!
//! Every APK of a bundle must be signed with the base APK's certificate, of the same package
//! and version; a bundle that is not is refused before anything reaches Android. Expansion
//! (OBB) files in the bundle go with the install and land in `Android/obb/<package>/`.
//!
//! APKs stored in the bundle are read where they are. A compressed one is unpacked into a file
//! of its own in the folder the caller gives for temporary files, removed with the [`Bundle`];
//! an expansion file is
//! unpacked as it is sent instead, since it can be gigabytes.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use crate::apk::{self, ApkFile, InstallPart};
use crate::apk_info::zip::{Archive, Entry};
use crate::apk_info::{attr, axml, signing, ApkInfo};
use crate::arm_translation::google::inflate;
use crate::host_api::ArmTranslationStatus;
use crate::native_abi::{self, NativeCode, NativeLibraries, TranslationOff, WontRun};
pub use crate::native_abi::{ARM64, X86_64};

/// The largest manifest read from a split.
const MAX_MANIFEST: usize = 4 * 1024 * 1024;

/// The file extensions of what goes to the install flow rather than to an app: APKs, the
/// bundles taken, and App Bundles, which the flow refuses with its reason.
pub const INSTALL_EXTENSIONS: [&str; 5] = ["apk", "xapk", "apks", "apkm", "aab"];

/// Whether the file at `path` goes to the install flow, by its extension. What it is, the flow
/// tells from its content.
pub fn is_install_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            INSTALL_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// The kinds of split-APK bundle taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// APKPure's `.xapk`: the APKs, a `manifest.json` and any expansion files.
    Xapk,
    /// bundletool's and SAI's `.apks`: the APKs, with bundletool's `toc.pb`.
    Apks,
    /// APKMirror's `.apkm`: the APKs and an `info.json`.
    Apkm,
}

impl Format {
    /// A stable code for programs, such as the install window.
    pub fn code(self) -> &'static str {
        match self {
            Format::Xapk => "xapk",
            Format::Apks => "apks",
            Format::Apkm => "apkm",
        }
    }
}

/// What a file is, as its content says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Apk,
    Bundle(Format),
    /// An Android App Bundle, which is uploaded to a store and is not installed as it is.
    AppBundle,
    /// A bundle whose APKs are encrypted, so they cannot be read.
    EncryptedBundle,
}

/// Why a bundle is not installed, found before anything reaches Android.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The file is an Android App Bundle (`.aab`).
    AppBundle,
    /// The bundle's APKs are encrypted.
    Encrypted,
    /// A split is signed with another certificate than the base APK.
    SignatureMismatch { split: String },
    /// The bundle is not one app, or it is damaged, as a sentence.
    Invalid(String),
    /// The file is no APK or bundle that can be read at all, as a sentence.
    NotAnApp(String),
    /// The app's native code is none the guest runs.
    UnsupportedAbi(WontRun),
    /// The app's 64-bit native code is for arm64-v8a only, and the running Android does not
    /// translate ARM code.
    TranslationOff(TranslationOff),
}

impl Refusal {
    /// A stable code for programs, such as the install window, that say the reason in the
    /// user's language.
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::AppBundle => "app-bundle",
            Refusal::Encrypted => "encrypted-bundle",
            Refusal::SignatureMismatch { .. } => "split-signature-mismatch",
            Refusal::Invalid(_) => "invalid-bundle",
            Refusal::NotAnApp(_) => "not-an-apk",
            Refusal::UnsupportedAbi(why) => why.code(),
            Refusal::TranslationOff(why) => why.code(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::AppBundle => f.write_str(
                "this is an Android App Bundle (.aab), which is made to be uploaded to an app \
                 store and cannot be installed as it is: making APKs of it means signing them \
                 with another key, and the app would then no longer take updates from where it \
                 came from. Get the app as an APK, or as an .xapk, .apks or .apkm bundle",
            ),
            Refusal::Encrypted => f.write_str(
                "the bundle is encrypted, so its APKs cannot be read; some older .apkm files \
                 are, and only the app that made them opens them. Get the app as an APK, or as \
                 a newer bundle",
            ),
            Refusal::SignatureMismatch { split } => write!(
                f,
                "{split} is signed with another certificate than the base APK, so the bundle \
                 was changed after it was made; Android would refuse it"
            ),
            Refusal::Invalid(why) | Refusal::NotAnApp(why) => f.write_str(why),
            Refusal::UnsupportedAbi(why) => write!(f, "{why}"),
            Refusal::TranslationOff(why) => write!(f, "{why}"),
        }
    }
}

/// What part of the app a split APK is, from its split name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SplitKind {
    Base,
    /// A feature split, which every device gets.
    Feature,
    /// The native libraries for one ABI of the base (`of` empty) or of feature `of`.
    Abi {
        of: String,
        abi: String,
    },
    /// The resources for one screen density.
    Density {
        of: String,
        dpi: u32,
    },
    /// The resources for one language.
    Language {
        of: String,
        language: String,
    },
    /// Any other configuration, such as a texture format.
    Config {
        of: String,
    },
}

/// The ABI a configuration split names (`arm64_v8a`), as the package manager names it.
fn abi_of(config: &str) -> Option<&'static str> {
    Some(match config {
        "x86_64" => X86_64,
        "arm64_v8a" => ARM64,
        "x86" => "x86",
        "armeabi_v7a" => "armeabi-v7a",
        "armeabi" => "armeabi",
        "riscv64" => "riscv64",
        "mips" => "mips",
        "mips64" => "mips64",
        _ => return None,
    })
}

/// The density a configuration split names (`xxhdpi`, or `420dpi`), in dots per inch.
fn density_of(config: &str) -> Option<u32> {
    Some(match config {
        "ldpi" => 120,
        "mdpi" => 160,
        "tvdpi" => 213,
        "hdpi" => 240,
        "xhdpi" => 320,
        "xxhdpi" => 480,
        "xxxhdpi" => 640,
        other => {
            let digits = other.strip_suffix("dpi")?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse().ok()?
        }
    })
}

/// The language a configuration split names (`en`, `zh_TW`, `pt-rBR`), as a lower-case code.
fn language_of(config: &str) -> Option<String> {
    let language = config.split(['_', '-']).next()?;
    ((2..=3).contains(&language.len()) && language.bytes().all(|b| b.is_ascii_lowercase()))
        .then(|| language.to_string())
}

/// What a split is, from the split name its manifest gives: none for the base, `config.<x>`
/// for a configuration of the base, `<feature>.config.<x>` for one of a feature, anything else
/// for a feature.
pub fn classify(split: Option<&str>) -> SplitKind {
    let Some(split) = split else {
        return SplitKind::Base;
    };
    let (of, config) = match split.strip_prefix("config.") {
        Some(config) => ("", config),
        None => match split.split_once(".config.") {
            Some((of, config)) => (of, config),
            None => return SplitKind::Feature,
        },
    };
    let of = of.to_string();
    if let Some(abi) = abi_of(config) {
        SplitKind::Abi {
            of,
            abi: abi.to_string(),
        }
    } else if let Some(dpi) = density_of(config) {
        SplitKind::Density { of, dpi }
    } else if let Some(language) = language_of(config) {
        SplitKind::Language { of, language }
    } else {
        SplitKind::Config { of }
    }
}

/// What a device takes from a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The ABIs the device runs, preferred first, such as `["x86_64", "arm64-v8a"]`.
    pub abis: Vec<String>,
    /// The density apps draw at, in dots per inch.
    pub density_dpi: u32,
    /// The device's languages as lower-case codes, preferred first, such as `["ko"]`.
    pub languages: Vec<String>,
}

impl Device {
    /// The 64-bit ABIs a device runs, preferred first. `abilist` is what the running Android
    /// says (`ro.product.cpu.abilist`); without it, they follow from the host: an ARM64 host
    /// runs `arm64-v8a`, any other `x86_64`, and `arm64-v8a` after it while ARM translation
    /// is installed and on.
    pub fn abis_for(abilist: Option<&str>, arm64_host: bool, translation: bool) -> Vec<String> {
        let mut abis: Vec<String> = Vec::new();
        for abi in abilist.unwrap_or("").split(',').map(str::trim) {
            if (abi == X86_64 || abi == ARM64) && !abis.iter().any(|a| a == abi) {
                abis.push(abi.to_string());
            }
        }
        if abis.is_empty() {
            if arm64_host {
                abis.push(ARM64.to_string());
            } else {
                abis.push(X86_64.to_string());
                if translation {
                    abis.push(ARM64.to_string());
                }
            }
        }
        abis
    }

    /// What a host's Android takes: the ABIs as [`abis_for`](Self::abis_for) says them from
    /// what the service says of ARM translation (`None` when it cannot be asked), that is what
    /// the running Android lists, else whether a translator is in use; the density apps draw
    /// at on the host's main screen; and the host's display languages.
    pub fn of_host(
        translation: Option<&ArmTranslationStatus>,
        arm64_host: bool,
        density_dpi: u32,
        locales: &[String],
    ) -> Device {
        let abilist = translation
            .and_then(|status| status.guest.as_ref())
            .map(|guest| guest.abilist.as_str())
            .filter(|abilist| !abilist.is_empty());
        // A translator is in use while the setting is on and the user installed one or the
        // image has its own.
        let translating = translation.is_some_and(|status| status.translator.is_some());
        Device {
            abis: Device::abis_for(abilist, arm64_host, translating),
            density_dpi,
            languages: Device::languages_of(locales),
        }
    }

    /// The language codes of `locales` (BCP 47 tags such as `ko-KR` or POSIX names such as
    /// `ko_KR.UTF-8`), preferred first, without repeats.
    pub fn languages_of(locales: &[String]) -> Vec<String> {
        let mut languages: Vec<String> = Vec::new();
        for locale in locales {
            let language = locale
                .split(['-', '_', '.', '@'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if (2..=3).contains(&language.len())
                && language.bytes().all(|b| b.is_ascii_lowercase())
                && !languages.contains(&language)
            {
                languages.push(language);
            }
        }
        languages
    }
}

/// Where an APK or an expansion file of a bundle is.
#[derive(Debug, Clone)]
enum Source {
    /// Stored in the bundle at `offset`.
    Stored { offset: u64, length: u64 },
    /// Unpacked into a file of its own.
    Unpacked(PathBuf),
    /// Compressed in the bundle at `offset`, `length` bytes of it.
    Deflated { offset: u64, length: u64 },
}

/// One APK of a bundle.
#[derive(Debug, Clone)]
pub struct BundleApk {
    /// Its path in the bundle.
    pub entry: String,
    /// The split name its manifest gives; none for the base.
    pub split: Option<String>,
    pub kind: SplitKind,
    /// Its length in bytes, uncompressed.
    pub size: u64,
    /// The package its manifest names, and the full version code.
    pub package: String,
    pub version_code: i64,
    /// The SHA-256 digest of its signing certificate; none when it is not signed.
    pub signer: Option<Vec<u8>>,
    /// Its native libraries, by ABI.
    pub libraries: NativeLibraries,
    /// Its native code, as far as whether the guest can run it at all.
    code: NativeCode,
    source: Source,
}

impl BundleApk {
    /// The name it has in the install session: `base.apk`, or `split_<name>.apk`.
    pub fn install_name(&self) -> String {
        match &self.split {
            None => "base.apk".to_string(),
            Some(split) => {
                let split: String = split
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                format!("split_{split}.apk")
            }
        }
    }
}

/// One expansion file of a bundle.
#[derive(Debug, Clone)]
pub struct BundleObb {
    /// Its path in the bundle.
    pub entry: String,
    /// Its file name, such as `main.42.com.example.game.obb`.
    pub name: String,
    pub size: u64,
    source: Source,
}

/// What of a bundle a device takes: indices into [`Bundle::apks`] (the base first) and
/// [`Bundle::obbs`], and the ABI chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub apks: Vec<usize>,
    pub obbs: Vec<usize>,
    pub abi: Option<String>,
}

/// A temporary file, removed when dropped.
#[derive(Debug)]
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// An app's description before the base APK is read.
fn placeholder_info() -> ApkInfo {
    ApkInfo {
        package: String::new(),
        label: None,
        version_code: 0,
        version_name: None,
        min_sdk: 0,
        target_sdk: 0,
        permissions: Vec::new(),
        icon: None,
        signer: None,
        abis: Vec::new(),
        size: 0,
    }
}

/// A split-APK bundle on the host, opened to be read and installed.
#[derive(Debug)]
pub struct Bundle {
    pub format: Format,
    /// The bundle's file name, without any folder.
    pub name: String,
    /// What the base APK says about the app. Its `abis` are those of the whole bundle.
    pub base: ApkInfo,
    pub apks: Vec<BundleApk>,
    pub obbs: Vec<BundleObb>,
    path: PathBuf,
    /// The folder unpacked APKs go in.
    tmp: PathBuf,
    temp: Vec<TempFile>,
}

/// Reads `length` bytes of a file from `start`, as a file of its own.
struct Window {
    file: File,
    start: u64,
    length: u64,
    at: u64,
    placed: bool,
}

impl Window {
    fn open(path: &Path, start: u64, length: u64) -> io::Result<Window> {
        Ok(Window {
            file: File::open(path)?,
            start,
            length,
            at: 0,
            placed: false,
        })
    }
}

impl Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.length - self.at;
        if left == 0 || buf.is_empty() {
            return Ok(0);
        }
        if !self.placed {
            self.file.seek(SeekFrom::Start(self.start + self.at))?;
            self.placed = true;
        }
        let wanted = (buf.len() as u64).min(left) as usize;
        let read = self.file.read(&mut buf[..wanted])?;
        self.at += read as u64;
        Ok(read)
    }
}

impl Seek for Window {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::End(delta) => i128::from(self.length) + i128::from(delta),
            SeekFrom::Current(delta) => i128::from(self.at) + i128::from(delta),
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a seek before the start",
            ));
        }
        self.at = (target as u64).min(self.length);
        self.placed = false;
        Ok(self.at)
    }
}

/// An expansion file's content, unpacked on another thread as it is read.
struct Inflating {
    pieces: mpsc::Receiver<Result<Vec<u8>, String>>,
    piece: Vec<u8>,
    at: usize,
}

impl Inflating {
    fn start(input: Window) -> Inflating {
        let (tx, pieces) = mpsc::sync_channel::<Result<Vec<u8>, String>>(4);
        thread::spawn(move || {
            let result = {
                let mut sink = |piece: &[u8]| -> Result<bool, String> {
                    tx.send(Ok(piece.to_vec()))
                        .map(|()| true)
                        .map_err(|_| "the install stopped".to_string())
                };
                inflate::inflate(BufReader::new(input), &mut sink)
            };
            if let Err(why) = result {
                let _ = tx.send(Err(why));
            }
        });
        Inflating {
            pieces,
            piece: Vec::new(),
            at: 0,
        }
    }
}

impl Read for Inflating {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.piece.len() {
            match self.pieces.recv() {
                Ok(Ok(piece)) => {
                    self.piece = piece;
                    self.at = 0;
                }
                Ok(Err(why)) => return Err(io::Error::new(io::ErrorKind::InvalidData, why)),
                Err(_) => return Ok(0),
            }
        }
        let count = buf.len().min(self.piece.len() - self.at);
        buf[..count].copy_from_slice(&self.piece[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

/// Whether `name` is an APK of a bundle that a device may install: an `.apk` anywhere but in
/// bundletool's folders of standalone, instant and system APKs, which are other ways to ship
/// the same app.
fn is_bundled_apk(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".apk")
        && !lower.ends_with('/')
        && !["standalones/", "instant/", "system/", "archive/"]
            .iter()
            .any(|folder| lower.starts_with(folder))
}

fn is_obb(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".obb")
}

/// What a file is, from the entries of the ZIP archive it is, or why it is none of the
/// files taken.
pub fn kind_of(entries: &[Entry]) -> Result<FileKind, String> {
    let has = |name: &str| entries.iter().any(|entry| entry.name == name);
    if has("AndroidManifest.xml") {
        return Ok(FileKind::Apk);
    }
    if has("BundleConfig.pb") || has("base/manifest/AndroidManifest.xml") {
        return Ok(FileKind::AppBundle);
    }
    let apks = entries.iter().any(|entry| is_bundled_apk(&entry.name));
    let format = if has("manifest.json") {
        Format::Xapk
    } else if has("toc.pb") {
        Format::Apks
    } else if has("info.json") {
        Format::Apkm
    } else if apks {
        Format::Apks
    } else {
        return Err(NOT_AN_APP.into());
    };
    if entries.iter().any(Entry::encrypted) {
        return Ok(FileKind::EncryptedBundle);
    }
    if !apks {
        // An old APKMirror bundle keeps its APKs under other names, encrypted.
        return if format == Format::Apkm {
            Ok(FileKind::EncryptedBundle)
        } else {
            Err("the bundle holds no APK".into())
        };
    }
    Ok(FileKind::Bundle(format))
}

/// Why a file is not installed whose content is none of the files taken.
const NOT_AN_APP: &str = "the file is not an APK file or a bundle of APKs (.xapk, .apks or .apkm)";

/// What the file at `path` is, from its content.
pub fn detect(path: &Path) -> Result<FileKind, String> {
    let file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let archive = Archive::open(BufReader::new(file)).map_err(|why| {
        if why.contains("not a ZIP archive") {
            NOT_AN_APP.to_string()
        } else {
            why
        }
    })?;
    kind_of(&archive.entries)
}

/// What a split's manifest says of it.
struct SplitManifest {
    package: String,
    split: Option<String>,
    version_code: i64,
}

fn split_manifest<R: Read + Seek>(archive: &mut Archive<R>) -> Result<SplitManifest, String> {
    let entry = archive
        .entry("AndroidManifest.xml")
        .cloned()
        .ok_or("it has no AndroidManifest.xml, so it is not an APK")?;
    let data = archive.read(&entry, MAX_MANIFEST)?;
    let document = axml::parse(&data).map_err(|why| format!("AndroidManifest.xml: {why}"))?;
    let root = document
        .elements
        .first()
        .filter(|element| element.name == "manifest")
        .ok_or("AndroidManifest.xml has no manifest element")?;
    let package = root
        .string(0, "package", &document.pool)
        .filter(|package| apk::valid_package_name(package))
        .ok_or("AndroidManifest.xml names no valid package")?;
    let split = root
        .string(0, "split", &document.pool)
        .filter(|split| !split.is_empty());
    let version_code = i64::from(root.integer(attr::VERSION_CODE, "versionCode").unwrap_or(0))
        | (i64::from(
            root.integer(attr::VERSION_CODE_MAJOR, "versionCodeMajor")
                .unwrap_or(0),
        ) << 32);
    Ok(SplitManifest {
        package,
        split,
        version_code,
    })
}

/// The native libraries an APK holds, by ABI.
fn libraries_of(entries: &[Entry]) -> NativeLibraries {
    let mut libraries = NativeLibraries::new();
    for entry in entries {
        libraries.add_entry(&entry.name);
    }
    libraries
}

/// A file name in `tmp` that no other process and no other unpacked APK uses.
fn temp_path(tmp: &Path) -> PathBuf {
    tmp.join(format!(
        "dsa-bundle-{}-{}.apk",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ))
}

impl Bundle {
    /// Opens the bundle at `path` and reads every APK in it, refusing a file that is not a
    /// bundle taken here, a bundle Android would refuse, or one whose splits disagree.
    /// Compressed APKs are unpacked into files in `tmp`, which is made when needed.
    pub fn open(path: &Path, tmp: &Path) -> Result<Bundle, Refusal> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Refusal::Invalid(format!("{} names no file", path.display())))?
            .to_string();
        let file = File::open(path)
            .map_err(|e| Refusal::Invalid(format!("cannot read {}: {e}", path.display())))?;
        let mut archive = Archive::open(BufReader::new(file)).map_err(Refusal::Invalid)?;
        let format = match kind_of(&archive.entries).map_err(Refusal::Invalid)? {
            FileKind::Bundle(format) => format,
            FileKind::AppBundle => return Err(Refusal::AppBundle),
            FileKind::EncryptedBundle => return Err(Refusal::Encrypted),
            FileKind::Apk => {
                return Err(Refusal::Invalid(
                    "the file is one APK, not a bundle of them".into(),
                ))
            }
        };
        let mut bundle = Bundle {
            format,
            name,
            base: placeholder_info(),
            apks: Vec::new(),
            obbs: Vec::new(),
            path: path.to_path_buf(),
            tmp: tmp.to_path_buf(),
            temp: Vec::new(),
        };
        let entries = archive.entries.clone();
        for entry in &entries {
            if entry.name.ends_with('/') {
                continue;
            }
            let apk_entry = is_bundled_apk(&entry.name);
            if !apk_entry && !is_obb(&entry.name) {
                continue;
            }
            let (method, offset, length) = archive.data_span(entry).map_err(Refusal::Invalid)?;
            let source = match method {
                0 => Source::Stored {
                    offset,
                    length: entry.size(),
                },
                8 => Source::Deflated { offset, length },
                other => {
                    return Err(Refusal::Invalid(format!(
                        "{} is compressed in a way bundles do not use (method {other})",
                        entry.name
                    )))
                }
            };
            if apk_entry {
                let source = match source {
                    Source::Deflated { offset, length } => Source::Unpacked(bundle.unpack(
                        &entry.name,
                        offset,
                        length,
                        entry.size(),
                    )?),
                    source => source,
                };
                let apk = bundle.read_apk(&entry.name, entry.size(), source)?;
                bundle.apks.push(apk);
            } else {
                let file_name = entry.name.rsplit('/').next().unwrap_or("").to_string();
                apk::check_file_name(&file_name)
                    .map_err(|why| Refusal::Invalid(format!("{}: {why}", entry.name)))?;
                if bundle
                    .obbs
                    .iter()
                    .any(|obb| obb.name.eq_ignore_ascii_case(&file_name))
                {
                    return Err(Refusal::Invalid(format!(
                        "the bundle holds two expansion files called {file_name}"
                    )));
                }
                bundle.obbs.push(BundleObb {
                    entry: entry.name.clone(),
                    name: file_name,
                    size: entry.size(),
                    source,
                });
            }
        }
        bundle.check()?;
        Ok(bundle)
    }

    /// Unpacks the compressed entry `entry` into a temporary file of its own.
    fn unpack(
        &mut self,
        entry: &str,
        offset: u64,
        length: u64,
        size: u64,
    ) -> Result<PathBuf, Refusal> {
        let failed = |why: String| Refusal::Invalid(format!("{entry} cannot be unpacked: {why}"));
        std::fs::create_dir_all(&self.tmp).map_err(|e| failed(e.to_string()))?;
        let path = temp_path(&self.tmp);
        let file = File::create(&path).map_err(|e| failed(e.to_string()))?;
        self.temp.push(TempFile(path.clone()));
        let input = Window::open(&self.path, offset, length).map_err(|e| failed(e.to_string()))?;
        let mut out = BufWriter::new(file);
        let mut written = 0u64;
        {
            let mut sink = |piece: &[u8]| -> Result<bool, String> {
                written += piece.len() as u64;
                if written > size {
                    return Err(format!("it holds more than the {size} bytes it says"));
                }
                out.write_all(piece).map_err(|e| e.to_string())?;
                Ok(true)
            };
            inflate::inflate(BufReader::new(input), &mut sink).map_err(failed)?;
        }
        out.flush().map_err(|e| failed(e.to_string()))?;
        if written != size {
            return Err(failed(format!(
                "it holds {written} bytes, not the {size} it says"
            )));
        }
        Ok(path)
    }

    /// A reader of the APK or expansion file at `source`, from its start.
    fn open_source(&self, source: &Source) -> io::Result<Box<dyn ReadSeek>> {
        Ok(match source {
            Source::Stored { offset, length } => {
                Box::new(BufReader::new(Window::open(&self.path, *offset, *length)?))
            }
            Source::Unpacked(path) => Box::new(BufReader::new(File::open(path)?)),
            Source::Deflated { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "a compressed entry is read only in order",
                ))
            }
        })
    }

    fn read_apk(&self, entry: &str, size: u64, source: Source) -> Result<BundleApk, Refusal> {
        let invalid = |why: String| Refusal::Invalid(format!("{entry}: {why}"));
        let reader = self
            .open_source(&source)
            .map_err(|e| invalid(e.to_string()))?;
        let mut archive = match Archive::open(reader) {
            Ok(archive) => archive,
            // An APKMirror bundle whose APKs are encrypted keeps their names; what is in
            // them is no ZIP archive.
            Err(why) if self.format == Format::Apkm && why.contains("not a ZIP archive") => {
                return Err(Refusal::Encrypted)
            }
            Err(why) => return Err(invalid(why)),
        };
        let manifest = split_manifest(&mut archive).map_err(invalid)?;
        let libraries = libraries_of(&archive.entries);
        let code = NativeCode::of_archive(&mut archive);
        let signer = signing::signer(&mut archive).map(|signer| signer.sha256());
        Ok(BundleApk {
            entry: entry.to_string(),
            kind: classify(manifest.split.as_deref()),
            split: manifest.split,
            size,
            package: manifest.package,
            version_code: manifest.version_code,
            signer,
            libraries,
            code,
            source,
        })
    }

    /// Checks that the APKs make one app: one base, and every split of its package, version
    /// and signer, none twice. Puts the base first and reads its description of the app.
    fn check(&mut self) -> Result<(), Refusal> {
        let bases: Vec<usize> = (0..self.apks.len())
            .filter(|&index| self.apks[index].kind == SplitKind::Base)
            .collect();
        let base = match bases.as_slice() {
            [base] => *base,
            [] => return Err(Refusal::Invalid("the bundle holds no base APK".into())),
            [first, second, ..] => {
                return Err(Refusal::Invalid(format!(
                    "the bundle holds two base APKs, {} and {}",
                    self.apks[*first].entry, self.apks[*second].entry
                )))
            }
        };
        let base_apk = self.apks.remove(base);
        self.apks.insert(0, base_apk);
        let base = &self.apks[0];
        for (index, apk) in self.apks.iter().enumerate().skip(1) {
            if apk.package != base.package {
                return Err(Refusal::Invalid(format!(
                    "{} is an APK of {}, not of {}",
                    apk.entry, apk.package, base.package
                )));
            }
            if apk.version_code != base.version_code {
                return Err(Refusal::Invalid(format!(
                    "{} is of version {} of the app, and the base APK of version {}",
                    apk.entry, apk.version_code, base.version_code
                )));
            }
            if apk.signer != base.signer {
                return Err(Refusal::SignatureMismatch {
                    split: apk.entry.clone(),
                });
            }
            if self.apks[..index]
                .iter()
                .any(|other| other.split == apk.split)
            {
                return Err(Refusal::Invalid(format!(
                    "the bundle holds the split {} twice",
                    apk.split.as_deref().unwrap_or_default()
                )));
            }
        }
        let reader = self
            .open_source(&self.apks[0].source)
            .map_err(|e| Refusal::Invalid(format!("{}: {e}", self.apks[0].entry)))?;
        let mut base_info = ApkInfo::read_from(reader, self.apks[0].size)
            .map_err(|why| Refusal::Invalid(format!("{}: {why}", self.apks[0].entry)))?;
        let libraries = NativeLibraries::merged(self.apks.iter().map(|apk| &apk.libraries));
        base_info.abis = libraries.abis().map(str::to_string).collect();
        self.base = base_info;
        // Every ABI split counts: a bundle without any 64-bit code the guest runs is refused,
        // whichever splits a device would take.
        if let Some(why) = NativeCode::merged(self.apks.iter().map(|apk| &apk.code)).wont_run() {
            return Err(Refusal::UnsupportedAbi(why));
        }
        Ok(())
    }

    /// The size of every file of the install `selection` makes, together.
    pub fn total_size(&self, selection: &Selection) -> u64 {
        selection
            .apks
            .iter()
            .map(|&index| self.apks[index].size)
            .chain(selection.obbs.iter().map(|&index| self.obbs[index].size))
            .sum()
    }

    /// The native libraries of every APK of the bundle together, whichever splits a device
    /// takes.
    pub fn all_libraries(&self) -> NativeLibraries {
        NativeLibraries::merged(self.apks.iter().map(|apk| &apk.libraries))
    }

    /// What `device` takes from the bundle.
    pub fn select(&self, device: &Device) -> Selection {
        let libraries = self.all_libraries();
        let abi = if device.abis.first().is_some_and(|abi| abi == X86_64) {
            let translation = device.abis.iter().any(|abi| abi == ARM64);
            native_abi::choose(&libraries, || translation).map(str::to_string)
        } else {
            device.abis.iter().find(|abi| libraries.has(abi)).cloned()
        };
        let mut best_density: BTreeMap<&str, u32> = BTreeMap::new();
        for apk in &self.apks {
            if let SplitKind::Density { of, dpi } = &apk.kind {
                let nearer = match best_density.get(of.as_str()) {
                    None => true,
                    Some(best) => {
                        let distance = |d: u32| d.abs_diff(device.density_dpi);
                        distance(*dpi) < distance(*best)
                            || (distance(*dpi) == distance(*best) && dpi > best)
                    }
                };
                if nearer {
                    best_density.insert(of.as_str(), *dpi);
                }
            }
        }
        let wants_language = |language: &str| {
            language == "en" || device.languages.iter().any(|wanted| wanted == language)
        };
        let apks = (0..self.apks.len())
            .filter(|&index| match &self.apks[index].kind {
                SplitKind::Base | SplitKind::Feature | SplitKind::Config { .. } => true,
                SplitKind::Abi { abi: split_abi, .. } => abi.as_deref() == Some(split_abi.as_str()),
                SplitKind::Density { of, dpi } => best_density.get(of.as_str()) == Some(dpi),
                SplitKind::Language { language, .. } => wants_language(language.as_str()),
            })
            .collect();
        Selection {
            apks,
            obbs: (0..self.obbs.len()).collect(),
            abi,
        }
    }

    /// The native libraries of the APKs `selection` takes, together, from which the service
    /// chooses the ABI the app is installed for.
    pub fn libraries(&self, selection: &Selection) -> NativeLibraries {
        let chosen = |&index: &usize| &self.apks[index].libraries;
        NativeLibraries::merged(selection.apks.iter().map(chosen))
    }

    /// The files of the install `selection` makes, in the order their bytes are sent.
    pub fn parts(&self, selection: &Selection) -> Vec<InstallPart> {
        let mut parts = Vec::new();
        for &index in &selection.apks {
            parts.push(InstallPart {
                name: self.apks[index].install_name(),
                size: self.apks[index].size,
                obb: false,
            });
        }
        for &index in &selection.obbs {
            parts.push(InstallPart {
                name: self.obbs[index].name.clone(),
                size: self.obbs[index].size,
                obb: true,
            });
        }
        parts
    }

    /// The bytes of the install `selection` makes: every file of [`Bundle::parts`], one after
    /// another.
    pub fn content(&self, selection: &Selection) -> Content<'_> {
        let mut sources = Vec::new();
        for &index in &selection.apks {
            sources.push((self.apks[index].source.clone(), self.apks[index].size));
        }
        for &index in &selection.obbs {
            sources.push((self.obbs[index].source.clone(), self.obbs[index].size));
        }
        Content {
            bundle: self,
            sources,
            next: 0,
            current: None,
        }
    }
}

/// A file to install: one APK, or a bundle of them.
pub enum InstallFile {
    Apk(ApkFile),
    /// Boxed: a bundle holds its whole table of parts, many times the size of one APK.
    Bundle(Box<Bundle>),
}

impl InstallFile {
    /// Opens the file at `path` as what it is. A file named `.apk` is an APK, which Android
    /// judges as before; any other is what its content says, so a bundle with another
    /// extension, or an APK named as a bundle, installs too. An App Bundle, an encrypted
    /// bundle and a bundle whose splits disagree are refused with their reasons, and a file
    /// that is none of these with [`Refusal::NotAnApp`], before Android is started for it.
    /// So is an app whose native code is none the guest runs ([`Refusal::UnsupportedAbi`]).
    /// A bundle's compressed APKs are unpacked into `tmp`.
    pub fn open(path: &Path, tmp: &Path) -> Result<InstallFile, Refusal> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Refusal::NotAnApp(format!("{} names no file", path.display())))?;
        if apk::check_name(name).is_ok() {
            let apk = ApkFile::open(path).map_err(Refusal::NotAnApp)?;
            check_native_code(path)?;
            return Ok(InstallFile::Apk(apk));
        }
        match detect(path).map_err(Refusal::NotAnApp)? {
            FileKind::Apk => {
                let stem = Path::new(name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("app");
                let apk =
                    ApkFile::open_as(path, &format!("{stem}.apk")).map_err(Refusal::NotAnApp)?;
                check_native_code(path)?;
                Ok(InstallFile::Apk(apk))
            }
            FileKind::Bundle(_) => {
                Bundle::open(path, tmp).map(|bundle| InstallFile::Bundle(Box::new(bundle)))
            }
            FileKind::AppBundle => Err(Refusal::AppBundle),
            FileKind::EncryptedBundle => Err(Refusal::Encrypted),
        }
    }

    /// The native libraries of every APK the file holds, whichever splits a device takes.
    pub fn libraries(&self) -> NativeLibraries {
        match self {
            InstallFile::Apk(apk) => apk.native_libraries.clone(),
            InstallFile::Bundle(bundle) => bundle.all_libraries(),
        }
    }

    /// Refuses an app whose 64-bit native code is for arm64-v8a only while the running Android
    /// does not translate ARM code, as `translation` (what the service says of ARM translation,
    /// `None` when it cannot be asked) says, with [`Refusal::TranslationOff`]. A bundle is
    /// judged by all its APKs: a device that takes no arm64-v8a split would otherwise get the
    /// app without its native code. When the running Android's ABI list is not known, the app
    /// is left to Android.
    pub fn check_translation(
        &self,
        translation: Option<&ArmTranslationStatus>,
    ) -> Result<(), Refusal> {
        let translates = translation
            .and_then(|status| status.guest.as_ref())
            .filter(|guest| !guest.abilist.is_empty())
            .map(|guest| native_abi::translates_arm64(&guest.abilist));
        native_abi::install_abi(&self.libraries(), || translates)
            .map(|_| ())
            .map_err(Refusal::TranslationOff)
    }
}

/// Refuses the APK at `path` when its native code is none the guest runs
/// ([`NativeCode::wont_run`]). A file that cannot be read as an APK passes: Android judges it,
/// or reading it for the install says why.
pub fn check_native_code(path: &Path) -> Result<(), Refusal> {
    let Ok(file) = File::open(path) else {
        return Ok(());
    };
    match NativeCode::read(BufReader::new(file)).map(|code| code.wont_run()) {
        Ok(Some(why)) => Err(Refusal::UnsupportedAbi(why)),
        _ => Ok(()),
    }
}

/// A reader that can seek, as an APK's archive needs.
trait ReadSeek: Read + Seek {}

impl<T: Read + Seek> ReadSeek for T {}

/// The bytes of an install from a bundle, file after file.
pub struct Content<'a> {
    bundle: &'a Bundle,
    sources: Vec<(Source, u64)>,
    next: usize,
    current: Option<io::Take<Box<dyn Read + Send>>>,
}

impl Read for Content<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(current) = &mut self.current {
                let read = current.read(buf)?;
                if read > 0 || buf.is_empty() {
                    return Ok(read);
                }
                if current.limit() > 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "a file of the bundle ends early",
                    ));
                }
                self.current = None;
            }
            let Some((source, size)) = self.sources.get(self.next).cloned() else {
                return Ok(0);
            };
            self.next += 1;
            let reader: Box<dyn Read + Send> = match source {
                Source::Stored { offset, length } => {
                    Box::new(Window::open(&self.bundle.path, offset, length)?)
                }
                Source::Unpacked(path) => Box::new(File::open(path)?),
                Source::Deflated { offset, length } => {
                    let compressed = Window::open(&self.bundle.path, offset, length)?;
                    Box::new(Inflating::start(compressed))
                }
            };
            self.current = Some(reader.take(size));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk_info::axml::TYPE_INT_DEC;
    use crate::apk_info::fixture::{document, notes_apk, signing_block, v2_value, Attr, El};
    use crate::arm_translation::google::inflate::fixture::deflate;

    /// The certificate every APK of a good bundle is signed with, and another one.
    const KEY: &[u8] = b"\x30\x03\x02\x01\x07";
    const OTHER_KEY: &[u8] = b"\x30\x03\x02\x01\x09";

    /// One entry of an archive: its name, its content, and whether it is deflated.
    type Item<'a> = (&'a str, &'a [u8], bool);

    fn stored<'a>(name: &'a str, data: &'a [u8]) -> Item<'a> {
        (name, data, false)
    }

    fn deflated<'a>(name: &'a str, data: &'a [u8]) -> Item<'a> {
        (name, data, true)
    }

    /// A ZIP archive of `entries` with general purpose `flags`, and `before_directory` between
    /// the last entry and the central directory, where an APK keeps its signing block.
    fn zip(entries: &[Item<'_>], flags: u16, before_directory: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (name, data, compress) in entries {
            let (method, body) = if *compress {
                (8u16, deflate(data))
            } else {
                (0u16, data.to_vec())
            };
            let offset = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&body);
            directory.extend_from_slice(b"PK\x01\x02");
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&20u16.to_le_bytes());
            directory.extend_from_slice(&flags.to_le_bytes());
            directory.extend_from_slice(&method.to_le_bytes());
            directory.extend_from_slice(&[0, 0, 0, 0]);
            directory.extend_from_slice(&0u32.to_le_bytes());
            directory.extend_from_slice(&(body.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&[0; 12]);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        out.extend_from_slice(before_directory);
        let directory_offset = out.len() as u32;
        out.extend_from_slice(&directory);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        out.extend_from_slice(&directory_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// A bundle of `entries`.
    fn bundle_of(entries: &[Item<'_>]) -> Vec<u8> {
        zip(entries, 0, b"")
    }

    /// An APK of com.example.notes version 42: the base when `split` is none, else that split,
    /// holding the native libraries `libraries` and signed with `key`.
    fn apk(split: Option<&str>, libraries: &[&str], key: &[u8]) -> Vec<u8> {
        let mut attributes = vec![
            Attr::Typed("versionCode", attr::VERSION_CODE, TYPE_INT_DEC, 42),
            Attr::Str("package", 0, "com.example.notes"),
        ];
        if let Some(split) = split {
            attributes.push(Attr::Str("split", 0, split));
        }
        let manifest = document(&[El("manifest", attributes, false)]);
        let mut entries = vec![stored("AndroidManifest.xml", &manifest)];
        for library in libraries {
            entries.push(stored(library, b"\x7fELF"));
        }
        let block = signing_block(&[(0x7109_871a, &v2_value(key)[..])]);
        zip(&entries, 0, &block)
    }

    fn base() -> Vec<u8> {
        apk(None, &[], KEY)
    }

    fn split(name: &str, libraries: &[&str]) -> Vec<u8> {
        apk(Some(name), libraries, KEY)
    }

    /// Writes `data` as `name` in a folder of the test's own.
    fn file(test: &str, name: &str, data: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-bundle-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, data).unwrap();
        path
    }

    fn names(bundle: &Bundle, selection: &Selection) -> Vec<String> {
        bundle
            .parts(selection)
            .into_iter()
            .map(|part| part.name)
            .collect()
    }

    fn device(abis: &[&str], density_dpi: u32, languages: &[&str]) -> Device {
        Device {
            abis: abis.iter().map(|abi| abi.to_string()).collect(),
            density_dpi,
            languages: languages
                .iter()
                .map(|language| language.to_string())
                .collect(),
        }
    }

    const OBB: &[u8] = b"expansion file of the notes app";
    const OBB_PATH: &str = "Android/obb/com.example.notes/main.42.com.example.notes.obb";

    /// An APKPure bundle of the notes app, its files deflated or stored.
    fn xapk(compress: bool) -> Vec<u8> {
        let base = base();
        let x86_64 = split("config.x86_64", &["lib/x86_64/libnotes.so"]);
        let arm64 = split("config.arm64_v8a", &["lib/arm64-v8a/libnotes.so"]);
        let mdpi = split("config.mdpi", &[]);
        let xhdpi = split("config.xhdpi", &[]);
        let xxhdpi = split("config.xxhdpi", &[]);
        let en = split("config.en", &[]);
        let ko = split("config.ko", &[]);
        let de = split("config.de", &[]);
        let entry = if compress { deflated } else { stored };
        bundle_of(&[
            entry("manifest.json", b"{\"package_name\":\"com.example.notes\"}"),
            stored("icon.png", b"\x89PNG"),
            entry("com.example.notes.apk", &base),
            entry("config.x86_64.apk", &x86_64),
            entry("config.arm64_v8a.apk", &arm64),
            entry("config.mdpi.apk", &mdpi),
            entry("config.xhdpi.apk", &xhdpi),
            entry("config.xxhdpi.apk", &xxhdpi),
            entry("config.en.apk", &en),
            entry("config.ko.apk", &ko),
            entry("config.de.apk", &de),
            entry(OBB_PATH, OBB),
        ])
    }

    #[test]
    fn fr11_a_bundle_is_told_by_its_content_not_its_name() {
        let test = "content";
        let renamed = file(test, "notes.zip", &xapk(false));
        assert_eq!(detect(&renamed), Ok(FileKind::Bundle(Format::Xapk)));
        let apk_named_as_bundle = file(test, "notes.xapk", &notes_apk());
        assert_eq!(detect(&apk_named_as_bundle), Ok(FileKind::Apk));
        let base = base();
        let toc = bundle_of(&[
            stored("toc.pb", b""),
            stored("splits/base-master.apk", &base),
        ]);
        let toc = file(test, "a.bin", &toc);
        assert_eq!(detect(&toc), Ok(FileKind::Bundle(Format::Apks)));
        let info = bundle_of(&[stored("info.json", b"{}"), stored("base.apk", &base)]);
        let info = file(test, "b.bin", &info);
        assert_eq!(detect(&info), Ok(FileKind::Bundle(Format::Apkm)));
        let sai = file(test, "c.bin", &bundle_of(&[stored("base.apk", &base)]));
        assert_eq!(detect(&sai), Ok(FileKind::Bundle(Format::Apks)));
        let aab = bundle_of(&[
            stored("BundleConfig.pb", b""),
            stored("base/manifest/AndroidManifest.xml", b""),
        ]);
        assert_eq!(detect(&file(test, "d.bin", &aab)), Ok(FileKind::AppBundle));
        let junk = file(test, "e.bin", b"not an archive at all, really");
        assert!(detect(&junk).is_err());
        let other = bundle_of(&[stored("readme.txt", b"hello")]);
        assert!(detect(&file(test, "f.bin", &other)).is_err());
    }

    #[test]
    fn fr11_an_xapk_on_an_x86_64_device_takes_its_abi_density_and_languages_and_the_obb() {
        let path = file("xapk", "notes.xapk", &xapk(false));
        let bundle = Bundle::open(&path, &std::env::temp_dir()).unwrap();
        assert_eq!(bundle.format, Format::Xapk);
        assert_eq!(bundle.base.package, "com.example.notes");
        assert_eq!(bundle.base.version_code, 42);
        assert_eq!(bundle.base.abis, [ARM64, X86_64]);
        assert_eq!(bundle.apks.len(), 9);
        let selection = bundle.select(&device(&[X86_64], 320, &["ko"]));
        assert_eq!(selection.abi.as_deref(), Some(X86_64));
        assert_eq!(
            names(&bundle, &selection),
            [
                "base.apk",
                "split_config.x86_64.apk",
                "split_config.xhdpi.apk",
                "split_config.en.apk",
                "split_config.ko.apk",
                "main.42.com.example.notes.obb",
            ]
        );
        let parts = bundle.parts(&selection);
        assert!(parts[..5].iter().all(|part| !part.obb));
        assert!(parts[5].obb);
        assert_eq!(parts[5].size, OBB.len() as u64);
        let total: u64 = parts.iter().map(|part| part.size).sum();
        assert_eq!(bundle.total_size(&selection), total);
        assert_eq!(apk::check_parts(&bundle.name, &parts), Ok(total));
        // The bytes are each file's, one after another, the expansion file last.
        let mut content = Vec::new();
        bundle
            .content(&selection)
            .read_to_end(&mut content)
            .unwrap();
        assert_eq!(content.len() as u64, total);
        assert!(content.starts_with(&base()));
        assert!(content.ends_with(OBB));
    }

    #[test]
    fn fr11_with_translation_on_whole_x86_64_libraries_still_win() {
        let path = file("translation-on", "notes.xapk", &xapk(false));
        let bundle = Bundle::open(&path, &std::env::temp_dir()).unwrap();
        let selection = bundle.select(&device(&[X86_64, ARM64], 480, &[]));
        assert_eq!(selection.abi.as_deref(), Some(X86_64));
        assert_eq!(
            names(&bundle, &selection),
            [
                "base.apk",
                "split_config.x86_64.apk",
                "split_config.xxhdpi.apk",
                "split_config.en.apk",
                "main.42.com.example.notes.obb",
            ]
        );
    }

    #[test]
    fn fr11_of_two_densities_as_near_the_higher_one_is_taken() {
        let path = file("density", "notes.xapk", &xapk(false));
        let bundle = Bundle::open(&path, &std::env::temp_dir()).unwrap();
        let names = names(&bundle, &bundle.select(&device(&[X86_64], 240, &["de"])));
        let has = |name: &str| names.iter().any(|taken| taken == name);
        assert!(has("split_config.xhdpi.apk"), "{names:?}");
        assert!(!has("split_config.mdpi.apk"), "{names:?}");
        assert!(has("split_config.de.apk"), "{names:?}");
    }

    /// A bundletool bundle whose x86_64 split lacks a library its arm64-v8a split has.
    fn apks() -> Vec<u8> {
        let master = base();
        let x86_64 = split("config.x86_64", &["lib/x86_64/libmain.so"]);
        let arm64 = split(
            "config.arm64_v8a",
            &["lib/arm64-v8a/libmain.so", "lib/arm64-v8a/libextra.so"],
        );
        let xxhdpi = split("config.xxhdpi", &[]);
        let standalone = base();
        bundle_of(&[
            stored("toc.pb", b"\x0a\x00"),
            stored("splits/base-master.apk", &master),
            stored("splits/base-x86_64.apk", &x86_64),
            stored("splits/base-arm64_v8a.apk", &arm64),
            stored("splits/base-xxhdpi.apk", &xxhdpi),
            stored("standalones/standalone-x86_64_xxhdpi.apk", &standalone),
        ])
    }

    #[test]
    fn fr11_incomplete_x86_64_libraries_take_arm64_only_while_translation_is_on() {
        let path = file("apks", "notes.apks", &apks());
        let bundle = Bundle::open(&path, &std::env::temp_dir()).unwrap();
        assert_eq!(bundle.format, Format::Apks);
        // The standalone APK is another way to ship the app, not a second base.
        assert_eq!(bundle.apks.len(), 4);
        let on = bundle.select(&device(&[X86_64, ARM64], 480, &[]));
        assert_eq!(on.abi.as_deref(), Some(ARM64));
        assert_eq!(
            names(&bundle, &on),
            [
                "base.apk",
                "split_config.arm64_v8a.apk",
                "split_config.xxhdpi.apk",
            ]
        );
        let off = bundle.select(&device(&[X86_64], 480, &[]));
        assert_eq!(off.abi.as_deref(), Some(X86_64));
        assert_eq!(
            names(&bundle, &off),
            [
                "base.apk",
                "split_config.x86_64.apk",
                "split_config.xxhdpi.apk",
            ]
        );
    }

    #[test]
    fn fr11_an_apkm_on_an_arm64_device_takes_the_arm64_splits_and_every_feature() {
        let base = base();
        let arm64 = split("config.arm64_v8a", &["lib/arm64-v8a/libnotes.so"]);
        let x86_64 = split("config.x86_64", &["lib/x86_64/libnotes.so"]);
        let hdpi = split("config.hdpi", &[]);
        let camera = split("camera", &[]);
        let camera_arm64 = split("camera.config.arm64_v8a", &["lib/arm64-v8a/libcam.so"]);
        let camera_x86_64 = split("camera.config.x86_64", &["lib/x86_64/libcam.so"]);
        let apkm = bundle_of(&[
            stored("info.json", b"{\"pname\":\"com.example.notes\"}"),
            stored("base.apk", &base),
            stored("split_config.arm64_v8a.apk", &arm64),
            stored("split_config.x86_64.apk", &x86_64),
            stored("split_config.hdpi.apk", &hdpi),
            stored("split_camera.apk", &camera),
            stored("split_camera.config.arm64_v8a.apk", &camera_arm64),
            stored("split_camera.config.x86_64.apk", &camera_x86_64),
        ]);
        let path = file("apkm", "notes.apkm", &apkm);
        let bundle = Bundle::open(&path, &std::env::temp_dir()).unwrap();
        assert_eq!(bundle.format, Format::Apkm);
        let selection = bundle.select(&device(&[ARM64], 480, &["ko"]));
        assert_eq!(selection.abi.as_deref(), Some(ARM64));
        assert_eq!(
            names(&bundle, &selection),
            [
                "base.apk",
                "split_config.arm64_v8a.apk",
                "split_config.hdpi.apk",
                "split_camera.apk",
                "split_camera.config.arm64_v8a.apk",
            ]
        );
    }

    #[test]
    fn fr11_a_split_signed_with_another_key_is_refused() {
        let base = base();
        let x86_64 = split("config.x86_64", &["lib/x86_64/libnotes.so"]);
        let foreign = apk(Some("config.ko"), &[], OTHER_KEY);
        let xapk = bundle_of(&[
            stored("manifest.json", b"{}"),
            stored("com.example.notes.apk", &base),
            stored("config.x86_64.apk", &x86_64),
            stored("config.ko.apk", &foreign),
        ]);
        let path = file("signature", "notes.xapk", &xapk);
        let refusal = Bundle::open(&path, &std::env::temp_dir()).unwrap_err();
        assert_eq!(
            refusal,
            Refusal::SignatureMismatch {
                split: "config.ko.apk".into()
            }
        );
        assert_eq!(refusal.code(), "split-signature-mismatch");
        let text = refusal.to_string();
        assert!(text.contains("another certificate"), "{text}");
    }

    #[test]
    fn fr11_an_app_bundle_is_refused_with_its_reason() {
        let aab = bundle_of(&[
            stored("BundleConfig.pb", b"\x0a\x00"),
            stored("base/manifest/AndroidManifest.xml", b"\x03\x00"),
            stored("base/dex/classes.dex", b"dex\n"),
        ]);
        let path = file("aab", "notes.aab", &aab);
        let refusal = Bundle::open(&path, &std::env::temp_dir()).unwrap_err();
        assert_eq!(refusal, Refusal::AppBundle);
        assert_eq!(refusal.code(), "app-bundle");
        let text = refusal.to_string();
        assert!(text.contains("App Bundle"), "{text}");
        assert!(text.contains("another key"), "{text}");
        assert!(text.contains("as an APK"), "{text}");
    }

    #[test]
    fn fr11_an_encrypted_apkm_is_refused_with_its_reason() {
        let base = base();
        let entries = [stored("info.json", b"{}"), stored("base.apk", &base)];
        let flagged = zip(&entries, 1, b"");
        let path = file("encrypted", "flagged.apkm", &flagged);
        assert_eq!(detect(&path), Ok(FileKind::EncryptedBundle));
        assert_eq!(
            Bundle::open(&path, &std::env::temp_dir()).unwrap_err(),
            Refusal::Encrypted
        );
        // Older APKMirror bundles keep the APKs' names but not what is in them.
        let scrambled = bundle_of(&[
            stored("info.json", b"{}"),
            stored("base.apk", b"\x13\x37 sealed with a key only the maker has"),
        ]);
        let path = file("encrypted", "scrambled.apkm", &scrambled);
        let refusal = Bundle::open(&path, &std::env::temp_dir()).unwrap_err();
        assert_eq!(refusal, Refusal::Encrypted);
        assert_eq!(refusal.code(), "encrypted-bundle");
        assert!(refusal.to_string().contains("encrypted"), "{refusal}");
    }

    #[test]
    fn fr11_compressed_apks_and_expansion_files_are_unpacked() {
        let compressed = Bundle::open(
            &file("deflated", "notes.xapk", &xapk(true)),
            &std::env::temp_dir(),
        )
        .unwrap();
        let stored = Bundle::open(
            &file("deflated", "stored.xapk", &xapk(false)),
            &std::env::temp_dir(),
        )
        .unwrap();
        let device = device(&[X86_64], 320, &["ko"]);
        let selection = compressed.select(&device);
        let reference = stored.select(&device);
        assert_eq!(names(&compressed, &selection), names(&stored, &reference));
        let mut content = Vec::new();
        compressed
            .content(&selection)
            .read_to_end(&mut content)
            .unwrap();
        let mut expected = Vec::new();
        stored
            .content(&reference)
            .read_to_end(&mut expected)
            .unwrap();
        assert_eq!(content, expected);
        // The unpacked APKs go when the bundle does.
        let unpacked: Vec<PathBuf> = compressed.temp.iter().map(|temp| temp.0.clone()).collect();
        assert!(!unpacked.is_empty());
        assert!(unpacked.iter().all(|path| path.exists()));
        drop(compressed);
        assert!(unpacked.iter().all(|path| !path.exists()));
    }

    #[test]
    fn fr11_a_bundle_of_two_bases_or_none_is_refused() {
        let base = base();
        let two = bundle_of(&[stored("base.apk", &base), stored("other.apk", &base)]);
        let refusal =
            Bundle::open(&file("bases", "two.apks", &two), &std::env::temp_dir()).unwrap_err();
        assert_eq!(refusal.code(), "invalid-bundle");
        assert!(refusal.to_string().contains("two base APKs"), "{refusal}");
        let en = split("config.en", &[]);
        let none = bundle_of(&[stored("split.apk", &en)]);
        let refusal =
            Bundle::open(&file("bases", "none.apks", &none), &std::env::temp_dir()).unwrap_err();
        assert!(refusal.to_string().contains("no base APK"), "{refusal}");
    }

    #[test]
    fn fr11_a_file_is_installed_as_what_its_content_says() {
        let test = "install-file";
        let bundle = file(test, "notes.bin", &xapk(false));
        assert!(matches!(
            InstallFile::open(&bundle, &std::env::temp_dir()),
            Ok(InstallFile::Bundle(_))
        ));
        match InstallFile::open(
            &file(test, "notes.xapk", &notes_apk()),
            &std::env::temp_dir(),
        ) {
            Ok(InstallFile::Apk(apk)) => assert_eq!(apk.name, "notes.apk"),
            Ok(InstallFile::Bundle(_)) => panic!("an APK was read as a bundle"),
            Err(refusal) => panic!("an APK named as a bundle was refused: {refusal}"),
        }
        // A file named .apk goes to Android as before, which judges what is in it.
        let named = file(test, "broken.apk", b"PK\x03\x04 not really");
        assert!(matches!(
            InstallFile::open(&named, &std::env::temp_dir()),
            Ok(InstallFile::Apk(_))
        ));
        let aab = bundle_of(&[stored("BundleConfig.pb", b"")]);
        let aab = InstallFile::open(&file(test, "notes.aab", &aab), &std::env::temp_dir());
        assert_eq!(aab.err(), Some(Refusal::AppBundle));
        match InstallFile::open(&file(test, "notes.zip", b"PK"), &std::env::temp_dir()) {
            Err(Refusal::NotAnApp(why)) => assert!(why.contains("not an APK file"), "{why}"),
            Err(other) => panic!("expected the file to be no APK, got {other}"),
            Ok(_) => panic!("a file that is no APK was taken"),
        }
    }

    #[test]
    fn fr11_an_app_with_only_32_bit_code_is_refused_before_android_sees_it() {
        let test = "no-64-bit";
        // One APK whose code is for x86 only.
        let x86 = apk(None, &["lib/x86/libnotes.so"], KEY);
        match InstallFile::open(&file(test, "notes.apk", &x86), &std::env::temp_dir()) {
            Err(Refusal::UnsupportedAbi(why)) => {
                assert_eq!(
                    why,
                    WontRun::ThirtyTwoBitOnly {
                        abis: vec!["x86".into()]
                    }
                );
                let text = why.to_string();
                assert!(text.contains("32-bit only (x86)"), "{text}");
                assert!(text.contains("x86_64 or arm64-v8a build"), "{text}");
            }
            Err(other) => panic!("expected the 32-bit app to be refused for it, got {other}"),
            Ok(_) => panic!("an app with 32-bit code only was taken"),
        }
        // A bundle whose ABI splits are 32-bit only, whichever a device would take.
        let base = base();
        let arm32 = split("config.armeabi_v7a", &["lib/armeabi-v7a/libnotes.so"]);
        let x86 = split("config.x86", &["lib/x86/libnotes.so"]);
        let bundle = bundle_of(&[
            stored("base.apk", &base),
            stored("split_config.armeabi_v7a.apk", &arm32),
            stored("split_config.x86.apk", &x86),
        ]);
        let refusal = InstallFile::open(&file(test, "notes.apks", &bundle), &std::env::temp_dir())
            .err()
            .expect("refused");
        assert_eq!(refusal.code(), "unsupported-abi");
        assert!(
            refusal.to_string().contains("armeabi-v7a, x86"),
            "{refusal}"
        );
        // With a 64-bit split beside them it installs, as before.
        let wide = split("config.x86_64", &["lib/x86_64/libnotes.so"]);
        let bundle = bundle_of(&[
            stored("base.apk", &base),
            stored("split_config.x86.apk", &x86),
            stored("split_config.x86_64.apk", &wide),
        ]);
        assert!(matches!(
            InstallFile::open(&file(test, "both.apks", &bundle), &std::env::temp_dir()),
            Ok(InstallFile::Bundle(_))
        ));
    }

    /// A base APK shaped as Chrome's Trichrome builds are: it uses the Trichrome library, asks
    /// to run as 32-bit code when `prefers_32_bit`, and holds `files`, each with content or
    /// empty.
    fn trichrome_base(prefers_32_bit: bool, files: &[(&str, bool)]) -> Vec<u8> {
        let mut application = Vec::new();
        if prefers_32_bit {
            application.push(Attr::Typed(
                "use32bitAbi",
                0x0101_0515,
                crate::apk_info::axml::TYPE_INT_BOOLEAN,
                u32::MAX,
            ));
        }
        let manifest = document(&[
            El(
                "manifest",
                vec![
                    Attr::Typed("versionCode", attr::VERSION_CODE, TYPE_INT_DEC, 42),
                    Attr::Str("package", 0, "com.example.notes"),
                ],
                false,
            ),
            El("application", application, false),
            El(
                "uses-static-library",
                vec![Attr::Str(
                    "name",
                    attr::NAME,
                    "com.google.android.trichromelibrary",
                )],
                true,
            ),
        ]);
        let mut entries = vec![stored("AndroidManifest.xml", &manifest)];
        for (name, content) in files {
            let data: &[u8] = if *content { b"\x7fELF" } else { b"" };
            entries.push(stored(name, data));
        }
        let block = signing_block(&[(0x7109_871a, &v2_value(KEY)[..])]);
        zip(&entries, 0, &block)
    }

    #[test]
    fn fr11_a_build_made_to_run_as_32_bit_code_is_refused_and_a_64_bit_one_is_not() {
        let test = "trichrome";
        // Chrome's 32-bit build: its own code under the 32-bit ABI, an empty placeholder under
        // the 64-bit one, and use32bitAbi.
        let thirty_two = trichrome_base(
            true,
            &[
                ("lib/armeabi-v7a/libchrome.so", true),
                ("lib/arm64-v8a/libplaceholder.so", false),
            ],
        );
        let thirty_two = bundle_of(&[stored("com.android.chrome.apk", &thirty_two)]);
        let refusal = InstallFile::open(
            &file(test, "chrome.xapk", &thirty_two),
            &std::env::temp_dir(),
        )
        .err()
        .expect("refused");
        assert_eq!(
            refusal,
            Refusal::UnsupportedAbi(WontRun::ThirtyTwoBitOnly {
                abis: vec!["armeabi-v7a".into()]
            })
        );
        // Its 64-bit build: the code under x86_64, a placeholder under x86. It installs.
        let sixty_four = trichrome_base(
            false,
            &[
                ("lib/x86/libplaceholder.so", false),
                ("lib/x86_64/libchrome.so", true),
            ],
        );
        assert!(matches!(
            InstallFile::open(
                &file(test, "chrome.apk", &sixty_four),
                &std::env::temp_dir()
            ),
            Ok(InstallFile::Apk(_))
        ));
        // Trichrome WebView: 32-bit code of its own, its 64-bit code in the library it uses,
        // a placeholder under x86_64, and no use32bitAbi. It installs too.
        let webview = trichrome_base(
            false,
            &[
                ("lib/x86/libwebview.so", true),
                ("lib/x86_64/libplaceholder.so", false),
            ],
        );
        assert!(matches!(
            InstallFile::open(&file(test, "webview.apk", &webview), &std::env::temp_dir()),
            Ok(InstallFile::Apk(_))
        ));
    }

    #[test]
    fn fr11_split_names_say_what_part_of_the_app_they_are() {
        let base_config = String::new;
        assert_eq!(classify(None), SplitKind::Base);
        assert_eq!(classify(Some("camera")), SplitKind::Feature);
        assert_eq!(
            classify(Some("config.arm64_v8a")),
            SplitKind::Abi {
                of: base_config(),
                abi: ARM64.into()
            }
        );
        assert_eq!(
            classify(Some("camera.config.x86_64")),
            SplitKind::Abi {
                of: "camera".into(),
                abi: X86_64.into()
            }
        );
        assert_eq!(
            classify(Some("config.xxhdpi")),
            SplitKind::Density {
                of: base_config(),
                dpi: 480
            }
        );
        assert_eq!(
            classify(Some("config.420dpi")),
            SplitKind::Density {
                of: base_config(),
                dpi: 420
            }
        );
        assert_eq!(
            classify(Some("config.zh_TW")),
            SplitKind::Language {
                of: base_config(),
                language: "zh".into()
            }
        );
        assert_eq!(
            classify(Some("config.astc")),
            SplitKind::Config { of: base_config() }
        );
    }

    #[test]
    fn fr25_a_bundle_with_arm64_code_only_is_refused_with_its_reason_while_translation_is_off() {
        use crate::arm_translation::GuestArmTranslation;
        let base = base();
        let arm64 = split("config.arm64_v8a", &["lib/arm64-v8a/libnotes.so"]);
        let path = file(
            "arm64-only",
            "notes.xapk",
            &bundle_of(&[
                stored("manifest.json", b"{\"package_name\":\"com.example.notes\"}"),
                stored("com.example.notes.apk", &base),
                stored("config.arm64_v8a.apk", &arm64),
            ]),
        );
        let bundle = InstallFile::open(&path, &std::env::temp_dir()).unwrap();
        let status = |abilist: &str| ArmTranslationStatus {
            package: None,
            default: None,
            translator: None,
            enabled: true,
            guest: Some(GuestArmTranslation {
                abilist: abilist.into(),
                ..Default::default()
            }),
            restart_needed: false,
        };
        // An x86_64 device takes no split of it: without the check it would get the app
        // without its native code.
        let refusal = bundle
            .check_translation(Some(&status("x86_64")))
            .unwrap_err();
        assert_eq!(refusal.code(), "translation-off");
        assert!(
            refusal.to_string().contains("ARM translation is not on"),
            "{refusal}"
        );
        assert_eq!(
            bundle.check_translation(Some(&status("x86_64,arm64-v8a"))),
            Ok(())
        );
        // An arm64 device runs it as it is; an Android that cannot be asked judges it.
        assert_eq!(bundle.check_translation(Some(&status("arm64-v8a"))), Ok(()));
        assert_eq!(bundle.check_translation(Some(&status(""))), Ok(()));
        assert_eq!(bundle.check_translation(None), Ok(()));
    }

    #[test]
    fn fr25_a_device_lists_x86_64_first_and_arm64_only_with_translation() {
        assert_eq!(Device::abis_for(None, false, false), [X86_64]);
        assert_eq!(Device::abis_for(None, false, true), [X86_64, ARM64]);
        assert_eq!(Device::abis_for(None, true, false), [ARM64]);
        assert_eq!(
            Device::abis_for(Some("x86_64,arm64-v8a,x86"), false, false),
            [X86_64, ARM64]
        );
        assert_eq!(Device::abis_for(Some(""), false, true), [X86_64, ARM64]);
    }

    #[test]
    fn fr11_the_device_languages_are_the_hosts_display_languages() {
        let locales = ["ko-KR", "en-US", "ko", "pt_BR.UTF-8", "C"].map(String::from);
        assert_eq!(Device::languages_of(&locales), ["ko", "en", "pt"]);
    }
}
