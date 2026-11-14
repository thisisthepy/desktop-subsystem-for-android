//! An app's request to put one of its shortcuts on the Desktop (`requestPinShortcut`), as
//! the Mac asks about it: a banner with [Add to desktop] and [Cancel] in the Notification
//! Center ([`PIN_CATEGORY`]). On yes the window manager makes the shortcut's bundle and its
//! link on the Desktop ([`super::app_stubs::place_pinned_in`]); a question nobody answers is
//! declined after [`ASK_FOR`], so the app is not left waiting.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dsa_host_core::tasks::PinShortcutRequest;

/// The category of the question, whose actions are [`ADD`] and [`CANCEL`].
pub const PIN_CATEGORY: &str = "org.thisisthepy.dsa.pin-shortcut";

/// The identifier of [Add to desktop].
pub const ADD: &str = "org.thisisthepy.dsa.pin-shortcut.add";

/// The identifier of [Cancel].
pub const CANCEL: &str = "org.thisisthepy.dsa.pin-shortcut.cancel";

/// What every question's request identifier starts with, before the request's number.
const PREFIX: &str = "org.thisisthepy.dsa.pin-shortcut.request.";

/// How long a question waits for the user before it is declined.
pub const ASK_FOR: Duration = Duration::from_secs(180);

/// The labels of [Add to desktop] and [Cancel], in Korean when `korean`.
pub fn buttons(korean: bool) -> (&'static str, &'static str) {
    if korean {
        ("바탕 화면에 추가", "취소")
    } else {
        ("Add to desktop", "Cancel")
    }
}

/// The identifier of the question about request `request_id`.
pub fn identifier(request_id: u32) -> String {
    format!("{PREFIX}{request_id}")
}

/// The request a question's identifier is about; `None` for any other banner.
pub fn request_of(identifier: &str) -> Option<u32> {
    identifier.strip_prefix(PREFIX)?.parse().ok()
}

/// The question's title and body, in Korean when `korean`.
pub fn texts(request: &PinShortcutRequest, korean: bool) -> (String, String) {
    let app = if request.app_label.trim().is_empty() {
        request.package_name.as_str()
    } else {
        request.app_label.as_str()
    };
    let name = request.shortcut.label();
    if korean {
        (
            format!("\u{201c}{name}\u{201d}을(를) 바탕 화면에 추가할까요?"),
            format!("{app}에서 이 바로 가기를 바탕 화면에 두려고 합니다."),
        )
    } else {
        (
            format!("Add \u{201c}{name}\u{201d} to the desktop?"),
            format!("{app} wants to put this shortcut on your desktop."),
        )
    }
}

/// The user's answer to a question: the action's identifier, [`ADD`] for yes; [`CANCEL`]
/// and a dismissal are no. A click on the banner itself is no answer.
pub fn answer(action: &str, dismissed: bool) -> Option<bool> {
    match action {
        ADD => Some(true),
        CANCEL => Some(false),
        _ if dismissed => Some(false),
        _ => None,
    }
}

/// The questions on screen, by request.
#[derive(Debug, Default)]
pub struct Pending {
    asked: BTreeMap<u32, (PinShortcutRequest, Instant)>,
}

impl Pending {
    pub fn new() -> Pending {
        Pending::default()
    }

    pub fn asked(&mut self, request: PinShortcutRequest, now: Instant) {
        self.asked.insert(request.request_id, (request, now));
    }

    /// The request answered: taken, so it is answered once.
    pub fn take(&mut self, request_id: u32) -> Option<PinShortcutRequest> {
        self.asked.remove(&request_id).map(|(request, _)| request)
    }

    /// The questions older than [`ASK_FOR`] at `now`, which are declined.
    pub fn expired(&mut self, now: Instant) -> Vec<PinShortcutRequest> {
        let old: Vec<u32> = self
            .asked
            .iter()
            .filter(|(_, (_, at))| now.duration_since(*at) >= ASK_FOR)
            .map(|(id, _)| *id)
            .collect();
        old.into_iter().filter_map(|id| self.take(id)).collect()
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
        assert_eq!(buttons(false), ("Add to desktop", "Cancel"));
        assert_eq!(buttons(true), ("바탕 화면에 추가", "취소"));
        let (title, body) = texts(&request(1), false);
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
