//! The daemon over real vsock (loopback, CID 1), against the host side of the handshake.
//!
//! Needs the vsock_loopback kernel module, so it runs only with the
//! `vsock-loopback-tests` feature, as CI does after loading the module.
#![cfg(all(target_os = "linux", feature = "vsock-loopback-tests"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_guest_channeld::{connect, RetryPolicy};
use dsa_guest_transport::vsock::{VsockConnector, VsockListener, VMADDR_CID_ANY, VMADDR_CID_LOCAL};
use dsa_protocol::control::Role;
use dsa_protocol::handshake::{host_accept, local_hello};
use dsa_transport::Listener;

#[test]
fn pr1_the_guest_daemon_connects_and_is_accepted_over_vsock() {
    let port = 47_000 + std::process::id() % 1000;
    let mut listener = VsockListener::bind(VMADDR_CID_ANY, port).expect("binding vsock");
    let host = thread::spawn(move || {
        let mut connection = listener.accept(Duration::from_secs(10)).ok()?;
        let guest = host_accept(
            connection.as_mut(),
            &local_hello(Role::Host, "dsa-hostd vsock"),
            Duration::from_secs(10),
        )
        .ok()?;
        Some(guest.build)
    });
    let mut connector = VsockConnector {
        cid: VMADDR_CID_LOCAL,
        port,
    };
    // Without working vsock loopback the daemon would retry forever; fail instead.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(15));
            stop.store(true, Ordering::Relaxed);
        });
    }
    let (_connection, host_hello) = connect(
        &mut connector,
        &local_hello(Role::Guest, "dsa-channeld vsock"),
        RetryPolicy::default(),
        &stop,
    )
    .expect("connected over vsock within 15 s; is vsock_loopback loaded?");
    assert_eq!(host_hello.build, "dsa-hostd vsock");
    assert_eq!(host.join().unwrap().as_deref(), Some("dsa-channeld vsock"));
}
