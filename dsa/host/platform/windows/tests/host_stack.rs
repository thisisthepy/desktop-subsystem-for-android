//! The pieces dsa-hostd puts together (session, VM-following listener, protocol
//! handshake), with the VM and Hyper-V sockets replaced by fakes: the launcher publishes
//! a new VM ID on every start, and the listener binds the in-memory transport to it.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_host_windows::guid::Guid;
use dsa_host_windows::vm_listener::{VmBoundListener, VmIdSlot};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::Role;
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(10);

/// A VM that gets a fresh ID each time it starts.
struct FakeVm {
    slot: VmIdSlot,
    starts: u32,
    /// The running boot's secret, as the guest's daemon would read it.
    secret: Arc<Mutex<Option<BootSecret>>>,
}

impl GuestLauncher for FakeVm {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.secret.lock().unwrap() = Some(secret.clone());
        self.starts += 1;
        *self.slot.lock().unwrap() = Some(Guid::new(self.starts, 0, 0, [0; 8]));
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.slot.lock().unwrap() = None;
        Ok(())
    }
}

/// Where "the guest" reaches the host: the connector of each binding, and the secret the
/// most recent start handed it.
#[derive(Clone, Default)]
struct Bindings {
    connectors: Arc<Mutex<Vec<(Guid, MemoryConnector)>>>,
    secret: Arc<Mutex<Option<BootSecret>>>,
}

fn host() -> (Session, VmIdSlot, Bindings) {
    let slot: VmIdSlot = Arc::default();
    let bindings = Bindings::default();
    let record = Arc::clone(&bindings.connectors);
    let listener = VmBoundListener::new(
        Arc::clone(&slot),
        Box::new(move |vm_id| {
            let (listener, connector) = memory::pair();
            record.lock().unwrap().push((vm_id, connector));
            Ok(Box::new(listener) as Box<dyn Listener>)
        }),
    );
    let launcher = FakeVm {
        slot: Arc::clone(&slot),
        starts: 0,
        secret: Arc::clone(&bindings.secret),
    };
    let config = SessionConfig {
        connect_timeout: WAIT,
        stop_timeout: WAIT,
        ..SessionConfig::default()
    };
    let session = Session::new(
        launcher,
        Box::new(listener),
        ControlHandshake::new("dsa-hostd test", WAIT),
        config,
    );
    (session, slot, bindings)
}

/// Connects as the guest's channel daemon, to the VM's current binding.
fn guest(bindings: &Bindings, vm: u32) -> Box<dyn Connection> {
    let expected = Guid::new(vm, 0, 0, [0; 8]);
    for _ in 0..1000 {
        if let Some((_, connector)) = bindings
            .connectors
            .lock()
            .unwrap()
            .iter_mut()
            .find(|(id, _)| *id == expected)
        {
            let mut connection = connector.connect(WAIT).unwrap();
            let secret = bindings.secret.lock().unwrap().clone();
            guest_connect(
                connection.as_mut(),
                &local_hello(Role::Guest, "dsa-guestd test"),
                &secret.expect("the VM was started"),
                WAIT,
            )
            .unwrap();
            return connection;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("the host never listened on VM {vm}");
}

#[test]
fn fr2_the_host_reaches_active_through_the_vm_bound_listener_and_the_handshake() {
    let (session, slot, bindings) = host();
    session.start();
    let _guest = guest(&bindings, 1);
    assert!(session.wait_for(SessionState::Active, WAIT));

    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(
        *slot.lock().unwrap(),
        None,
        "the stopped VM's ID is withdrawn"
    );
}

#[test]
fn fr2_after_a_restart_the_host_listens_on_the_new_vm() {
    let (session, _slot, bindings) = host();
    session.start();
    let _first = guest(&bindings, 1);
    assert!(session.wait_for(SessionState::Active, WAIT));
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));

    session.start();
    let _second = guest(&bindings, 2);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let ids: Vec<u32> = bindings
        .connectors
        .lock()
        .unwrap()
        .iter()
        .map(|(id, _)| id.data1)
        .collect();
    assert_eq!(ids, [1, 2]);
}

#[test]
fn nfr5_a_guest_that_dies_leaves_the_host_listening_for_the_next_vm() {
    let (session, slot, bindings) = host();
    session.start();
    let guest1 = guest(&bindings, 1);
    assert!(session.wait_for(SessionState::Active, WAIT));

    // The guest VM dies and its channel closes.
    drop(guest1);
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(*slot.lock().unwrap(), None, "the dead VM's ID is withdrawn");

    // The host remains alive and can start a new VM.
    session.start();
    let _guest2 = guest(&bindings, 2);
    assert!(session.wait_for(SessionState::Active, WAIT));

    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
}
