//! The in-memory local transport: the conformance suite, and the peer it reports.

use std::time::Duration;

use dsa_transport::local::{self, LocalListener, Peer, PeerIgnoringListener};
use dsa_transport::{Connector, Listener};

fn local_pair() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let (listener, connector) = local::pair();
    (
        Box::new(PeerIgnoringListener(listener)),
        Box::new(connector),
    )
}

dsa_transport::transport_conformance!(local_pair());

#[test]
fn pr5_each_connection_comes_with_the_peer_that_made_it() {
    let (mut listener, mut connector) = local::pair();
    let alice = Peer {
        user: "S-1-5-21-1-2-3-1001".into(),
    };
    let bob = Peer {
        user: "uid:1002".into(),
    };
    let mut from_alice = connector.connect_as(alice.clone()).unwrap();
    let _from_bob = connector.connect_as(bob.clone()).unwrap();
    let (mut first, first_peer) = listener.accept(Duration::from_secs(1)).unwrap();
    let (_, second_peer) = listener.accept(Duration::from_secs(1)).unwrap();
    assert_eq!((first_peer, second_peer), (alice, bob));
    from_alice.send(b"hi").unwrap();
    assert_eq!(first.recv(Some(Duration::from_secs(1))).unwrap(), b"hi");
}
