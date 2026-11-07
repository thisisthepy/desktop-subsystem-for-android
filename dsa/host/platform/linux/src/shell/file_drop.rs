//! Files dropped on a task window, as a `text/uri-list`.
//!
//! A file manager drags files as a list of `file://` URIs, one a line, with `#` lines as
//! comments and characters outside ASCII percent-encoded. Each local file becomes a path; a
//! URI of another scheme (a web link) or of another machine is left out, since the guest is
//! handed the file's content.

use std::path::PathBuf;

/// The type a drop of files is read as.
pub const URI_LIST: &str = "text/uri-list";

/// The local files a `text/uri-list` names, in order.
pub fn paths(list: &str) -> Vec<PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(local_path)
        .collect()
}

/// The path of a `file://` URI on this machine (no host, or `localhost`).
fn local_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")?
    };
    let bytes = percent_decode(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    // "file:///C:/dir/name" is the path "C:\dir\name": the slash before the drive goes.
    #[cfg(not(unix))]
    {
        let path = String::from_utf8(bytes).ok()?;
        let path = match path.as_bytes() {
            [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => &path[1..],
            _ => &path[..],
        };
        Some(PathBuf::from(path.replace('/', "\\")))
    }
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = text.get(at + 1..at + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr9_dropped_files_are_read_from_their_uris() {
        let list = "# from the file manager\r\nfile:///home/me/a.txt\r\n\
                    file://localhost/home/me/My%20Photo.png\r\n";
        assert_eq!(
            paths(list),
            vec![
                PathBuf::from("/home/me/a.txt"),
                PathBuf::from("/home/me/My Photo.png")
            ]
        );
    }

    #[test]
    fn fr9_a_web_link_or_a_file_of_another_machine_is_left_out() {
        let list = "https://example.com/a\nfile://server/share/a\nfile:///ok\n";
        assert_eq!(paths(list), vec![PathBuf::from("/ok")]);
    }

    #[test]
    fn fr9_a_name_outside_ascii_is_decoded() {
        assert_eq!(
            paths("file:///tmp/%ED%95%9C.txt"),
            vec![PathBuf::from("/tmp/\u{d55c}.txt")]
        );
        assert_eq!(paths("file:///bad%zz"), Vec::<PathBuf>::new());
    }
}
