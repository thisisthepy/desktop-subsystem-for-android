//! The backup folder: what it holds and how it is laid out.
//!
//! ```text
//! <folder>/backup.json      the folder's format, its key's id and its restore token (plain)
//! <folder>/index.dsab       which apps it holds and in which files (encrypted)
//! <folder>/data/<id>.dsab   each app's key-value records, full backup and APKs (encrypted)
//! ```
//!
//! Only `backup.json` is plain, and it holds nothing about the apps: their names, versions and
//! sizes are in the index, which is encrypted like the data. Each data file has a random name,
//! and the index (which is authenticated) says which slot it fills, so a file moved into
//! another's place does not open. The folder is moved as a whole: to another computer, or
//! back after a data reset, it reads the same once the key is there.
//!
//! Every change writes new files and then the index, so a crash leaves the backup as it was
//! before the change or after it, never between.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::crypto::{BackupKey, DecryptingReader, EncryptingWriter, OpenError};
use crate::host_keys::write_replacing;

pub const METADATA: &str = "backup.json";
pub const INDEX: &str = "index.dsab";
pub const DATA: &str = "data";
const EXTENSION: &str = "dsab";
const PART: &str = "part";
/// The index's slot, which binds its encryption to being the index.
const INDEX_SLOT: &str = "index";

/// The plain metadata of a backup folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metadata {
    pub format: u32,
    /// The id of the key the folder is encrypted with, in hex.
    pub key_id: String,
    /// The restore set's token, made once with the folder.
    pub token: u64,
}

/// One encrypted file of the folder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// The file's random name, without its extension.
    pub file: String,
    /// Its size before encryption.
    pub size: u64,
}

/// What the backup holds of one package.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageEntry {
    pub version_code: i64,
    #[serde(default)]
    pub key_value: Option<FileEntry>,
    #[serde(default)]
    pub full: Option<FileEntry>,
    /// The package's APKs, by name, at `apk_version`.
    #[serde(default)]
    pub apks: BTreeMap<String, FileEntry>,
    #[serde(default)]
    pub apk_version: i64,
    pub backed_up_ms: i64,
}

impl PackageEntry {
    /// The files of the entry.
    pub fn files(&self) -> impl Iterator<Item = &FileEntry> {
        self.key_value
            .iter()
            .chain(self.full.iter())
            .chain(self.apks.values())
    }

    pub fn size(&self) -> u64 {
        self.files().map(|f| f.size).sum()
    }
}

/// The encrypted index.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Index {
    #[serde(default)]
    pub packages: BTreeMap<String, PackageEntry>,
    /// When an app was last backed up and the backup last restored, in milliseconds since the
    /// Unix epoch.
    #[serde(default)]
    pub last_backup_ms: i64,
    #[serde(default)]
    pub last_restore_ms: i64,
    /// What went wrong in the last backup or restore.
    #[serde(default)]
    pub last_error: String,
    /// The computer the folder was made on, for the restore set's name.
    #[serde(default)]
    pub made_on: String,
}

/// Why the folder cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderError {
    /// The folder was made with another key than the one given.
    OtherKey,
    /// The folder's files are damaged or cannot be read; the text says how.
    Broken(String),
}

impl std::fmt::Display for FolderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FolderError::OtherKey => f.write_str(
                "the backup folder was made with another backup key; enter its recovery key \
                 (dsa backup key import) to use it here",
            ),
            FolderError::Broken(why) => f.write_str(why),
        }
    }
}

/// The plain metadata of `dir`, if it is a backup folder.
pub fn read_metadata(dir: &Path) -> Result<Option<Metadata>, FolderError> {
    let path = dir.join(METADATA);
    match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data)
            .map(Some)
            .map_err(|e| FolderError::Broken(format!("{} cannot be read: {e}", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FolderError::Broken(format!(
            "{} cannot be read: {e}",
            path.display()
        ))),
    }
}

/// Removes every file a backup made in `dir`: the metadata, the index, the data files and what
/// an interrupted write left, then the folders when nothing else is in them. A file the user
/// put there is kept, and so is the folder that holds it. A folder that is not there is no
/// error.
pub fn delete(dir: &Path) -> Result<(), String> {
    let remove = |path: &Path| match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{} cannot be removed: {e}", path.display())),
    };
    let data = dir.join(DATA);
    match std::fs::read_dir(&data) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                let ours = path.is_file()
                    && path
                        .extension()
                        .is_some_and(|e| e == EXTENSION || e == PART);
                if ours {
                    remove(&path)?;
                }
            }
            let _ = std::fs::remove_dir(&data);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{} cannot be read: {e}", data.display())),
    }
    for name in [INDEX, METADATA] {
        remove(&dir.join(name))?;
        remove(&dir.join(format!("{name}.new")))?;
    }
    // Removed only when empty: whatever else is there is the user's.
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

/// An open backup folder, with the key it is encrypted with.
pub struct Folder {
    dir: PathBuf,
    key: BackupKey,
    pub metadata: Metadata,
    pub index: Index,
}

fn random_name() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(dsa_protocol::auth::to_hex(&bytes))
}

fn broken(what: &str, error: impl std::fmt::Display) -> FolderError {
    FolderError::Broken(format!("{what}: {error}"))
}

impl Folder {
    /// Opens the folder `dir` with `key`, making it when it is not a backup folder yet.
    /// `made_on` names this computer, for a new folder.
    pub fn open(dir: &Path, key: BackupKey, made_on: &str) -> Result<Folder, FolderError> {
        let metadata = match read_metadata(dir)? {
            Some(metadata) => metadata,
            None => {
                let mut token = [0u8; 8];
                getrandom::getrandom(&mut token).map_err(|e| broken("random", e))?;
                let metadata = Metadata {
                    format: 1,
                    key_id: key.id_hex(),
                    // Android takes a restore set's token of 0 to mean none.
                    token: u64::from_le_bytes(token).max(1),
                };
                std::fs::create_dir_all(dir.join(DATA))
                    .map_err(|e| broken(&format!("making {}", dir.display()), e))?;
                let mut folder = Folder {
                    dir: dir.to_path_buf(),
                    key,
                    metadata: metadata.clone(),
                    index: Index {
                        made_on: made_on.to_string(),
                        ..Index::default()
                    },
                };
                folder.save_index()?;
                let text = serde_json::to_vec_pretty(&metadata).map_err(|e| broken("json", e))?;
                write_replacing(&dir.join(METADATA), &text)
                    .map_err(|e| broken(&format!("writing {}", dir.display()), e))?;
                return Ok(folder);
            }
        };
        if metadata.key_id != key.id_hex() {
            return Err(FolderError::OtherKey);
        }
        let index = match File::open(dir.join(INDEX)) {
            Ok(file) => {
                let reader =
                    DecryptingReader::new(file, &key, INDEX_SLOT).map_err(|e| match e {
                        OpenError::OtherKey(_) => FolderError::OtherKey,
                        e => broken("the backup's index", e),
                    })?;
                let data = reader
                    .read_all()
                    .map_err(|e| broken("the backup's index", e))?;
                serde_json::from_slice(&data).map_err(|e| broken("the backup's index", e))?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Index::default(),
            Err(e) => return Err(broken("the backup's index", e)),
        };
        let folder = Folder {
            dir: dir.to_path_buf(),
            key,
            metadata,
            index,
        };
        folder.remove_parts();
        Ok(folder)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn data_path(&self, name: &str, extension: &str) -> PathBuf {
        self.dir.join(DATA).join(format!("{name}.{extension}"))
    }

    /// Removes what uploads cut short left behind.
    fn remove_parts(&self) {
        if let Ok(entries) = std::fs::read_dir(self.dir.join(DATA)) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == PART) {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }

    /// Writes the index again.
    pub fn save_index(&mut self) -> Result<(), FolderError> {
        let data = serde_json::to_vec(&self.index).map_err(|e| broken("json", e))?;
        let mut sealed = Vec::with_capacity(data.len() + 128);
        let mut writer = EncryptingWriter::new(&mut sealed, &self.key, INDEX_SLOT)
            .map_err(|e| broken("encrypting the index", e))?;
        writer
            .write_all(&data)
            .map_err(|e| broken("encrypting the index", e))?;
        writer
            .finish()
            .map_err(|e| broken("encrypting the index", e))?;
        write_replacing(&self.dir.join(INDEX), &sealed)
            .map_err(|e| broken(&format!("writing the index in {}", self.dir.display()), e))
    }

    /// Starts a new data file; it becomes part of the backup only through
    /// [`NewFile::keep`] and [`Folder::save_index`].
    pub fn new_file(&self) -> Result<NewFile, FolderError> {
        let name = random_name().map_err(|e| broken("random", e))?;
        let part = self.data_path(&name, PART);
        std::fs::create_dir_all(self.dir.join(DATA))
            .map_err(|e| broken(&format!("making {}", self.dir.display()), e))?;
        let file = File::create(&part).map_err(|e| broken(&format!("{}", part.display()), e))?;
        let writer = EncryptingWriter::new(io::BufWriter::new(file), &self.key, &name)
            .map_err(|e| broken(&format!("{}", part.display()), e))?;
        Ok(NewFile {
            name,
            part,
            writer: Some(writer),
        })
    }

    /// Opens a data file of the backup for reading.
    pub fn open_file(&self, entry: &FileEntry) -> Result<DecryptingReader<File>, FolderError> {
        let path = self.data_path(&entry.file, EXTENSION);
        let file = File::open(&path).map_err(|e| broken(&format!("{}", path.display()), e))?;
        DecryptingReader::new(file, &self.key, &entry.file).map_err(|e| match e {
            OpenError::OtherKey(_) => FolderError::OtherKey,
            e => broken(&format!("{}", path.display()), e),
        })
    }

    /// Reads a whole data file, checking it.
    pub fn read_file(&self, entry: &FileEntry) -> Result<Vec<u8>, FolderError> {
        self.open_file(entry)?
            .read_all()
            .map_err(|e| broken(&format!("backup file {}", entry.file), e))
    }

    /// Removes a data file that the index no longer names.
    pub fn remove_file(&self, entry: &FileEntry) {
        let _ = std::fs::remove_file(self.data_path(&entry.file, EXTENSION));
    }

    /// Writes a whole data file of `data` and keeps it.
    pub fn write_file(&self, data: &[u8]) -> Result<FileEntry, FolderError> {
        let mut file = self.new_file()?;
        file.write_all(data)
            .map_err(|e| broken("writing a backup file", e))?;
        file.keep(self)
    }
}

/// A data file being written.
pub struct NewFile {
    name: String,
    part: PathBuf,
    writer: Option<EncryptingWriter<io::BufWriter<File>>>,
}

impl NewFile {
    /// The plaintext bytes written so far.
    pub fn written(&self) -> u64 {
        self.writer.as_ref().map_or(0, |w| w.written())
    }

    /// Seals the file and puts it in place, ready for the index to name.
    pub fn keep(mut self, folder: &Folder) -> Result<FileEntry, FolderError> {
        let writer = self.writer.take().expect("a file is kept once");
        let size = writer.written();
        let inner = writer
            .finish()
            .map_err(|e| broken("finishing a backup file", e))?;
        let file = inner
            .into_inner()
            .map_err(|e| broken("finishing a backup file", e.error().to_string()))?;
        file.sync_all()
            .map_err(|e| broken("finishing a backup file", e))?;
        drop(file);
        let path = folder.data_path(&self.name, EXTENSION);
        std::fs::rename(&self.part, &path)
            .map_err(|e| broken(&format!("{}", path.display()), e))?;
        Ok(FileEntry {
            file: self.name.clone(),
            size,
        })
    }
}

impl Write for NewFile {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.writer
            .as_mut()
            .ok_or_else(|| io::Error::other("the file is already kept"))?
            .write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        match self.writer.as_mut() {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for NewFile {
    fn drop(&mut self) {
        if self.writer.take().is_some() {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}
