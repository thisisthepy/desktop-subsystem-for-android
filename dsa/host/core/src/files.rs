//! Files from the host that the user opens, or shares, in an app in their Android.
//!
//! A client reads the file on the host (the service never opens a user's files itself), sends
//! it to the service in pieces over the local API, and the service sends it to the guest's
//! agent in chunks over the control channel. The agent keeps it in a folder of its own for the
//! transfer and asks the user which app opens it.

use std::fmt;
use std::path::Path;

/// The largest file that crosses, in bytes: 64 MiB.
///
/// The service holds the whole file in memory while it sends it, and the agent stores it in
/// the guest's data partition, so a file costs its size twice over for a moment. 64 MiB takes
/// documents, photos, music and short videos; a larger file is refused with a message rather
/// than tying up that much of the service's memory.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The size of one chunk on the control channel: 128 KiB.
///
/// Inside the guest each chunk reaches the agent in a one-way binder call, and one-way calls
/// to a process share half of its 1 MiB binder buffer. A call that does not fit is dropped,
/// so the host keeps at most [`CHUNKS_IN_FLIGHT`] chunks (256 KiB) ahead of the agent's
/// progress reports, which leaves room for the agent's other frames.
pub const CHUNK_BYTES: usize = 128 * 1024;

/// How many chunks the host sends before the agent has said it stored the first of them.
pub const CHUNKS_IN_FLIGHT: u64 = 2;

/// The size of one piece of a file on the local API: 1 MiB, well below the transport's
/// largest frame, and large enough that a file needs few frames.
pub const LOCAL_CHUNK_BYTES: usize = 1024 * 1024;

/// The longest file name that crosses, in bytes: what the guest's file system takes.
pub const MAX_NAME_BYTES: usize = 255;

/// What the user wants done with the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// Open it in an app the user picks.
    View,
    /// Share it with an app the user picks.
    Send,
}

impl fmt::Display for FileAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileAction::View => "open",
            FileAction::Send => "share",
        })
    }
}

/// A file to send to the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransfer {
    /// The file's name, without any folder.
    pub name: String,
    pub content: Vec<u8>,
    pub action: FileAction,
}

impl FileTransfer {
    /// Reads the file at `path`, refusing it before reading when it is too large or its name
    /// cannot be used.
    pub fn read(path: &Path, action: FileAction) -> Result<FileTransfer, String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} names no file", path.display()))?;
        check_name(name)?;
        let metadata =
            std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        check_size(metadata.len())?;
        let content =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        check_size(content.len() as u64)?;
        Ok(FileTransfer {
            name: name.to_string(),
            content,
            action,
        })
    }

    /// Checks the file's name and size.
    pub fn check(&self) -> Result<(), String> {
        check_name(&self.name)?;
        check_size(self.content.len() as u64)
    }
}

/// Refuses a file larger than [`MAX_FILE_BYTES`], saying why.
pub fn check_size(size: u64) -> Result<(), String> {
    if size > MAX_FILE_BYTES {
        return Err(format!(
            "the file is {} MiB; files up to {} MiB can be opened in Android",
            size.div_ceil(1024 * 1024),
            MAX_FILE_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

/// Refuses a name the guest could not store the file under: empty, `.` or `..`, with a folder
/// separator or a control character, or longer than [`MAX_NAME_BYTES`].
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err("the file has no name".into());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(format!(
            "the file's name is longer than the {MAX_NAME_BYTES} bytes Android takes"
        ));
    }
    if name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(format!("{name:?} is not a file name Android can use"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr9_a_file_name_without_a_folder_is_taken() {
        assert!(check_name("photo.jpg").is_ok());
        assert!(check_name("\u{d55c}\u{ae00}.pdf").is_ok());
        for bad in ["", ".", "..", "a/b", "..\\x", "a\nb", &"a".repeat(256)] {
            assert!(check_name(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn fr9_a_file_larger_than_the_limit_is_refused() {
        assert!(check_size(MAX_FILE_BYTES).is_ok());
        let refused = check_size(MAX_FILE_BYTES + 1).unwrap_err();
        assert!(refused.contains("64 MiB"), "{refused}");
    }

    #[test]
    fn fr9_a_file_is_read_with_its_name() {
        let dir = std::env::temp_dir().join(format!("dsa-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.txt");
        std::fs::write(&path, b"hello").unwrap();
        let file = FileTransfer::read(&path, FileAction::Send).unwrap();
        assert_eq!(file.name, "note.txt");
        assert_eq!(file.content, b"hello");
        assert!(FileTransfer::read(&dir, FileAction::View).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
