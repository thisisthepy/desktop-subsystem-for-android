//! Who gets which frame: the daemon's routing between the host and the guest's own users of
//! the channel.
//!
//! - A frame from the host goes to every live listener.
//! - A frame a guest client sends goes to the host. A task's bounds or its end
//!   (`TaskUpdated`, `TaskRemoved`) also go to every listener in the other guest processes,
//!   because the hwcomposer groups layers by the task bounds the agent reports.
//! - A listener whose process has died is dropped.
//! - The daemon keeps the latest state of each live task (its `TaskUpdated` and its
//!   `TaskFrames`). A listener that subscribes later, such as a restarted hwcomposer, is
//!   told the tasks already there, and so is the host each time it connects again, since a
//!   client sends a task's state only when it changes.
//!
//! None of this knows about binder: a listener is anything that can take a frame, so the
//! rules run and are tested on any platform. The binder service in `binder` is a thin layer
//! that turns calls into these.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};

use dsa_protocol::control::envelope::Body;

/// The guest process a call came from (its process ID, as binder reports the caller).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Peer(pub i32);

/// Names one subscription, so the listener can be dropped when its process dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(pub u64);

/// What became of one frame handed to a listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// The listener's process is gone; it never takes another frame.
    Gone,
    /// This frame did not arrive (the listener's queue was full, say), but the listener
    /// lives and takes the next one.
    Failed,
}

/// A guest client's callback for frames. It must not block: the router calls it with its
/// state locked, which keeps every listener's frames in the order they were routed.
pub trait Listener: Send + Sync {
    fn deliver(&self, frame: &[u8]) -> Delivery;
}

/// Why a frame a guest client sent was not passed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// It is not one control message.
    Malformed(String),
    /// It is a message only the daemon itself may send, or carries no message at all.
    NotAllowed(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Malformed(why) | SendError::NotAllowed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SendError {}

struct Subscription {
    id: ListenerId,
    peer: Peer,
    listener: Arc<dyn Listener>,
}

/// The latest state of one live task, as the frames that carried it.
#[derive(Default)]
struct Task {
    /// The last `TaskUpdated`, and who sent it.
    updated: Option<(Peer, Vec<u8>)>,
    /// The last `TaskFrames`.
    frames: Option<Vec<u8>>,
}

#[derive(Default)]
struct State {
    listeners: Vec<Subscription>,
    tasks: BTreeMap<i32, Task>,
    next_id: u64,
}

impl State {
    /// Hands `frame` to every listener not in `except`'s process, and drops those whose
    /// process is gone.
    fn deliver(&mut self, frame: &[u8], except: Option<Peer>) {
        self.listeners
            .retain(|s| Some(s.peer) == except || s.listener.deliver(frame) != Delivery::Gone);
    }
}

/// Routes frames between the host connection and the guest's listeners.
pub struct Router {
    to_host: Sender<Vec<u8>>,
    state: Mutex<State>,
}

impl Router {
    /// A router that hands the host's share of guest frames to `to_host`, the channel the
    /// connection pump drains.
    pub fn new(to_host: Sender<Vec<u8>>) -> Router {
        Router {
            to_host,
            state: Mutex::new(State::default()),
        }
    }

    /// Adds a listener for `peer` and tells it the live tasks other processes reported. It
    /// takes frames until it is reported gone or removed.
    pub fn subscribe(&self, peer: Peer, listener: Arc<dyn Listener>) -> ListenerId {
        let mut state = self.lock();
        state.next_id += 1;
        let id = ListenerId(state.next_id);
        for task in state.tasks.values() {
            if let Some((sender, frame)) = &task.updated {
                if *sender != peer && listener.deliver(frame) == Delivery::Gone {
                    return id;
                }
            }
        }
        state.listeners.push(Subscription { id, peer, listener });
        id
    }

    /// Removes a listener, as when its process dies. Returns whether it was still there.
    pub fn remove(&self, id: ListenerId) -> bool {
        let mut state = self.lock();
        let before = state.listeners.len();
        state.listeners.retain(|s| s.id != id);
        state.listeners.len() != before
    }

    /// How many listeners take frames now.
    pub fn listener_count(&self) -> usize {
        self.lock().listeners.len()
    }

    /// Passes a frame from the host to every listener.
    pub fn from_host(&self, frame: &[u8]) {
        self.lock().deliver(frame, None);
    }

    /// Tells a host that has just connected the state of every live task. Frames that were
    /// waiting for the connection go first, so the host ends with the latest state.
    pub fn host_connected(&self) {
        let state = self.lock();
        for task in state.tasks.values() {
            let frames = task
                .updated
                .iter()
                .map(|(_, f)| f)
                .chain(task.frames.iter());
            for frame in frames {
                let _ = self.to_host.send(frame.clone());
            }
        }
    }

    /// Passes a frame a guest client sent: to the host, and task bounds and task ends also
    /// to the listeners of every other process.
    pub fn from_guest(&self, sender: Peer, frame: Vec<u8>) -> Result<(), SendError> {
        dsa_transport::check_frame_len(frame.len())
            .map_err(|error| SendError::Malformed(error.to_string()))?;
        let envelope = dsa_protocol::decode(&frame)
            .map_err(|error| SendError::Malformed(error.to_string()))?;
        let mut state = self.lock();
        match envelope.body {
            None => {
                return Err(SendError::NotAllowed(
                    "the frame carries no message".to_string(),
                ))
            }
            Some(
                Body::Hello(_)
                | Body::HelloAccepted(_)
                | Body::HelloRejected(_)
                | Body::AuthChallenge(_)
                | Body::AuthProof(_),
            ) => {
                return Err(SendError::NotAllowed(
                    "the handshake is the daemon's own; a guest client cannot send its messages"
                        .to_string(),
                ))
            }
            Some(Body::TaskUpdated(updated)) => {
                state.tasks.entry(updated.task_id).or_default().updated =
                    Some((sender, frame.clone()));
                state.deliver(&frame, Some(sender));
            }
            Some(Body::TaskRemoved(removed)) => {
                state.tasks.remove(&removed.task_id);
                state.deliver(&frame, Some(sender));
            }
            Some(Body::TaskFrames(frames)) => {
                state.tasks.entry(frames.task_id).or_default().frames = Some(frame.clone());
            }
            Some(_) => {}
        }
        // Sent with the state locked, so the host gets the frames in the order the state
        // changed. The pump owns the other end for as long as the daemon runs; if it has
        // gone, the daemon is stopping and the frame has nowhere to go.
        let _ = self.to_host.send(frame);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves the state itself whole; keep routing.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
