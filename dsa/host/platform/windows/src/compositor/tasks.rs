//! The compositor's use of the local API for tasks: it watches the guest's tasks, and asks
//! the guest to end a task whose window the user closed.
//!
//! Neither part names a window, so both run against the in-memory transport in tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::tasks::TaskRequest;
use dsa_transport::Connector;

use super::hosts::ManagerEvent;
use super::log::{note, CompositorLogger};

/// The build the compositor names itself by on the local API.
const BUILD: &str = "dsa-wm";

/// How long the compositor waits for the service to answer.
const ANSWER: Duration = Duration::from_secs(2);

/// How long the watch waits before trying the service again.
const RETRY: Duration = Duration::from_millis(500);

/// How often the watch looks at `stop` while no task changes.
const POLL: Duration = Duration::from_millis(200);

/// Watches the guest's tasks until `stop` is set, handing every change to `deliver`.
///
/// Each new watch begins with [`ManagerEvent::Sync`], so the windows match the tasks there are
/// then. When the service cannot be reached, [`ManagerEvent::Lost`] is delivered once, and the
/// watch tries again. It ends early when `deliver` returns false.
pub fn watch(
    mut connector: Box<dyn Connector>,
    mut deliver: impl FnMut(ManagerEvent) -> bool,
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
                    if !deliver(ManagerEvent::Lost) {
                        return;
                    }
                }
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        watching = true;
        if !deliver(ManagerEvent::Sync(stream.current().events())) {
            return;
        }
        while !stop.load(Ordering::Relaxed) {
            match stream.next(POLL) {
                Ok(Some(event)) => {
                    if !deliver(ManagerEvent::Task(event)) {
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
/// the guest is handed to `deliver` as [`ManagerEvent::CloseFailed`], so its window opens again.
pub fn serve_requests(
    mut connector: Box<dyn Connector>,
    requests: Receiver<TaskRequest>,
    mut deliver: impl FnMut(ManagerEvent) -> bool,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut client: Option<HostApiClient> = None;
    while let Ok(request) = requests.recv() {
        let mut outcome = Err(String::new());
        // A connection the service dropped is made again, once.
        for _ in 0..2 {
            let connected = match client.as_mut() {
                Some(client) => Ok(client),
                None => HostApiClient::connect(&mut *connector, BUILD, ANSWER)
                    .map(|connected| client.insert(connected)),
            };
            outcome = match connected {
                Ok(connected) => match &request {
                    TaskRequest::Close { task_id } => connected.close_task(*task_id),
                    TaskRequest::Launch { package_name } => connected.launch_app(package_name),
                }
                .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if outcome.is_ok() {
                break;
            }
            client = None;
        }
        if let Err(why) = outcome {
            let what = match &request {
                TaskRequest::Close { task_id } => format!("end task {task_id}"),
                TaskRequest::Launch { package_name } => format!("launch {package_name}"),
            };
            note(
                logger.as_deref(),
                &format!("could not ask Android to {what}: {why}"),
            );
            if let TaskRequest::Close { task_id } = request {
                if !deliver(ManagerEvent::CloseFailed(task_id)) {
                    return;
                }
            }
        }
    }
}
