//! The behaviour every transport must have, as tests that run against any of them.
//!
//! A crate that implements a transport runs the whole suite with
//! [`transport_conformance!`](crate::transport_conformance), passing an expression that
//! makes a fresh listener and a connector that reaches it. The in-memory fake runs the
//! same suite, which is what lets tests above the transport trust the fake.

use std::thread;
use std::time::Duration;

use crate::{Connection, Connector, Listener, TransportError, MAX_FRAME_LEN};

/// Makes a fresh listener and a connector that reaches it.
pub type MakeTransport<'a> = &'a mut dyn FnMut() -> (Box<dyn Listener>, Box<dyn Connector>);

const WAIT: Duration = Duration::from_secs(10);

/// Generates one `#[test]` per conformance check, named after the requirement it defends.
///
/// ```text
/// dsa_transport::transport_conformance!(memory_pair());
/// ```
#[macro_export]
macro_rules! transport_conformance {
    ($make:expr) => {
        #[test]
        fn pr1_frames_arrive_whole_and_in_order() {
            $crate::conformance::frames_arrive_whole_and_in_order(&mut || $make);
        }
        #[test]
        fn pr1_frames_travel_in_both_directions() {
            $crate::conformance::frames_travel_in_both_directions(&mut || $make);
        }
        #[test]
        fn pr1_a_receive_timeout_leaves_the_connection_usable() {
            $crate::conformance::a_receive_timeout_leaves_the_connection_usable(&mut || $make);
        }
        #[test]
        fn pr1_frames_sent_before_close_arrive_then_close_is_reported() {
            $crate::conformance::frames_sent_before_close_arrive_then_close_is_reported(
                &mut || $make,
            );
        }
        #[test]
        fn pr1_a_dropped_connection_is_reported_as_closed() {
            $crate::conformance::a_dropped_connection_is_reported_as_closed(&mut || $make);
        }
        #[test]
        fn pr1_an_oversized_frame_is_refused_before_sending() {
            $crate::conformance::an_oversized_frame_is_refused_before_sending(&mut || $make);
        }
        #[test]
        fn pr1_accept_times_out_when_nobody_connects() {
            $crate::conformance::accept_times_out_when_nobody_connects(&mut || $make);
        }
    };
}

fn connect(
    make: MakeTransport<'_>,
) -> (Box<dyn Listener>, Box<dyn Connection>, Box<dyn Connection>) {
    let (mut listener, mut connector) = make();
    let client = connector.connect(WAIT).expect("connect");
    let server = listener.accept(WAIT).expect("accept");
    (listener, server, client)
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

pub fn frames_arrive_whole_and_in_order(make: MakeTransport<'_>) {
    let (_listener, mut server, mut client) = connect(make);
    // Sizes around the header length and the read chunk, plus the largest allowed frame.
    let sizes = [
        0,
        1,
        3,
        4,
        5,
        1000,
        65_535,
        65_536,
        65_537,
        1 << 20,
        MAX_FRAME_LEN,
    ];
    let sender = thread::spawn(move || {
        for (seed, len) in sizes.iter().enumerate() {
            client.send(&pattern(*len, seed as u8)).expect("send");
        }
        client
    });
    for (seed, len) in sizes.iter().enumerate() {
        let frame = server.recv(Some(WAIT)).expect("recv");
        assert_eq!(frame.len(), *len, "frame {seed} has the wrong length");
        assert!(
            frame == pattern(*len, seed as u8),
            "frame {seed} arrived corrupted"
        );
    }
    drop(sender.join().expect("sender thread"));
}

pub fn frames_travel_in_both_directions(make: MakeTransport<'_>) {
    let (_listener, mut server, mut client) = connect(make);
    for round in 0..100u32 {
        client.send(&round.to_le_bytes()).expect("client send");
        let got = server.recv(Some(WAIT)).expect("server recv");
        assert_eq!(got, round.to_le_bytes());
        server
            .send(&(round + 1).to_le_bytes())
            .expect("server send");
        let got = client.recv(Some(WAIT)).expect("client recv");
        assert_eq!(got, (round + 1).to_le_bytes());
    }
}

pub fn a_receive_timeout_leaves_the_connection_usable(make: MakeTransport<'_>) {
    let (_listener, mut server, mut client) = connect(make);
    assert_eq!(
        server.recv(Some(Duration::from_millis(50))),
        Err(TransportError::Timeout)
    );
    assert_eq!(
        server.recv(Some(Duration::ZERO)),
        Err(TransportError::Timeout)
    );
    client.send(b"after the timeout").expect("send");
    assert_eq!(server.recv(Some(WAIT)).expect("recv"), b"after the timeout");
}

pub fn frames_sent_before_close_arrive_then_close_is_reported(make: MakeTransport<'_>) {
    let (_listener, mut server, mut client) = connect(make);
    client.send(b"one").expect("send");
    client.send(b"two").expect("send");
    client.close();
    assert_eq!(server.recv(Some(WAIT)).expect("recv"), b"one");
    assert_eq!(server.recv(Some(WAIT)).expect("recv"), b"two");
    assert_eq!(server.recv(Some(WAIT)), Err(TransportError::Closed));
    assert_eq!(client.recv(Some(WAIT)), Err(TransportError::Closed));
}

pub fn a_dropped_connection_is_reported_as_closed(make: MakeTransport<'_>) {
    let (_listener, mut server, client) = connect(make);
    drop(client);
    assert_eq!(server.recv(Some(WAIT)), Err(TransportError::Closed));
}

pub fn an_oversized_frame_is_refused_before_sending(make: MakeTransport<'_>) {
    let (_listener, mut server, mut client) = connect(make);
    let oversized = vec![0u8; MAX_FRAME_LEN + 1];
    assert_eq!(
        client.send(&oversized),
        Err(TransportError::FrameTooLarge {
            len: MAX_FRAME_LEN + 1,
            max: MAX_FRAME_LEN
        })
    );
    client.send(b"still fine").expect("send after the refusal");
    assert_eq!(server.recv(Some(WAIT)).expect("recv"), b"still fine");
}

pub fn accept_times_out_when_nobody_connects(make: MakeTransport<'_>) {
    let (mut listener, _connector) = make();
    match listener.accept(Duration::from_millis(50)) {
        Err(TransportError::Timeout) => {}
        Err(other) => panic!("expected a timeout, got {other}"),
        Ok(_) => panic!("expected a timeout, got a connection"),
    }
}
