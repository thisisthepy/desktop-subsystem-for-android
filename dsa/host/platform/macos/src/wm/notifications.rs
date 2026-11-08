//! The notifications of the user's Android, and DSA's own news about updates, in macOS's
//! Notification Center.
//!
//! The window manager watches the user's notifications over the service's local API and posts
//! each one ([`crate::shell::notifications`]), reconnecting whenever the service restarts. What
//! the user does with a banner comes back from the Notification Center: a click or a clearing
//! of an Android notification goes to the service, which passes it to Android; a click on DSA's
//! news about an update opens the settings app's Updates page instead.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::native_abi::in_user_language;
use dsa_host_core::notifications::{NotificationAction, NotificationEvent};
use dsa_host_core::update::alerts::is_update_key;
use dsa_host_unix::socket::UnixLocalConnector;

use super::hosts::BUILD;
use super::log;
use crate::compositor::menu_bar::korean;
use crate::shell::notifications::{NotificationCenter, UPDATES_URL};

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
fn act(host_api: PathBuf, actions: Receiver<NotificationAction>) {
    for action in actions {
        if is_update_key(action.key()) {
            if matches!(action, NotificationAction::Open { .. }) {
                open_updates();
            }
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

/// Shows the user's notifications until `stop`, from a thread of its own. Without the
/// Notification Center (a program not run from DSA.app) it says why and shows none.
pub fn show(host_api: PathBuf, icons: PathBuf, stop: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("dsa-wm-notifications".into())
        .spawn(move || {
            let (actions, acted) = channel();
            let center = match NotificationCenter::new(actions, icons) {
                Ok(center) => center,
                Err(why) => return log(&format!("notifications are not shown: {why}")),
            };
            {
                let host_api = host_api.clone();
                let _ = thread::Builder::new()
                    .name("dsa-wm-notification-actions".into())
                    .spawn(move || act(host_api, acted));
            }
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
                        thread::sleep(RETRY);
                        continue;
                    }
                };
                warned = false;
                // Android's notifications shown, to take out when the watch ends.
                let mut posted: HashSet<String> = HashSet::new();
                while !stop.load(Ordering::Relaxed) {
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
