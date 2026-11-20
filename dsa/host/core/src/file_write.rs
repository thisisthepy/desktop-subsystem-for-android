//! Writing a file so that a reader never sees half of it, the same on every host.
//!
//! The content goes to a second file next to the target, which then takes the target's place
//! in one step. A reader sees the old content or the new, never a part.

use std::fs;
use std::io;
use std::path::Path;

/// Writes `content` to `path`, replacing what is there. `partial_extension` is the extension
/// of the file written first and renamed over `path` (`path` with its extension replaced).
pub fn replace(path: &Path, content: &[u8], partial_extension: &str) -> io::Result<()> {
    let partial = path.with_extension(partial_extension);
    fs::write(&partial, content)?;
    fs::rename(&partial, path)
}

/// Like [`replace`], and the folder that holds `path` is made first when it is missing.
pub fn replace_in_new_folder(
    path: &Path,
    content: &[u8],
    partial_extension: &str,
) -> io::Result<()> {
    if let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        fs::create_dir_all(folder)?;
    }
    replace(path, content, partial_extension)
}

/// Like [`replace`], and nothing is written when `path` already holds exactly `content`.
pub fn replace_if_changed(path: &Path, content: &[u8], partial_extension: &str) -> io::Result<()> {
    if fs::read(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    replace(path, content, partial_extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("dsa-file-write-{name}-{}", std::process::id()))
    }

    #[test]
    fn d9_a_replaced_file_holds_the_new_content_and_leaves_no_partial_file() {
        let dir = folder("replace");
        let path = dir.join("record.txt");
        replace_in_new_folder(&path, b"one", "txt.new").unwrap();
        replace_in_new_folder(&path, b"two", "txt.new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!dir.join("record.txt.new").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d9_a_file_that_already_holds_the_content_is_not_written_again() {
        let dir = folder("changed");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("icon.png");
        replace_if_changed(&path, b"png", "partial").unwrap();
        let first = fs::metadata(&path).unwrap().modified().unwrap();
        replace_if_changed(&path, b"png", "partial").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), first);
        replace_if_changed(&path, b"other", "partial").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"other");
        let _ = fs::remove_dir_all(&dir);
    }
}
