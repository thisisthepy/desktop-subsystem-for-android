//! The guest's notifications as the desktop's, through the freedesktop notification service.
//!
//! Each guest notification becomes one desktop notification, which a later post with the
//! same key replaces and a removal closes. Clicking it opens it in the guest; dismissing it
//! dismisses it there. The service numbers notifications itself, so the host keeps which
//! number stands for which key.
//!
//! The same watch carries DSA's own news about updates: that an Android image is ready, was
//! applied, or failed. Those show under DSA's settings app (its launcher entry,
//! [`SETTINGS_ENTRY`]), one per kind of update, so the news that an update was applied
//! replaces the news that it was ready. A click on one opens the settings app's Updates
//! page; nothing about it goes to the guest.

use std::collections::BTreeMap;

use dsa_host_core::notifications::{Notification, NotificationAction};
use dsa_host_core::update::alerts::{is_update_key, thread_of};

use super::desktop_entry::app_id;

/// The action a click on the notification's body invokes.
pub const DEFAULT_ACTION: &str = "default";

/// The service's reason for a notification the user dismissed (`NotificationClosed`).
pub const CLOSED_BY_USER: u32 = 2;

/// The launcher entry of DSA's settings app, which DSA's own notifications show under.
pub const SETTINGS_ENTRY: &str = "io.github.thisisthepy.DSA.Settings";

/// The arguments that open DSA's settings on the Updates page.
pub const UPDATE_PAGE_ARGUMENTS: [&str; 2] = ["--page", "updates"];

/// Where a notification with `key` is shown: its own place, except that DSA's news about one
/// kind of update shares one, so a newer replaces the older.
fn slot(key: &str) -> String {
    thread_of(key).unwrap_or_else(|| key.to_string())
}

/// The urgency hint: 0 low, 1 normal, 2 critical.
pub fn urgency(notification: &Notification) -> u8 {
    match notification.importance {
        i32::MIN..=2 => 0,
        _ => 1,
    }
}

/// Text in the body, with the characters the service's markup gives meaning escaped.
pub fn body_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// The arguments of `org.freedesktop.Notifications.Notify` for one notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notify {
    pub app_name: String,
    pub replaces_id: u32,
    pub summary: String,
    pub body: String,
    pub actions: Vec<String>,
    pub urgency: u8,
    /// The `desktop-entry` hint: the app's launcher entry, so the desktop shows its icon and
    /// groups its notifications.
    pub desktop_entry: String,
    /// Whether the notification stays until the user acts on it (one that may not be
    /// dismissed, as an ongoing call's).
    pub resident: bool,
}

/// What the host's notifications and the service's numbers stand for.
#[derive(Debug, Default)]
pub struct Shown {
    /// The number of what is shown in each place ([`slot`]).
    by_key: BTreeMap<String, u32>,
    /// The key of the notification each number shows.
    by_id: BTreeMap<u32, String>,
}

impl Shown {
    pub fn new() -> Shown {
        Shown::default()
    }

    /// The `Notify` call that shows `notification`, replacing the one with its key.
    pub fn notify(&self, notification: &Notification) -> Notify {
        let app_name = if notification.app_label.is_empty() {
            notification.package_name.clone()
        } else {
            notification.app_label.clone()
        };
        let desktop_entry = if is_update_key(&notification.key) {
            SETTINGS_ENTRY.to_string()
        } else {
            app_id(&notification.package_name)
        };
        Notify {
            app_name,
            replaces_id: self
                .by_key
                .get(&slot(&notification.key))
                .copied()
                .unwrap_or(0),
            summary: notification.title.clone(),
            body: body_text(&notification.text),
            actions: vec![DEFAULT_ACTION.into(), "Open".into()],
            urgency: urgency(notification),
            desktop_entry,
            resident: !notification.clearable,
        }
    }

    /// The service numbered the notification with `key` as `id`.
    pub fn shown(&mut self, key: &str, id: u32) {
        if let Some(old) = self.by_key.insert(slot(key), id) {
            if old != id {
                self.by_id.remove(&old);
            }
        }
        self.by_id.insert(id, key.to_string());
    }

    /// The notification with `key` is gone from the guest: the number to close, if shown.
    pub fn removed(&mut self, key: &str) -> Option<u32> {
        let id = self.by_key.remove(&slot(key))?;
        self.by_id.remove(&id);
        Some(id)
    }

    /// `ActionInvoked`: what the guest is told.
    pub fn action_invoked(&self, id: u32, action: &str) -> Option<NotificationAction> {
        let key = self.by_id.get(&id)?;
        (action == DEFAULT_ACTION).then(|| NotificationAction::Open { key: key.clone() })
    }

    /// `NotificationClosed`: what the guest is told. Only the user's dismissal reaches the
    /// guest; one that expired stays posted there, as on Windows.
    pub fn closed(&mut self, id: u32, reason: u32) -> Option<NotificationAction> {
        let key = self.by_id.remove(&id)?;
        self.by_key.remove(&slot(&key));
        (reason == CLOSED_BY_USER).then_some(NotificationAction::Dismiss { key })
    }

    /// Every number the guest's notifications are shown under, to close them all when the
    /// guest stops. DSA's own news about updates stays: it is not the guest's.
    pub fn clear(&mut self) -> Vec<u32> {
        let guest: Vec<u32> = self
            .by_id
            .iter()
            .filter(|(_, key)| !is_update_key(key))
            .map(|(id, _)| *id)
            .collect();
        for id in &guest {
            if let Some(key) = self.by_id.remove(id) {
                self.by_key.remove(&slot(&key));
            }
        }
        guest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(key: &str) -> Notification {
        Notification {
            key: key.into(),
            package_name: "com.example.chat".into(),
            app_label: "Chat".into(),
            title: "Ann".into(),
            text: "see <you> & me".into(),
            importance: 3,
            clearable: true,
            ..Notification::default()
        }
    }

    #[test]
    fn fr8_a_notification_shows_with_its_app_title_and_text() {
        let shown = Shown::new();
        let notify = shown.notify(&notification("k1"));
        assert_eq!(notify.app_name, "Chat");
        assert_eq!(notify.summary, "Ann");
        assert_eq!(notify.body, "see &lt;you&gt; &amp; me");
        assert_eq!(notify.replaces_id, 0);
        assert_eq!(notify.desktop_entry, "dsa-com.example.chat");
        assert_eq!(notify.urgency, 1);
        assert!(!notify.resident);
    }

    #[test]
    fn fr8_a_new_post_with_the_same_key_replaces_the_shown_one() {
        let mut shown = Shown::new();
        shown.shown("k1", 41);
        assert_eq!(shown.notify(&notification("k1")).replaces_id, 41);
        assert_eq!(shown.removed("k1"), Some(41));
        assert_eq!(shown.removed("k1"), None);
    }

    #[test]
    fn fr8_clicking_opens_it_in_the_guest_and_dismissing_dismisses_it() {
        let mut shown = Shown::new();
        shown.shown("k1", 7);
        assert_eq!(
            shown.action_invoked(7, DEFAULT_ACTION),
            Some(NotificationAction::Open { key: "k1".into() })
        );
        assert_eq!(
            shown.closed(7, CLOSED_BY_USER),
            Some(NotificationAction::Dismiss { key: "k1".into() })
        );
        assert_eq!(shown.closed(7, CLOSED_BY_USER), None);
    }

    #[test]
    fn fr8_an_expired_notification_stays_posted_in_the_guest() {
        let mut shown = Shown::new();
        shown.shown("k1", 9);
        assert_eq!(shown.closed(9, 1), None);
    }

    #[test]
    fn fr14_update_news_shows_under_the_settings_app_and_the_newer_replaces_the_older() {
        use dsa_host_core::update::alerts::{AlertKind, Subject, UpdateAlert};
        let mut shown = Shown::new();
        let ready = UpdateAlert::new(Subject::Android, AlertKind::Ready, "16.0.3").notification();
        let notify = shown.notify(&ready);
        assert_eq!(notify.app_name, "DSA");
        assert_eq!(notify.desktop_entry, SETTINGS_ENTRY);
        assert_eq!(notify.replaces_id, 0);
        shown.shown(&ready.key, 12);
        let applied =
            UpdateAlert::new(Subject::Android, AlertKind::Applied, "16.0.3").notification();
        assert_eq!(shown.notify(&applied).replaces_id, 12);
        shown.shown(&applied.key, 12);
        // A click is the host's to act on: it names the alert, which opens the settings.
        let click = shown.action_invoked(12, DEFAULT_ACTION).unwrap();
        assert!(is_update_key(click.key()));
        assert_eq!(UPDATE_PAGE_ARGUMENTS, ["--page", "updates"]);
        // The guest stopping closes its own notifications only.
        shown.shown("0|com.example.chat|1|null|10100", 13);
        assert_eq!(shown.clear(), vec![13]);
        assert_eq!(shown.notify(&applied).replaces_id, 12);
    }

    #[test]
    fn fr8_a_quiet_notification_is_low_urgency_and_an_ongoing_one_stays() {
        let shown = Shown::new();
        let mut quiet = notification("k2");
        quiet.importance = 2;
        quiet.clearable = false;
        let notify = shown.notify(&quiet);
        assert_eq!(notify.urgency, 0);
        assert!(notify.resident);
    }
}
