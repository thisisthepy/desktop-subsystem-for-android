//! When the daemon pages an app that no window shows back out to zram after something woke it.

use std::collections::BTreeSet;
use std::sync::mpsc;
use std::time::Duration;

use dsa_guest_channeld::pageout::{
    anonymous_ranges, cpu_ticks, eligible, has_window, is_exiting, is_google_play, skip_reason,
    state, Pageout, PageoutPolicy, ProcessSample, Recheck, Status, QUIET_AFTER_WAKE,
};
use dsa_guest_channeld::router::{Peer, Router};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{Envelope, TaskRemoved, TaskUpdated};

const MIB: u64 = 1024;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

fn gms(cpu_ticks: u64, rss_anon_mib: u64) -> ProcessSample {
    ProcessSample {
        pid: 700,
        name: "com.google.android.gms".into(),
        uid: 10_123,
        oom_score_adj: 500,
        cpu_ticks,
        rss_anon_kb: rss_anon_mib * MIB,
        exiting: false,
    }
}

fn no_windows() -> BTreeSet<String> {
    BTreeSet::new()
}

#[test]
fn nfr2_a_woken_google_play_services_is_paged_out_once_it_goes_quiet() {
    let mut pageout = Pageout::new(PageoutPolicy::default());
    // First seen: whatever it did before counts as a wake, but it has to be quiet first.
    assert!(pageout
        .look(secs(0), &[gms(100, 30)], &no_windows())
        .is_empty());
    assert!(pageout
        .look(secs(4), &[gms(100, 30)], &no_windows())
        .is_empty());
    let due = pageout.look(secs(6), &[gms(100, 30)], &no_windows());
    assert_eq!(due.len(), 1);
    assert!(due[0].google_play);
    assert_eq!(due[0].grown_kb, 30 * MIB);
    pageout.paged(700, 8 * MIB);
    // Quiet and paged out: nothing more until it wakes.
    assert!(pageout
        .look(secs(60), &[gms(100, 8)], &no_windows())
        .is_empty());

    // A wake (a dumpsys, a sync) reads 40 MiB back; it is paged out a quiet stretch later,
    // not at the next global reclaim.
    assert!(pageout
        .look(secs(62), &[gms(180, 48)], &no_windows())
        .is_empty());
    assert!(pageout
        .look(secs(64), &[gms(185, 48)], &no_windows())
        .is_empty());
    let due = pageout.look(secs(70), &[gms(185, 48)], &no_windows());
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].grown_kb, 40 * MIB);
}

#[test]
fn nfr2_a_pageout_is_rate_limited_per_process_and_per_look() {
    let policy = PageoutPolicy::default();
    let mut pageout = Pageout::new(policy);
    let other = |pid: i32, rss: u64| ProcessSample {
        pid,
        name: format!("org.example.app{pid}"),
        uid: 10_200 + pid as u32,
        oom_score_adj: 900,
        cpu_ticks: 1,
        rss_anon_kb: rss * MIB,
        exiting: false,
    };
    let all = [gms(1, 30), other(800, 50), other(801, 10)];
    pageout.look(secs(0), &all, &no_windows());
    // One process per look, the most grown first.
    let first = pageout.look(secs(6), &all, &no_windows());
    assert_eq!(first.iter().map(|t| t.pid).collect::<Vec<_>>(), [800]);
    let second = pageout.look(secs(8), &all, &no_windows());
    assert_eq!(second.iter().map(|t| t.pid).collect::<Vec<_>>(), [700]);
    let third = pageout.look(secs(10), &all, &no_windows());
    assert_eq!(third.iter().map(|t| t.pid).collect::<Vec<_>>(), [801]);
    pageout.paged(800, 0);

    // It wakes again at once: the next pageout waits for min_interval after the last.
    let mut busy = [gms(1, 30), other(800, 1), other(801, 10)];
    busy[1].cpu_ticks = 2;
    busy[1].rss_anon_kb = 40 * MIB;
    pageout.look(secs(12), &busy, &no_windows());
    assert!(pageout.look(secs(20), &busy, &no_windows()).is_empty());
    let due = pageout.look(secs(6) + policy.min_interval, &busy, &no_windows());
    assert_eq!(due.iter().map(|t| t.pid).collect::<Vec<_>>(), [800]);
    // A wake that brought back less than min_growth_kb is left alone.
    let mut small = busy;
    small[1].cpu_ticks = 3;
    small[1].rss_anon_kb = 40 * MIB + 1024;
    pageout.paged(800, 40 * MIB);
    pageout.look(secs(100), &small, &no_windows());
    assert!(pageout.look(secs(200), &small, &no_windows()).is_empty());
}

#[test]
fn nfr2_the_foreground_app_and_a_process_with_a_window_are_never_paged_out() {
    let windows: BTreeSet<String> = ["org.example.notes".to_string()].into();
    let app = |name: &str, adj: i32| ProcessSample {
        pid: 900,
        name: name.into(),
        uid: 10_300,
        oom_score_adj: adj,
        cpu_ticks: 0,
        rss_anon_kb: 100 * MIB,
        exiting: false,
    };
    // A window's package, in its main process and its others.
    assert!(!eligible(&app("org.example.notes", 900), &windows));
    assert!(!eligible(&app("org.example.notes:sync", 900), &windows));
    assert!(!eligible(&app("org.example.notes.remote", 900), &windows));
    assert!(eligible(&app("org.example.notesextra", 900), &windows));
    // The foreground app and what the user sees.
    assert!(!eligible(&app("org.example.player", 0), &windows));
    assert!(!eligible(&app("org.example.player", 100), &windows));
    assert!(!eligible(&app("org.example.player", 199), &windows));
    // Perceptible or lower.
    assert!(eligible(&app("org.example.player", 200), &windows));
    assert!(eligible(&app("org.example.player", 999), &windows));
    // Google Play services from visible up, as the system binds it without anything to show.
    assert!(eligible(
        &app("com.google.android.gms.persistent", 100),
        &windows
    ));
    assert!(!eligible(
        &app("com.google.android.gms.persistent", 0),
        &windows
    ));
    assert!(!eligible(
        &app("com.google.android.gms.persistent", -800),
        &windows
    ));
    // The system's own processes are not apps.
    let mut system = app("system_server", 900);
    system.uid = 1000;
    assert!(!eligible(&system, &windows));
    let mut second_user = app("org.example.player", 900);
    second_user.uid = 1_010_300;
    assert!(eligible(&second_user, &windows));

    let mut pageout = Pageout::new(PageoutPolicy::default());
    let shown = [app("org.example.notes", 900)];
    pageout.look(secs(0), &shown, &windows);
    assert!(pageout.look(secs(60), &shown, &windows).is_empty());
    // Its window closed: now it may go.
    assert_eq!(pageout.look(secs(62), &shown, &no_windows()).len(), 1);
}

#[test]
fn nfr2_google_play_services_is_known_by_its_process_names() {
    for name in [
        "com.google.android.gms",
        "com.google.android.gms.unstable",
        "com.google.android.gms:snet",
        "com.android.vending",
        "com.android.vending:background",
        "com.google.process.gapps",
    ] {
        assert!(is_google_play(name), "{name}");
    }
    for name in [
        "com.android.settings",
        "org.example.app",
        "com.android.vendingx",
    ] {
        assert!(!is_google_play(name), "{name}");
    }
    let windows: BTreeSet<String> = ["com.google.android.gms".to_string()].into();
    assert!(has_window("com.google.android.gms.ui", &windows));
}

#[test]
fn nfr2_a_process_id_used_again_starts_over() {
    let mut pageout = Pageout::new(PageoutPolicy::default());
    pageout.look(secs(0), &[gms(1, 30)], &no_windows());
    assert_eq!(pageout.look(secs(6), &[gms(1, 30)], &no_windows()).len(), 1);
    pageout.paged(700, 0);
    let mut reused = gms(1, 30);
    reused.name = "org.example.other".into();
    reused.oom_score_adj = 900;
    assert!(pageout
        .look(secs(8), &[reused.clone()], &no_windows())
        .is_empty());
    // New to the daemon: it is quiet from here, then paged out, with no wait for the old one.
    assert_eq!(pageout.look(secs(14), &[reused], &no_windows()).len(), 1);
}

#[test]
fn nfr2_proc_files_are_read_for_cpu_time_memory_and_anonymous_ranges() {
    let stat = "700 (com.google.android.gms (x) y) S 500 500 0 0 -1 1077952832 1000 0 0 0 \
                123 45 0 0 20 0 80 0 900 0\n";
    assert_eq!(cpu_ticks(stat), Some(168));
    assert_eq!(cpu_ticks("garbage"), None);

    let status = Status::parse(
        "Name:\tgms\nUid:\t10123\t10123\t10123\t10123\nRssAnon:\t   30720 kB\nVmSwap:\t   76800 kB\n",
    );
    assert_eq!(
        status,
        Status {
            uid: 10123,
            rss_anon_kb: 30720,
            swap_kb: 76800,
            has_mm: false,
        }
    );

    let maps = "\
12c00000-32c00000 rw-p 00000000 00:00 0                                  [anon:dalvik-main space (region space)]
5600000000-5600001000 r--p 00000000 fe:00 1234                           /system/bin/app_process64
5600001000-5600002000 rw-p 00001000 fe:00 1234                           /system/bin/app_process64
7000000000-7000100000 rw-p 00000000 00:00 0                              [heap]
7000100000-7000200000 ---p 00000000 00:00 0
7000200000-7000300000 rw-p 00000000 00:00 0
7000300000-7000400000 rw-s 00000000 00:01 77                             /memfd:jit-cache (deleted)
7ffc000000-7ffc100000 rw-p 00000000 00:00 0                              [stack]
";
    assert_eq!(
        anonymous_ranges(maps),
        [
            (0x12c0_0000, 0x2000_0000),
            (0x70_0000_0000, 0x10_0000),
            (0x70_0020_0000, 0x10_0000),
        ]
    );
}

fn frame(body: Body) -> Vec<u8> {
    dsa_protocol::encode(&Envelope { body: Some(body) })
}

#[test]
fn nfr2_the_router_says_which_packages_have_a_window() {
    let (to_host, _host) = mpsc::channel();
    let router = Router::new(to_host, |_| Err("no agent here".to_string()));
    router
        .from_guest(
            Peer(10),
            frame(Body::TaskUpdated(TaskUpdated {
                task_id: 1,
                package_name: "org.example.notes".to_string(),
                ..Default::default()
            })),
        )
        .unwrap();
    assert_eq!(
        router.task_packages(),
        ["org.example.notes".to_string()].into()
    );
    router
        .from_guest(
            Peer(10),
            frame(Body::TaskRemoved(TaskRemoved { task_id: 1 })),
        )
        .unwrap();
    assert!(router.task_packages().is_empty());
}

#[test]
fn nfr2_an_exiting_process_is_never_paged_out() {
    // A zombie, or a process whose address space is gone, is being torn down by the kernel:
    // paging it out raced with that, and the kernel reported a bad rss-counter.
    let mut pageout = Pageout::new(PageoutPolicy::default());
    let exiting = ProcessSample {
        exiting: true,
        ..gms(100, 30)
    };
    assert!(!eligible(&exiting, &no_windows()));
    assert!(pageout
        .look(secs(0), std::slice::from_ref(&exiting), &no_windows())
        .is_empty());
    assert!(pageout.look(secs(30), &[exiting], &no_windows()).is_empty());

    // Read from /proc: state Z or X, or no VmSize line.
    let zombie = "700 (com.google.android.gms) Z 500 500 0 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 9 0\n";
    let dead = "700 (com.google.android.gms) X 500 500 0 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 9 0\n";
    let sleeping = "700 (com.google.android.gms) S 500 500 0 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 9 0\n";
    let alive = Status::parse("Uid:\t10123\nVmSize:\t 1000 kB\nRssAnon:\t 30720 kB\n");
    let no_mm = Status::parse("Uid:\t10123\n");
    assert_eq!(state(zombie), Some('Z'));
    assert!(is_exiting(zombie, &alive));
    assert!(is_exiting(dead, &alive));
    assert!(is_exiting(sleeping, &no_mm));
    assert!(!is_exiting(sleeping, &alive));
    assert!(alive.has_mm);
}

#[test]
fn nfr2_a_process_that_used_the_cpu_within_the_grace_period_is_not_paged_out() {
    let mut pageout = Pageout::new(PageoutPolicy::default());
    assert_eq!(PageoutPolicy::default().quiet, QUIET_AFTER_WAKE);
    assert!(pageout
        .look(secs(0), &[gms(100, 30)], &no_windows())
        .is_empty());
    // It runs again just before the grace period ends: the wait starts over.
    let woke_at = QUIET_AFTER_WAKE - secs(1);
    assert!(pageout
        .look(woke_at, &[gms(110, 30)], &no_windows())
        .is_empty());
    assert!(pageout
        .look(QUIET_AFTER_WAKE, &[gms(110, 30)], &no_windows())
        .is_empty());
    let due = pageout.look(woke_at + QUIET_AFTER_WAKE, &[gms(110, 30)], &no_windows());
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].cpu_ticks, 110);

    // Read again right before the pageout: a process that woke since the look, began to exit,
    // or came forward is left alone.
    let target = &due[0];
    let quiet = Recheck {
        oom_score_adj: 500,
        cpu_ticks: 110,
        exiting: false,
    };
    assert_eq!(skip_reason(target, &quiet), None);
    assert!(skip_reason(
        target,
        &Recheck {
            cpu_ticks: 111,
            ..quiet
        }
    )
    .unwrap()
    .contains("used the CPU"));
    assert!(skip_reason(
        target,
        &Recheck {
            exiting: true,
            ..quiet
        }
    )
    .unwrap()
    .contains("exiting"));
    assert!(skip_reason(
        target,
        &Recheck {
            oom_score_adj: 0,
            ..quiet
        }
    )
    .unwrap()
    .contains("more important"));
}
