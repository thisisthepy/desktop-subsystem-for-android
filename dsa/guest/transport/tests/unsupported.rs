//! Where vsock cannot reach, connecting fails with an error rather than hanging or
//! panicking.
#![cfg(target_os = "linux")]

use std::time::Duration;

use dsa_guest_transport::vsock::VsockConnector;
use dsa_transport::Connector;

#[test]
fn pr1_connecting_to_a_cid_that_does_not_exist_fails_within_the_timeout() {
    // CID 0xfffffffe is reserved and never assigned to a machine.
    let mut connector = VsockConnector {
        cid: 0xffff_fffe,
        port: 1,
    };
    let started = std::time::Instant::now();
    assert!(connector.connect(Duration::from_millis(500)).is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
}
