//! The compositor's use of the local API for tasks: it watches the guest's tasks, asks the
//! guest to end a task whose window the user closed, and passes on the pointer and key input in
//! the task windows.
//!
//! None of these parts names a window, so all run against the in-memory transport in tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::resize_pacing::ResizePacer;
use dsa_host_core::compositor::window_tasks::WindowTasks;
use dsa_host_core::host_api::{HostApiClient, InputStream};
use dsa_host_core::input::InputEvent;
use dsa_host_core::tasks::{coalesce, TaskRequest};
use dsa_transport::Connector;

use super::hosts::ManagerEvent;
use super::log::{note, trace_stage, CompositorLogger};
use dsa_host_core::resize_trace::millis;

/// The build the compositor names itself by on the local API.
const BUILD: &str = "dsa-wm";

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
                    if !deliver(ManagerEvent::Task(Box::new(event))) {
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

/// The size of the latest frame each task drew since the request thread last looked.
static DRAWN: Mutex<Vec<(i32, (u32, u32))>> = Mutex::new(Vec::new());

/// How often the request thread looks for drawn frames while a size of a drag is held back.
const PACE_TICK: Duration = Duration::from_millis(4);

/// The app of `task_id` drew a frame of `size`: a size of a drag held back for it may go.
pub fn frame_drawn(task_id: i32, size: (u32, u32)) {
    let mut drawn = DRAWN.lock().unwrap_or_else(|e| e.into_inner());
    match drawn.iter_mut().find(|(id, _)| *id == task_id) {
        Some(entry) => entry.1 = size,
        None => drawn.push((task_id, size)),
    }
}

fn take_drawn(pacer: &mut ResizePacer) {
    let drawn = std::mem::take(&mut *DRAWN.lock().unwrap_or_else(|e| e.into_inner()));
    for (task_id, size) in drawn {
        pacer.frame_drawn(task_id, size);
    }
}

/// Sends each request to the guest until `requests` closes. A close that does not reach
/// the guest is handed to `deliver` as [`ManagerEvent::CloseFailed`], so its window opens again.
///
/// Requests that queued up while the last one was on its way are folded together first
/// ([`coalesce`]): during a live resize only each task's latest size is sent, never the
/// sizes it passed through. A size of a drag also waits until the app has drawn the size sent
/// before it (`ResizePacer`), so an app slower than the drag is never handed a new size before
/// it has drawn the last; the size that ends the drag is never held back.
pub fn serve_requests(
    mut connector: Box<dyn Connector>,
    requests: Receiver<TaskRequest>,
    mut deliver: impl FnMut(ManagerEvent) -> bool,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut client: Option<HostApiClient> = None;
    let mut pacer = ResizePacer::new();
    let mut held: Vec<TaskRequest> = Vec::new();
    loop {
        // Held sizes go first, so a newer size of the same task replaces them.
        let before = std::mem::take(&mut held);
        let mut waiting = before.clone();
        if waiting.is_empty() {
            match requests.recv() {
                Ok(first) => waiting.push(first),
                Err(_) => break,
            }
        } else {
            match requests.recv_timeout(PACE_TICK) {
                Ok(next) => waiting.push(next),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        waiting.extend(requests.try_iter());
        take_drawn(&mut pacer);
        let (go, still_held) = pacer.pace(coalesce(waiting), Instant::now());
        for request in &still_held {
            if let TaskRequest::Resize {
                task_id,
                width,
                height,
                ..
            } = request
            {
                // Once per size held: the next pass carries the same request.
                if !before.contains(request) {
                    trace_stage(
                        logger.as_deref(),
                        *task_id,
                        "wm.request_held",
                        (*width, *height),
                        "waiting for the app to draw the size before",
                    );
                }
            }
        }
        held = still_held;
        for request in go {
            let started = Instant::now();
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
            if let TaskRequest::Resize {
                task_id,
                width,
                height,
                live,
            } = &request
            {
                trace_stage(
                    logger.as_deref(),
                    *task_id,
                    "wm.request",
                    (*width, *height),
                    &format!(
                        "live={live} sent={} took={}",
                        outcome.is_ok(),
                        millis(started.elapsed())
                    ),
                );
            }
            if let Err(why) = outcome {
                note(
                    logger.as_deref(),
                    &format!("could not ask Android to {}: {why}", request.describe()),
                );
                if let TaskRequest::Close { task_id } = request {
                    if !deliver(ManagerEvent::CloseFailed(task_id)) {
                        return;
                    }
                }
            }
        }
    }
}

/// Passes every pointer and key event from the task windows to the guest until `events`
/// closes.
///
/// The events go on one connection that streams input, so none waits for an answer. When
/// the service cannot be reached, the events are dropped and the connection is tried again
/// after a short wait; a connection that breaks is made again at once, for the same event.
pub fn serve_input(
    connector: Box<dyn Connector>,
    events: Receiver<InputEvent>,
    logger: Option<Arc<CompositorLogger>>,
) {
    serve_window_input(connector, events, WindowTasks::new(), logger)
}

/// [`serve_input`] for a window host's windows, which name the task each window opened for:
/// each event goes to the task its window shows now, as `tasks` says, which is another task
/// once one took the window over after a GPU reset.
pub fn serve_window_input(
    mut connector: Box<dyn Connector>,
    events: Receiver<InputEvent>,
    tasks: WindowTasks,
    logger: Option<Arc<CompositorLogger>>,
) {
    crate::scheduling::raise_this_thread();
    let mut stream: Option<InputStream> = None;
    let mut retry_at: Option<Instant> = None;
    let mut last_error = String::new();
    while let Ok(event) = events.recv() {
        let event = tasks.input(event);
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
                                &format!("input cannot reach Android: {error}"),
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
            match connected.send_input(&event) {
                Ok(()) => break,
                Err(_) => stream = None,
            }
        }
    }
}

/// How often the window manager reads the user's window settings for apps, so a change made
/// in the settings app applies to the next window that opens.
const APP_WINDOWS_POLL: Duration = Duration::from_secs(2);

/// Reads the user's window settings for apps until `stop` is set, handing them to `deliver`
/// when they start and whenever they change. It ends early when `deliver` returns false.
pub fn follow_app_windows(
    mut connector: Box<dyn Connector>,
    mut deliver: impl FnMut(Vec<dsa_host_core::app_windows::AppWindowSettings>) -> bool,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut client: Option<HostApiClient> = None;
    let mut last = None;
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        if client.is_none() {
            client = HostApiClient::connect(&mut *connector, BUILD, ANSWER).ok();
        }
        if let Some(connected) = client.as_mut() {
            match connected.app_window_settings() {
                Ok(apps) => {
                    last_error.clear();
                    if last.as_ref() != Some(&apps) {
                        last = Some(apps.clone());
                        if !deliver(apps) {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let error = error.to_string();
                    if error != last_error {
                        note(
                            logger.as_deref(),
                            &format!("cannot read the window settings of apps: {error}"),
                        );
                        last_error = error;
                    }
                    client = None;
                }
            }
        }
        let deadline = Instant::now() + APP_WINDOWS_POLL;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }
}
