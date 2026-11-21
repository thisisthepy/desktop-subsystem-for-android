//! An app's request to put one of its shortcuts on the Desktop (`requestPinShortcut`), as
//! the Mac asks about it: a banner with [Add to desktop] and [Cancel] in the Notification
//! Center ([`PIN_CATEGORY`]). On yes the window manager makes the shortcut's bundle and its
//! link on the Desktop ([`super::app_stubs::place_pinned_in`]); a question nobody answers is
//! declined after [`ASK_FOR`], so the app is not left waiting.

use std::time::Instant;

use dsa_host_core::pin_questions::OpenQuestions;
use dsa_host_core::tasks::PinShortcutRequest;
use dsa_host_core::ui_language::UiLanguage;
use dsa_host_core::ui_text::fill;
use dsa_host_core::ui_text::pin_shortcut::pin_question_text;

/// The category of the question, whose actions are [`ADD`] and [`CANCEL`].
pub const PIN_CATEGORY: &str = "org.thisisthepy.dsa.pin-shortcut";

/// The identifier of [Add to desktop].
pub const ADD: &str = "org.thisisthepy.dsa.pin-shortcut.add";

/// The identifier of [Cancel].
pub const CANCEL: &str = "org.thisisthepy.dsa.pin-shortcut.cancel";

/// What every question's request identifier starts with, before the request's number.
const PREFIX: &str = "org.thisisthepy.dsa.pin-shortcut.request.";

pub use dsa_host_core::pin_questions::ASK_FOR;

/// The labels of [Add to desktop] and [Cancel], in `lang`.
pub fn buttons(lang: UiLanguage) -> (&'static str, &'static str) {
    let text = pin_question_text(lang);
    (text.add, text.cancel)
}

/// The identifier of the question about request `request_id`.
pub fn identifier(request_id: u32) -> String {
    format!("{PREFIX}{request_id}")
}

/// The request a question's identifier is about; `None` for any other banner.
pub fn request_of(identifier: &str) -> Option<u32> {
    identifier.strip_prefix(PREFIX)?.parse().ok()
}

/// The question's title and body, in `lang`.
pub fn texts(request: &PinShortcutRequest, lang: UiLanguage) -> (String, String) {
    let text = pin_question_text(lang);
    (
        fill(text.heading, &[("name", request.shortcut.label())]),
        fill(text.body, &[("app", request.app_name())]),
    )
}

/// The user's answer to a question: the action's identifier, [`ADD`] for yes; [`CANCEL`]
/// and a dismissal are no. A click on the banner itself is no answer.
pub fn answer(action: &str, dismissed: bool) -> Option<bool> {
    dsa_host_core::pin_questions::answer(action, ADD, CANCEL, dismissed)
}

/// The questions on screen, by request.
#[derive(Debug, Default)]
pub struct Pending {
    open: OpenQuestions<u32>,
}

impl Pending {
    pub fn new() -> Pending {
        Pending::default()
    }

    pub fn asked(&mut self, request: PinShortcutRequest, now: Instant) {
        self.open.asked(request.request_id, request, now);
    }

    /// The request answered: taken, so it is answered once.
    pub fn take(&mut self, request_id: u32) -> Option<PinShortcutRequest> {
        self.open.take(request_id)
    }

    /// The questions older than [`ASK_FOR`] at `now`, which are declined.
    pub fn expired(&mut self, now: Instant) -> Vec<PinShortcutRequest> {
        self.open
            .expired(now)
            .into_iter()
            .map(|(_, request)| request)
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
                short_label: "Ana".into(),
                ..AppShortcut::default()
            },
        }
    }

    #[test]
    fn fr7_a_pin_question_has_add_to_desktop_and_cancel() {
        assert_eq!(buttons(UiLanguage::English), ("Add to desktop", "Cancel"));
        assert_eq!(buttons(UiLanguage::Korean), ("바탕 화면에 추가", "취소"));
        let (title, body) = texts(&request(1), UiLanguage::English);
        assert!(title.contains("Ana"));
        assert!(body.starts_with("Chat"));
        assert_eq!(request_of(&identifier(42)), Some(42));
        assert_eq!(request_of("com.example.chat|1|tag"), None);
        assert_eq!(answer(ADD, false), Some(true));
        assert_eq!(answer(CANCEL, false), Some(false));
        assert_eq!(answer("dismiss", true), Some(false));
        assert_eq!(answer("default", false), None);
    }

    #[test]
    fn fr17_a_pin_question_is_asked_in_every_language() {
        let request = request(1);
        for lang in UiLanguage::ALL {
            let (add, cancel) = buttons(lang);
            let (title, body) = texts(&request, lang);
            for text in [add, cancel, title.as_str(), body.as_str()] {
                assert!(!text.trim().is_empty(), "{lang:?}");
                assert!(!crate::ui_language::has_em_dash(text), "{lang:?}: {text}");
            }
            assert_ne!(add, cancel, "{lang:?}");
            // The title names the shortcut and the body the app, in every language.
            assert!(title.contains("Ana"), "{lang:?}: {title}");
            assert!(body.contains("Chat"), "{lang:?}: {body}");
            if lang != UiLanguage::English {
                assert_ne!(add, "Add to desktop", "{lang:?} is not translated");
            }
        }
        // An app without a label is named by its package.
        let mut bare = request.clone();
        bare.app_label.clear();
        for lang in UiLanguage::ALL {
            assert!(
                texts(&bare, lang).1.contains("com.example.chat"),
                "{lang:?}"
            );
        }
    }

    #[test]
    fn fr7_a_pin_question_is_answered_once_and_declined_when_left() {
        let now = Instant::now();
        let mut pending = Pending::new();
        pending.asked(request(1), now);
        pending.asked(request(2), now);
        assert_eq!(pending.take(1).map(|r| r.request_id), Some(1));
        assert_eq!(pending.take(1), None);
        assert!(pending.expired(now + ASK_FOR / 2).is_empty());
        assert_eq!(pending.expired(now + ASK_FOR)[0].request_id, 2);
    }
}
