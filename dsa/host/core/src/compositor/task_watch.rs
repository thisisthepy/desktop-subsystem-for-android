//! How a window manager follows the guest's tasks and sends its requests, the same on every
//! host.
//!
//! [`follow_tasks`] keeps a watch of the guest's tasks open: it opens it again when it ends,
//! tells the manager once when the service cannot be reached, and reports each failure once
//! rather than at every attempt. [`next_batch`] and [`send_request`] are the parts of the
//! request loop that do not depend on the host: waiting for requests, with a tick while sizes
//! are held for a frame, and sending a request on a connection that is made again once if the
//! service dropped it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::compositor::resize_queue::PACE_TICK;
use crate::host_api::{HostApiClient, TaskStream};
use crate::tasks::{TaskEvent, TaskRequest};

/// What a watch of the guest's tasks tells the window manager.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskFeed {
    /// A new watch began; these are the guest's tasks now.
    Sync(Vec<TaskEvent>),
    /// A change to the guest's tasks.
    Task(Box<TaskEvent>),
    /// The service cannot be reached, so the guest's tasks are unknown.
    Lost,
}

/// Watches the guest's tasks until `stop` is set, handing every change to `deliver`.
///
/// Each new watch begins with [`TaskFeed::Sync`], so the windows match the tasks there are
/// then. When the service cannot be reached after a watch, [`TaskFeed::Lost`] is delivered
/// once, and the watch is tried again after `retry`. A watch that ends is opened again at
/// once, and the windows stay until the next watch says which tasks are left. It ends early
/// when `deliver` returns false. `poll` is how often `stop` is looked at while no task
/// changes. `log` hears each reason once.
pub fn follow_tasks(
    mut connect: impl FnMut() -> Result<TaskStream, String>,
    mut deliver: impl FnMut(TaskFeed) -> bool,
    stop: &AtomicBool,
    retry: Duration,
    poll: Duration,
    log: &dyn Fn(&str),
) {
    let mut watching = false;
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let mut stream = match connect() {
            Ok(stream) => stream,
            Err(error) => {
                if error != last_error {
                    log(&format!("cannot watch Android's tasks: {error}"));
                    last_error = error;
                }
                if watching {
                    watching = false;
                    if !deliver(TaskFeed::Lost) {
                        return;
                    }
                }
                std::thread::sleep(retry);
                continue;
            }
        };
        last_error.clear();
        watching = true;
        if !deliver(TaskFeed::Sync(stream.current().events())) {
            return;
        }
        while !stop.load(Ordering::Relaxed) {
            match stream.next(poll) {
                Ok(Some(event)) => {
                    if !deliver(TaskFeed::Task(Box::new(event))) {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    log(&format!("the watch of Android's tasks ended: {error}"));
                    break;
                }
            }
        }
    }
}

/// Waits for the next requests: one, and whatever else is queued by then. With `held` (sizes
/// waiting for a frame) it waits only [`PACE_TICK`] and may return none; without, until one
/// comes. `None` when the senders are gone.
pub fn next_batch<T>(requests: &Receiver<T>, held: bool) -> Option<Vec<T>> {
    let mut batch = Vec::new();
    if held {
        match requests.recv_timeout(PACE_TICK) {
            Ok(next) => batch.push(next),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    } else {
        match requests.recv() {
            Ok(next) => batch.push(next),
            Err(_) => return None,
        }
    }
    batch.extend(requests.try_iter());
    Some(batch)
}

/// Sends `request` on `client`, connecting with `connect` first when there is no connection. A
/// connection the service dropped is made again, once. The reason is returned when both
/// attempts fail.
pub fn send_request(
    client: &mut Option<HostApiClient>,
    request: &TaskRequest,
    mut connect: impl FnMut() -> Result<HostApiClient, String>,
) -> Result<(), String> {
    let mut outcome = Err(String::new());
    for _ in 0..2 {
        let connected = match client.as_mut() {
            Some(client) => Ok(client),
            None => connect().map(|connected| client.insert(connected)),
        };
        outcome = match connected {
            Ok(connected) => connected
                .request(request)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        };
        if outcome.is_ok() {
            break;
        }
        *client = None;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn fr7_a_service_that_cannot_be_reached_is_reported_once_and_the_loop_ends_on_stop() {
        let stop = AtomicBool::new(false);
        let attempts = std::cell::Cell::new(0);
        let logged = std::cell::RefCell::new(Vec::new());
        let log = |line: &str| logged.borrow_mut().push(line.to_string());
        follow_tasks(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 3 {
                    stop.store(true, Ordering::Relaxed);
                }
                Err("refused".into())
            },
            |_| true,
            &stop,
            Duration::from_millis(1),
            Duration::from_millis(1),
            &log,
        );
        assert_eq!(attempts.get(), 3);
        assert_eq!(
            *logged.borrow(),
            vec!["cannot watch Android's tasks: refused".to_string()]
        );
    }

    #[test]
    fn fr7_requests_are_waited_for_together_and_a_closed_queue_ends_the_wait() {
        let (sender, receiver) = mpsc::channel();
        sender.send(1).unwrap();
        sender.send(2).unwrap();
        assert_eq!(next_batch(&receiver, false), Some(vec![1, 2]));
        // With sizes held, nothing queued is an empty batch after the tick.
        assert_eq!(next_batch(&receiver, true), Some(vec![]));
        drop(sender);
        assert_eq!(next_batch(&receiver, true), None);
        assert_eq!(next_batch(&receiver, false), None);
    }
}
