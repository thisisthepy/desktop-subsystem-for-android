//! The local API over a real Unix socket.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dsa_host_unix::socket::{current_uid, UnixLocalConnector, UnixLocalListener};
use dsa_transport::local::{LocalListener, PeerIgnoringListener};
use dsa_transport::{Connector, Listener};

fn socket_path() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("local-api");
    std::fs::create_dir_all(&dir).unwrap();
    // Socket paths are limited to about 100 bytes, so keep the name short.
    dir.join(format!(
        "{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
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
