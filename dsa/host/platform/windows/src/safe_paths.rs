//! Comparing Windows paths by their text, for the checks of [`crate::safe_fs`]: the same on
//! every platform the tests run on.

use std::path::Path;

/// `path` as Windows compares it: without the `\\?\` prefix a final path has, with
/// backslashes, without a separator at the end, and in lower case.
fn normalised(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    };
    text.trim_end_matches('\\').to_lowercase()
}

/// Whether `a` and `b` name the same place.
pub fn same_path(a: &Path, b: &Path) -> bool {
    normalised(a) == normalised(b)
}

/// Whether `inner` is `root` or a place inside it, by whole names: `C:\DSA2` is not inside
/// `C:\DSA`, and a path with a `..` part is never inside anything.
pub fn is_below(inner: &Path, root: &Path) -> bool {
    let (inner, root) = (normalised(inner), normalised(root));
    if inner.split('\\').any(|part| part == ".." || part == ".") {
        return false;
    }
    inner == root || inner.starts_with(&format!("{root}\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr32_final_paths_compare_without_their_prefix_case_or_last_separator() {
        assert!(same_path(
            Path::new(r"\\?\C:\Users\Me\AppData\Local\DSA"),
            Path::new(r"c:\users\me\appdata\local\dsa\")
        ));
        assert!(same_path(
            Path::new(r"\\?\UNC\server\share\DSA"),
            Path::new(r"\\server\share\dsa")
        ));
        assert!(!same_path(
            Path::new(r"\\?\C:\Windows\System32"),
            Path::new(r"C:\Users\me\AppData\Local\DSA")
        ));
    }

    #[test]
    fn fr32_a_place_is_inside_a_folder_only_by_whole_names() {
        let root = Path::new(r"C:\Users\me\AppData\Local\DSA");
        assert!(is_below(
            Path::new(r"C:\Users\me\AppData\Local\DSA\keys"),
            root
        ));
        assert!(is_below(root, root));
        assert!(!is_below(
            Path::new(r"C:\Users\me\AppData\Local\DSA2"),
            root
        ));
        assert!(!is_below(
            Path::new(r"C:\Users\me\AppData\Local\DSA\..\..\Roaming"),
            root
        ));
    }
}
