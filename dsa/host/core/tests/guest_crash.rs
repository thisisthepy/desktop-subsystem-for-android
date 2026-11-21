//! A crashed guest, as every host finds it on the console and tells it.

use dsa_host_core::guest_crash::{
    crash_reason, crashed, localized_crash, notification, summary, ConsoleEvent, ConsoleWatch,
    CrashNotices, NOTICE_KEY,
};
use dsa_host_core::session::SessionState;
use dsa_host_core::ui_language::UiLanguage;

/// A kernel panic in binder, then the fresh boot the VM reset into, as the macOS guest's
/// console showed it with `panic=-1`.
const RESET_CONSOLE: &str = "\
[    0.000000] Booting Linux on physical CPU 0x0000000000 [0x610f0000]
[    8.137707] init: starting service 'dsa-channeld'
[  212.768219] BUG: Bad rss-counter state mm:00000000b1d4cef6 type:MM_ANONPAGES val:19 Comm:kworker/0:4 Pid:3265
[  229.330312] Internal error: Oops: 0000000096000006 [#1]  SMP
[  229.331224] pstate: 81400005 (Nzcv daif +PAN -UAO -TCO +DIT -SSBS BTYPE=--)
[  229.331505] pc : rb_insert_color+0x10/0x140
[  229.332915] lr : binder_inc_ref_for_node+0x1f0/0x528
[  229.333755] Call trace:
[  229.333767]  rb_insert_color+0x10/0x140 (P)
[  229.333800]  binder_transaction+0x11a8/0x2b70
[  229.333828]  binder_thread_write+0xd60/0x1c18
[  229.334569] ---[ end trace 0000000000000000 ]---
[  229.334583] Kernel panic - not syncing: Oops: Fatal exception
[  229.334615] SMP: stopping secondary CPUs
[  229.335034] Memory Limit: none
[    0.142206] random: crng init done
[    0.142318] [drm] Initialized vgem 1.0.0 for vgem on minor 0
";

#[test]
fn nfr5_a_kernel_panic_is_found_on_the_console_as_it_grows() {
    let mut watch = ConsoleWatch::new();
    let (before, after) = RESET_CONSOLE.split_at(RESET_CONSOLE.find("Kernel panic").unwrap() + 5);
    assert_eq!(watch.feed(before), None, "half a line is not read yet");
    assert_eq!(watch.feed(after), Some(ConsoleEvent::KernelPanic));
    // What comes later only finishes the report.
    assert_eq!(watch.feed("[    1.0] more\n"), None);
    let report = watch.report().unwrap();
    assert!(report.contains("Call trace:"), "{report}");
    assert!(report.contains("Memory Limit: none"), "{report}");
    assert!(
        !report.contains("crng init done"),
        "the next boot is not in it: {report}"
    );
    assert!(summary(&report)
        .unwrap()
        .starts_with("kernel panic in binder_transaction"));
}

#[test]
fn nfr5_a_reset_in_place_without_a_panic_is_found_by_the_new_boot() {
    let mut watch = ConsoleWatch::new();
    let console = "\
[    0.000000] Booting Linux on physical CPU 0x0
[  120.000000] init: something
[  121.500000] binder: last words
[    0.000000] Booting Linux on physical CPU 0x0
";
    assert_eq!(watch.feed(console), Some(ConsoleEvent::Reset));
    assert!(watch.report().unwrap().contains("last words"));
}

#[test]
fn nfr5_the_first_boot_is_not_taken_for_a_reset() {
    let mut watch = ConsoleWatch::new();
    let console = "\
[    0.000000] Booting Linux on physical CPU 0x0
[    0.000000] Linux version 6.18.54.1-dsa
[    0.100000] cma: reserved
[    0.050000] out of order early line
[    9.000000] init: starting service 'dsa-channeld'
";
    assert_eq!(watch.feed(console), None);
    assert_eq!(watch.event(), None);
    assert_eq!(watch.report(), None);
}

#[test]
fn nfr5_the_crash_is_summed_up_by_the_caller_of_the_faulting_routine() {
    let why = summary(RESET_CONSOLE).unwrap();
    assert!(
        why.starts_with("kernel panic in binder_transaction"),
        "{why}"
    );
    assert!(
        why.contains("; before it: BUG: Bad rss-counter state"),
        "{why}"
    );
    assert_eq!(summary("[ 1.0] init: all is well\n"), None);
    assert_eq!(
        summary("[ 3.0] BUG: Bad page state in process x"),
        Some("kernel bug: BUG: Bad page state in process x".into())
    );
}

#[test]
fn nfr5_a_crash_is_found_in_the_status_and_said_in_every_language() {
    let reason = format!("{}\nthe guest's console ...", crashed("kernel panic in x"));
    assert_eq!(crash_reason(&reason), Some("kernel panic in x"));
    assert_eq!(crash_reason("the control channel was lost: closed"), None);
    for lang in UiLanguage::ALL {
        let text = localized_crash(lang, &reason).unwrap();
        assert!(text.ends_with("kernel panic in x"), "{lang:?}: {text}");
        assert!(!text.contains("{why}"), "{lang:?}: {text}");
        let notice = notification("kernel panic in x", lang);
        assert_eq!(notice.key, NOTICE_KEY);
        assert!(notice.text.ends_with("kernel panic in x"), "{lang:?}");
        if lang != UiLanguage::English {
            assert_ne!(
                notice.title,
                notification("x", UiLanguage::English).title,
                "{lang:?} is translated"
            );
        }
    }
    assert_eq!(
        notification("x", UiLanguage::English).title,
        "Android stopped unexpectedly"
    );
}

#[test]
fn nfr5_a_crash_is_told_once_when_the_session_stops_for_it() {
    let crash = crashed("kernel panic in binder_transaction");
    let mut notices = CrashNotices::new();
    // A stop already there when the window manager starts is not news.
    assert_eq!(notices.observe(SessionState::Stopped, Some(&crash)), None);
    assert_eq!(notices.observe(SessionState::Active, None), None);
    assert_eq!(
        notices
            .observe(SessionState::Stopped, Some(&crash))
            .as_deref(),
        Some("kernel panic in binder_transaction")
    );
    assert_eq!(notices.observe(SessionState::Stopped, Some(&crash)), None);
    // A stop on request is not a crash.
    assert_eq!(notices.observe(SessionState::Active, None), None);
    assert_eq!(
        notices.observe(SessionState::Stopped, Some("stopped on request")),
        None
    );
}
