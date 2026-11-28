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
fn fr14_a_boot_whose_console_was_not_kept_does_not_blame_the_image() {
    // Without a console nothing shows that the guest's kernel ran: a VM worker that died at
    // start looks the same as a guest that died later.
    assert_eq!(FailureSide::from_console(None), FailureSide::Host);
    assert_eq!(FailureSide::from_console(Some(false)), FailureSide::Host);
    assert_eq!(FailureSide::from_console(Some(true)), FailureSide::Guest);
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

/// What the hypervisor service answered on each attempt in #1065, where no hypervisor ran.
const HYPER_V_NOT_INSTALLED: &str = "HcsCreateComputeSystem failed (HRESULT 0x80370102): \
    {\"Error\":-2143878910,\"ErrorMessage\":\"The virtual machine could not be started because \
    a required feature is not installed.\",\"ErrorEvents\":[{\"Message\":\"Cannot create system \
    'dsa-android' since Hyper-V is not installed on the host.\"}]}";

#[test]
fn fr14_virtualization_off_is_read_from_the_hosts_words_and_codes() {
    use dsa_host_core::start_failure::{virtualization_off, HYPERVISOR_NOT_RUNNING};
    for reason in [
        HYPER_V_NOT_INSTALLED.to_string(),
        host_problem(HYPER_V_NOT_INSTALLED),
        format!("the guest did not start: {HYPER_V_NOT_INSTALLED}"),
        "HcsStartComputeSystem failed (HRESULT 0xC0351000)".to_string(),
        "Cannot create system 'x' since Hyper-V is not installed on the host.".to_string(),
        "opening HCS: the Host Compute System is not available".to_string(),
        host_problem(HYPERVISOR_NOT_RUNNING),
    ] {
        assert!(virtualization_off(&reason), "{reason}");
    }
    for reason in [
        "HcsStartComputeSystem failed (HRESULT 0x8007000e)",
        "it did not finish booting within 300 seconds",
        // The console's hex numbers below the first line are addresses, not error codes.
        "the guest did not start: x\n[    0.1] mapped 0x80370102",
        "",
    ] {
        assert!(!virtualization_off(reason), "{reason}");
    }
}

#[test]
fn fr14_a_failure_recorded_for_the_host_is_told_apart_from_the_images() {
    use dsa_host_core::start_failure::host_side_reason;
    assert!(host_side_reason(&host_problem(
        "the network service refused"
    )));
    assert!(host_side_reason(&format!(
        "the guest did not start: {HYPER_V_NOT_INSTALLED}"
    )));
    assert!(!host_side_reason("17.0.0-1 did not finish booting"));
    assert!(!host_side_reason(
        "the guest did not start: 17.0.0-1 has no kernel"
    ));
}

#[test]
fn fr14_every_language_says_virtualization_is_off_with_its_three_fixes() {
    use dsa_host_core::start_failure::{
        virtualization_notification, virtualization_off_text, virtualization_off_title,
    };
    let english = virtualization_off_text(UiLanguage::English);
    for words in [
        "virtualization is off",
        "Intel VT-x or AMD-V",
        "UEFI/BIOS",
        "Task Manager",
        "\"Virtualization: Enabled\"",
        "\"Virtual Machine Platform\"",
        "`hypervisorlaunchtype Off`",
        "`bcdedit /set hypervisorlaunchtype auto`",
        "The Android version in use is kept",
        "restart the computer",
    ] {
        assert!(english.contains(words), "{words:?} in {english}");
    }
    let reason = format!("{}\nconsole tail", host_problem(HYPER_V_NOT_INSTALLED));
    for lang in UiLanguage::ALL {
        let text = virtualization_off_text(lang);
        // The three causes in every language: the firmware's virtualization, the Windows
        // feature, the boot configuration.
        for words in [
            "VT-x",
            "AMD-V",
            "UEFI/BIOS",
            "Windows",
            "hypervisorlaunchtype Off",
            "1.",
            "2.",
            "3.",
        ] {
            assert!(text.contains(words), "{lang:?}: {words:?} in {text}");
        }
        assert!(text.starts_with(virtualization_off_title(lang)), "{lang:?}");
        assert!(
            !text.contains('\n'),
            "{lang:?}: one line, for the status line"
        );
        assert!(!text.contains('\u{2014}'), "{lang:?}: no em dash");
        if lang != UiLanguage::English {
            assert_ne!(text, english, "{lang:?} is translated");
        }
        // Every surface that shows a status's reason says it this way, not the raw error.
        assert_eq!(localized_host_problem(lang, &reason), Some(text.clone()));
        let notice = virtualization_notification(lang);
        assert_eq!(notice.title, virtualization_off_title(lang));
        assert!(text.ends_with(&notice.text), "{lang:?}");
        assert!(
            notice.key.starts_with("dsa:recovery:"),
            "shown as DSA's own"
        );
    }
}

#[test]
fn fr14_the_desktop_is_told_virtualization_is_off_at_first_look_and_at_each_new_stop() {
    use dsa_host_core::session::SessionState;
    use dsa_host_core::start_failure::VirtualizationNotices;
    let off = host_problem(HYPER_V_NOT_INSTALLED);
    let mut notices = VirtualizationNotices::new();
    // A window manager that starts after the service found it: told at once.
    assert!(notices.observe(SessionState::Stopped, Some(&off)));
    // Once only while nothing changes.
    assert!(!notices.observe(SessionState::Stopped, Some(&off)));
    // The user opens an app; the start is refused again: told again.
    assert!(!notices.observe(SessionState::Starting, Some(&off)));
    assert!(notices.observe(SessionState::Stopped, Some(&off)));
    // Any other stop is not this notice.
    assert!(!notices.observe(SessionState::Stopped, Some("stopped on request")));
    assert!(!notices.observe(SessionState::Stopped, None));
    let mut fresh = VirtualizationNotices::new();
    assert!(!fresh.observe(SessionState::Stopped, Some("stopped on request")));
}
