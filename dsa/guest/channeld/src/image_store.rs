//! An image the host sends for the guest to lay over its system at the next boot (an ARM
//! translation package, the Google apps), kept on the user's disk.
//!
//! The image is written beside the one already there, and put in that one's place only once
//! all of it has arrived and its length and SHA-256 match what the host announced, so a
//! transfer that breaks off leaves the guest as it was. One transfer runs at a time; a new one
//! drops an unfinished one.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// The image's name in its folder.
pub const IMAGE: &str = "system.img";
/// The name it is written under until it is complete and checked.
const PARTIAL: &str = "system.img.part";

/// How a message about the image went: the answer's common part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// The transfer this answers, or 0.
    pub transfer_id: u32,
    /// How many bytes of that transfer have been written.
    pub received: u64,
    /// The transfer is complete and the image checked and in place, or the removal is done.
    pub done: bool,
    /// Why the transfer or the removal failed, in words a person can act on; empty otherwise.
    pub error: String,
}

struct Transfer {
    id: u32,
    size: u64,
    sha256: Vec<u8>,
    file: File,
    hasher: Sha256,
    received: u64,
}

/// One image on the user's disk and the transfer under way.
pub struct ImageStore {
    dir: PathBuf,
    /// What the image is called in what the host shows the user.
    name: &'static str,
    max_bytes: u64,
    transfer: Mutex<Option<Transfer>>,
}

impl ImageStore {
    /// Keeps the image called `name` in `dir`, which init makes at boot, refusing one larger
    /// than `max_bytes`.
    pub fn new(dir: impl Into<PathBuf>, name: &'static str, max_bytes: u64) -> ImageStore {
        ImageStore {
            dir: dir.into(),
            name,
            max_bytes,
            transfer: Mutex::new(None),
        }
    }

    /// The folder the image is in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether an image is on the disk.
    pub fn installed(&self) -> bool {
        self.dir.join(IMAGE).is_file()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Transfer>> {
        self.transfer.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn progress(transfer_id: u32, received: u64, done: bool, error: String) -> Progress {
        Progress {
            transfer_id,
            received,
            done,
            error,
        }
    }

    fn failed(&self, transfer_id: u32, received: u64, why: String) -> Progress {
        let _ = fs::remove_file(self.dir.join(PARTIAL));
        Self::progress(transfer_id, received, false, why)
    }

    /// Starts transfer `id` of an image of `size` bytes whose SHA-256 is `sha256`.
    pub fn begin(&self, id: u32, size: u64, sha256: &[u8]) -> Progress {
        let name = self.name;
        let mut transfer = self.lock();
        // A transfer still under way has been given up by the host.
        *transfer = None;
        if size == 0 || size > self.max_bytes {
            let why = format!(
                "{name} is {size} bytes; it must have from 1 byte to {} MiB",
                self.max_bytes >> 20
            );
            return self.failed(id, 0, why);
        }
        if sha256.len() != 32 {
            let why = format!("{name} came without its SHA-256");
            return self.failed(id, 0, why);
        }
        if let Err(error) = fs::create_dir_all(&self.dir) {
            let why = format!("cannot make {}: {error}", self.dir.display());
            return self.failed(id, 0, why);
        }
        match File::create(self.dir.join(PARTIAL)) {
            Ok(file) => {
                *transfer = Some(Transfer {
                    id,
                    size,
                    sha256: sha256.to_vec(),
                    file,
                    hasher: Sha256::new(),
                    received: 0,
                });
                Self::progress(id, 0, false, String::new())
            }
            Err(error) => {
                let why = format!("cannot write {name} in {}: {error}", self.dir.display());
                self.failed(id, 0, why)
            }
        }
    }

    /// Writes `data`, which starts at `offset` in the image, for transfer `id`.
    pub fn write(&self, id: u32, offset: u64, data: &[u8]) -> Progress {
        let name = self.name;
        let mut slot = self.lock();
        let Some(transfer) = slot.as_mut().filter(|t| t.id == id) else {
            let why = format!("no transfer {id} of {name} is under way");
            return Self::progress(id, 0, false, why);
        };
        let (received, size) = (transfer.received, transfer.size);
        let length = data.len() as u64;
        if offset != received || received + length > size {
            *slot = None;
            let why = format!(
                "a piece of {name} arrived out of place (at {offset}, {length} bytes, after \
                 {received} of {size})"
            );
            return self.failed(id, received, why);
        }
        if let Err(error) = transfer.file.write_all(data) {
            *slot = None;
            let why = format!("cannot write {name}: {error}");
            return self.failed(id, received, why);
        }
        transfer.hasher.update(data);
        transfer.received += length;
        let received = transfer.received;
        if received < size {
            return Self::progress(id, received, false, String::new());
        }
        let Some(Transfer {
            file,
            hasher,
            sha256,
            ..
        }) = slot.take()
        else {
            return Self::progress(id, received, false, String::new());
        };
        if let Err(error) = file.sync_all() {
            let why = format!("cannot save {name}: {error}");
            return self.failed(id, received, why);
        }
        drop(file);
        if hasher.finalize().as_slice() != sha256.as_slice() {
            let why =
                format!("{name} did not arrive intact (its SHA-256 differs); install it again");
            return self.failed(id, received, why);
        }
        if let Err(error) = fs::rename(self.dir.join(PARTIAL), self.dir.join(IMAGE)) {
            let why = format!("cannot put {name} in place: {error}");
            return self.failed(id, received, why);
        }
        Self::progress(id, received, true, String::new())
    }

    /// Removes the image, and drops a transfer under way.
    pub fn remove(&self) -> Progress {
        *self.lock() = None;
        let _ = fs::remove_file(self.dir.join(PARTIAL));
        match fs::remove_file(self.dir.join(IMAGE)) {
            Ok(()) => Self::progress(0, 0, true, String::new()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::progress(0, 0, true, String::new())
            }
            Err(error) => {
                let why = format!("cannot remove {}: {error}", self.name);
                Self::progress(0, 0, false, why)
            }
        }
    }
}
