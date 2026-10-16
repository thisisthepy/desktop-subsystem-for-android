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
