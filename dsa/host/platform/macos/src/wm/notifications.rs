//! The notifications of the user's Android, and DSA's own news about updates, in macOS's
//! Notification Center.
//!
//! The window manager watches the user's notifications over the service's local API and posts
//! each one ([`crate::shell::notifications`]), reconnecting whenever the service restarts. What
//! the user does with a banner comes back from the Notification Center: a click or a clearing
//! of an Android notification goes to the service, which passes it to Android; a click on DSA's
//! news about an update opens the settings app's Updates page instead, and [Restart now] on
//! its question whether Android restarts into a downloaded image asks the service to restart.
//! An app's request to pin a shortcut is asked the same way, with [Add to desktop] and
//! [Cancel]; the answer goes to Android, and on yes the shortcut goes on the Desktop.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::compositor::frame_stall::is_recovery_key;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::native_abi::in_user_language;
use dsa_host_core::notifications::{Notification, NotificationAction, NotificationEvent};
use dsa_host_core::tasks::PinShortcutRequest;
use dsa_host_core::update::alerts::is_update_key;
use dsa_host_unix::socket::UnixLocalConnector;

use super::hosts::BUILD;
use super::launch::AppLauncher;
use super::log;
use crate::compositor::menu_bar::korean;
use crate::shell::notifications::{NotificationCenter, Response, UPDATES_URL};
use crate::shell::pin_shortcuts::Pending;

/// The pin questions on screen, which the banner thread asks and the actions thread answers.
type PendingPins = Arc<Mutex<Pending>>;

/// Gives Android the user's answer to `request`, and on yes puts the shortcut on the Desktop.
fn answer_pin(
    host_api: &Path,
    launcher: &AppLauncher,
    request: &PinShortcutRequest,
    accepted: bool,
) {
    let answered = connect(host_api).and_then(|mut client| {
        client
            .answer_pin_shortcut(request.request_id, accepted)
            .map_err(|e| e.to_string())
    });
    if let Err(why) = answered {
        log(&format!(
            "{} did not hear the answer about its shortcut: {why}",
            request.package_name
        ));
    }
    if accepted {
        match launcher.pin(request) {
            Ok(link) => log(&format!(
                "put a shortcut on the Desktop: {}",
                link.display()
            )),
            Err(why) => log(&format!("the shortcut is not on the Desktop: {why}")),
        }
    }
}

/// How long to wait for the service before trying again.
const RETRY: Duration = Duration::from_secs(2);
/// How long one call to the service may take.
const TIMEOUT: Duration = Duration::from_secs(5);

fn connect(host_api: &Path) -> Result<HostApiClient, String> {
    let mut connector = UnixLocalConnector {
        path: host_api.to_path_buf(),
    };
    HostApiClient::connect(&mut connector, BUILD, TIMEOUT).map_err(|e| e.to_string())
}

/// Opens the settings app on its Updates page, through its URL scheme, which reaches it whether
/// it runs already (in the menu bar) or has to start.
fn open_updates() {
    match std::process::Command::new("/usr/bin/open")
        .arg(UPDATES_URL)
        .spawn()
    {
        Ok(mut child) => {
            let _ = thread::Builder::new()
                .name("dsa-wm-open-updates".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
        Err(why) => log(&format!("could not open {UPDATES_URL}: {why}")),
    }
}

/// Passes what the user did with each banner on: to the service for Android's notifications,
/// to the settings app for DSA's news about updates.
fn act(
    host_api: PathBuf,
    responses: Receiver<Response>,
    pins: PendingPins,
    launcher: Arc<AppLauncher>,
) {
    for response in responses {
        let action = match response {
            Response::Notification(action) => action,
            Response::Pin {
                request_id,
                accepted,
            } => {
                let request = pins
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(request_id);
                if let Some(request) = request {
                    answer_pin(&host_api, &launcher, &request, accepted);
                }
                continue;
            }
            // [Restart now] on DSA's question: the service restarts Android into the new image.
            Response::RestartForUpdate => {
                let asked = connect(&host_api)
                    .and_then(|mut client| client.restart_for_update().map_err(|e| e.to_string()));
                if let Err(why) = asked {
                    log(&format!("Android was not restarted: {why}"));
                }
                continue;
            }
        };
        if is_update_key(action.key()) {
            if matches!(action, NotificationAction::Open { .. }) {
                open_updates();
            }
            continue;
        }
        // A notice about a recovery says what DSA did; Android has nothing to open.
        if is_recovery_key(action.key()) {
            continue;
        }
        let result = connect(&host_api).and_then(|mut client| {
            client
                .act_on_notification(&action)
                .map_err(|e| e.to_string())
        });
        if let Err(why) = result {
            log(&format!(
                "what was done with a notification did not reach Android: {why}"
            ));
        }
    }
}

/// Shows the user's notifications until `stop`, from a thread of its own, and asks about each
/// app's request to pin a shortcut that arrives on `pin_requests`. Without the Notification
/// Center (a program not run from DSA.app) it says why and shows none, and each pin request
/// is declined.
pub fn show(
    host_api: PathBuf,
    icons: PathBuf,
    stop: Arc<AtomicBool>,
    pin_requests: Receiver<PinShortcutRequest>,
    news: Receiver<Notification>,
    launcher: Arc<AppLauncher>,
) {
    let _ = thread::Builder::new()
        .name("dsa-wm-notifications".into())
        .spawn(move || {
            let (actions, acted) = channel();
            let center = match NotificationCenter::new(actions, icons, korean()) {
                Ok(center) => center,
                Err(why) => {
                    log(&format!("notifications are not shown: {why}"));
                    // With nobody to ask, the apps hear no at once.
                    for request in pin_requests {
                        answer_pin(&host_api, &launcher, &request, false);
                    }
                    return;
                }
            };
            let pins: PendingPins = Arc::default();
            {
                let (host_api, pins, launcher) =
                    (host_api.clone(), Arc::clone(&pins), Arc::clone(&launcher));
                let _ = thread::Builder::new()
                    .name("dsa-wm-notification-actions".into())
                    .spawn(move || act(host_api, acted, pins, launcher));
            }
            // Asks about each pin request as it comes, and declines the ones left too long.
            // DSA's own news that the window manager raises (that Android restarts because
            // its graphics did not recover) is posted as it comes too; it needs no service.
            let ask_pins = |center: &NotificationCenter| {
                while let Ok(notice) = news.try_recv() {
                    if let Err(why) = center.post(&notice) {
                        log(&format!("DSA's notice was not shown: {why}"));
                    }
                }
                while let Ok(request) = pin_requests.try_recv() {
                    match center.ask_pin(&request, korean()) {
                        Ok(()) => pins
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .asked(request, std::time::Instant::now()),
                        Err(why) => {
                            log(&format!("could not ask about a shortcut to pin: {why}"));
                            answer_pin(&host_api, &launcher, &request, false);
                        }
                    }
                }
                let expired = pins
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .expired(std::time::Instant::now());
                for request in expired {
                    center.remove(&crate::shell::pin_shortcuts::identifier(request.request_id));
                    answer_pin(&host_api, &launcher, &request, false);
                }
            };
            let mut warned = false;
            while !stop.load(Ordering::Relaxed) {
                let watch = connect(&host_api)
                    .and_then(|client| client.watch_notifications().map_err(|e| e.to_string()));
                let mut stream = match watch {
                    Ok(stream) => stream,
                    Err(why) => {
                        if !warned {
                            log(&format!("notifications wait for the DSA service: {why}"));
                            warned = true;
                        }
                        ask_pins(&center);
                        thread::sleep(RETRY);
                        continue;
                    }
                };
                warned = false;
                // Android's notifications shown, to take out when the watch ends.
                let mut posted: HashSet<String> = HashSet::new();
                while !stop.load(Ordering::Relaxed) {
                    ask_pins(&center);
                    match stream.next(Duration::from_secs(1)) {
                        Ok(Some(NotificationEvent::Posted(notification))) => {
                            let notification = in_user_language(notification, korean());
                            if let Err(why) = center.post(&notification) {
                                log(&format!("a notification was not shown: {why}"));
                            } else if !is_update_key(&notification.key) {
                                posted.insert(notification.key);
                            }
                        }
                        Ok(Some(NotificationEvent::Removed { key })) => {
                            posted.remove(&key);
                            center.remove(&key);
                        }
                        Ok(None) => {}
                        Err(why) => {
                            log(&format!("the notifications' watch ended: {why}"));
                            break;
                        }
                    }
                }
                // Android's notifications are news no longer once the service is gone; DSA's
                // news about updates stays.
                for key in posted {
                    center.remove(&key);
                }
                thread::sleep(RETRY);
            }
        });
}
