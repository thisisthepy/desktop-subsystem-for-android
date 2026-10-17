//! The transport conformance suite over a real Hyper-V socket, host to host.
//!
//! `Guid::LOOPBACK` lets a host process connect to its own Hyper-V socket listener, so the
//! production transport runs the same checks as the in-memory fake without a VM. It needs
//! the Hyper-V socket provider, which the Virtual Machine Platform feature (required by
//! WSL 2) installs.
#![cfg(windows)]

use std::sync::atomic::{AtomicU16, Ordering};

use dsa_host_windows::guid::Guid;
use dsa_host_windows::hvsocket::{HvSocketConnector, HvSocketListener};
use dsa_transport::{Connector, Listener};

/// A service ID no other test (or test process) is using.
fn fresh_service() -> Guid {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    Guid::new(pid, n, 0x4453, *b"dsa-test")
}

fn hvsocket_loopback() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let service_id = fresh_service();
    let listener = HvSocketListener::bind(Guid::LOOPBACK, service_id)
        .expect("binding a Hyper-V socket on the loopback partition");
    let connector = HvSocketConnector {
        vm_id: Guid::LOOPBACK,
        service_id,
    };
    (Box::new(listener), Box::new(connector))
}

dsa_transport::transport_conformance!(hvsocket_loopback());
