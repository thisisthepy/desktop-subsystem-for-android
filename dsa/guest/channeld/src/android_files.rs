//! Serving Android's shared storage to the host: `dsa-channeld --android-files`.
//!
//! The host asks for it (`AndroidFiles`); the daemon writes the port and the token to the
//! property [`PROPERTY`] ([`property_value`]), and init restarts the `dsa_files` service, which
//! runs this as Android's `media_rw` user, the owner of the storage, so the files it writes
//! belong where Android's own do. It listens on the vsock port, takes connections from the
//! host only, and answers each with the WebDAV server in [`crate::webdav`].

use dsa_protocol::control::AndroidFiles;

/// The property that carries the port and the token: [`property_value`].
pub const PROPERTY: &str = "vendor.dsa.files";

/// The property's value for `files`: `off`, or `<port> <token>`.
pub fn property_value(files: &AndroidFiles) -> String {
    let token_ok = !files.token.is_empty()
        && files.token.len() <= 64
        && files.token.chars().all(|c| c.is_ascii_alphanumeric());
    if files.port == 0 || !token_ok {
        return "off".into();
    }
    format!("{} {}", files.port, files.token)
}

/// The port and the token a property value asks for; `None` for `off`.
pub fn parse(value: &str) -> Option<(u32, String)> {
    let (port, token) = value.trim().split_once(' ')?;
    let port = port.parse::<u32>().ok().filter(|port| *port != 0)?;
    (!token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| (port, token.to_string()))
}

/// What the server says when it cannot reach the shared storage. A permission denial on a
/// folder the service's user owns is the guest's SELinux policy, so it names that.
pub fn unreachable_storage(error: &std::io::Error) -> String {
    let storage = crate::share_mount::STORAGE;
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "cannot reach {storage}: {error}; the guest's SELinux policy refuses the way down \
             (look for an avc denial for dsa_files), so every request will fail"
        )
    } else {
        format!("cannot reach {storage}: {error}; every request will fail")
    }
}

/// Runs the server until the process is stopped. Returns the exit status.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn run(read_property: &dyn Fn(&str) -> String, say: &(dyn Fn(&str) + Sync)) -> i32 {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let Some((port, token)) = parse(&read_property(PROPERTY)) else {
        say("the host does not ask for Android's files");
        return 0;
    };
    // The host's own folders are not served back to it.
    let hidden = crate::share_mount::parse(&read_property(crate::share_mount::PROPERTY))
        .map(|wanted| vec![wanted.location])
        .unwrap_or_else(|| vec![crate::share_mount::LOCATION.to_string()]);
    // Said once when the server starts: every request would fail the same way, and the
    // host's tries would otherwise go unexplained.
    if let Err(error) = std::fs::symlink_metadata(crate::share_mount::STORAGE) {
        say(&unreachable_storage(&error));
    }
    let config = crate::webdav::Config {
        root: std::path::PathBuf::from(crate::share_mount::STORAGE),
        token,
        hidden,
    };
    // SAFETY: plain socket calls, each result checked.
    let listener = unsafe {
        let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            say(&format!(
                "cannot serve Android's files: {}",
                std::io::Error::last_os_error()
            ));
            return 1;
        }
        let socket = OwnedFd::from_raw_fd(fd);
        let mut address: libc::sockaddr_vm = std::mem::zeroed();
        address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        address.svm_cid = libc::VMADDR_CID_ANY;
        address.svm_port = port;
        if libc::bind(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_vm).cast(),
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        ) != 0
            || libc::listen(socket.as_raw_fd(), 16) != 0
        {
            say(&format!(
                "cannot listen on vsock port {port}: {}",
                std::io::Error::last_os_error()
            ));
            return 1;
        }
        socket
    };
    say(&format!(
        "serving Android's files to the host on vsock port {port}"
    ));
    loop {
        // SAFETY: an out address of the size given.
        let (fd, peer) = unsafe {
            let mut peer: libc::sockaddr_vm = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
            let fd = libc::accept4(
                listener.as_raw_fd(),
                (&mut peer as *mut libc::sockaddr_vm).cast(),
                &mut len,
                libc::SOCK_CLOEXEC,
            );
            (fd, peer.svm_cid)
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            say(&format!("accepting a connection failed: {error}"));
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }
        // SAFETY: accept4 returned it and nothing else owns it.
        let connection = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        // Only the host is served; an app in the guest that reaches the port is not.
        if peer != libc::VMADDR_CID_HOST {
            continue;
        }
        let config = config.clone();
        let spawned = std::thread::Builder::new()
            .name("dsa-files".into())
            .spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _ = crate::webdav::serve(connection, &config);
                }));
            });
        if let Err(error) = spawned {
            say(&format!("a connection cannot be served: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr29_the_hosts_request_for_androids_files_becomes_a_property_and_back() {
        let files = AndroidFiles {
            port: 5591,
            token: "3f9a".into(),
        };
        assert_eq!(property_value(&files), "5591 3f9a");
        assert_eq!(parse("5591 3f9a"), Some((5591, "3f9a".to_string())));
        assert_eq!(
            property_value(&AndroidFiles {
                port: 0,
                ..files.clone()
            }),
            "off"
        );
        assert_eq!(
            property_value(&AndroidFiles {
                token: "a b;c".into(),
                ..files
            }),
            "off"
        );
        assert_eq!(parse("off"), None);
    }

    #[test]
    fn fr29_a_refused_storage_is_reported_as_the_policy() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let said = unreachable_storage(&denied);
        assert!(said.contains("/data/media/0"), "{said}");
        assert!(said.contains("SELinux"), "{said}");
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(!unreachable_storage(&missing).contains("SELinux"));
    }

    /// The server reaches /data/media/0 through /data and /data/media, which have labels of
    /// their own; without the rules its sandbox refuses the way down, and every request was
    /// answered 403.
    #[test]
    fn fr29_the_file_server_may_pass_through_data_media_to_the_shared_storage() {
        let policy = include_str!("../../image/sepolicy/files.cil");
        assert!(
            policy
                .lines()
                .any(|line| line.trim() == "(allow dsa_files media_userdir_file (dir (search)))"),
            "files.cil does not let dsa_files pass through /data/media"
        );
    }

    /// The server reaches /data/media/0 through /data, which has a label of its own; without
    /// the rule its sandbox refuses the way down, and every request was answered 403.
    #[test]
    fn fr29_the_file_server_may_pass_through_data_to_the_shared_storage() {
        let policy = include_str!("../../image/sepolicy/files.cil");
        assert!(
            policy.lines().any(|line| line.trim()
                == "(allow dsa_files system_data_root_file (dir (search getattr)))"),
            "files.cil does not let dsa_files pass through /data"
        );
    }
}
