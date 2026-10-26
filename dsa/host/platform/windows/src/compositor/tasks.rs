//! The compositor's use of the local API for tasks: it watches the guest's tasks, asks the
//! guest to end a task whose window the user closed, and passes on the pointer input in
//! the task windows.
//!
//! None of these parts names a window, so all run against the in-memory transport in tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::{HostApiClient, InputStream};
use dsa_host_core::input::PointerEvent;
use dsa_host_core::tasks::{coalesce, TaskRequest};
use dsa_transport::Connector;

use super::log::{note, CompositorLogger};
use super::window::UiEvent;

/// The build the compositor names itself by on the local API.
const BUILD: &str = "dsa-compositor";

/// How long the compositor waits for the service to answer.
const ANSWER: Duration = Duration::from_secs(2);

/// How long the watch waits before trying the service again.
const RETRY: Duration = Duration::from_millis(500);

/// How often the watch looks at `stop` while no task changes.
const POLL: Duration = Duration::from_millis(200);

/// How long the input waits before trying the service again. Input that arrives meanwhile
/// is dropped rather than held, so a press is never delivered long after it happened.
const INPUT_RETRY: Duration = Duration::from_millis(500);

/// Watches the guest's tasks until `stop` is set, handing every change to `deliver`.
///
/// Each new watch begins with [`UiEvent::Sync`], so the windows match the tasks there are
/// then. When the service cannot be reached, [`UiEvent::Lost`] is delivered once, and the
/// watch tries again. It ends early when `deliver` returns false.
pub fn watch(
    mut connector: Box<dyn Connector>,
    mut deliver: impl FnMut(UiEvent) -> bool,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut watching = false;
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_tasks());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    note(
                        logger.as_deref(),
                        &format!("cannot watch Android's tasks: {error}"),
                    );
                    last_error = error;
                }
                if watching {
                    watching = false;
                    if !deliver(UiEvent::Lost) {
                        return;
                    }
                }
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        watching = true;
        if !deliver(UiEvent::Sync(stream.current().events())) {
            return;
        }
        while !stop.load(Ordering::Relaxed) {
            match stream.next(POLL) {
                Ok(Some(event)) => {
                    if !deliver(UiEvent::Task(event)) {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    // Reconnect. The windows stay until the next watch says which tasks are
                    // left, or the service cannot be reached at all.
                    note(
                        logger.as_deref(),
                        &format!("the watch of Android's tasks ended: {error}"),
                    );
                    break;
                }
            }
        }
    }
}

/// Sends each request to the guest until `requests` closes. A close that does not reach
/// the guest is handed to `deliver` as [`UiEvent::CloseFailed`], so its window opens again.
///
/// Requests that queued up while the last one was on its way are folded together first
/// ([`coalesce`]): during a live resize only each task's latest size is sent, never the
/// sizes it passed through.
pub fn serve_requests(
    mut connector: Box<dyn Connector>,
    requests: Receiver<TaskRequest>,
    mut deliver: impl FnMut(UiEvent) -> bool,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut client: Option<HostApiClient> = None;
    while let Ok(first) = requests.recv() {
        let mut waiting = vec![first];
        waiting.extend(requests.try_iter());
        for request in coalesce(waiting) {
            let mut outcome = Err(String::new());
            // A connection the service dropped is made again, once.
            for _ in 0..2 {
                let connected = match client.as_mut() {
                    Some(client) => Ok(client),
                    None => HostApiClient::connect(&mut *connector, BUILD, ANSWER)
                        .map(|connected| client.insert(connected)),
                };
                outcome = match connected {
                    Ok(connected) => connected
                        .request(&request)
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                if outcome.is_ok() {
                    break;
                }
                client = None;
            }
            if let Err(why) = outcome {
                note(
                    logger.as_deref(),
                    &format!("could not ask Android to {}: {why}", request.describe()),
                );
                if let TaskRequest::Close { task_id } = request {
                    if !deliver(UiEvent::CloseFailed(task_id)) {
                        return;
                    }
                }
            }
        }
    }
}

/// Passes every pointer event from the task windows to the guest until `events` closes.
///
/// The events go on one connection that streams input, so none waits for an answer. When
/// the service cannot be reached, the events are dropped and the connection is tried again
/// after a short wait; a connection that breaks is made again at once, for the same event.
pub fn serve_input(
    mut connector: Box<dyn Connector>,
    events: Receiver<PointerEvent>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut stream: Option<InputStream> = None;
    let mut retry_at: Option<Instant> = None;
    let mut last_error = String::new();
    while let Ok(event) = events.recv() {
        for _ in 0..2 {
            if stream.is_none() {
                if retry_at.is_some_and(|at| Instant::now() < at) {
                    break;
                }
                match HostApiClient::connect(&mut *connector, BUILD, ANSWER)
                    .and_then(|client| client.stream_input())
                {
                    Ok(connected) => {
                        stream = Some(connected);
                        retry_at = None;
                        last_error.clear();
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if error != last_error {
                            note(
                                logger.as_deref(),
                                &format!("pointer input cannot reach Android: {error}"),
                            );
                            last_error = error;
                        }
                        retry_at = Some(Instant::now() + INPUT_RETRY);
                        break;
                    }
                }
            }
            let Some(connected) = stream.as_mut() else {
                break;
            };
            match connected.send(&event) {
                Ok(()) => break,
                Err(_) => stream = None,
            }
        }
    }
}
