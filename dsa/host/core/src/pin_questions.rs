//! The questions asked of the user about an app's request to put one of its shortcuts on the
//! desktop (`requestPinShortcut`): how long a question waits, and which are still open.
//!
//! A host shows each question its own way (a notification, a dialog). A question nobody
//! answers is declined once it has waited [`ASK_FOR`], so the app is not left waiting.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::tasks::PinShortcutRequest;

/// How long a question waits for the user before it is declined.
pub const ASK_FOR: Duration = Duration::from_secs(180);

/// The questions on screen, by the key the host tells them apart with (the number its
/// notification service gave, or the request's own).
#[derive(Debug)]
pub struct OpenQuestions<K: Ord + Copy> {
    open: BTreeMap<K, (PinShortcutRequest, Instant)>,
}

impl<K: Ord + Copy> Default for OpenQuestions<K> {
    fn default() -> Self {
        OpenQuestions {
            open: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Copy> OpenQuestions<K> {
    pub fn new() -> OpenQuestions<K> {
        OpenQuestions::default()
    }

    /// The question about `request` is shown as `key`, from `now`.
    pub fn asked(&mut self, key: K, request: PinShortcutRequest, now: Instant) {
        self.open.insert(key, (request, now));
    }

    /// The request of the question `key`, taken so it is answered once.
    pub fn take(&mut self, key: K) -> Option<PinShortcutRequest> {
        self.open.remove(&key).map(|(request, _)| request)
    }

    /// Whether the question `key` is open.
    pub fn is_open(&self, key: K) -> bool {
        self.open.contains_key(&key)
    }

    /// The questions older than [`ASK_FOR`] at `now`, which are declined: taken, with their keys.
    pub fn expired(&mut self, now: Instant) -> Vec<(K, PinShortcutRequest)> {
        let old: Vec<K> = self
            .open
            .iter()
            .filter(|(_, (_, asked))| now.duration_since(*asked) >= ASK_FOR)
            .map(|(key, _)| *key)
            .collect();
        old.into_iter()
            .filter_map(|key| self.take(key).map(|request| (key, request)))
            .collect()
    }
}

/// The user's answer to a question, from the action chosen: `add` is yes, `cancel` and a
/// dismissal are no. A click on the question itself is no answer.
pub fn answer(action: &str, add: &str, cancel: &str, dismissed: bool) -> Option<bool> {
    if action == add {
        Some(true)
    } else if action == cancel || dismissed {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(request_id: u32) -> PinShortcutRequest {
        PinShortcutRequest {
            request_id,
            ..PinShortcutRequest::default()
        }
    }

    #[test]
    fn fr7_an_unanswered_pin_question_is_declined_after_three_minutes() {
        let now = Instant::now();
        let mut questions = OpenQuestions::new();
        questions.asked(10, request(1), now);
        questions.asked(11, request(2), now + ASK_FOR / 2);
        assert!(questions.expired(now + ASK_FOR / 2).is_empty());
        let left = questions.expired(now + ASK_FOR);
        assert_eq!(left.len(), 1);
        assert_eq!((left[0].0, left[0].1.request_id), (10, 1));
        assert!(!questions.is_open(10));
        assert!(questions.is_open(11));
    }

    #[test]
    fn fr7_a_pin_question_is_answered_once() {
        let mut questions = OpenQuestions::new();
        questions.asked(7u32, request(3), Instant::now());
        assert_eq!(questions.take(7).map(|r| r.request_id), Some(3));
        assert_eq!(questions.take(7), None);
    }

    #[test]
    fn fr7_only_add_means_yes_and_a_click_on_the_question_means_nothing() {
        assert_eq!(answer("add", "add", "cancel", false), Some(true));
        assert_eq!(answer("cancel", "add", "cancel", false), Some(false));
        assert_eq!(answer("dismiss", "add", "cancel", true), Some(false));
        assert_eq!(answer("default", "add", "cancel", false), None);
    }
}
