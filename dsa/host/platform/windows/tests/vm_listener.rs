//! The VM-following listener, over the in-memory transport.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_windows::guid::Guid;
use dsa_host_windows::vm_listener::{VmBoundListener, VmIdSlot};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

/// A listener whose bind function records every ID it was asked for and hands out the
/// connector for that binding.
/// Every binding made so far: the VM ID and the connector that reaches it.
type Binds = Arc<Mutex<Vec<(Guid, MemoryConnector)>>>;

fn listener() -> (VmBoundListener, VmIdSlot, Binds) {
    let slot: VmIdSlot = Arc::new(Mutex::new(None));
    let binds = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&binds);
    let listener = VmBoundListener::new(
        Arc::clone(&slot),
        Box::new(move |id| {
            let (listener, connector) = memory::pair();
            record.lock().unwrap().push((id, connector));
            Ok(Box::new(listener))
        }),
    );
    (listener, slot, binds)
}

fn id(n: u32) -> Guid {
    Guid::new(n, 0, 0, [0; 8])
}

#[test]
fn pr1_without_a_running_vm_accept_times_out_without_binding() {
    let (mut listener, _slot, binds) = listener();
    assert!(matches!(
        listener.accept(Duration::from_millis(60)),
        Err(TransportError::Timeout)
    ));
    assert!(binds.lock().unwrap().is_empty());
}

#[test]
fn pr1_the_listener_binds_to_the_vm_once_its_id_is_published() {
    let (mut listener, slot, binds) = listener();
    let publisher = {
        let slot = Arc::clone(&slot);
        let binds = Arc::clone(&binds);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            *slot.lock().unwrap() = Some(id(1));
            // The guest connects once the host is listening.
            loop {
                if let Some((_, connector)) = binds.lock().unwrap().first_mut() {
                    return connector.connect(WAIT).unwrap();
                }
                thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let mut accepted = listener.accept(WAIT).expect("accepts once the VM exists");
    let mut guest = publisher.join().unwrap();
    guest.send(b"hello").unwrap();
    assert_eq!(accepted.recv(Some(WAIT)).unwrap(), b"hello");
    assert_eq!(binds.lock().unwrap()[0].0, id(1));
}

#[test]
fn pr1_a_restarted_vm_with_a_new_id_gets_a_new_binding() {
    let (mut listener, slot, binds) = listener();
    *slot.lock().unwrap() = Some(id(1));
    assert!(matches!(
        listener.accept(Duration::from_millis(30)),
        Err(TransportError::Timeout)
    ));
    *slot.lock().unwrap() = Some(id(1));
    assert!(matches!(
        listener.accept(Duration::from_millis(30)),
        Err(TransportError::Timeout)
    ));
    assert_eq!(
        binds.lock().unwrap().len(),
        1,
        "the same VM keeps its binding"
    );

    *slot.lock().unwrap() = Some(id(2));
    assert!(matches!(
        listener.accept(Duration::from_millis(30)),
        Err(TransportError::Timeout)
    ));
    let ids: Vec<Guid> = binds.lock().unwrap().iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [id(1), id(2)]);
}

#[test]
fn pr1_a_failed_bind_is_reported() {
    let slot: VmIdSlot = Arc::new(Mutex::new(Some(id(7))));
    let mut listener = VmBoundListener::new(
        slot,
        Box::new(|_| Err(TransportError::Unsupported("no Hyper-V sockets".into()))),
    );
    assert_eq!(
        listener.accept(WAIT).err(),
        Some(TransportError::Unsupported("no Hyper-V sockets".into()))
    );
}
