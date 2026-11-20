//! The words of the administrator approval dialog a host shows itself: who asks, what it runs
//! and how long the person has, in every interface language.

use std::time::Duration;

use dsa_host_core::root_approval::{ApprovalRequest, Purpose};
use dsa_host_core::tasks::AppInfo;
use dsa_host_core::ui_language::{placeholders, UiLanguage};
use dsa_host_core::ui_text::root_approval::{
    confirm_text, confirm_text_for, confirm_words, time_limit_text, time_words, AskedWords,
};

fn su_from(package: &str, uid: u32, label: &str, language: &str) -> ApprovalRequest {
    ApprovalRequest {
        request_id: 1,
        app: AppInfo {
            package_name: package.into(),
            label: label.into(),
            ..Default::default()
        },
        uid,
        command: vec!["id".into()],
        interactive: false,
        purpose: Purpose::Run,
        shared_apps: Vec::new(),
        language: language.into(),
    }
}

/// Panics when `text`, the `language` translation of `english`, is empty, holds an em dash or
/// has other placeholders than the English one.
fn assert_translation(language: UiLanguage, what: &str, english: &str, text: &str) {
    let tag = language.tag();
    assert!(!text.trim().is_empty(), "{what} is empty in {tag}");
    assert!(
        !text.contains(char::from_u32(0x2014).unwrap()),
        "{what} has an em dash in {tag}: {text}"
    );
    assert_eq!(
        placeholders(text),
        placeholders(english),
        "{what} in {tag} has other placeholders than in English: {text}"
    );
}

#[test]
fn fr35_confirm_text_names_the_command() {
    let request = su_from("com.example.backup", 10_200, "백업", "ko-KR");
    let words = AskedWords::of(&request);
    assert_eq!(words.label, "백업");
    assert_eq!(words.detail, "com.example.backup");
    assert_eq!(words.language, UiLanguage::Korean);
    // With UAC off, the program's own dialog has the name, then the package under it.
    let shown = confirm_text(&words.label, &words.detail, &words.command, words.language);
    assert_eq!(shown.caption, "백업: 관리자 승인");
    assert!(
        shown
            .text
            .starts_with("백업\ncom.example.backup\n\nAndroid에서 다음을 루트 권한으로"),
        "{}",
        shown.text
    );
    let english = confirm_text("Backup", "com.example.backup", "id", UiLanguage::English);
    assert!(english
        .text
        .starts_with("Backup\ncom.example.backup\n\nwants to run this"));
}

#[test]
fn fr17_the_root_prompt_speaks_every_language() {
    let english = confirm_words(UiLanguage::English);
    for language in UiLanguage::ALL {
        let words = confirm_words(language);
        for (what, en, text) in [
            ("the unnamed app", english.unnamed, words.unnamed),
            ("the caption", english.caption, words.caption),
            ("the text", english.text, words.text),
            ("the time limit", english.time_limit, words.time_limit),
        ] {
            assert_translation(language, what, en, text);
        }
        // The dialog names who asks, the package and the command, and says how long it has.
        let shown = confirm_text_for(&AskedWords {
            label: "Backup".into(),
            detail: "com.example.backup".into(),
            command: "id -u".into(),
            language,
            seconds: 90,
        });
        assert!(shown.caption.contains("Backup"), "{language:?}");
        assert!(
            shown.text.starts_with("Backup\ncom.example.backup"),
            "{language:?}"
        );
        assert!(shown.text.contains("id -u"), "{language:?}");
        assert!(shown.text.contains("90"), "{language:?}: {}", shown.text);
        assert!(!shown.text.contains('{'), "{language:?}: {}", shown.text);
        assert!(shown.text.contains("Desktop Subsystem for Android"));
        let unnamed = confirm_text(" ", "", "id", language);
        assert!(unnamed.caption.contains("Android"), "{language:?}");
    }
}

#[test]
fn fr35_time_limit_is_said_in_every_language() {
    for language in UiLanguage::ALL {
        for seconds in [1, 2, 5, 21, 60, 120, 300] {
            let time = time_words(seconds, language);
            let limit = time_limit_text(seconds, language).unwrap();
            assert!(limit.contains(&time), "{language:?}: {limit}");
            assert!(!limit.contains('{'), "{language:?}: {limit}");
        }
    }
    assert_eq!(time_words(1, UiLanguage::English), "1 second");
    assert_eq!(time_words(120, UiLanguage::German), "2 Minuten");
    assert_eq!(time_words(60, UiLanguage::Russian), "1 минуты");
    assert_eq!(time_words(21, UiLanguage::Russian), "21 секунды");
    assert_eq!(time_words(300, UiLanguage::Russian), "5 минут");
    assert_eq!(time_words(11, UiLanguage::Russian), "11 секунд");
    assert_eq!(
        time_limit_text(60, UiLanguage::English).as_deref(),
        Some("If nobody answers within 1 minute, the request is refused.")
    );
    assert_eq!(
        time_limit_text(300, UiLanguage::Korean).as_deref(),
        Some("5분 안에 답하지 않으면 요청은 거부됩니다.")
    );
    // A program started by an older service, which gives no time, says nothing about it.
    assert_eq!(time_limit_text(0, UiLanguage::English), None);
}

#[test]
fn fr18_the_own_dialog_says_how_long_it_has_once_without_a_countdown() {
    let request = su_from("com.example.backup", 10_200, "Backup", "en-US");
    let words = AskedWords::of(&request).within(Duration::from_secs(300));
    assert_eq!(words.seconds, 300);
    // The dialog's text says it once, at its end, after what is asked.
    let shown = confirm_text_for(&words);
    assert!(shown.text.starts_with(
        &confirm_text(&words.label, &words.detail, &words.command, words.language).text
    ));
    assert!(
        shown
            .text
            .ends_with("If nobody answers within 5 minutes, the request is refused."),
        "{}",
        shown.text
    );
    let korean = su_from("com.example.backup", 10_200, "Backup", "ko-KR");
    let korean = confirm_text_for(&AskedWords::of(&korean).within(Duration::from_secs(55)));
    assert!(
        korean
            .text
            .ends_with("55초 안에 답하지 않으면 요청은 거부됩니다."),
        "{}",
        korean.text
    );
    // No time given: nothing about it.
    let untimed = AskedWords {
        seconds: 0,
        ..words.clone()
    };
    assert_eq!(
        confirm_text_for(&untimed),
        confirm_text(&words.label, &words.detail, &words.command, words.language)
    );
}
