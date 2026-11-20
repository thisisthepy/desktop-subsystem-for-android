//! Telling a start the host refused apart from one the image failed.

use dsa_host_core::start_failure::{
    host_problem, host_problem_reason, host_problem_text, localized_host_problem, FailureSide,
};
use dsa_host_core::ui_language::UiLanguage;

#[test]
fn fr14_a_guest_that_printed_nothing_failed_on_the_host() {
    assert_eq!(FailureSide::from_guest_output(false), FailureSide::Host);
    assert_eq!(FailureSide::from_guest_output(true), FailureSide::Guest);
    assert_eq!(FailureSide::default(), FailureSide::Guest);
}

#[test]
fn fr14_a_host_problem_keeps_its_reason() {
    let reason = host_problem("HcsStartComputeSystem failed (HRESULT 0x8007042b)");
    assert_eq!(
        host_problem_reason(&reason),
        Some("HcsStartComputeSystem failed (HRESULT 0x8007042b)")
    );
    assert_eq!(host_problem_reason("the guest did not start: x"), None);
}

#[test]
fn fr14_every_language_says_a_host_problem_with_its_reason() {
    let reason = format!("{}\nconsole tail", host_problem("0x803b0014"));
    for lang in UiLanguage::ALL {
        let text = localized_host_problem(lang, &reason).unwrap();
        assert!(text.ends_with("0x803b0014"), "{lang:?}: {text}");
        assert!(!text.contains("{why}"), "{lang:?}: {text}");
        if lang != UiLanguage::English {
            assert_ne!(
                text,
                host_problem_text(UiLanguage::English, "0x803b0014"),
                "{lang:?} is translated"
            );
        }
    }
    assert_eq!(
        localized_host_problem(UiLanguage::Korean, "stopped on request"),
        None
    );
}
