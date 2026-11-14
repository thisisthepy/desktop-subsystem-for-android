//! An app's request to put one of its shortcuts on the desktop (`requestPinShortcut`), as
//! the desktop asks about it: a notification with [Add to desktop] and [Cancel]. On yes the
//! compositor writes the shortcut's desktop entry ([`super::desktop_entry::place_pinned_in`]);
//! a question nobody answers is declined after [`ASK_FOR`], so the app is not left waiting.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dsa_host_core::tasks::PinShortcutRequest;

use super::desktop_entry::app_id;
use super::notifications::{body_text, Notify};

/// The action of [Add to desktop].
pub const ADD: &str = "dsa-pin-add";

/// The action of [Cancel].
pub const CANCEL: &str = "dsa-pin-cancel";

/// How long a question waits for the user before it is declined.
pub const ASK_FOR: Duration = Duration::from_secs(180);

/// The `Notify` call that asks the user about `request`, in Korean when `korean`.
pub fn question(request: &PinShortcutRequest, korean: bool) -> Notify {
    let app = if request.app_label.trim().is_empty() {
        request.package_name.as_str()
    } else {
        request.app_label.as_str()
    };
    let name = request.shortcut.label();
    let (summary, body, add, cancel) = if korean {
        (
            format!("\u{201c}{name}\u{201d}을(를) 바탕 화면에 추가할까요?"),
            format!("{app}에서 이 바로 가기를 바탕 화면에 두려고 합니다."),
            "바탕 화면에 추가",
            "취소",
        )
    } else {
        (
            format!("Add \u{201c}{name}\u{201d} to the desktop?"),
            format!("{app} wants to put this shortcut on your desktop."),
            "Add to desktop",
            "Cancel",
        )
    };
    Notify {
        app_name: app.to_string(),
        replaces_id: 0,
        summary,
        body: body_text(&body),
        actions: vec![ADD.into(), add.into(), CANCEL.into(), cancel.into()],
        urgency: 1,
        desktop_entry: app_id(&request.package_name),
        resident: true,
    }
}

/// The questions on screen, by the number the notification service gave each.
#[derive(Debug, Default)]
pub struct Questions {
    pending: BTreeMap<u32, (PinShortcutRequest, Instant)>,
}

impl Questions {
    pub fn new() -> Questions {
        Questions::default()
    }

    /// The question about `request` is shown as `id`, from `now`.
    pub fn asked(&mut self, id: u32, request: PinShortcutRequest, now: Instant) {
        self.pending.insert(id, (request, now));
    }

    /// `ActionInvoked` on `id`: the request and the user's answer, when `id` is one of ours.
    /// A click on the notification itself is no answer.
    pub fn action(&mut self, id: u32, action: &str) -> Option<(PinShortcutRequest, bool)> {
        let accepted = match action {
            ADD => true,
            CANCEL => false,
            _ => return None,
        };
        self.pending
            .remove(&id)
            .map(|(request, _)| (request, accepted))
    }

    /// `NotificationClosed` on `id`: a question dismissed unanswered is declined.
    pub fn closed(&mut self, id: u32) -> Option<PinShortcutRequest> {
        self.pending.remove(&id).map(|(request, _)| request)
    }

    /// The questions older than [`ASK_FOR`] at `now`, which are declined and closed.
    pub fn expired(&mut self, now: Instant) -> Vec<(u32, PinShortcutRequest)> {
        let old: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, (_, asked))| now.duration_since(*asked) >= ASK_FOR)
            .map(|(id, _)| *id)
            .collect();
        old.into_iter()
            .filter_map(|id| self.pending.remove(&id).map(|(request, _)| (id, request)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::tasks::AppShortcut;

    fn request(request_id: u32) -> PinShortcutRequest {
        PinShortcutRequest {
            request_id,
            package_name: "com.example.chat".into(),
            app_label: "Chat".into(),
            shortcut: AppShortcut {
                id: "ana".into(),
                short_label: "Ana <3".into(),
                ..AppShortcut::default()
            },
        }
    }

    #[test]
    fn fr7_a_pin_request_asks_with_add_to_desktop_and_cancel() {
        let asked = question(&request(1), false);
        assert_eq!(asked.actions, vec![ADD, "Add to desktop", CANCEL, "Cancel"]);
        assert!(asked.summary.contains("Ana <3"));
        assert!(asked.body.starts_with("Chat"));
        assert_eq!(asked.desktop_entry, "dsa-com.example.chat");
        assert!(asked.resident);
        let korean = question(&request(1), true);
        assert_eq!(korean.actions[1], "바탕 화면에 추가");
        assert_eq!(korean.actions[3], "취소");
    }

    #[test]
    fn fr7_a_pin_question_is_answered_once_and_declined_when_left() {
        let now = Instant::now();
        let mut questions = Questions::new();
        questions.asked(10, request(1), now);
        questions.asked(11, request(2), now);
        questions.asked(12, request(3), now);
        // Another notification's action, or a click on the body, answers nothing.
        assert_eq!(questions.action(99, ADD), None);
        assert_eq!(questions.action(10, "default"), None);
        let (answered, yes) = questions.action(10, ADD).unwrap();
        assert_eq!((answered.request_id, yes), (1, true));
        assert_eq!(questions.action(10, ADD), None, "answered once");
        assert_eq!(questions.closed(11).map(|r| r.request_id), Some(2));
        assert!(questions.expired(now + ASK_FOR / 2).is_empty());
        let left = questions.expired(now + ASK_FOR);
        assert_eq!(left.len(), 1);
        assert_eq!((left[0].0, left[0].1.request_id), (12, 3));
    }
}
