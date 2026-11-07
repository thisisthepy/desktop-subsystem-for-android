//! Files an app drags out of its task window, as the desktop takes them (`text/uri-list`).
//!
//! Wayland has no virtual files: a drop target reads a list of `file://` URIs and copies the
//! files it names. So when a target asks for the list, the files are streamed from the guest
//! into a folder of their own, readable only by the user, under `$XDG_RUNTIME_DIR` (memory,
//! not disk, on every desktop that has it), and the list names them there. The folder is
//! removed when the drag is cancelled, and a while after a drop, once the target has had time
//! to copy what it took. A folder a crash left behind is removed with the next drag.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dsa_host_core::drag::{DragChunk, DragContent, DragOffer, DragRead};

/// The prefix of each drag's folder.
const FOLDER_PREFIX: &str = "drag-";

/// How old a folder of an earlier drag is before the next drag removes it.
pub const LEFTOVER_AGE: Duration = Duration::from_secs(10 * 60);

/// Where the folders of the drags go: `$XDG_RUNTIME_DIR/dsa`, or the temporary folder when
/// the session has no runtime folder.
pub fn drags_root() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dsa")
}

/// A new folder, readable only by the user, for drag `drag_id`, after removing the folders
/// earlier drags left that are older than [`LEFTOVER_AGE`].
pub fn drag_folder(root: &Path, drag_id: u32) -> io::Result<PathBuf> {
    remove_leftovers(root, LEFTOVER_AGE);
    let folder = root.join(format!("{FOLDER_PREFIX}{}-{drag_id}", std::process::id()));
    if folder.exists() {
        fs::remove_dir_all(&folder)?;
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&folder)?;
    Ok(folder)
}

/// Removes the drag folders under `root` older than `age`.
pub fn remove_leftovers(root: &Path, age: Duration) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(FOLDER_PREFIX) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|modified| now.duration_since(modified).unwrap_or_default() >= age)
            .unwrap_or(false);
        if old {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Streams each file of `offer` into `folder` through `read`, under its own name (with
/// " (2)" and on for a name already taken), and returns their paths in order. On a failure
/// the folder is emptied, so a target never copies half a file.
pub fn materialize(
    folder: &Path,
    offer: &DragOffer,
    mut read: impl FnMut(DragRead) -> Result<DragChunk, String>,
) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for (index, item) in offer.files() {
        let path = unique_path(folder, &item.name);
        let written = (|| -> io::Result<()> {
            let mut file = fs::File::create(&path)?;
            let mut content = DragContent::new(offer.drag_id, index, &mut read);
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let n = content.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buffer[..n])?;
            }
            file.flush()
        })();
        if let Err(error) = written {
            for path in paths.iter().chain(std::iter::once(&path)) {
                let _ = fs::remove_file(path);
            }
            return Err(format!(
                "{} could not be copied from Android: {error}",
                item.name
            ));
        }
        paths.push(path);
    }
    Ok(paths)
}

/// `folder/name`, or `folder/stem (n).ext` for the first `n` not taken yet.
fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// The `text/uri-list` of `paths`: one `file://` URI a line, every byte outside the
/// characters a path keeps as they are percent-encoded.
pub fn uri_list(paths: &[PathBuf]) -> String {
    let mut list = String::new();
    for path in paths {
        list.push_str("file://");
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            path.as_os_str().as_bytes().to_vec()
        };
        // A Windows path is named "file:///C:/..." (RFC 8089): the drive gets a slash before it.
        #[cfg(not(unix))]
        let bytes = {
            let path = path.to_string_lossy().replace('\\', "/");
            if path.starts_with('/') {
                path
            } else {
                format!("/{path}")
            }
            .into_bytes()
        };
        for byte in bytes {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                list.push(byte as char);
            } else {
                list.push_str(&format!("%{byte:02X}"));
            }
        }
        list.push_str("\r\n");
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::drag::DragItem;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-file-drag-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn offer() -> DragOffer {
        DragOffer {
            drag_id: 9,
            items: vec![
                DragItem {
                    name: "holiday photo.jpg".into(),
                    size: Some(200_000),
                    mime_type: "image/jpeg".into(),
                    text: None,
                },
                DragItem {
                    name: String::new(),
                    size: None,
                    mime_type: "text/plain".into(),
                    text: Some("hello".into()),
                },
                DragItem {
                    name: "holiday photo.jpg".into(),
                    size: Some(10),
                    mime_type: "image/jpeg".into(),
                    text: None,
                },
            ],
        }
    }

    fn content(item: u32) -> Vec<u8> {
        let len = if item == 0 { 200_000 } else { 10 };
        (0..len).map(|i| (i as u32 + item) as u8).collect()
    }

    #[test]
    fn fr29_dragged_files_land_whole_under_their_names_and_the_list_names_them() {
        let root = scratch("whole");
        let folder = drag_folder(&root, 9).unwrap();
        let paths = materialize(&folder, &offer(), |read| {
            let all = content(read.item);
            let start = read.offset as usize;
            let end = (start + read.length as usize).min(all.len());
            Ok(DragChunk {
                data: all[start..end].to_vec(),
                end: end == all.len(),
            })
        })
        .unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].file_name().unwrap(), "holiday photo.jpg");
        assert_eq!(paths[1].file_name().unwrap(), "holiday photo (2).jpg");
        assert_eq!(fs::read(&paths[0]).unwrap(), content(0));
        assert_eq!(fs::read(&paths[1]).unwrap(), content(2));
        let list = uri_list(&paths);
        assert!(list.contains("holiday%20photo.jpg"));
        assert_eq!(crate::shell::file_drop::paths(&list), paths);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn fr29_a_file_that_cannot_be_read_leaves_nothing_behind() {
        let root = scratch("fails");
        let folder = drag_folder(&root, 9).unwrap();
        let failed = materialize(&folder, &offer(), |read| {
            if read.offset > 0 {
                Err("the app's file went away".into())
            } else {
                Ok(DragChunk {
                    data: vec![1; read.length as usize],
                    end: false,
                })
            }
        });
        assert!(failed.is_err());
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
        remove_leftovers(&root, Duration::ZERO);
        assert!(!folder.exists(), "a leftover folder is removed");
        let _ = fs::remove_dir_all(&root);
    }
}
