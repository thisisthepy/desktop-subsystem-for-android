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

/// The window manager starts processes (a fresh graphics renderer when Android's VM stops,
/// the app windows) while it listens for the guest. A listening socket those processes
/// inherited kept its address after the window manager closed it, so when Android started
/// again in the same VM the window manager's audio, video, camera and sensor ports could
/// not listen again (AddrInUse) for as long as the renderer ran.
#[test]
fn nfr5_a_child_process_does_not_keep_a_closed_listeners_address() {
    let service_id = fresh_service();
    let listener = HvSocketListener::bind(Guid::LOOPBACK, service_id)
        .expect("binding a Hyper-V socket on the loopback partition");
    // A child that outlives the listener, started the way the window manager starts its
    // renderer: std::process::Command, which lets the child inherit inheritable handles.
    let mut child = std::process::Command::new("cmd")
        .args(["/c", "ping", "-n", "6", "127.0.0.1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("starting a child process");
    drop(listener);
    let again = HvSocketListener::bind(Guid::LOOPBACK, service_id);
    let _ = child.kill();
    let _ = child.wait();
    again.expect("listening again on the address a closed listener used, while a child runs");
}
