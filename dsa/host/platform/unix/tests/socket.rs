//! The local API over a real Unix socket.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dsa_host_unix::socket::{current_uid, UnixLocalConnector, UnixLocalListener};
use dsa_transport::local::{LocalListener, PeerIgnoringListener};
use dsa_transport::{Connector, Listener};

/// A fresh socket path under the build's temporary directory.
///
/// A socket path is limited to about 100 bytes, which a deep checkout (such as a CI
/// runner's) exceeds, so the path is given relative to the test's working directory.
fn socket_path() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("l");
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!(
        "{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    relative_to_cwd(&dir).join(name)
}

fn relative_to_cwd(target: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let target = target.canonicalize().unwrap();
    let common = cwd
        .components()
        .zip(target.components())
        .take_while(|(a, b)| a == b)
        .count();
    let mut relative: PathBuf = cwd.components().skip(common).map(|_| "..").collect();
    relative.extend(target.components().skip(common));
    relative
}

fn unix_pair() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let path = socket_path();
    let listener = UnixLocalListener::bind(&path, 0o600).expect("binding the socket");
    (
        Box::new(PeerIgnoringListener(listener)),
        Box::new(UnixLocalConnector { path }),
    )
}

dsa_transport::transport_conformance!(unix_pair());

#[test]
fn pr5_the_server_learns_the_clients_user_from_the_kernel() {
    let path = socket_path();
    let mut listener = UnixLocalListener::bind(&path, 0o600).unwrap();
    let _client = UnixLocalConnector { path }
        .connect(Duration::from_secs(1))
        .unwrap();
    let (_, peer) = listener.accept(Duration::from_secs(5)).unwrap();
    assert_eq!(peer.user, format!("uid:{}", current_uid()));
}

#[test]
fn pr5_the_socket_file_gets_the_permissions_asked_for() {
    let path = socket_path();
    let _listener = UnixLocalListener::bind(&path, 0o600).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn pr5_a_stale_socket_is_replaced_but_another_file_is_never_deleted() {
    let path = socket_path();
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap()); // leaves the file behind
    UnixLocalListener::bind(&path, 0o600).expect("a stale socket file is replaced");

    let regular = socket_path();
    std::fs::write(&regular, b"keep me").unwrap();
    assert!(UnixLocalListener::bind(&regular, 0o600).is_err());
    assert_eq!(std::fs::read(&regular).unwrap(), b"keep me");
}

#[test]
fn pr5_the_socket_file_is_removed_when_the_listener_goes() {
    let path = socket_path();
    drop(UnixLocalListener::bind(&path, 0o600).unwrap());
    assert!(!path.exists());
}
