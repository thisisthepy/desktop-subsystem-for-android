//! The local API over a real named pipe.
#![cfg(windows)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dsa_host_windows::pipe::{NamedPipeConnector, NamedPipeListener, DEFAULT_PIPE_SDDL};
use dsa_transport::local::{LocalListener, PeerIgnoringListener};
use dsa_transport::{Connector, Listener};

/// Grants the current user too, since CI runs tests in a service session, not
/// interactively, and IU would not match.
const TEST_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GRGW;;;IU)(A;;GRGW;;;OW)(A;;GRGW;;;AU)";

fn fresh_name() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "dsa-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn pipe_pair() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let name = fresh_name();
    let listener = NamedPipeListener::bind(&name, TEST_SDDL).expect("creating the pipe");
    (
        Box::new(PeerIgnoringListener(listener)),
        Box::new(NamedPipeConnector { name }),
    )
}

dsa_transport::transport_conformance!(pipe_pair());

#[test]
fn pr5_the_service_learns_the_clients_user_from_windows() {
    let name = fresh_name();
    let mut listener = NamedPipeListener::bind(&name, TEST_SDDL).unwrap();
    let _client = NamedPipeConnector { name }
        .connect(Duration::from_secs(5))
        .unwrap();
    let (_, peer) = listener.accept(Duration::from_secs(5)).unwrap();
    let expected = dsa_host_windows::pipe::current_user_sid().unwrap();
    assert_eq!(peer.user, expected);
}

#[test]
fn pr5_a_second_server_cannot_take_over_the_name() {
    let name = fresh_name();
    let _first = NamedPipeListener::bind(&name, TEST_SDDL).unwrap();
    assert!(NamedPipeListener::bind(&name, TEST_SDDL).is_err());
}

#[test]
fn pr5_an_invalid_security_descriptor_is_refused() {
    assert!(NamedPipeListener::bind(&fresh_name(), "not sddl").is_err());
    assert!(DEFAULT_PIPE_SDDL.starts_with("D:P"));
}
