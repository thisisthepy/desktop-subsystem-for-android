//! An app's request to put one of its shortcuts on the desktop (`requestPinShortcut`), as
//! the desktop asks about it: a notification with [Add to desktop] and [Cancel]. On yes the
//! compositor writes the shortcut's desktop entry ([`super::desktop_entry::place_pinned_in`]);
//! a question nobody answers is declined after [`ASK_FOR`], so the app is not left waiting.

use std::time::Instant;

use dsa_host_core::pin_questions::{answer, OpenQuestions};
use dsa_host_core::tasks::PinShortcutRequest;
use dsa_host_core::ui_language::UiLanguage;
use dsa_host_core::ui_text::fill;
use dsa_host_core::ui_text::pin_shortcut::pin_question_text;

use super::desktop_entry::app_id;
use super::notifications::{body_text, Notify};

/// The action of [Add to desktop].
pub const ADD: &str = "dsa-pin-add";

/// The action of [Cancel].
pub const CANCEL: &str = "dsa-pin-cancel";

pub use dsa_host_core::pin_questions::ASK_FOR;

/// The `Notify` call that asks the user about `request`, in `language`.
pub fn question(request: &PinShortcutRequest, language: UiLanguage) -> Notify {
    let app = request.app_name();
    let name = request.shortcut.label();
    let text = pin_question_text(language);
    let summary = fill(text.heading, &[("name", name)]);
    let body = fill(text.body, &[("app", app)]);
    let (add, cancel) = (text.add, text.cancel);
    Notify {
        app_name: app.to_string(),
        replaces_id: 0,
        summary,
        body: body_text(&body),
        actions: vec![ADD.into(), add.into(), CANCEL.into(), cancel.into()],
        urgency: 1,
        desktop_entry: app_id(&request.package_name),
        resident: true,
        reply_placeholder: String::new(),
    }
}

/// The questions on screen, by the number the notification service gave each.
#[derive(Debug, Default)]
pub struct Questions {
    open: OpenQuestions<u32>,
}

impl Questions {
    pub fn new() -> Questions {
        Questions::default()
    }

    /// The question about `request` is shown as `id`, from `now`.
    pub fn asked(&mut self, id: u32, request: PinShortcutRequest, now: Instant) {
        self.open.asked(id, request, now);
    }

    /// `ActionInvoked` on `id`: the request and the user's answer, when `id` is one of ours.
    /// A click on the notification itself is no answer.
    pub fn action(&mut self, id: u32, action: &str) -> Option<(PinShortcutRequest, bool)> {
        let accepted = answer(action, ADD, CANCEL, false)?;
        self.open.take(id).map(|request| (request, accepted))
    }

    /// `NotificationClosed` on `id`: a question dismissed unanswered is declined.
    pub fn closed(&mut self, id: u32) -> Option<PinShortcutRequest> {
        self.open.take(id)
    }

    /// The questions older than [`ASK_FOR`] at `now`, which are declined and closed.
    pub fn expired(&mut self, now: Instant) -> Vec<(u32, PinShortcutRequest)> {
        self.open.expired(now)
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
        let asked = question(&request(1), UiLanguage::English);
        assert_eq!(asked.actions, vec![ADD, "Add to desktop", CANCEL, "Cancel"]);
        assert!(asked.summary.contains("Ana <3"));
        assert!(asked.body.starts_with("Chat"));
        assert_eq!(asked.desktop_entry, "dsa-com.example.chat");
        assert!(asked.resident);
        let korean = question(&request(1), UiLanguage::Korean);
        assert_eq!(korean.actions[1], "바탕 화면에 추가");
        assert_eq!(korean.actions[3], "취소");
    }

    #[test]
    fn fr17_a_pin_request_asks_in_every_interface_language() {
        use dsa_host_core::ui_language::placeholders;
        let english = pin_question_text(UiLanguage::English);
        for language in UiLanguage::ALL {
            let text = pin_question_text(language);
            for words in [text.heading, text.body, text.add, text.cancel] {
                assert!(!words.trim().is_empty(), "{language:?}");
                assert!(
                    !words.contains(char::from_u32(0x2014).unwrap()),
                    "{language:?}: {words}"
                );
            }
            assert_eq!(placeholders(text.heading), placeholders(english.heading));
            assert_eq!(placeholders(text.body), placeholders(english.body));
            assert!(placeholders(text.add).is_empty() && placeholders(text.cancel).is_empty());
            if language != UiLanguage::English {
                assert_ne!(text.add, english.add, "{language:?}");
            }
            let asked = question(&request(1), language);
            // The shortcut and the app are named; no placeholder is left.
            assert!(asked.summary.contains("Ana <3"), "{language:?}");
            assert!(asked.body.contains("Chat"), "{language:?}");
            assert!(!asked.summary.contains("{name}") && !asked.body.contains("{app}"));
            assert_eq!(asked.actions[0], ADD);
            assert_eq!(asked.actions[1], text.add);
            assert_eq!(asked.actions[2], CANCEL);
            assert_eq!(asked.actions[3], text.cancel);
        }
        assert_eq!(fill("{app} {name}", &[("app", "{name}")]), "{name} {name}");
        assert_eq!(fill("{other}", &[("app", "x")]), "{other}");
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
