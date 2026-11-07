//! Android images side by side on disk.
//!
//! ```text
//! <root>/state.json           which image is current, which came before, which failed
//! <root>/images/<version>/    one folder per image, holding its files
//! <root>/images/.staging-*/   a download in progress
//! ```
//!
//! A new image is downloaded into a staging folder and every file checked against its
//! size and SHA-256 before the folder takes the image's name. Only then does the state
//! name it current, on trial, with the image before it kept as previous. The state file is
//! replaced atomically (written beside, then renamed), so a crash leaves either the old
//! state or the new one.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{is_plain_name, ImageEntry, Manifest};
use super::policy::{check_for, InstalledImage};
use super::versions::{self, Channel, ChooseError, VersionReport};
use super::{from_hex, to_hex, UpdateError};

/// Downloads a URL.
pub trait Fetch {
    /// Writes the body of `url` to `out`, returning how many bytes were written.
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String>;
}

/// Fetches `file:` URLs and plain paths, for a local mirror and for tests.
pub struct FileFetch;

impl Fetch for FileFetch {
    fn fetch(&self, url: &str, out: &mut dyn Write) -> Result<u64, String> {
        let path = url.strip_prefix("file://").unwrap_or(url);
        let mut file = fs::File::open(path).map_err(|e| format!("opening {path}: {e}"))?;
        io::copy(&mut file, out).map_err(|e| format!("reading {path}: {e}"))
    }
}

/// What the store records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub current: InstalledImage,
    /// The image before `current`, kept until `current` has booted.
    #[serde(default)]
    pub previous: Option<InstalledImage>,
    /// Set while `current` has not yet booted here.
    #[serde(default)]
    pub on_trial: bool,
    /// Versions that failed to boot here and are not tried again by the same build.
    #[serde(default)]
    pub failed: Vec<String>,
    /// The DSA build that saw them fail. A failure can be the host's fault rather than the
    /// image's, so a new build of the host tries those images again.
    #[serde(default)]
    pub failed_build: Option<String>,
    /// Which images this computer takes: the newest major, or one the user chose.
    #[serde(default)]
    pub channel: Channel,
    /// Set once the user agreed to erase their data to move to an older major; cleared when
    /// that image is staged.
    #[serde(default)]
    pub older_allowed: bool,
    /// The supported majors the last release list named. Empty until one was read; then this
    /// build's own list stands in.
    #[serde(default)]
    pub supported_majors: Vec<u32>,
}

/// What [`ImageStore::recover`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// The current image has its files.
    Usable,
    /// The current image was broken, and `current` replaced it.
    Moved {
        broken: InstalledImage,
        current: InstalledImage,
    },
    /// The current image is broken and no other image is usable or named.
    NothingBetter { broken: InstalledImage },
}

/// Every VHDX file starts with this signature.
const VHDX_SIGNATURE: &[u8] = b"vhdxfile";

fn is_vhdx(path: &Path) -> bool {
    let mut head = [0u8; 8];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok_and(|()| head == VHDX_SIGNATURE)
}

/// The guest architecture this host runs, as an image's `image.json` names it in `arch`:
/// `x86_64` on an x86_64 host, `arm64` on an ARM64 host. A VM runs only a guest of the
/// host's own architecture.
pub fn guest_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

/// Whether the image folder `dir` holds files an Android VM can boot from. An image with an
/// `image.json` has every file it names, at the size it names, and its disks are VHDX files;
/// when it names its `arch`, that is the host's ([`guest_arch`]). An image from before
/// images named their architecture is taken as it is: each architecture reads its own
/// image list, so it only ever downloaded images of its own architecture.
/// An older image without an `image.json` has at least one disk, and every disk in it is a
/// VHDX file.
pub fn image_is_usable(dir: &Path) -> bool {
    let is_disk = |name: &str| name.to_ascii_lowercase().ends_with(".vhdx");
    match fs::read(dir.join("image.json")) {
        Ok(text) => {
            let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&text) else {
                return false;
            };
            let Some(files) = manifest["files"].as_object() else {
                return false;
            };
            if manifest
                .get("arch")
                .is_some_and(|arch| arch.as_str() != Some(guest_arch()))
            {
                return false;
            }
            !files.is_empty()
                && files.iter().all(|(name, entry)| {
                    let path = dir.join(name);
                    let Ok(meta) = fs::metadata(&path) else {
                        return false;
                    };
                    is_plain_name(name)
                        && meta.is_file()
                        && entry["size"].as_u64().is_none_or(|size| size == meta.len())
                        && (!is_disk(name) || is_vhdx(&path))
                })
        }
        Err(_) => {
            let Ok(entries) = fs::read_dir(dir) else {
                return false;
            };
            let disks: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| is_disk(&path.file_name().unwrap_or_default().to_string_lossy()))
                .collect();
            !disks.is_empty() && disks.iter().all(|disk| is_vhdx(disk))
        }
    }
}

pub struct ImageStore {
    root: PathBuf,
    /// The DSA build using the store, recorded with each failure.
    build: Option<String>,
}

fn store_error(what: &str, path: &Path, error: impl std::fmt::Display) -> UpdateError {
    UpdateError::Store(format!("{what} {}: {error}", path.display()))
}

impl ImageStore {
    /// Opens a store whose first image is already in `images/<version>/`, as the installer
    /// leaves it. Creates the state when there is none.
    pub fn open(root: &Path, installed: &InstalledImage) -> Result<ImageStore, UpdateError> {
        Self::open_with(root, installed, None)
    }

    /// [`open`](Self::open) for the DSA build `build`. Images that failed under another
    /// build, or under one that did not record itself, may be tried again.
    pub fn open_as(
        root: &Path,
        installed: &InstalledImage,
        build: &str,
    ) -> Result<ImageStore, UpdateError> {
        let store = Self::open_with(root, installed, Some(build.to_string()))?;
        let mut state = store.state()?;
        if !state.failed.is_empty() && state.failed_build.as_deref() != Some(build) {
            state.failed.clear();
            state.failed_build = None;
            store.write_state(&state)?;
        }
        Ok(store)
    }

    fn open_with(
        root: &Path,
        installed: &InstalledImage,
        build: Option<String>,
    ) -> Result<ImageStore, UpdateError> {
        let store = ImageStore {
            root: root.to_path_buf(),
            build,
        };
        if !store.state_path().exists() {
            if !is_plain_name(&installed.version) {
                return Err(UpdateError::Store(format!(
                    "{:?} is not a plain image name",
                    installed.version
                )));
            }
            store.write_state(&State {
                current: installed.clone(),
                previous: None,
                on_trial: false,
                failed: Vec::new(),
                failed_build: None,
                channel: Channel::Latest,
                older_allowed: false,
                supported_majors: Vec::new(),
            })?;
        }
        store.state()?;
        Ok(store)
    }

    fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// The folder of the image named `version`.
    pub fn image_dir(&self, version: &str) -> PathBuf {
        self.root.join("images").join(version)
    }

    /// Whether the image named `version` has its files here. An installer that bundles no
    /// image records a first image by name only, and nothing can boot from that.
    pub fn has_image(&self, version: &str) -> bool {
        self.image_dir(version).is_dir()
    }

    pub fn state(&self) -> Result<State, UpdateError> {
        let path = self.state_path();
        let text = fs::read(&path).map_err(|e| store_error("reading", &path, e))?;
        serde_json::from_slice(&text).map_err(|e| store_error("reading", &path, e))
    }

    fn write_state(&self, state: &State) -> Result<(), UpdateError> {
        fs::create_dir_all(&self.root).map_err(|e| store_error("creating", &self.root, e))?;
        let path = self.state_path();
        let temporary = self.root.join("state.json.new");
        let text = serde_json::to_vec_pretty(state).expect("the state serialises");
        let mut file =
            fs::File::create(&temporary).map_err(|e| store_error("writing", &temporary, e))?;
        file.write_all(&text)
            .and_then(|()| file.sync_all())
            .map_err(|e| store_error("writing", &temporary, e))?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|e| store_error("replacing", &path, e))
    }

    /// Downloads `image`, checks every file, and makes it current on trial. Refuses an
    /// image the rules do not allow, with the rule's reason.
    pub fn stage(
        &self,
        image: &ImageEntry,
        manifest: &Manifest,
        fetch: &dyn Fetch,
    ) -> Result<(), UpdateError> {
        self.stage_reporting(image, manifest, fetch, &mut |_| {})
    }

    /// [`stage`](Self::stage), telling `progress` how many bytes of the image's files have
    /// been downloaded so far, after every write.
    pub fn stage_reporting(
        &self,
        image: &ImageEntry,
        manifest: &Manifest,
        fetch: &dyn Fetch,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), UpdateError> {
        let mut state = self.state()?;
        if state.on_trial {
            return Err(UpdateError::Refused(format!(
                "{} has not finished its first boot yet",
                state.current.version
            )));
        }
        check_for(
            &state.current,
            image,
            manifest,
            &state.failed,
            state.older_allowed,
        )
        .map_err(UpdateError::Refused)?;
        let staging = self
            .root
            .join("images")
            .join(format!(".staging-{}", image.version));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|e| store_error("creating", &staging, e))?;
        let mut before = 0;
        let result = image.files.iter().try_for_each(|file| {
            download(file, &staging, fetch, &mut |written| {
                progress(before + written)
            })?;
            before += file.size;
            Ok(())
        });
        if let Err(error) = result {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        let target = self.image_dir(&image.version);
        let _ = fs::remove_dir_all(&target);
        fs::rename(&staging, &target).map_err(|e| store_error("placing", &target, e))?;
        let previous = std::mem::replace(&mut state.current, InstalledImage::of(image));
        state.previous = Some(previous);
        state.on_trial = true;
        state.older_allowed = false;
        self.write_state(&state)
    }

    /// The supported majors: those the last release list named, or this build's own.
    pub fn supported_majors(&self) -> Result<Vec<u32>, UpdateError> {
        let state = self.state()?;
        Ok(if state.supported_majors.is_empty() {
            versions::supported_majors()
        } else {
            state.supported_majors
        })
    }

    /// Records the supported majors a verified release list names. A chosen major that is not
    /// among them any more moves up to the one [`versions::target_major`] names; the news for
    /// the user is returned.
    pub fn follow_supported(&self, supported: &[u32]) -> Result<Option<String>, UpdateError> {
        let mut state = self.state()?;
        let mut sorted = supported.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let mut notice = None;
        if let Channel::Major(chosen) = state.channel {
            if !sorted.contains(&chosen) {
                if let Some(next) = versions::target_major(state.channel, &sorted) {
                    state.channel = Channel::Major(next);
                    notice = Some(format!(
                        "Android {chosen} is no longer supported, so this computer moves up to \
                         Android {next}; apps and their data are kept"
                    ));
                }
            }
        }
        if sorted != state.supported_majors || notice.is_some() {
            state.supported_majors = sorted;
            self.write_state(&state)?;
        }
        Ok(notice)
    }

    /// Chooses which images this computer takes. A channel that leads to an older major than
    /// the current image is taken only with `erase_data`, since Android cannot read data a
    /// newer major wrote.
    pub fn choose(&self, channel: Channel, erase_data: bool) -> Result<(), ChooseError> {
        let store = |e: UpdateError| ChooseError::Store(e.to_string());
        let supported = self.supported_majors().map_err(store)?;
        let list = supported
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if let Channel::Major(major) = channel {
            if !supported.contains(&major) {
                return Err(ChooseError::Unsupported(format!(
                    "Android {major} is not a supported version (supported: {list})"
                )));
            }
        }
        let mut state = self.state().map_err(store)?;
        let target = versions::target_major(channel, &supported)
            .ok_or_else(|| ChooseError::Unsupported("no Android version is supported".into()))?;
        let older = target < state.current.android_major;
        if older && !erase_data {
            return Err(ChooseError::DataWouldBeLost(format!(
                "Android {target} is older than the installed Android {}, and Android cannot \
                 read data a newer version wrote. Moving to it erases the Android data of every \
                 user of this computer: the apps they installed, their accounts and every \
                 app's data. Agree to that to move (dsa android use {target} --erase-data)",
                state.current.android_major
            )));
        }
        state.channel = channel;
        state.older_allowed = older;
        self.write_state(&state).map_err(store)
    }

    /// The Android version as a user sees it, reading the build ID and security patch from
    /// the current image's `image.json`.
    pub fn report(&self, notice: Option<String>) -> Result<VersionReport, UpdateError> {
        let state = self.state()?;
        let supported = self.supported_majors()?;
        let details = fs::read(self.image_dir(&state.current.version).join("image.json"))
            .ok()
            .and_then(|text| serde_json::from_slice::<serde_json::Value>(&text).ok());
        let source = |key: &str| {
            details
                .as_ref()
                .and_then(|d| d["source"][key].as_str().map(str::to_string))
                .filter(|v| !v.is_empty())
        };
        Ok(VersionReport {
            build_id: source("build_id"),
            security_patch: source("security_patch"),
            target_major: versions::target_major(state.channel, &supported)
                .unwrap_or(state.current.android_major),
            version: state.current.version,
            android_major: state.current.android_major,
            build: state.current.build,
            channel: state.channel,
            supported_majors: supported,
            on_trial: state.on_trial,
            notice,
        })
    }

    /// Whether the chosen channel leads to another major than the current image: a switch
    /// the user asked for, which the update service runs even with image updates off.
    pub fn switch_pending(&self) -> bool {
        let (Ok(state), Ok(supported)) = (self.state(), self.supported_majors()) else {
            return false;
        };
        versions::target_major(state.channel, &supported)
            .is_some_and(|target| target != state.current.android_major)
    }

    /// Records that the current image booted: it stays, and images other than it and the
    /// previous one are removed.
    pub fn confirm(&self) -> Result<(), UpdateError> {
        let mut state = self.state()?;
        if !state.on_trial {
            return Ok(());
        }
        state.on_trial = false;
        self.write_state(&state)?;
        self.remove_others(&state);
        Ok(())
    }

    /// Goes back to the previous image after the current one failed to boot. The failed
    /// version is remembered and its files removed. Returns the image now current.
    pub fn roll_back(&self) -> Result<InstalledImage, UpdateError> {
        let mut state = self.state()?;
        let previous = state.previous.take().ok_or_else(|| {
            UpdateError::Store(format!(
                "{} failed and there is no previous image to go back to",
                state.current.version
            ))
        })?;
        let failed = std::mem::replace(&mut state.current, previous);
        state.failed.push(failed.version.clone());
        state.failed_build = self.build.clone();
        state.on_trial = false;
        self.write_state(&state)?;
        let _ = fs::remove_dir_all(self.image_dir(&failed.version));
        Ok(state.current)
    }

    /// Makes the image an installer just put in the store current, when it is newer than the
    /// current one and has all its files. An upgrade bundles a newer image beside the ones
    /// already here, and nothing else would ever boot it. It boots on trial like a
    /// downloaded image: the image before it stays as previous (or, when the current image
    /// is itself still on trial, the previous one stays), and one that fails is not adopted
    /// again by the same build. Returns whether it was adopted.
    pub fn adopt_bundled(&self, bundled: &InstalledImage) -> Result<bool, UpdateError> {
        let mut state = self.state()?;
        let newer = (bundled.android_major, bundled.build)
            > (state.current.android_major, state.current.build);
        if !newer
            || bundled.version == state.current.version
            || !is_plain_name(&bundled.version)
            || state.failed.contains(&bundled.version)
            || !image_is_usable(&self.image_dir(&bundled.version))
        {
            return Ok(false);
        }
        let before = std::mem::replace(&mut state.current, bundled.clone());
        // Only an image that still has its files can be gone back to: removing the older
        // package can take the image it installed with it.
        let usable = |image: &InstalledImage| {
            is_plain_name(&image.version) && image_is_usable(&self.image_dir(&image.version))
        };
        if !state.on_trial || !state.previous.as_ref().is_some_and(usable) {
            state.previous = Some(before).filter(usable);
        }
        state.on_trial = true;
        self.write_state(&state)?;
        Ok(true)
    }

    /// Makes sure the current image is one that can boot. When the state names an image whose
    /// files are missing or broken (a test package's stand-in, a folder emptied by hand), it
    /// moves to a usable image instead: the previous one, another complete image in the store
    /// (the newest), or the image the installer put there. With none of those usable, it goes
    /// back to the installer's image by name, so that an installation without an image
    /// downloads its first one rather than waiting on files that will never boot.
    pub fn recover(&self, installed: &InstalledImage) -> Result<Recovery, UpdateError> {
        let mut state = self.state()?;
        if image_is_usable(&self.image_dir(&state.current.version)) {
            return Ok(Recovery::Usable);
        }
        let broken = state.current.clone();
        let mut candidates: Vec<InstalledImage> = state.previous.iter().cloned().collect();
        let mut others = self.complete_images();
        others.sort_by_key(|image| std::cmp::Reverse((image.android_major, image.build)));
        candidates.extend(others);
        candidates.push(installed.clone());
        let usable = candidates.into_iter().find(|image| {
            image.version != broken.version
                && is_plain_name(&image.version)
                && image_is_usable(&self.image_dir(&image.version))
        });
        let next = match usable {
            Some(image) => image,
            None if broken.version != installed.version && is_plain_name(&installed.version) => {
                installed.clone()
            }
            None => return Ok(Recovery::NothingBetter { broken }),
        };
        state.current = next.clone();
        state.previous = None;
        state.on_trial = false;
        self.write_state(&state)?;
        Ok(Recovery::Moved {
            broken,
            current: next,
        })
    }

    /// The images in the store that carry an `image.json` naming them.
    fn complete_images(&self) -> Vec<InstalledImage> {
        let Ok(entries) = fs::read_dir(self.root.join("images")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let text = fs::read(entry.path().join("image.json")).ok()?;
                let value: serde_json::Value = serde_json::from_slice(&text).ok()?;
                let image = InstalledImage {
                    version: value["version"].as_str()?.to_string(),
                    android_major: u32::try_from(value["android_major"].as_u64()?).ok()?,
                    build: value["build"].as_u64()?,
                };
                (image.version == name).then_some(image)
            })
            .collect()
    }

    fn remove_others(&self, state: &State) {
        let Ok(entries) = fs::read_dir(self.root.join("images")) else {
            return;
        };
        let keep: Vec<&str> = std::iter::once(state.current.version.as_str())
            .chain(state.previous.as_ref().map(|p| p.version.as_str()))
            .collect();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name.as_str()) && !name.starts_with(".staging-") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// A writer that hashes and counts what passes through it.
struct Hashing<'a, W> {
    inner: W,
    hash: Sha256,
    written: u64,
    limit: u64,
    /// The list the expected size comes from, for the error.
    list: &'a str,
    /// Told the bytes written so far, after every write.
    progress: &'a mut dyn FnMut(u64),
}

impl<W: Write> Write for Hashing<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > self.limit {
            return Err(io::Error::other(format!(
                "longer than the {} says",
                self.list
            )));
        }
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        self.written += n as u64;
        (self.progress)(self.written);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Why [`download_checked`] failed.
pub(crate) enum DownloadFailure {
    /// The download failed, or what arrived is not what the list describes.
    Download(String),
    /// Writing the file failed.
    Store(String),
}

/// What a signed list says about one file: where it is, its size and its SHA-256.
pub(crate) struct Expected<'a> {
    /// The file's name in messages.
    pub name: &'a str,
    pub url: &'a str,
    pub size: u64,
    /// SHA-256 in hexadecimal, already checked to be well formed.
    pub sha256: &'a str,
    /// The list that describes the file, for messages: "release list", "program list".
    pub list: &'a str,
}

/// Downloads `file` to `path`, refusing it as soon as it is longer than the list says and
/// at the end when its size or SHA-256 differs. The file is left behind on failure; the
/// caller removes it.
pub(crate) fn download_checked(
    file: &Expected,
    path: &Path,
    fetch: &dyn Fetch,
    progress: &mut dyn FnMut(u64),
) -> Result<(), DownloadFailure> {
    let stored = |what: &str, e: &dyn std::fmt::Display| {
        DownloadFailure::Store(format!("{what} {}: {e}", path.display()))
    };
    let out = fs::File::create(path).map_err(|e| stored("creating", &e))?;
    let mut writer = Hashing {
        inner: io::BufWriter::new(out),
        hash: Sha256::new(),
        written: 0,
        limit: file.size,
        list: file.list,
        progress,
    };
    fetch
        .fetch(file.url, &mut writer)
        .map_err(|e| DownloadFailure::Download(format!("{}: {e}", file.name)))?;
    writer.flush().map_err(|e| stored("writing", &e))?;
    let written = writer.written;
    let actual: [u8; 32] = writer.hash.finalize().into();
    let file_handle = writer
        .inner
        .into_inner()
        .map_err(|e| stored("writing", &e.into_error()))?;
    file_handle.sync_all().map_err(|e| stored("writing", &e))?;
    if written != file.size {
        return Err(DownloadFailure::Download(format!(
            "{} is {written} bytes; the {} says {}",
            file.name, file.list, file.size
        )));
    }
    let expected = from_hex::<32>(file.sha256).expect("checked when the list was verified");
    if actual != expected {
        return Err(DownloadFailure::Download(format!(
            "{} has SHA-256 {}; the {} says {}",
            file.name,
            to_hex(&actual),
            file.list,
            file.sha256
        )));
    }
    Ok(())
}

fn download(
    file: &super::manifest::FileEntry,
    folder: &Path,
    fetch: &dyn Fetch,
    progress: &mut dyn FnMut(u64),
) -> Result<(), UpdateError> {
    let expected = Expected {
        name: &file.name,
        url: &file.url,
        size: file.size,
        sha256: &file.sha256,
        list: "release list",
    };
    download_checked(&expected, &folder.join(&file.name), fetch, progress).map_err(|failure| {
        match failure {
            DownloadFailure::Download(why) => UpdateError::Download(why),
            DownloadFailure::Store(why) => UpdateError::Store(why),
        }
    })
}

/// Reads all of a `Read`, for small files such as the release list.
pub fn read_all(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}
