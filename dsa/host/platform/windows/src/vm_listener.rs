//! A listener for the Android VM's control channel that follows the VM's ID.
//!
//! Hyper-V sockets are addressed by the VM's runtime ID, which HCS assigns each time the VM
//! is created, so the host cannot bind its listener before the VM exists, and must bind
//! again after a restart. [`VmBoundListener`] waits until the launcher publishes the ID in
//! a [`VmIdSlot`], binds then, and rebinds when the ID changes.
//!
//! The binding itself is a function, so this logic is tested on every platform with the
//! in-memory transport; on Windows the function binds an `HvSocketListener`.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_transport::{Connection, Listener, TransportError};

use crate::guid::Guid;

/// Where the launcher publishes the running VM's ID, and clears it when the VM stops.
pub type VmIdSlot = Arc<Mutex<Option<Guid>>>;

/// Makes the listener for one VM ID.
pub type BindFn = Box<dyn FnMut(Guid) -> Result<Box<dyn Listener>, TransportError> + Send>;

/// How often to look for the VM ID while none is published.
const POLL: Duration = Duration::from_millis(20);

pub struct VmBoundListener {
    slot: VmIdSlot,
    bind: BindFn,
    bound: Option<(Guid, Box<dyn Listener>)>,
}

impl VmBoundListener {
    pub fn new(slot: VmIdSlot, bind: BindFn) -> VmBoundListener {
        VmBoundListener {
            slot,
            bind,
            bound: None,
        }
    }

    fn current_id(&self) -> Option<Guid> {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Listener for VmBoundListener {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let deadline = Instant::now() + timeout;
        let id = loop {
            if let Some(id) = self.current_id() {
                break id;
            }
            // No VM, so nobody can connect; a listener for an old VM is no use either.
            self.bound = None;
            let now = Instant::now();
            if now >= deadline {
                return Err(TransportError::Timeout);
            }
            thread::sleep(POLL.min(deadline - now));
        };
        if self.bound.as_ref().map(|(bound_id, _)| *bound_id) != Some(id) {
            self.bound = None;
            let listener = (self.bind)(id)?;
            self.bound = Some((id, listener));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (_, listener) = self.bound.as_mut().expect("bound above");
        listener.accept(remaining)
    }
}
