//! The notifications of a user's Android, as the host shows them.
//!
//! The guest's agent forwards each notification a person would see, with its key. The host
//! shows it in its own notification centre, replaces it when a notification with the same key
//! comes again, and removes it when the guest says it is gone. What the user does with it on
//! the host goes back to the guest by key: a click opens it, a dismissal cancels it.
//!
//! A notification from the guest is checked before it goes on ([`checked`]): one without a
//! key, or with a key, text or icon past the limits here, is cut down or dropped, so a bad
//! guest can neither crash the host nor make it hold more than a bounded amount per
//! notification. How many reach the host is limited too ([`RateLimit`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dsa_protocol::control;

/// The longest key the host keeps, in bytes. Android's keys are a few dozen bytes: the user,
/// the package, the id and the tag.
pub const MAX_KEY_BYTES: usize = 512;

/// The longest title, app name or package the host keeps, in characters. A longer one is cut;
/// a notification centre shows one line of it.
pub const MAX_LINE_CHARS: usize = 256;

/// The longest body the host keeps, in characters. A longer one is cut; a notification
/// centre shows a few lines of it.
pub const MAX_TEXT_CHARS: usize = 4096;

/// The largest icon the host takes, in bytes of PNG. The agent draws icons at 96 pixels on a
/// side, which takes far less; a larger icon is left out and the notification still shows.
pub const MAX_ICON_BYTES: usize = 64 * 1024;

/// One notification of the guest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Notification {
    /// Android's key: unique in the guest while it is posted.
    pub key: String,
    pub package_name: String,
    pub app_label: String,
    pub title: String,
    pub text: String,
    /// A PNG image; empty for none.
    pub icon_png: Vec<u8>,
    /// As Android numbers it: 1 minimum, 2 low, 3 default, 4 high.
    pub importance: i32,
    pub high_priority: bool,
    /// Whether the user may dismiss it.
    pub clearable: bool,
    pub when_millis: i64,
}

/// A change to the guest's notifications, as a watcher hears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationEvent {
    /// A notification is new, or replaces the one with its key.
    Posted(Notification),
    /// The notification with this key is gone.
    Removed { key: String },
}

/// What the user did with a notification on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationAction {
    /// Clicked it: the guest opens it.
    Open { key: String },
    /// Dismissed it: the guest cancels it, unless it may not be dismissed.
    Dismiss { key: String },
}

impl NotificationAction {
    pub fn key(&self) -> &str {
        match self {
            NotificationAction::Open { key } | NotificationAction::Dismiss { key } => key,
        }
    }
}

fn cut(text: String, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text;
    }
    text.chars().take(chars).collect()
}

/// Whether `key` is one the host keeps: not empty, and not longer than [`MAX_KEY_BYTES`].
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_BYTES
}

/// The notification the guest sent, cut to the limits here, or why it is dropped.
pub fn checked(message: control::GuestNotification) -> Result<Notification, String> {
    if !valid_key(&message.key) {
        return Err(format!(
            "a key of {} bytes; keys are 1 to {MAX_KEY_BYTES} bytes",
            message.key.len()
        ));
    }
    let icon_png = if message.icon_png.len() > MAX_ICON_BYTES {
        eprintln!(
            "notifications: left out an icon of {} bytes from {}; icons are at most {MAX_ICON_BYTES}",
            message.icon_png.len(),
            message.package_name
        );
        Vec::new()
    } else {
        message.icon_png
    };
    Ok(Notification {
        key: message.key,
        package_name: cut(message.package_name, MAX_LINE_CHARS),
        app_label: cut(message.app_label, MAX_LINE_CHARS),
        title: cut(message.title, MAX_LINE_CHARS),
        text: cut(message.text, MAX_TEXT_CHARS),
        icon_png,
        importance: message.importance.clamp(0, 5),
        high_priority: message.high_priority,
        clearable: message.clearable,
        when_millis: message.when_millis,
    })
}

/// How many notifications reach the host at once before the rest wait.
pub const BURST: u32 = 10;

/// How often one more may go once the burst is spent.
pub const REFILL: Duration = Duration::from_millis(500);

/// The most notifications that wait for their turn. Past that, the newest is dropped.
pub const MAX_WAITING: usize = 64;

/// Limits how many notifications reach the host.
///
/// An app that posts in a loop, by a bug or on purpose, would otherwise cover the user's
/// screen with notifications, fill the notification centre, and keep the host busy drawing
/// them. Notifications up to [`BURST`] go at once; after that one goes every [`REFILL`]. The
/// ones that wait are kept by key, so a notification an app updates many times while it
/// waits (a download's progress) goes once, in its latest form. A removal never waits: it
/// takes a notification away, and drops a waiting one with its key.
#[derive(Debug)]
pub struct RateLimit {
    tokens: u32,
    refilled: Instant,
    waiting: Vec<String>,
    latest: HashMap<String, Notification>,
}

impl RateLimit {
    pub fn new(now: Instant) -> RateLimit {
        RateLimit {
            tokens: BURST,
            refilled: now,
            waiting: Vec::new(),
            latest: HashMap::new(),
        }
    }

    fn refill(&mut self, now: Instant) {
        while self.tokens < BURST && now.duration_since(self.refilled) >= REFILL {
            self.tokens += 1;
            self.refilled += REFILL;
        }
        if self.tokens == BURST {
            self.refilled = now;
        }
    }

    /// Takes a notification: `Some` when it may go now, `None` when it waits or is dropped.
    pub fn offer(&mut self, notification: Notification, now: Instant) -> Option<Notification> {
        self.refill(now);
        if let Some(waiting) = self.latest.get_mut(&notification.key) {
            *waiting = notification;
            return None;
        }
        if self.waiting.is_empty() && self.tokens > 0 {
            self.tokens -= 1;
            return Some(notification);
        }
        if self.waiting.len() >= MAX_WAITING {
            eprintln!(
                "notifications: dropped one from {}: {MAX_WAITING} are already waiting their turn",
                notification.package_name
            );
            return None;
        }
        self.waiting.push(notification.key.clone());
        self.latest.insert(notification.key.clone(), notification);
        None
    }

    /// Drops the waiting notification with `key`. True when one was waiting.
    pub fn remove(&mut self, key: &str) -> bool {
        if self.latest.remove(key).is_some() {
            self.waiting.retain(|waiting| waiting != key);
            true
        } else {
            false
        }
    }

    /// The waiting notifications whose turn has come, oldest first.
    pub fn due(&mut self, now: Instant) -> Vec<Notification> {
        self.refill(now);
        let mut due = Vec::new();
        while self.tokens > 0 && !self.waiting.is_empty() {
            let key = self.waiting.remove(0);
            if let Some(notification) = self.latest.remove(&key) {
                self.tokens -= 1;
                due.push(notification);
            }
        }
        due
    }

    /// Forgets every waiting notification, as when the guest stops.
    pub fn clear(&mut self) {
        self.waiting.clear();
        self.latest.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(key: &str, title: &str) -> Notification {
        Notification {
            key: key.into(),
            title: title.into(),
            ..Notification::default()
        }
    }

    #[test]
    fn fr8_a_burst_goes_at_once_and_the_rest_wait_their_turn() {
        let start = Instant::now();
        let mut limit = RateLimit::new(start);
        for i in 0..BURST {
            assert!(limit.offer(note(&format!("k{i}"), "x"), start).is_some());
        }
        assert!(limit.offer(note("late", "x"), start).is_none());
        assert!(limit.due(start).is_empty());
        let due = limit.due(start + REFILL);
        assert_eq!(due, vec![note("late", "x")]);
    }

    #[test]
    fn fr8_a_waiting_notification_goes_once_in_its_latest_form() {
        let start = Instant::now();
        let mut limit = RateLimit::new(start);
        for i in 0..BURST {
            limit.offer(note(&format!("k{i}"), "x"), start);
        }
        limit.offer(note("progress", "1%"), start);
        limit.offer(note("progress", "50%"), start);
        limit.offer(note("progress", "99%"), start);
        assert_eq!(
            limit.due(start + REFILL * 3),
            vec![note("progress", "99%")]
        );
    }

    #[test]
    fn fr8_removing_a_waiting_notification_drops_it() {
        let start = Instant::now();
        let mut limit = RateLimit::new(start);
        for i in 0..BURST {
            limit.offer(note(&format!("k{i}"), "x"), start);
        }
        limit.offer(note("gone", "x"), start);
        assert!(limit.remove("gone"));
        assert!(limit.due(start + REFILL * 5).is_empty());
    }

    #[test]
    fn fr8_past_the_waiting_limit_the_newest_is_dropped() {
        let start = Instant::now();
        let mut limit = RateLimit::new(start);
        for i in 0..(BURST as usize + MAX_WAITING + 5) {
            limit.offer(note(&format!("k{i}"), "x"), start);
        }
        let due = limit.due(start + REFILL * 1000);
        assert_eq!(due.len(), MAX_WAITING);
    }

    #[test]
    fn nfr5_a_notification_without_a_key_is_dropped_and_a_large_icon_left_out() {
        assert!(checked(control::GuestNotification::default()).is_err());
        let long_key = control::GuestNotification {
            key: "k".repeat(MAX_KEY_BYTES + 1),
            ..Default::default()
        };
        assert!(checked(long_key).is_err());
        let big = checked(control::GuestNotification {
            key: "0|com.example|1|null|10001".into(),
            title: "t".repeat(MAX_LINE_CHARS + 10),
            icon_png: vec![0; MAX_ICON_BYTES + 1],
            importance: 99,
            ..Default::default()
        })
        .expect("kept");
        assert!(big.icon_png.is_empty());
        assert_eq!(big.title.chars().count(), MAX_LINE_CHARS);
        assert_eq!(big.importance, 5);
    }
}
