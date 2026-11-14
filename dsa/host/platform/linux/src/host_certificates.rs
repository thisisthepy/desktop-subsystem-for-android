//! The root certificates an administrator added to the Linux system's certificate store, for
//! the user's Android to trust beside its own.
//!
//! Every distribution builds its trust store from the Mozilla bundle it packages and from the
//! folders an administrator puts certificates in. Those folders hold only what someone added,
//! so they are what the service reads ([`ADDED_FOLDERS`]): `update-ca-certificates` reads
//! `/usr/local/share/ca-certificates` (Debian, Ubuntu, Alpine), `update-ca-trust` reads
//! `/etc/pki/ca-trust/source/anchors` (Fedora, RHEL), `trust` reads
//! `/etc/ca-certificates/trust-source/anchors` (Arch) and `update-ca-certificates` on SUSE reads
//! `/etc/pki/trust/anchors`. A certificate there that is also in the distribution's Mozilla bundle
//! ([`BUNDLE_FOLDERS`]) is left out, so the bundle's roots are never given twice.
//!
//! The folders are followed with inotify: a certificate added or removed wakes the watcher. A
//! folder that does not exist yet is followed through its nearest parent that does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dsa_host_core::host_certificates::{self, source, FoundCertificate, HostRootStore};

/// The folders an administrator adds root certificates to, on each family of distributions.
pub const ADDED_FOLDERS: &[&str] = &[
    "/usr/local/share/ca-certificates",
    "/etc/pki/ca-trust/source/anchors",
    "/etc/ca-certificates/trust-source/anchors",
    "/etc/pki/trust/anchors",
];

/// The distributions' own bundles, as one certificate per file.
pub const BUNDLE_FOLDERS: &[&str] = &["/usr/share/ca-certificates/mozilla"];

/// How deep a folder of added certificates is read: Debian's tools read its subfolders.
const MAX_DEPTH: usize = 4;

/// The system's added certificates, under `root` (`/` but in tests).
pub struct LinuxRootStore {
    root: PathBuf,
}

impl LinuxRootStore {
    pub fn system() -> LinuxRootStore {
        LinuxRootStore::under("/")
    }

    pub fn under(root: impl Into<PathBuf>) -> LinuxRootStore {
        LinuxRootStore { root: root.into() }
    }

    fn folder(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}

impl HostRootStore for LinuxRootStore {
    fn added_roots(&self) -> Result<Vec<FoundCertificate>, String> {
        let mut bundle = std::collections::HashSet::new();
        for folder in BUNDLE_FOLDERS {
            for (_, der) in certificates_under(&self.folder(folder), 0) {
                bundle.insert(der);
            }
        }
        let mut found = Vec::new();
        for folder in ADDED_FOLDERS {
            for (file, der) in certificates_under(&self.folder(folder), 0) {
                found.push(FoundCertificate::new(
                    der,
                    source::SYSTEM,
                    file.display().to_string(),
                ));
            }
        }
        Ok(host_certificates::added_roots(found, |certificate| {
            bundle.contains(&certificate.der)
        }))
    }

    fn wait_for_change(&self, timeout: Duration) {
        let folders: Vec<PathBuf> = ADDED_FOLDERS.iter().map(|f| self.folder(f)).collect();
        wait_for_folder_change(&folders, timeout);
    }
}

/// Each certificate in the files under `folder`, with its file, in the order of the files'
/// names. Links are followed, as the distributions' tools do; a file that cannot be read is
/// skipped.
fn certificates_under(folder: &Path, depth: usize) -> Vec<(PathBuf, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    let mut found = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if depth + 1 < MAX_DEPTH {
                found.extend(certificates_under(&path, depth + 1));
            }
            continue;
        }
        if !meta.is_file() || meta.len() > 4 * 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        for der in host_certificates::certificates_in_file(&bytes) {
            found.push((path.clone(), der));
        }
    }
    found
}

/// Waits until a file in `folders` (or below them) is added, changed, moved or removed, or
/// `timeout` passes.
#[cfg(target_os = "linux")]
fn wait_for_folder_change(folders: &[PathBuf], timeout: Duration) {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: a new inotify descriptor, closed below.
    let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    if fd < 0 {
        std::thread::sleep(timeout);
        return;
    }
    let mask = libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MODIFY
        | libc::IN_CLOSE_WRITE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_ATTRIB
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF;
    let mut watching = 0;
    for folder in folders {
        // The folder, its subfolders, or the nearest parent that exists.
        let mut targets: Vec<PathBuf> = Vec::new();
        if folder.is_dir() {
            targets.push(folder.clone());
            if let Ok(entries) = std::fs::read_dir(folder) {
                targets.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        } else if let Some(parent) = folder.ancestors().skip(1).find(|p| p.is_dir()) {
            targets.push(parent.to_path_buf());
        }
        for target in targets {
            let Ok(path) = std::ffi::CString::new(target.as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: an open inotify descriptor and a NUL-terminated path.
            if unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } >= 0 {
                watching += 1;
            }
        }
    }
    if watching > 0 {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one pollfd for the open descriptor.
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready > 0 {
            // A tool writes several files at once; the rest of them land before the read.
            std::thread::sleep(Duration::from_millis(500));
        }
    } else {
        std::thread::sleep(timeout);
    }
    // SAFETY: the descriptor opened above.
    unsafe { libc::close(fd) };
}

#[cfg(not(target_os = "linux"))]
fn wait_for_folder_change(_folders: &[PathBuf], timeout: Duration) {
    std::thread::sleep(timeout);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dsa-linux-certs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A small certificate the host can read: a subject and issuer of one common name.
    fn certificate(common_name: &str) -> Vec<u8> {
        fn der(tag: u8, contents: &[u8]) -> Vec<u8> {
            // DER gives a length below 128 in one byte, and a longer one after a byte that
            // says how many bytes it takes.
            let mut out = vec![tag];
            match contents.len() {
                short @ 0..=127 => out.push(short as u8),
                medium @ 128..=255 => out.extend_from_slice(&[0x81, medium as u8]),
                long => out.extend_from_slice(&[0x82, (long >> 8) as u8, long as u8]),
            }
            out.extend_from_slice(contents);
            out
        }
        let name = der(
            0x30,
            &der(
                0x31,
                &der(
                    0x30,
                    &[
                        der(0x06, &[0x55, 0x04, 0x03]),
                        der(0x0c, common_name.as_bytes()),
                    ]
                    .concat(),
                ),
            ),
        );
        let algorithm = der(
            0x30,
            &der(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]),
        );
        let tbs = der(
            0x30,
            &[
                der(0x02, &[1]),
                algorithm.clone(),
                name.clone(),
                der(
                    0x30,
                    &[der(0x17, b"250101000000Z"), der(0x17, b"300101000000Z")].concat(),
                ),
                name,
                der(0x30, &[algorithm.clone(), der(0x03, &[0, 1])].concat()),
            ]
            .concat(),
        );
        der(0x30, &[tbs, algorithm, der(0x03, &[0, 1])].concat())
    }

    #[test]
    fn fr28_certificates_an_administrator_added_are_read_and_the_bundles_are_not() {
        let root = scratch("added");
        let store = LinuxRootStore::under(&root);
        let bundled = certificate("Bundled Root");
        let added = certificate("Company Root");
        let fedora = certificate("Fedora Admin Root");
        let mozilla = store.folder(BUNDLE_FOLDERS[0]);
        std::fs::create_dir_all(&mozilla).unwrap();
        std::fs::write(mozilla.join("Bundled_Root.crt"), &bundled).unwrap();
        let local = store.folder("/usr/local/share/ca-certificates/company");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("company.crt"), &added).unwrap();
        // An administrator copied a bundled root there too; it is not given twice.
        std::fs::write(local.join("bundled-copy.crt"), &bundled).unwrap();
        let anchors = store.folder("/etc/pki/ca-trust/source/anchors");
        std::fs::create_dir_all(&anchors).unwrap();
        std::fs::write(anchors.join("fedora.der"), &fedora).unwrap();

        let found = store.added_roots().unwrap();
        let ders: Vec<&Vec<u8>> = found.iter().map(|f| &f.der).collect();
        assert_eq!(ders, vec![&added, &fedora]);
        assert!(found.iter().all(|f| f.source == source::SYSTEM));
        assert!(
            found[0].location.ends_with("company.crt"),
            "{}",
            found[0].location
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
