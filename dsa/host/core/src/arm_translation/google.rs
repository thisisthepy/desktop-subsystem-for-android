//! Google's ARM translator, which the user may fetch from Google's SDK repository.
//!
//! DSA never ships or redistributes it. When the user asks for it, their own computer reads
//! Google's list of emulator system images, downloads the Google APIs x86_64 image for the
//! Android version their Android runs, checks it against the SHA-1 and size the list gives,
//! and takes out only the translator's files. Those become an ordinary translation package,
//! the same folder `dsa arm-translation install` takes, so the rest of the path (the checks,
//! the copy to the user's Android, the boot flag) is the one every package goes through. The
//! downloaded image is deleted once the package is made, whether or not that worked.
//!
//! What is taken, as the image has it under `system/`:
//!
//! ```text
//! lib64/libndk_translation.so        the native bridge
//! lib64/libndk_translation_*.so      the libraries it loads (proxies of host libraries)
//! lib64/arm64/**, bin/arm64/**       the ARM64 guest libraries, linker and app_process
//! bin/ndk_translation_*arm64*        the program runner binfmt_misc starts
//! etc/binfmt_misc/arm64_exe, _dyn    the binfmt_misc rules
//! etc/ld.config.arm64.txt            the guest linker's namespaces
//! etc/cpuinfo.arm64.txt, etc/berberis/*arm64*   what ARM apps read as /proc/cpuinfo
//! ```
//!
//! Nothing 32-bit is taken: the guest has no 32-bit zygote. Nor is the image's init script for
//! the translator: the guest's own registers the binfmt_misc rules once the package is laid
//! over `/system`.

mod disk;
mod ext4;
pub(crate) mod inflate;
mod sha1;

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{open_package, Package, ARM64, IMAGE, MANIFEST};
use crate::apk_info::zip::Archive;
use crate::erofs::Builder;
use crate::update::store::Fetch;

use ext4::{Ext4, Kind};

/// Google's repository of Google APIs system images: its lists, and the archives they name.
pub const REPOSITORY: &str = "https://dl.google.com/android/repository/sys-img/google_apis/";
/// The lists in the repository, newest schema first. Each lists the same images; an older one
/// is read only when a newer one cannot be.
pub const LISTS: [&str; 3] = ["sys-img2-5.xml", "sys-img2-4.xml", "sys-img2-3.xml"];
/// Google's terms for what the SDK repository gives.
pub const TERMS: &str = "https://developer.android.com/studio/terms";
/// What the package is called.
pub const NAME: &str = "Google ARM translator";
/// Its licence, as the package names it.
pub const LICENSE: &str = "Android SDK License";
/// The translator's native bridge library.
pub const LIBRARY: &str = "libndk_translation.so";
/// The name the guest's properties give the bridge.
const BRIDGE_LINK: &str = "system/lib64/libdsa_native_bridge.so";
/// The most the translator's files may hold together.
const MAX_TAKEN: u64 = 512 << 20;

/// One system image Google's list offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkImage {
    /// The list's name for it, such as `system-images;android-36;google_apis;x86_64`.
    pub path: String,
    /// Its API level as the list gives it: `36`, `36.1`, `37.0`.
    pub api_level: String,
    /// Its revision, such as `7`.
    pub revision: String,
    /// Where to download it.
    pub url: String,
    /// Its size in bytes, and its SHA-1 in lower-case hexadecimal.
    pub size: u64,
    pub sha1: String,
    /// The licence it is offered under, as the list names it.
    pub license: String,
}

/// An image as the list describes it, with what decides whether it is the one.
#[derive(Debug, Clone)]
struct Listed {
    image: SdkImage,
    tag: String,
    abi: String,
    translated: Option<String>,
    base: bool,
    stable: bool,
}

/// The API level of an Android major version: Android 13 is API 33, and each major since
/// adds one.
pub fn api_level(android_major: u32) -> Option<u32> {
    (13..100)
        .contains(&android_major)
        .then_some(android_major + 20)
}

/// The text inside the first `<name>` element of `xml`, or "" for an empty element.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}");
    let mut from = 0;
    loop {
        let at = from + xml[from..].find(&open)?;
        let after = at + open.len();
        let next = xml[after..].chars().next()?;
        if next == '>' || next == '/' || next.is_whitespace() {
            let end = after + xml[after..].find('>')?;
            if xml[..end].ends_with('/') {
                return Some("");
            }
            let close = format!("</{name}>");
            let inner = end + 1;
            let stop = inner + xml[inner..].find(&close)?;
            return Some(&xml[inner..stop]);
        }
        from = after;
    }
}

/// The value of the attribute `name` in the first `<tag ...>` start tag of `xml`.
fn attribute(xml: &str, tag: &str, name: &str) -> Option<String> {
    let open = format!("<{tag} ");
    let at = xml.find(&open)? + open.len();
    let end = at + xml[at..].find('>')?;
    let start_tag = &xml[at..end];
    let key = format!("{name}=\"");
    let value = start_tag.find(&key)? + key.len();
    let stop = value + start_tag[value..].find('"')?;
    Some(start_tag[value..stop].to_string())
}

fn text(xml: &str, name: &str) -> Option<String> {
    let value = element(xml, name)?.trim();
    let value = value.replace("&lt;", "<").replace("&gt;", ">");
    Some(value.replace("&amp;", "&"))
}

/// Where an archive the list names is: a file name beside the list in `repository`, or a full
/// address on Google's download server. Anything else is not taken.
fn download_url(repository: &str, url: &str) -> Option<String> {
    if url.starts_with("https://dl.google.com/") && !url.contains("..") {
        return Some(url.to_string());
    }
    let plain = url
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let usable = plain && url.ends_with(".zip") && !url.starts_with('.');
    usable.then(|| format!("{repository}{url}"))
}

/// The revision as the list writes it: major, then minor and micro when given.
fn revision(xml: &str) -> Option<String> {
    let revision = element(xml, "revision")?;
    let mut parts = vec![text(revision, "major")?];
    for name in ["minor", "micro"] {
        if let Some(part) = text(revision, name) {
            parts.push(part);
        }
    }
    let numbers = parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()));
    numbers.then(|| parts.join("."))
}

/// The complete archive of a package that is the same for every host system.
fn archive(repository: &str, xml: &str) -> Option<(String, u64, String)> {
    let archives = element(xml, "archives")?;
    let mut rest = archives;
    while let Some(one) = element(rest, "archive") {
        let after = rest.find("</archive>")? + "</archive>".len();
        rest = &rest[after..];
        if element(one, "host-os").is_some() {
            continue;
        }
        let complete = element(one, "complete")?;
        let size = text(complete, "size")?.parse().ok()?;
        if attribute(complete, "checksum", "type").as_deref() != Some("sha1") {
            continue;
        }
        let sha1 = text(complete, "checksum")?.to_ascii_lowercase();
        let hex = sha1.len() == 40 && sha1.chars().all(|c| c.is_ascii_hexdigit());
        let url = download_url(repository, &text(complete, "url")?)?;
        if hex {
            return Some((url, size, sha1));
        }
    }
    None
}

/// The packages of a Google system image list in `repository`. A package the list describes
/// in a way this does not follow is left out.
fn parse(repository: &str, xml: &str) -> Vec<Listed> {
    let mut listed = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<remotePackage ") {
        let Some(end) = rest[at..].find("</remotePackage>") else {
            break;
        };
        let package = &rest[at..at + end];
        rest = &rest[at + end..];
        if let Some(one) = parse_package(repository, package) {
            listed.push(one);
        }
    }
    listed
}

fn parse_package(repository: &str, package: &str) -> Option<Listed> {
    let path = attribute(package, "remotePackage", "path")?;
    let details = element(package, "type-details")?;
    let tag = text(element(details, "tag")?, "id")?;
    let abi = text(details, "abi").or_else(|| text(details, "abis"))?;
    let (url, size, sha1) = archive(repository, package)?;
    let channel = attribute(package, "channelRef", "ref");
    let image = SdkImage {
        path,
        api_level: text(details, "api-level")?,
        revision: revision(package)?,
        url,
        size,
        sha1,
        license: attribute(package, "uses-license", "ref").unwrap_or_default(),
    };
    Some(Listed {
        image,
        tag,
        abi,
        translated: text(details, "translatedAbis"),
        base: text(details, "base-extension").as_deref() != Some("false"),
        stable: channel.is_none_or(|channel| channel == "channel-0"),
    })
}

/// An API level as the list writes it, as numbers: `36` is (36, 0), `36.1` is (36, 1).
fn api_numbers(api_level: &str) -> Option<(u32, u32)> {
    let (major, minor) = api_level.split_once('.').unwrap_or((api_level, "0"));
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn revision_numbers(revision: &str) -> Vec<u32> {
    revision.split('.').filter_map(|p| p.parse().ok()).collect()
}

/// The image to take for Android `android_major` from what a list offers: the Google APIs
/// x86_64 image of the same API level, on the stable channel, that translates ARM64. Of
/// several, the base release (the lowest minor API level) and then the newest revision.
fn choose(listed: &[Listed], android_major: u32) -> Result<SdkImage, String> {
    let Some(api) = api_level(android_major) else {
        return Err(format!("Android {android_major} is not known to DSA"));
    };
    let translates = |l: &Listed| {
        let abis = l.translated.as_deref().unwrap_or(ARM64);
        abis.split(',').any(|abi| abi.trim() == ARM64)
    };
    let mut fitting: Vec<(u32, Vec<u32>, &SdkImage)> = Vec::new();
    for one in listed {
        let Some((major, minor)) = api_numbers(&one.image.api_level) else {
            continue;
        };
        let kind = one.tag == "google_apis" && one.abi == "x86_64";
        if kind && one.base && one.stable && major == api && translates(one) {
            let revision = revision_numbers(&one.image.revision);
            fitting.push((minor, revision, &one.image));
        }
    }
    fitting.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    match fitting.first() {
        Some((_, _, image)) => Ok((*image).clone()),
        None => Err(format!(
            "Google's SDK repository lists no Google APIs x86_64 image with the ARM translator \
             for Android {android_major} (API {api}); nothing was changed"
        )),
    }
}

/// Reads the list in `repository` ([`REPOSITORY`], or a mirror of it) and picks the image for
/// Android `android_major`.
pub fn find_image(
    fetch: &dyn Fetch,
    repository: &str,
    android_major: u32,
) -> Result<SdkImage, String> {
    let mut repository = repository.to_string();
    if !repository.ends_with('/') {
        repository.push('/');
    }
    let mut problem = String::new();
    for name in LISTS {
        let list = format!("{repository}{name}");
        let mut body = Vec::new();
        if let Err(why) = fetch.fetch(&list, &mut body) {
            problem = why;
            continue;
        }
        let listed = parse(&repository, &String::from_utf8_lossy(&body));
        if listed.is_empty() {
            problem = format!("{list} lists no system image DSA can read");
            continue;
        }
        return choose(&listed, android_major);
    }
    Err(format!(
        "Google's SDK repository could not be read ({problem}); nothing was changed"
    ))
}

/// How making the package goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Bytes of the image downloaded so far, of the whole.
    Downloading { done: u64, total: u64 },
    /// Taking the system partition out of the image.
    Extracting,
    /// Taking the translator's files out of the system partition.
    Building,
}

/// A folder that is removed, with everything in it, when this goes.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The download being written: to the file, through the checksum, counted.
struct Download<'a> {
    file: BufWriter<File>,
    hasher: sha1::Sha1,
    done: u64,
    total: u64,
    progress: &'a mut dyn FnMut(Progress),
}

impl Write for Download<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write_all(bytes)?;
        self.hasher.update(bytes);
        self.done += bytes.len() as u64;
        let (done, total) = (self.done, self.total);
        (self.progress)(Progress::Downloading { done, total });
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Whether the file at `path` (under the image's `system/`) is one of the translator's.
pub fn wanted(path: &str) -> bool {
    if path.starts_with("lib64/arm64/") || path.starts_with("bin/arm64/") {
        return true;
    }
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    match folder {
        "lib64" => name.starts_with("libndk_translation") && name.ends_with(".so"),
        "bin" => name.starts_with("ndk_translation") && name.contains("arm64"),
        "etc" => matches!(name, "ld.config.arm64.txt" | "cpuinfo.arm64.txt"),
        "etc/binfmt_misc" => matches!(name, "arm64_exe" | "arm64_dyn"),
        "etc/berberis" => name.contains("arm64"),
        _ => false,
    }
}

/// Whether the folder at `path` (under the image's `system/`) can hold translator files.
fn searched(path: &str) -> bool {
    let arm64 = |root: &str| path == root || path.starts_with(&format!("{root}/"));
    let fixed = ["lib64", "bin", "etc", "etc/binfmt_misc", "etc/berberis"];
    fixed.contains(&path) || arm64("lib64/arm64") || arm64("bin/arm64")
}

/// What the files taken so far hold, kept under [`MAX_TAKEN`].
struct Taken {
    tree: Builder,
    bytes: u64,
}

fn take_folder<R: Read + Seek>(
    fs: &mut Ext4<R>,
    folder: &ext4::Inode,
    path: &str,
    taken: &mut Taken,
    depth: u32,
) -> Result<(), String> {
    if depth > 8 {
        return Err(format!("the image's system/{path} is nested too deeply"));
    }
    for (name, number) in fs.list(folder)? {
        let child_path = format!("{path}/{name}");
        let child = fs.inode(number)?;
        let target = format!("system/{child_path}");
        match child.kind() {
            Kind::Folder if searched(&child_path) => {
                take_folder(fs, &child, &child_path, taken, depth + 1)?;
            }
            Kind::File if wanted(&child_path) => {
                taken.bytes += child.size;
                if taken.bytes > MAX_TAKEN {
                    return Err("the translator's files are larger than any translator".into());
                }
                let data = fs.read(&child)?;
                taken.tree.file(&target, child.permissions(), data)?;
            }
            Kind::Link if wanted(&child_path) => {
                let link = String::from_utf8_lossy(&fs.read(&child)?).into_owned();
                // Only a link to a file beside it stays inside the package.
                if !link.contains('/') && link != ".." && link != "." {
                    taken.tree.link(&target, &link)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The program a binfmt_misc rule starts (its sixth field), as a path in the package.
fn interpreter(rule: &[u8]) -> Option<String> {
    let rule = String::from_utf8_lossy(rule);
    let program = rule.trim().split(':').nth(6)?;
    program.strip_prefix('/').map(str::to_string)
}

/// Checks that what was taken is a working translator: the bridge, the ARM64 libraries, both
/// binfmt_misc rules and the program they start.
fn check_taken(taken: &Taken, rules: &[Vec<u8>]) -> Result<(), String> {
    let files = taken.tree.files();
    let has = |path: &str| files.iter().any(|file| file == path);
    let missing = |what: &str| format!("the image has no ARM translator ({what} is missing)");
    let bridge = format!("system/lib64/{LIBRARY}");
    if !has(&bridge) {
        return Err(missing(&bridge));
    }
    let in_arm64 = |file: &String| file.starts_with("system/lib64/arm64/");
    if !files.iter().any(in_arm64) {
        return Err(missing("system/lib64/arm64"));
    }
    for rule in ["arm64_exe", "arm64_dyn"] {
        let path = format!("system/etc/binfmt_misc/{rule}");
        if !has(&path) {
            return Err(missing(&path));
        }
    }
    for rule in rules {
        match interpreter(rule) {
            Some(program) if has(&program) => {}
            Some(program) => return Err(missing(&program)),
            None => return Err(missing("the program a binfmt_misc rule starts")),
        }
    }
    Ok(())
}

/// The image's build ID, from its `build.prop`.
fn build_id(build_prop: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(build_prop);
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(key))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    value("ro.system.build.id=").or_else(|| value("ro.build.id="))
}

/// The package's version: the image's build, API level and revision.
fn version(image: &SdkImage, build: Option<&str>) -> String {
    let short = format!("API {}, r{}", image.api_level, image.revision);
    match build {
        Some(build) if build.len() + short.len() + 3 <= 64 => format!("{build} ({short})"),
        _ => short,
    }
}

fn io(what: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", what.display())
}

/// Takes the system partition out of the image archive at `zip`, into `out`.
fn extract_system(zip: &Path, out: &Path) -> Result<(), String> {
    let file = File::open(zip).map_err(io(zip))?;
    let mut archive = Archive::open(BufReader::new(file))?;
    let is_system = |name: &str| name == "system.img" || name.ends_with("/system.img");
    let entry = archive.entries.iter().find(|entry| is_system(&entry.name));
    let entry = entry.ok_or("the image archive holds no system.img")?;
    let entry = entry.clone();
    let (method, offset, length) = archive.data_span(&entry)?;
    let mut reader = archive.into_inner();
    reader.seek(SeekFrom::Start(offset)).map_err(io(zip))?;
    let input = reader.take(length);
    let output = File::create(out).map_err(io(out))?;
    let mut extract = disk::Extract::new(BufWriter::new(output), entry.size());
    match method {
        0 => {
            let mut input = input;
            let mut piece = vec![0u8; 1 << 20];
            loop {
                let read = input.read(&mut piece).map_err(io(zip))?;
                if read == 0 || !extract.take(&piece[..read])? {
                    break;
                }
            }
        }
        8 => {
            let mut sink = |piece: &[u8]| extract.take(piece);
            inflate::inflate(input, &mut sink)?;
        }
        other => return Err(format!("system.img uses compression method {other}")),
    }
    let (output, size) = extract.finish()?;
    let output = output.into_inner().map_err(|e| io(out)(e.into_error()))?;
    output.set_len(size).map_err(io(out))?;
    Ok(())
}

/// Takes the translator's files out of the system partition at `partition`.
fn take_translator(partition: &Path) -> Result<(Taken, Option<String>), String> {
    let file = File::open(partition).map_err(io(partition))?;
    let mut fs = Ext4::open(BufReader::new(file))?;
    // The partition's root is `/` with the system folder in it, or the system folder itself.
    let root = if fs.find("system/lib64")?.is_some() {
        "system/"
    } else {
        ""
    };
    let mut taken = Taken {
        tree: Builder::new(),
        bytes: 0,
    };
    for top in ["lib64", "bin", "etc"] {
        if let Some(folder) = fs.find(&format!("{root}{top}"))? {
            if folder.kind() == Kind::Folder {
                take_folder(&mut fs, &folder, top, &mut taken, 0)?;
            }
        }
    }
    let mut rules = Vec::new();
    for rule in ["arm64_exe", "arm64_dyn"] {
        if let Some(inode) = fs.find(&format!("{root}etc/binfmt_misc/{rule}"))? {
            rules.push(fs.read(&inode)?);
        }
    }
    check_taken(&taken, &rules)?;
    let build = match fs.find(&format!("{root}build.prop"))? {
        Some(inode) if inode.kind() == Kind::File => build_id(&fs.read(&inode)?),
        _ => None,
    };
    Ok((taken, build))
}

fn build(
    fetch: &dyn Fetch,
    image: &SdkImage,
    work: &Path,
    out: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Package, String> {
    let zip = work.join("image.zip");
    let file = File::create(&zip).map_err(io(&zip))?;
    let mut download = Download {
        file: BufWriter::new(file),
        hasher: sha1::Sha1::new(),
        done: 0,
        total: image.size,
        progress,
    };
    fetch
        .fetch(&image.url, &mut download)
        .map_err(|why| format!("downloading {} failed: {why}", image.url))?;
    download.flush().map_err(io(&zip))?;
    let Download {
        file,
        hasher,
        done,
        progress,
        ..
    } = download;
    drop(file);
    if done != image.size {
        return Err(format!(
            "the download of {} is {done} bytes, but Google's list says {}; nothing was changed",
            image.url, image.size
        ));
    }
    let digest = sha1::hex(&hasher.finish());
    if digest != image.sha1 {
        return Err(format!(
            "the download of {} has the SHA-1 {digest}, but Google's list says {}; nothing \
             was changed",
            image.url, image.sha1
        ));
    }
    progress(Progress::Extracting);
    let partition = work.join("system.part");
    extract_system(&zip, &partition)?;
    // The image is no longer needed; it is the largest file here.
    let _ = fs::remove_file(&zip);
    progress(Progress::Building);
    let (mut taken, build) = take_translator(&partition)?;
    let _ = fs::remove_file(&partition);
    taken.tree.link(BRIDGE_LINK, LIBRARY)?;
    let package = Package {
        name: NAME.into(),
        version: version(image, build.as_deref()),
        library: LIBRARY.into(),
        abis: vec![ARM64.into()],
        license: LICENSE.into(),
    };
    package.check()?;
    fs::create_dir_all(out).map_err(io(out))?;
    let system = out.join(IMAGE);
    let file = File::create(&system).map_err(io(&system))?;
    let mut writer = BufWriter::new(file);
    taken.tree.write(&mut writer)?;
    writer.flush().map_err(io(&system))?;
    drop(writer);
    let manifest = out.join(MANIFEST);
    let json = serde_json::to_string_pretty(&package).map_err(|e| e.to_string())?;
    fs::write(&manifest, json + "\n").map_err(io(&manifest))?;
    Ok(open_package(out)?.package)
}

/// Downloads `image` into the new folder `work`, checks it against Google's list, and makes a
/// translation package of the translator's files in the new folder `out`. `work` is removed
/// afterwards whatever happens, and so is `out` when the package could not be made.
pub fn make_package(
    fetch: &dyn Fetch,
    image: &SdkImage,
    work: &Path,
    out: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Package, String> {
    if out.exists() {
        return Err(format!("{} is in the way of the package", out.display()));
    }
    fs::create_dir_all(work).map_err(io(work))?;
    let scratch = Scratch(work.to_path_buf());
    let made = build(fetch, image, &scratch.0, out, progress);
    drop(scratch);
    if made.is_err() {
        let _ = fs::remove_dir_all(out);
    }
    made
}

/// Google's repository as fakes, for the tests here and of the programs that fetch from it.
#[cfg(any(test, feature = "conformance"))]
pub mod fixture {
    use super::ext4::fixture::{image as ext4_image, Item};
    use super::*;
    use crate::apk_info::fixture::archive;

    /// One `remotePackage` of a list.
    pub struct Offer<'a> {
        pub api: &'a str,
        pub tag: &'a str,
        pub abi: &'a str,
        pub revision: u32,
        pub base: bool,
        pub channel: &'a str,
        pub url: &'a str,
        pub size: u64,
        pub sha1: &'a str,
    }

    /// A stable Google APIs x86_64 image of `api` at `revision`, in the archive `url`.
    pub fn offer<'a>(api: &'a str, revision: u32, url: &'a str) -> Offer<'a> {
        Offer {
            api,
            tag: "google_apis",
            abi: "x86_64",
            revision,
            base: true,
            channel: "channel-0",
            url,
            size: 1,
            sha1: "0000000000000000000000000000000000000000",
        }
    }

    /// A list offering `offers`, written as Google writes its lists.
    pub fn list(offers: &[Offer<'_>]) -> String {
        let mut xml = String::from(
            "<?xml version='1.0' encoding='utf-8'?>\n<sys-img:sdk-sys-img \
             xmlns:sys-img=\"http://schemas.android.com/sdk/android/repo/sys-img2/05\">\n\
             <license id=\"android-sdk-license\" type=\"text\">Terms</license>\n",
        );
        for offer in offers {
            let api = offer.api;
            let tag = offer.tag;
            let abi = offer.abi;
            let revision = offer.revision;
            let base = offer.base;
            let channel = offer.channel;
            let url = offer.url;
            let size = offer.size;
            let sha1 = offer.sha1;
            xml.push_str(&format!(
                "<remotePackage path=\"system-images;android-{api};{tag};{abi}\">\n\
                 <type-details xsi:type=\"sys-img:sysImgDetailsType\">\
                 <api-level>{api}</api-level><base-extension>{base}</base-extension>\
                 <tag><id>{tag}</id><display>Google APIs</display></tag>\
                 <vendor><id>google</id></vendor><abi>{abi}</abi><abis>{abi}</abis>\
                 <translatedAbis>arm64-v8a</translatedAbis></type-details>\n\
                 <revision><major>{revision}</major></revision>\
                 <uses-license ref=\"android-sdk-license\"/><channelRef ref=\"{channel}\"/>\n\
                 <archives><archive><complete><size>{size}</size>\
                 <checksum type=\"sha1\">{sha1}</checksum><url>{url}</url></complete>\
                 </archive></archives>\n</remotePackage>\n"
            ));
        }
        xml.push_str("</sys-img:sdk-sys-img>\n");
        xml
    }

    /// Images a list offers beside the one to take: older revisions, a later minor release,
    /// extension and beta releases, other ABIs and other tags.
    pub fn decoys() -> Vec<Offer<'static>> {
        vec![
            offer("36", 6, "x86_64-36_r06.zip"),
            offer("36.1", 4, "x86_64-36.1_r04.zip"),
            Offer {
                base: false,
                ..offer("36", 9, "x86_64-36-ext19_r01.zip")
            },
            Offer {
                abi: "arm64-v8a",
                ..offer("36", 9, "arm64-v8a-36_r09.zip")
            },
            Offer {
                tag: "google_apis_playstore",
                ..offer("36", 9, "x86_64-36_ps.zip")
            },
            Offer {
                channel: "channel-3",
                ..offer("36", 10, "x86_64-36_r10.zip")
            },
            offer("37.0", 8, "x86_64-37.0_r08.zip"),
        ]
    }

    pub const RULE_EXE: &[u8] = b":arm64_exe:M::\\x7fELF\\x02::\
        /system/bin/ndk_translation_program_runner_binfmt_misc_arm64:P\n";
    pub const RULE_DYN: &[u8] = b":arm64_dyn:M::\\x7fELF\\x03::\
        /system/bin/ndk_translation_program_runner_binfmt_misc_arm64:P\n";
    pub const RUNNER: &str = "system/bin/ndk_translation_program_runner_binfmt_misc_arm64";
    pub const RUNNER_ARM: &str = "system/bin/ndk_translation_program_runner_binfmt_misc_arm";
    pub const PROXY: &str = "system/lib64/libndk_translation_proxy_libEGL.so";
    pub const BUILD_PROP: &[u8] = b"ro.system.build.id=BE2A.250530.026.F3\n";

    fn file<'a>(path: &'a str, data: &'a [u8]) -> Item<'a> {
        Item::File(path, 0o644, data)
    }

    fn program(path: &str) -> Item<'_> {
        Item::File(path, 0o755, b"\x7fELF")
    }

    /// A system partition as Google's emulator image has it: the translator among the rest.
    pub fn system_partition(with_translator: bool) -> Vec<u8> {
        let mut items = vec![
            file("system/build.prop", BUILD_PROP),
            file("system/lib64/libc.so", b"x86_64 libc"),
            file(PROXY, b"proxy"),
            file("system/lib64/arm64/libc.so", b"arm64 libc"),
            file("system/lib64/arm64/libGLESv2.so", b"gles"),
            Item::Link("system/lib64/arm64/libGLESv3.so", "libGLESv2.so"),
            Item::Link("system/lib64/arm64/libescape.so", "../../x.so"),
            program("system/bin/arm64/linker64"),
            program(RUNNER),
            program(RUNNER_ARM),
            program("system/bin/sh"),
            file("system/etc/binfmt_misc/arm64_exe", RULE_EXE),
            file("system/etc/binfmt_misc/arm64_dyn", RULE_DYN),
            file("system/etc/binfmt_misc/arm_exe", b":arm_exe:"),
            file("system/etc/ld.config.arm64.txt", b"ld"),
            file("system/etc/ld.config.arm.txt", b"ld"),
            file("system/etc/berberis/cpuinfo.arm64.txt", b"cpu"),
            file("system/etc/init/ndk_translation.rc", b"rc"),
            file("system/app/Maps/Maps.apk", b"apk"),
            Item::Folder("system/lib64/arm64/bootstrap"),
        ];
        if with_translator {
            items.push(file("system/lib64/libndk_translation.so", b"bridge"));
        }
        ext4_image(&items)
    }

    /// A zip holding `data` as `name`, deflated as Google's archives are.
    pub fn deflated_archive(name: &str, data: &[u8]) -> Vec<u8> {
        let compressed = inflate::fixture::deflate(data);
        let mut zip = archive(&[(name, &compressed[..])], b"");
        // The fixture stores entries; mark this one deflated, with its size once inflated.
        let size = (data.len() as u32).to_le_bytes();
        zip[8] = 8;
        zip[22..26].copy_from_slice(&size);
        let directory = 30 + name.len() + compressed.len();
        zip[directory + 10] = 8;
        zip[directory + 24..directory + 28].copy_from_slice(&size);
        zip
    }

    /// An emulator image archive as Google's: its disk, with the system partition, deflated.
    pub fn image_archive(with_translator: bool) -> Vec<u8> {
        let disk = disk::fixture::disk(&system_partition(with_translator));
        deflated_archive("x86_64/system.img", &disk)
    }

    /// The SHA-1 of `data`, as a list gives it.
    pub fn sha1_of(data: &[u8]) -> String {
        let mut hasher = sha1::Sha1::new();
        hasher.update(data);
        sha1::hex(&hasher.finish())
    }

    /// Writes a repository into `folder`: a list offering `zip` for each API level in `apis`
    /// (as `x86_64-<api>.zip`), among the decoys, with `sha1` (or the archive's own) as its
    /// checksum.
    pub fn write_repository(
        folder: &Path,
        zip: &[u8],
        sha1: Option<&str>,
        apis: &[&str],
    ) -> std::io::Result<()> {
        fs::create_dir_all(folder)?;
        let own = sha1_of(zip);
        let names: Vec<String> = apis.iter().map(|api| format!("x86_64-{api}.zip")).collect();
        let mut offers = decoys();
        for (api, name) in apis.iter().zip(&names) {
            offers.push(Offer {
                size: zip.len() as u64,
                sha1: sha1.unwrap_or(&own),
                ..offer(api, 7, name)
            });
            fs::write(folder.join(name), zip)?;
        }
        fs::write(folder.join(LISTS[0]), list(&offers))
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use crate::erofs::read::items;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Answers from a map of URLs, and remembers what was asked.
    #[derive(Default)]
    struct FakeFetch {
        bodies: HashMap<String, Vec<u8>>,
        asked: RefCell<Vec<String>>,
    }

    impl Fetch for FakeFetch {
        fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
            self.asked.borrow_mut().push(url.to_string());
            let body = self.bodies.get(url).ok_or("404 Not Found")?;
            out.write_all(body).map_err(|e| e.to_string())?;
            Ok(body.len() as u64)
        }
    }

    /// Google's list offering one image whose archive is `zip`, and the fetch that serves both.
    fn repository(zip: Vec<u8>, sha1: &str) -> FakeFetch {
        let mut offers = decoys();
        let name = "x86_64-36_r07.zip";
        let size = zip.len() as u64;
        offers.push(Offer {
            size,
            sha1,
            ..offer("36", 7, name)
        });
        let mut fetch = FakeFetch::default();
        let bodies = &mut fetch.bodies;
        let first = format!("{REPOSITORY}{}", LISTS[0]);
        bodies.insert(first, list(&offers).into_bytes());
        bodies.insert(format!("{REPOSITORY}{name}"), zip);
        fetch
    }

    fn folders(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("dsa-google-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        (base.join("work"), base.join("package"))
    }

    #[test]
    fn fr25_the_image_for_the_guests_android_version_is_picked_from_googles_list() {
        let mut offers = decoys();
        offers.push(offer("36", 7, "x86_64-36_r07.zip"));
        let listed = parse(REPOSITORY, &list(&offers));
        assert_eq!(listed.len(), offers.len());
        let sixteen = choose(&listed, 16).unwrap();
        assert_eq!(sixteen.url, format!("{REPOSITORY}x86_64-36_r07.zip"));
        assert_eq!(sixteen.api_level, "36");
        assert_eq!(sixteen.revision, "7");
        assert_eq!(sixteen.license, "android-sdk-license");
        let seventeen = choose(&listed, 17).unwrap();
        assert_eq!(seventeen.url, format!("{REPOSITORY}x86_64-37.0_r08.zip"));
        let none = choose(&listed, 15).unwrap_err();
        assert!(none.contains("Android 15 (API 35)"), "{none}");
        assert_eq!(api_level(16), Some(36));
        assert_eq!(api_level(9), None);
    }

    #[test]
    fn fr25_a_list_entry_pointing_elsewhere_is_not_taken() {
        for url in ["../x.zip", "https://example.com/x.zip", "x.tar", "a/b.zip"] {
            let listed = parse(REPOSITORY, &list(&[offer("36", 7, url)]));
            assert!(listed.is_empty(), "{url}");
        }
        let full = "https://dl.google.com/android/repository/x.zip";
        let listed = parse(REPOSITORY, &list(&[offer("36", 7, full)]));
        assert_eq!(listed[0].image.url, full);
    }

    #[test]
    fn fr25_only_the_translators_files_are_wanted() {
        for path in [
            "lib64/libndk_translation.so",
            "lib64/libndk_translation_proxy_libEGL.so",
            "lib64/arm64/libc.so",
            "lib64/arm64/nested/libx.so",
            "bin/arm64/linker64",
            "bin/ndk_translation_program_runner_binfmt_misc_arm64",
            "etc/binfmt_misc/arm64_exe",
            "etc/binfmt_misc/arm64_dyn",
            "etc/ld.config.arm64.txt",
            "etc/cpuinfo.arm64.txt",
            "etc/berberis/cpuinfo.arm64.txt",
        ] {
            assert!(wanted(path), "{path}");
        }
        for path in [
            "lib64/libc.so",
            "lib64/arm/libc.so",
            "bin/ndk_translation_program_runner_binfmt_misc_arm",
            "etc/binfmt_misc/arm_exe",
            "etc/ld.config.arm.txt",
            "etc/cpuinfo.arm.txt",
            "etc/init/ndk_translation.rc",
            "etc/berberis/cpuinfo.arm.txt",
            "app/Chrome/Chrome.apk",
            "build.prop",
        ] {
            assert!(!wanted(path), "{path}");
        }
    }

    #[test]
    fn fr25_the_google_translator_is_fetched_checked_and_made_into_a_package() {
        let zip = image_archive(true);
        let fetch = repository(zip.clone(), &sha1_of(&zip));
        let image = find_image(&fetch, REPOSITORY, 16).unwrap();
        let (work, out) = folders("made");
        let mut steps = Vec::new();
        let mut progress = |step: Progress| steps.push(step);
        let package = make_package(&fetch, &image, &work, &out, &mut progress).unwrap();
        assert_eq!(package.name, "Google ARM translator");
        assert_eq!(package.version, "BE2A.250530.026.F3 (API 36, r7)");
        assert_eq!(package.library, "libndk_translation.so");
        assert_eq!(package.abis, ["arm64-v8a"]);
        assert_eq!(package.license, "Android SDK License");
        assert!(steps.contains(&Progress::Extracting));
        assert!(!work.exists(), "the download is deleted");
        // The package passes the checks every package goes through.
        let opened = open_package(&out).unwrap();
        assert_eq!(opened.package, package);
        let image = fs::read(out.join(IMAGE)).unwrap();
        let found = items(&image);
        let mut files: Vec<&str> = found
            .iter()
            .filter(|item| item.mode & 0o170_000 != 0o040_000)
            .map(|item| item.path.as_str())
            .collect();
        files.sort_unstable();
        assert_eq!(
            files,
            [
                "system/bin/arm64/linker64",
                "system/bin/ndk_translation_program_runner_binfmt_misc_arm64",
                "system/etc/berberis/cpuinfo.arm64.txt",
                "system/etc/binfmt_misc/arm64_dyn",
                "system/etc/binfmt_misc/arm64_exe",
                "system/etc/ld.config.arm64.txt",
                "system/lib64/arm64/libGLESv2.so",
                "system/lib64/arm64/libGLESv3.so",
                "system/lib64/arm64/libc.so",
                "system/lib64/libdsa_native_bridge.so",
                "system/lib64/libndk_translation.so",
                "system/lib64/libndk_translation_proxy_libEGL.so",
            ]
        );
        let at = |path: &str| found.iter().find(|item| item.path == path).unwrap();
        assert_eq!(at(BRIDGE_LINK).data, b"libndk_translation.so");
        assert_eq!(at("system/lib64/arm64/libc.so").data, b"arm64 libc");
        assert_eq!(at(RUNNER).mode, 0o100_755);
        let _ = fs::remove_dir_all(out.parent().unwrap());
    }

    #[test]
    fn fr25_a_download_whose_hash_differs_from_googles_list_is_refused() {
        let wrong = "0123456789012345678901234567890123456789";
        let fetch = repository(image_archive(true), wrong);
        let image = find_image(&fetch, REPOSITORY, 16).unwrap();
        let (work, out) = folders("hash");
        let refused = make_package(&fetch, &image, &work, &out, &mut |_| {}).unwrap_err();
        assert!(refused.contains("SHA-1"), "{refused}");
        assert!(refused.contains("nothing was changed"), "{refused}");
        assert!(!out.exists(), "no package is made");
        assert!(!work.exists(), "the download is deleted");
        let _ = fs::remove_dir_all(out.parent().unwrap());
    }

    #[test]
    fn fr25_no_image_for_the_guests_version_is_refused_before_any_download() {
        let fetch = repository(b"zip".to_vec(), &sha1_of(b"zip"));
        let refused = find_image(&fetch, REPOSITORY, 15).unwrap_err();
        assert!(refused.contains("nothing was changed"), "{refused}");
        let first = format!("{REPOSITORY}{}", LISTS[0]);
        assert_eq!(*fetch.asked.borrow(), [first], "only the list is read");
        let unreachable = FakeFetch::default();
        assert!(find_image(&unreachable, REPOSITORY, 16).is_err());
        assert_eq!(unreachable.asked.borrow().len(), LISTS.len());
    }

    #[test]
    fn fr25_an_image_without_the_translator_is_refused() {
        let zip = image_archive(false);
        let fetch = repository(zip.clone(), &sha1_of(&zip));
        let image = find_image(&fetch, REPOSITORY, 16).unwrap();
        let (work, out) = folders("missing");
        let refused = make_package(&fetch, &image, &work, &out, &mut |_| {}).unwrap_err();
        assert!(refused.contains("libndk_translation.so"), "{refused}");
        assert!(!out.exists());
        assert!(!work.exists());
        let _ = fs::remove_dir_all(out.parent().unwrap());
    }

    #[test]
    fn fr25_a_repository_in_a_folder_is_read_as_googles() {
        let base = std::env::temp_dir().join(format!("dsa-google-repo-{}", std::process::id()));
        let zip = image_archive(true);
        write_repository(&base, &zip, None, &["36"]).unwrap();
        let fetch = crate::update::store::FileFetch;
        let folder = base.display().to_string();
        let image = find_image(&fetch, &folder, 16).unwrap();
        assert!(image.url.ends_with("x86_64-36.zip"), "{}", image.url);
        assert_eq!(image.sha1, sha1_of(&zip));
        let _ = fs::remove_dir_all(&base);
    }
}
