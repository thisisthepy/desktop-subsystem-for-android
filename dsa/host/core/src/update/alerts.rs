//! Telling the user about updates on the desktop: an update that is downloaded and waits to be
//! applied, one that was applied, and one that failed.
//!
//! The service raises each of these once ([`UpdateAlerts::raise`]), and every user's desktop
//! shows it as one of the host's own notifications, beside the notifications of the user's
//! Android: the watch of a user's notifications carries the alerts that user has not been told
//! yet ([`UpdateAlerts::untold`]), so a user whose desktop was not connected when an update
//! happened is told when it connects. Each alert is told to each user once, and an alert is
//! raised once for each version, so the same news never pops up twice.
//!
//! An alert's notification key ([`UpdateAlert::key`]) says what it is about, which the hosts use
//! three ways: to show it in the user's language ([`UpdateAlert::text`]), to let a newer alert
//! about the same update replace the one shown ([`thread_of`]), and to open the settings app's
//! update page when it is clicked rather than passing the click to Android
//! ([`is_update_key`]).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::notifications::{Notification, MAX_TEXT_CHARS};

/// What the keys of the update alerts start with. It is under the host's own prefix, which
/// no guest notification can have.
pub const KEY_PREFIX: &str = "dsa:update:";

/// The name an alert is shown under, as its app.
pub const SHOWN_AS: &str = "DSA";

/// How many alerts the service keeps. Only the latest of each [`Subject`] is told, so a few
/// are plenty; the rest are kept so that an alert raised again is still known.
const KEPT: usize = 32;

/// What an update is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subject {
    /// The Android image.
    Android,
    /// DSA itself.
    Program,
}

impl Subject {
    fn word(self) -> &'static str {
        match self {
            Subject::Android => "android",
            Subject::Program => "program",
        }
    }

    fn of_word(word: &str) -> Option<Subject> {
        [Subject::Android, Subject::Program]
            .into_iter()
            .find(|subject| subject.word() == word)
    }
}

/// What happened to an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertKind {
    /// It is downloaded and is applied later: an Android image the next time Android starts,
    /// DSA once no Android is running.
    Ready,
    /// It was applied and is in use.
    Applied,
    /// It was not applied; the previous version is still in use.
    Failed,
}

impl AlertKind {
    fn word(self) -> &'static str {
        match self {
            AlertKind::Ready => "ready",
            AlertKind::Applied => "applied",
            AlertKind::Failed => "failed",
        }
    }

    fn of_word(word: &str) -> Option<AlertKind> {
        [AlertKind::Ready, AlertKind::Applied, AlertKind::Failed]
            .into_iter()
            .find(|kind| kind.word() == word)
    }
}

/// One piece of news about an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAlert {
    pub subject: Subject,
    pub kind: AlertKind,
    /// The version the news is about: the one downloaded, applied, or that failed.
    pub version: String,
    /// Why it failed, in words a person can act on. Empty for the other kinds.
    pub detail: String,
}

impl UpdateAlert {
    pub fn new(subject: Subject, kind: AlertKind, version: impl Into<String>) -> UpdateAlert {
        UpdateAlert {
            subject,
            kind,
            version: version.into(),
            detail: String::new(),
        }
    }

    /// An alert that the update to `version` failed, because of `detail`.
    pub fn failed(subject: Subject, version: impl Into<String>, detail: impl Into<String>) -> Self {
        UpdateAlert {
            detail: detail.into(),
            ..UpdateAlert::new(subject, AlertKind::Failed, version)
        }
    }

    /// The key of its notification, which names it: `dsa:update:android:ready:16.0.3`.
    pub fn key(&self) -> String {
        format!(
            "{KEY_PREFIX}{}:{}:{}",
            self.subject.word(),
            self.kind.word(),
            self.version
        )
    }

    /// What the key of an alert's notification names, or `None` for any other key.
    pub fn parse_key(key: &str) -> Option<(Subject, AlertKind, &str)> {
        let rest = key.strip_prefix(KEY_PREFIX)?;
        let mut parts = rest.splitn(3, ':');
        let subject = Subject::of_word(parts.next()?)?;
        let kind = AlertKind::of_word(parts.next()?)?;
        let version = parts.next().filter(|version| !version.is_empty())?;
        Some((subject, kind, version))
    }

    /// The alert an update notification stands for. Its text is the alert's detail.
    pub fn of_notification(notification: &Notification) -> Option<UpdateAlert> {
        let (subject, kind, version) = UpdateAlert::parse_key(&notification.key)?;
        Some(UpdateAlert {
            subject,
            kind,
            version: version.to_string(),
            detail: notification.text.clone(),
        })
    }

    /// The notification that shows it. Its title is in English and its text is the detail; a
    /// host shows it through [`in_user_language`](crate::native_abi::in_user_language), which
    /// writes both in the user's language.
    pub fn notification(&self) -> Notification {
        let (title, _) = self.text(false);
        Notification {
            key: self.key(),
            package_name: String::new(),
            app_label: SHOWN_AS.into(),
            title,
            text: self.detail.chars().take(MAX_TEXT_CHARS).collect(),
            icon_png: Vec::new(),
            // It pops up: the user is meant to see it, but it is not urgent.
            importance: 3,
            high_priority: false,
            clearable: true,
            when_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_millis() as i64)
                .unwrap_or(0),
        }
    }

    /// The title and text, in Korean when `korean`, else in English.
    pub fn text(&self, korean: bool) -> (String, String) {
        let v = &self.version;
        let detail = self.detail.trim();
        match (self.subject, self.kind, korean) {
            (Subject::Android, AlertKind::Ready, false) => (
                "Android update ready".into(),
                format!("Android {v} is downloaded. It is used the next time Android starts."),
            ),
            (Subject::Android, AlertKind::Ready, true) => (
                "Android 업데이트가 준비되었습니다".into(),
                format!("Android {v}을(를) 내려받았습니다. 다음에 Android를 시작할 때 적용됩니다."),
            ),
            (Subject::Android, AlertKind::Applied, false) => (
                "Android updated".into(),
                format!("Android {v} started and is now in use."),
            ),
            (Subject::Android, AlertKind::Applied, true) => (
                "Android를 업데이트했습니다".into(),
                format!("Android {v}이(가) 시작되었고 이제 이 버전을 씁니다."),
            ),
            (Subject::Android, AlertKind::Failed, false) => (
                "Android update failed".into(),
                with_detail(format!("Android {v} was not applied."), detail, false),
            ),
            (Subject::Android, AlertKind::Failed, true) => (
                "Android 업데이트에 실패했습니다".into(),
                with_detail(
                    format!("Android {v}을(를) 적용하지 못했습니다."),
                    detail,
                    true,
                ),
            ),
            (Subject::Program, AlertKind::Ready, false) => (
                "DSA update ready".into(),
                format!(
                    "DSA {v} is downloaded. It installs when Android is not running; DSA \
                     Settings can install it now."
                ),
            ),
            (Subject::Program, AlertKind::Ready, true) => (
                "DSA 업데이트가 준비되었습니다".into(),
                format!(
                    "DSA {v}을(를) 내려받았습니다. Android가 실행 중이 아닐 때 설치되며, DSA \
                     설정에서 바로 설치할 수도 있습니다."
                ),
            ),
            (Subject::Program, AlertKind::Applied, false) => {
                ("DSA updated".into(), format!("DSA {v} is installed."))
            }
            (Subject::Program, AlertKind::Applied, true) => (
                "DSA를 업데이트했습니다".into(),
                format!("DSA {v}을(를) 설치했습니다."),
            ),
            (Subject::Program, AlertKind::Failed, false) => (
                "DSA update failed".into(),
                with_detail(
                    format!("DSA {v} was not installed; the previous version is still in use."),
                    detail,
                    false,
                ),
            ),
            (Subject::Program, AlertKind::Failed, true) => (
                "DSA 업데이트에 실패했습니다".into(),
                with_detail(
                    format!("DSA {v}을(를) 설치하지 못했습니다. 이전 버전을 그대로 씁니다."),
                    detail,
                    true,
                ),
            ),
        }
    }
}

/// `sentence`, then why, when there is a why. The why comes from the service in English.
fn with_detail(sentence: String, detail: &str, korean: bool) -> String {
    match (detail.is_empty(), korean) {
        (true, _) => sentence,
        (false, false) => format!("{sentence} Why: {detail}"),
        (false, true) => format!("{sentence} 이유: {detail}"),
    }
}

/// Whether `key` is an update alert's notification key, or the key the alerts about one
/// subject share ([`thread_of`]), which a host may show an alert under. A click on one opens
/// the settings app's update page on the host; nothing about it goes to Android.
pub fn is_update_key(key: &str) -> bool {
    UpdateAlert::parse_key(key).is_some()
        || key
            .strip_prefix(KEY_PREFIX)
            .is_some_and(|rest| Subject::of_word(rest).is_some())
}

/// The key that the alerts about one subject share, such as `dsa:update:android`: a host shows
/// one alert per subject at a time, so the news that an update was applied replaces the news
/// that it was ready. `None` for a key that is no update alert's.
pub fn thread_of(key: &str) -> Option<String> {
    let (subject, _, _) = UpdateAlert::parse_key(key)?;
    Some(format!("{KEY_PREFIX}{}", subject.word()))
}

/// The alerts the service raised, and which of them each user was told.
#[derive(Default)]
pub struct UpdateAlerts(Mutex<Board>);

#[derive(Default)]
struct Board {
    /// In the order they were raised.
    raised: Vec<UpdateAlert>,
    /// The keys of the alerts each user was told, by user.
    told: HashMap<String, HashSet<String>>,
}

impl UpdateAlerts {
    fn board(&self) -> std::sync::MutexGuard<'_, Board> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Raises `alert`. An alert raised before (the same subject, kind and version) is not
    /// raised again. Returns whether it was new.
    pub fn raise(&self, alert: UpdateAlert) -> bool {
        let mut board = self.board();
        let key = alert.key();
        if board.raised.iter().any(|known| known.key() == key) {
            return false;
        }
        board.raised.push(alert);
        if board.raised.len() > KEPT {
            board.raised.remove(0);
        }
        true
    }

    /// Every alert raised and kept, in the order they were raised.
    pub fn raised(&self) -> Vec<UpdateAlert> {
        self.board().raised.clone()
    }

    /// What `user` has not been told yet: for each subject, the latest alert, unless `user`
    /// was told it. An older alert is never told once a newer one about the same subject is
    /// raised, as it is news no longer.
    pub fn untold(&self, user: &str) -> Vec<UpdateAlert> {
        let board = self.board();
        let told = board.told.get(user);
        let mut latest: Vec<&UpdateAlert> = Vec::new();
        for alert in board.raised.iter().rev() {
            if !latest.iter().any(|seen| seen.subject == alert.subject) {
                latest.push(alert);
            }
        }
        latest
            .into_iter()
            .rev()
            .filter(|alert| told.is_none_or(|told| !told.contains(&alert.key())))
            .cloned()
            .collect()
    }

    /// `user`'s desktop was told `alert`.
    pub fn told(&self, user: &str, alert: &UpdateAlert) {
        let mut board = self.board();
        let key = alert.key();
        if board.raised.iter().any(|known| known.key() == key) {
            board.told.entry(user.to_string()).or_default().insert(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr14_an_alert_key_names_what_it_is_about() {
        let alert = UpdateAlert::new(Subject::Android, AlertKind::Ready, "17.0.0-16373615-8");
        assert_eq!(alert.key(), "dsa:update:android:ready:17.0.0-16373615-8");
        assert_eq!(
            UpdateAlert::parse_key(&alert.key()),
            Some((Subject::Android, AlertKind::Ready, "17.0.0-16373615-8"))
        );
        assert!(is_update_key(&alert.key()));
        assert!(is_update_key("dsa:update:program"));
        for other in [
            "dsa:update:",
            "dsa:update:kernel",
            "dsa:update:android:ready:",
            "dsa:update:android:soon:16.0.3",
            "dsa:update:kernel:ready:16.0.3",
            "dsa:run-as-arm:offer:com.example.game",
            "0|com.example.chat|1|null|10100",
        ] {
            assert!(!is_update_key(other), "{other}");
        }
        assert_eq!(
            thread_of("dsa:update:program:failed:0.1.10").as_deref(),
            Some("dsa:update:program")
        );
        assert_eq!(thread_of("0|com.example|1|null|1"), None);
    }

    #[test]
    fn fr15_an_alert_is_shown_in_english_and_korean() {
        let failed = UpdateAlert::failed(Subject::Program, "0.1.10", "setup ended with 1603");
        let (title, text) = failed.text(false);
        assert_eq!(title, "DSA update failed");
        assert!(text.contains("DSA 0.1.10") && text.ends_with("Why: setup ended with 1603"));
        let (title, text) = failed.text(true);
        assert_eq!(title, "DSA 업데이트에 실패했습니다");
        assert!(text.contains("이유: setup ended with 1603"), "{text}");
        let notification = failed.notification();
        assert_eq!(notification.title, "DSA update failed");
        assert_eq!(notification.text, "setup ended with 1603");
        assert_eq!(UpdateAlert::of_notification(&notification), Some(failed));
    }
}
