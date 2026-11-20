//! The status page says a start the host refused is a problem with this computer.

use dsa_host_core::start_failure::host_problem;
use dsa_host_core::ui_language::UiLanguage;
use dsa_settings_linux::view::last_reason_text;

#[test]
fn fr14_a_start_the_host_refused_reads_as_a_host_problem_in_the_users_language() {
    let reason = host_problem("HcsCreateComputeSystem failed (HRESULT 0x803b0014)");
    let korean = last_reason_text(&reason, UiLanguage::Korean);
    assert!(
        korean.starts_with("이 컴퓨터에서 Android를 실행하지 못했습니다"),
        "{korean}"
    );
    assert!(korean.ends_with("0x803b0014)"), "{korean}");
    let english = last_reason_text(&reason, UiLanguage::English);
    assert!(
        english.starts_with("This computer could not run Android."),
        "{english}"
    );
}

#[test]
fn fr14_any_other_reason_is_shown_as_the_service_gave_it() {
    assert_eq!(
        last_reason_text("stopped on request", UiLanguage::Korean),
        "stopped on request"
    );
}
