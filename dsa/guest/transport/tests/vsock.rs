//! The transport conformance suite over vsock loopback (CID 1).
//!
//! Loopback needs the `vsock_loopback` kernel module, which most machines do not load, so
//! this suite runs only with the `vsock-loopback-tests` feature:
//!
//! ```text
//! sudo modprobe vsock_loopback
//! cargo test -p dsa-guest-transport --features vsock-loopback-tests
//! ```
#![cfg(all(target_os = "linux", feature = "vsock-loopback-tests"))]

use std::sync::atomic::{AtomicU32, Ordering};

use dsa_guest_transport::vsock::{VsockConnector, VsockListener, VMADDR_CID_ANY, VMADDR_CID_LOCAL};
use dsa_transport::{Connector, Listener};

fn fresh_port() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // Spread by process so parallel test runs on one machine do not collide.
    40_000 + (std::process::id() % 1000) * 64 + NEXT.fetch_add(1, Ordering::Relaxed)
}

fn vsock_loopback() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let port = fresh_port();
    let listener = VsockListener::bind(VMADDR_CID_ANY, port).expect("binding a vsock listener");
    let connector = VsockConnector {
        cid: VMADDR_CID_LOCAL,
        port,
    };
    (Box::new(listener), Box::new(connector))
}

dsa_transport::transport_conformance!(vsock_loopback());
