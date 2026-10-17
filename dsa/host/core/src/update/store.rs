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
use super::policy::{check, InstalledImage};
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
    /// Versions that failed to boot here and are not tried again.
    #[serde(default)]
    pub failed: Vec<String>,
}

pub struct ImageStore {
    root: PathBuf,
}

fn store_error(what: &str, path: &Path, error: impl std::fmt::Display) -> UpdateError {
    UpdateError::Store(format!("{what} {}: {error}", path.display()))
}

impl ImageStore {
    /// Opens a store whose first image is already in `images/<version>/`, as the installer
    /// leaves it. Creates the state when there is none.
    pub fn open(root: &Path, installed: &InstalledImage) -> Result<ImageStore, UpdateError> {
        let store = ImageStore {
            root: root.to_path_buf(),
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
        let mut state = self.state()?;
        if state.on_trial {
            return Err(UpdateError::Refused(format!(
                "{} has not finished its first boot yet",
                state.current.version
            )));
        }
        check(&state.current, image, manifest, &state.failed).map_err(UpdateError::Refused)?;
        let staging = self
            .root
            .join("images")
            .join(format!(".staging-{}", image.version));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|e| store_error("creating", &staging, e))?;
        let result = image
            .files
            .iter()
            .try_for_each(|file| download(file, &staging, fetch));
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
        self.write_state(&state)
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
        state.on_trial = false;
        self.write_state(&state)?;
        let _ = fs::remove_dir_all(self.image_dir(&failed.version));
        Ok(state.current)
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
struct Hashing<W> {
    inner: W,
    hash: Sha256,
    written: u64,
    limit: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > self.limit {
            return Err(io::Error::other("longer than the release list says"));
        }
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn download(
    file: &super::manifest::FileEntry,
    folder: &Path,
    fetch: &dyn Fetch,
) -> Result<(), UpdateError> {
    let path = folder.join(&file.name);
    let out = fs::File::create(&path).map_err(|e| store_error("creating", &path, e))?;
    let mut writer = Hashing {
        inner: io::BufWriter::new(out),
        hash: Sha256::new(),
        written: 0,
        limit: file.size,
    };
    fetch
        .fetch(&file.url, &mut writer)
        .map_err(|e| UpdateError::Download(format!("{}: {e}", file.name)))?;
    writer
        .flush()
        .map_err(|e| store_error("writing", &path, e))?;
    if writer.written != file.size {
        return Err(UpdateError::Download(format!(
            "{} is {} bytes; the release list says {}",
            file.name, writer.written, file.size
        )));
    }
    let actual: [u8; 32] = writer.hash.finalize().into();
    let expected = from_hex::<32>(&file.sha256).expect("checked when the list was verified");
    if actual != expected {
        return Err(UpdateError::Download(format!(
            "{} has SHA-256 {}; the release list says {}",
            file.name,
            to_hex(&actual),
            file.sha256
        )));
    }
    Ok(())
}

/// Reads all of a `Read`, for small files such as the release list.
pub fn read_all(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}
