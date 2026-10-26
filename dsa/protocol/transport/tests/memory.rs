//! The in-memory fake passes the same conformance suite as the socket transports.

use dsa_transport::memory;
use dsa_transport::{Connector, Listener};

fn memory_pair() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let (listener, connector) = memory::pair();
    (Box::new(listener), Box::new(connector))
}

dsa_transport::transport_conformance!(memory_pair());

#[test]
fn pr1_connecting_without_a_listener_is_reported_as_closed() {
    let (listener, mut connector) = memory::pair();
    drop(listener);
    assert_eq!(
        connector.connect(std::time::Duration::from_secs(1)).err(),
        Some(dsa_transport::TransportError::Closed)
    );
}

#[test]
fn pr1_a_cloned_sender_sends_while_another_thread_waits_to_receive() {
    let (mut listener, mut connector) = memory::pair();
    let mut near = connector
        .connect(std::time::Duration::from_secs(1))
        .unwrap();
    let mut far = listener.accept(std::time::Duration::from_secs(1)).unwrap();
    let mut sender = near
        .try_clone_sender()
        .expect("memory connections share a sender");
    let reader = std::thread::spawn(move || near.recv(Some(std::time::Duration::from_secs(5))));
    sender.send(b"one").unwrap();
    sender.send(b"two").unwrap();
    assert_eq!(
        far.recv(Some(std::time::Duration::from_secs(5))).unwrap(),
        b"one"
    );
    assert_eq!(
        far.recv(Some(std::time::Duration::from_secs(5))).unwrap(),
        b"two"
    );
    far.send(b"back").unwrap();
    assert_eq!(reader.join().unwrap().unwrap(), b"back");
}
