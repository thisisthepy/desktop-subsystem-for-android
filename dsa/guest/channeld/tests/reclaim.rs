//! When the daemon asks init to drop the guest's caches so free page reporting can hand the
//! memory back to the host.

use std::sync::mpsc;
use std::time::Duration;

use dsa_guest_channeld::reclaim::{MemInfo, Reason, ReclaimPolicy, Reclaimer, RECLAIM_PROPERTY};
use dsa_guest_channeld::router::{Peer, Router};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{Envelope, HostMemory, ScreenState, TaskRemoved, TaskUpdated};

const MIB: u64 = 1024;

/// A guest of 4 GiB with `free` MiB free and `cached` MiB of page cache.
fn memory(free: u64, cached: u64) -> MemInfo {
    MemInfo {
        mem_total_kb: 4096 * MIB,
        mem_free_kb: free * MIB,
        buffers_kb: 0,
        cached_kb: cached * MIB,
        shmem_kb: 0,
        slab_reclaimable_kb: 0,
        anon_pages_kb: 0,
    }
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

#[test]
fn nfr2_meminfo_is_read_in_kib_and_shared_memory_is_not_droppable() {
    let info = MemInfo::parse(
        "MemTotal:        4015356 kB\nMemFree:          812444 kB\nBuffers:           10240 kB\n\
         Cached:          1503112 kB\nShmem:             20480 kB\nSReclaimable:      80000 kB\n\
         AnonPages:        700000 kB\n\
         HugePages_Total:       0\nnonsense\n",
    );
    assert_eq!(info.mem_total_kb, 4015356);
    assert_eq!(info.mem_free_kb, 812444);
    assert_eq!(info.droppable_kb(), 1503112 - 20480 + 10240 + 80000);
    assert_eq!(info.used_kb(), 4015356 - 812444);
    assert_eq!(info.anon_pages_kb, 700000);
    assert_eq!(info.reclaimable_kb(), info.droppable_kb() + 700000);
}

/// An idle guest whose cache is gone, holding `anon` MiB of anonymous memory.
fn anon_only(anon: u64) -> MemInfo {
    MemInfo {
        anon_pages_kb: anon * MIB,
        ..memory(3000, 0)
    }
}

#[test]
fn nfr1_minimized_tasks_do_not_hold_the_idle_reclaim_back() {
    let (to_host, _host) = mpsc::channel();
    let router = Router::new(to_host, |_| Err("no agent here".to_string()));
    router
        .from_guest(
            Peer(10),
            frame(Body::TaskUpdated(TaskUpdated {
                task_id: 1,
                package_name: "org.example.app".to_string(),
                ..Default::default()
            })),
        )
        .unwrap();
    assert_eq!(router.tasks_in_use(), 1);
    router.from_host(&frame(Body::ScreenState(ScreenState { on: false })));
    assert_eq!(router.tasks_in_use(), 0);
    assert_eq!(router.task_count(), 1);
    router.from_host(&frame(Body::ScreenState(ScreenState { on: true })));
    assert_eq!(router.tasks_in_use(), 1);
    // A host that connects again takes the screen to be on, and says so if it is not.
    router.from_host(&frame(Body::ScreenState(ScreenState { on: false })));
    router.host_connected();
    assert_eq!(router.tasks_in_use(), 1);
}

#[test]
fn nfr2_init_is_asked_through_a_vendor_property() {
    // Only vendor properties may be given a type in the image's vendor policy.
    assert!(RECLAIM_PROPERTY.starts_with("vendor."));
}

fn frame(body: Body) -> Vec<u8> {
    dsa_protocol::encode(&Envelope { body: Some(body) })
}

#[test]
fn nfr2_the_router_counts_the_open_tasks_the_reclaim_waits_for() {
    let (to_host, _host) = mpsc::channel();
    let router = Router::new(to_host, |_| Err("no agent here".to_string()));
    let agent = Peer(10);
    assert_eq!(router.task_count(), 0);
    for task_id in [1, 2] {
        router
            .from_guest(
                agent,
                frame(Body::TaskUpdated(TaskUpdated {
                    task_id,
                    package_name: format!("org.example.app{task_id}"),
                    ..Default::default()
                })),
            )
            .unwrap();
    }
    assert_eq!(router.task_count(), 2);
    router
        .from_guest(agent, frame(Body::TaskRemoved(TaskRemoved { task_id: 1 })))
        .unwrap();
    assert_eq!(router.task_count(), 1);
}

#[test]
fn nfr2_memory_pressure_is_read_from_the_some_line_and_unreadable_counts_as_busy() {
    use dsa_guest_channeld::reclaim::memory_pressure;
    let calm = "some avg10=0.50 avg60=0.20 avg300=0.10 total=1234\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
    let busy = "some avg10=7.25 avg60=1.00 avg300=0.10 total=1234\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
    assert!(!memory_pressure(calm, 5.0));
    assert!(memory_pressure(busy, 5.0));
    assert!(memory_pressure("", 5.0));
}

#[test]
fn nfr2_a_host_short_of_memory_gets_it_back_with_tasks_open_and_again_while_short() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    let cache = memory(2000, 600);
    // Tasks are open and cache is not most of memory: nothing is due on the guest's account.
    assert_eq!(reclaimer.decide_under(secs(0), 2, &cache, true), None);
    reclaimer.host_short(true);
    assert_eq!(
        reclaimer.decide_under(secs(30), 2, &cache, true),
        Some(Reason::HostShort)
    );
    // Not again at once, and not while nothing has come back: dropping the caches again
    // would only drop the open apps' working set, which they read straight back.
    assert_eq!(reclaimer.decide_under(secs(60), 2, &cache, true), None);
    assert_eq!(reclaimer.decide_under(secs(90), 2, &cache, true), None);
    assert_eq!(reclaimer.decide_under(secs(150), 2, &cache, true), None);
    // Once 64 MiB or more has come back since, again.
    let grown = memory(2000, 700);
    assert_eq!(
        reclaimer.decide_under(secs(180), 2, &grown, true),
        Some(Reason::HostShort)
    );
    // Once the host is no longer short, the guest keeps what it has.
    reclaimer.host_short(false);
    assert_eq!(reclaimer.decide_under(secs(300), 2, &grown, true), None);
    // A new shortage gets its reclaim at once.
    reclaimer.host_short(true);
    assert_eq!(
        reclaimer.decide_under(secs(360), 2, &grown, true),
        Some(Reason::HostShort)
    );
}

#[test]
fn nfr1_what_the_apps_read_back_right_after_a_full_reclaim_does_not_raise_the_target() {
    // The owner's PC, v0.2.3: each cache drop for a host short of memory was followed by some
    // 400 MiB read back within seconds, which raised the target toward 1 GiB.
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.refaults(0);
    reclaimer.host_short(true);
    assert_eq!(
        reclaimer.decide_under(secs(30), 2, &memory(2000, 600), true),
        Some(Reason::HostShort)
    );
    for look in 1..=6 {
        reclaimer.refaults(look * 66 * MIB);
    }
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
    // After half a minute, what comes back counts again.
    reclaimer.refaults(6 * 66 * MIB + 40 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 320 * MIB);
}

#[test]
fn nfr1_a_working_set_learnt_high_comes_down_by_half_with_each_trim_nobody_reads_back() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.refaults(0);
    // A trim of 1000 MiB came straight back: the working set was 1 GiB, the most it may be.
    assert_eq!(
        reclaimer.decide_under(secs(60), 1, &memory(1000, 1256), false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.trim_mib(), 1000);
    reclaimer.refaults(800 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 1024 * MIB);
    // The need passes: each trim no one reads back halves the way down to 256 MiB, at least
    // 64 MiB a step. A fixed 64 MiB a step would read 1024, 1024, 960, 896, 832, 768.
    let mut targets = Vec::new();
    let mut t = 60;
    for _ in 0..6 {
        for _ in 0..20 {
            reclaimer.refaults(800 * MIB);
        }
        let target = reclaimer.working_set_target_kb() / MIB;
        targets.push(target);
        t += 10;
        assert_eq!(
            reclaimer.decide_under(secs(t), 1, &memory(1000, target + 100), false),
            Some(Reason::Gradual)
        );
    }
    assert_eq!(targets, [1024, 1024, 640, 448, 352, 288]);
}

#[test]
fn nfr2_the_router_remembers_whether_the_host_is_short_of_memory() {
    let (to_host, _host) = mpsc::channel();
    let router = Router::new(to_host, |_| Err("no agent here".to_string()));
    assert!(!router.host_memory_low());
    router.from_host(&frame(Body::HostMemory(HostMemory { low: true })));
    assert!(router.host_memory_low());
    router.from_host(&frame(Body::HostMemory(HostMemory { low: false })));
    assert!(!router.host_memory_low());
    // A host that connects again says so again if it still is.
    router.from_host(&frame(Body::HostMemory(HostMemory { low: true })));
    router.host_connected();
    assert!(!router.host_memory_low());
}

#[test]
fn nfr1_a_trim_waits_until_the_open_tasks_have_settled() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    let cache = memory(2600, 600);
    assert_eq!(
        reclaimer.decide_under(secs(0), 0, &memory(3500, 100), false),
        None
    );
    // An app opens: it is reading its files, so nothing is trimmed yet.
    assert_eq!(reclaimer.decide_under(secs(10), 1, &cache, false), None);
    assert_eq!(reclaimer.decide_under(secs(30), 1, &cache, false), None);
    assert_eq!(
        reclaimer.decide_under(secs(40), 1, &cache, false),
        Some(Reason::Gradual)
    );
}

#[test]
fn nfr1_file_refaults_raise_the_working_set_target_and_hold_the_trim() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    let cache = memory(2600, 600);
    reclaimer.refaults(0);
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
    // 40 MiB of evicted cache came back: an app still uses it.
    reclaimer.refaults(40 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 320 * MIB);
    assert_eq!(reclaimer.decide_under(secs(0), 1, &cache, false), None);
    // Quiet again: the target falls back and the trim resumes.
    reclaimer.refaults(41 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
    assert_eq!(
        reclaimer.decide_under(secs(30), 1, &cache, false),
        Some(Reason::Gradual)
    );
}

#[test]
fn nfr1_quiet_refaults_let_the_target_fall_back_but_never_below_its_floor() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.refaults(0);
    for step in 1..=20 {
        reclaimer.refaults(step * 20 * MIB);
    }
    assert_eq!(reclaimer.working_set_target_kb(), 1024 * MIB);
    for _ in 0..30 {
        reclaimer.refaults(400 * MIB);
    }
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
}

#[test]
fn nfr1_a_trim_whose_cache_comes_back_slowly_is_not_taken_again() {
    // The owner's PC on v0.2.2, only the Play Store shown: each trim took about 100 MiB, it came
    // back at 8 MiB a look (under the limit of one look), and the next trim took it again every
    // 10 to 30 s.
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.refaults(0);
    assert_eq!(
        reclaimer.decide_under(secs(60), 1, &memory(2600, 356), false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.trim_mib(), 100);
    // A look's worth of it comes back: not yet a quarter.
    reclaimer.refaults(8 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
    // Three looks: 24 MiB, still under a quarter of 100.
    reclaimer.refaults(16 * MIB);
    reclaimer.refaults(24 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
    // The fourth brings back 32 MiB: the trim took cache in use, so the working set is what it
    // left plus what came back, and the cache refilled to it is not trimmed again.
    reclaimer.refaults(32 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), (256 + 32) * MIB);
    assert_eq!(
        reclaimer.decide_under(secs(80), 1, &memory(2600, 340), false),
        None
    );
    // Quiet looks do not take the target below what was learnt.
    for look in 1..=10 {
        reclaimer.refaults((32 + look) * MIB / 2 + 16 * MIB);
    }
    assert_eq!(reclaimer.working_set_target_kb(), (256 + 32) * MIB);
    // Files read once go back above it, in one trim.
    assert_eq!(
        reclaimer.decide_under(secs(140), 1, &memory(2600, 288 + 90), false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.trim_mib(), 90);
    // That trim is not read back: the next one may try below what was learnt, one step lower.
    reclaimer.refaults(40 * MIB);
    reclaimer.refaults(40 * MIB);
    assert_eq!(
        reclaimer.decide_under(secs(200), 1, &memory(2600, 288 + 70), false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.working_set_target_kb(), (256 + 32) * MIB);
    reclaimer.refaults(40 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
    // An app closing forgets what was learnt for it.
    reclaimer.refaults(40 * MIB);
    assert_eq!(
        reclaimer.decide_under(secs(260), 0, &memory(3000, 300), false),
        Some(Reason::Idle)
    );
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
}

#[test]
fn nfr1_vmstat_file_refaults_are_read_in_kib() {
    use dsa_guest_channeld::reclaim::file_refaults_kb;
    let vmstat = "nr_free_pages 1000\nworkingset_refault_anon 7\nworkingset_refault_file 2560\n";
    assert_eq!(file_refaults_kb(vmstat, 4), Some(10240));
    assert_eq!(file_refaults_kb("nr_free_pages 1\n", 4), None);
}

#[test]
fn nfr2_an_idle_guest_returns_its_memory_on_the_first_look_once_per_idle_stretch() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    let mostly_free = memory(3000, 400);
    // No waiting: the first look with no task open reclaims.
    assert_eq!(
        reclaimer.decide(secs(0), 0, &mostly_free),
        Some(Reason::Idle)
    );
    // Once per idle stretch, and the cache is no longer most of what is in use.
    assert_eq!(reclaimer.decide(secs(5), 0, &memory(3500, 200)), None);
}

#[test]
fn nfr2_opening_a_task_starts_a_new_idle_stretch() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    let small_cache = memory(2000, 300);
    assert_eq!(
        reclaimer.decide(secs(0), 0, &small_cache),
        Some(Reason::Idle)
    );
    assert_eq!(reclaimer.decide(secs(5), 2, &small_cache), None);
    // The last task closed: idle again, on the very next look.
    assert_eq!(
        reclaimer.decide(secs(10), 0, &small_cache),
        Some(Reason::Idle)
    );
}

#[test]
fn nfr2_with_tasks_open_caches_are_dropped_only_when_they_are_most_of_memory() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    // 1096 MiB in use, 300 MiB of it cache: not most of it, and under the working set
    // target plus the least trim.
    assert_eq!(reclaimer.decide(secs(0), 1, &memory(3000, 300)), None);
    // 2096 MiB in use, 1500 MiB of it cache.
    assert_eq!(
        reclaimer.decide(secs(5), 1, &memory(2000, 1500)),
        Some(Reason::MostlyCache)
    );
    // After the drop the cache is small: nothing more to do.
    assert_eq!(reclaimer.decide(secs(10), 1, &memory(3500, 280)), None);
}

#[test]
fn nfr2_a_little_cache_is_not_worth_dropping() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    let tiny = memory(4000, 64);
    assert_eq!(reclaimer.decide(secs(0), 0, &tiny), None);
    assert_eq!(reclaimer.decide(secs(120), 0, &tiny), None);
}

#[test]
fn nfr1_anonymous_memory_alone_is_worth_an_idle_reclaim_but_not_a_busy_one() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    assert_eq!(
        reclaimer.decide(secs(0), 0, &anon_only(700)),
        Some(Reason::Idle)
    );
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    assert_eq!(reclaimer.decide(secs(0), 1, &anon_only(700)), None);
}

#[test]
fn nfr2_an_idle_guest_reclaims_again_on_the_next_look_that_finds_memory_grown_back() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    assert_eq!(
        reclaimer.decide(secs(0), 0, &anon_only(900)),
        Some(Reason::Idle)
    );
    // What the reclaim left is the floor.
    assert_eq!(reclaimer.decide(secs(5), 0, &anon_only(300)), None);
    // It stayed small.
    assert_eq!(reclaimer.decide(secs(10), 0, &anon_only(350)), None);
    // Grown back: reclaimed at once, which is also how a reclaim made during boot gets its
    // second go.
    assert_eq!(
        reclaimer.decide(secs(15), 0, &anon_only(500)),
        Some(Reason::Idle)
    );
}

#[test]
fn nfr2_open_tasks_get_cold_cache_trimmed_on_every_look_it_is_found() {
    use dsa_guest_channeld::reclaim::TRIM_PROPERTY;
    assert!(TRIM_PROPERTY.starts_with("vendor."));
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    // 1.4 GiB used, 600 MiB of it cache: mostly not cache, but 344 MiB above what is kept.
    let busy = memory(2600, 600);
    assert_eq!(
        reclaimer.decide_under(secs(30), 2, &busy, false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.trim_mib(), 344);
    // Cache that came back is trimmed on the next look, with no gap to wait out.
    assert_eq!(
        reclaimer.decide_under(secs(35), 2, &busy, false),
        Some(Reason::Gradual)
    );
}

#[test]
fn nfr2_the_trim_leaves_the_cache_apps_use_and_waits_out_pressure() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    // 300 MiB of cache is under what the trim keeps plus its least slice.
    assert_eq!(
        reclaimer.decide_under(secs(30), 2, &memory(2600, 300), false),
        None
    );
    // Under memory pressure nothing is trimmed, however much cache there is.
    assert_eq!(
        reclaimer.decide_under(secs(35), 2, &memory(1500, 900), true),
        None
    );
}

#[test]
fn nfr2_cache_beyond_the_working_set_goes_back_whole_on_the_next_look() {
    use dsa_guest_channeld::reclaim::TRIM_SIZE_PROPERTY;
    assert!(TRIM_SIZE_PROPERTY.starts_with("vendor."));
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.host_short(false);
    // One app open, 1100 MiB of page cache, the host not short: all 844 MiB above the
    // working set target go back in one trim, not in slices over minutes.
    assert_eq!(
        reclaimer.decide_under(secs(0), 1, &memory(1200, 1100), false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.trim_mib(), 1100 - 256);
    // Just above the target: only the excess, in whole MiB.
    assert_eq!(
        reclaimer.decide_under(secs(5), 1, &memory(3000, 256 + 80), false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.trim_mib(), 80);
    // At the target: nothing.
    assert_eq!(
        reclaimer.decide_under(secs(10), 1, &memory(3000, 300), false),
        None
    );
}

#[test]
fn nfr2_closing_an_app_returns_the_cache_above_the_working_set_on_the_next_look() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.refaults(0);
    // Two apps open and settled; their files once came back, so the target grew.
    reclaimer.refaults(40 * MIB);
    assert_eq!(reclaimer.working_set_target_kb(), 320 * MIB);
    assert_eq!(
        reclaimer.decide_under(secs(60), 2, &memory(3000, 300), false),
        None
    );
    // One closes, leaving 900 MiB of cache: the next look trims everything above the floor
    // at once: no settling delay, no stepping the target down look by look, and the closed
    // app's refaults no longer hold the trim back.
    assert_eq!(
        reclaimer.decide_under(secs(65), 1, &memory(2000, 900), false),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.working_set_target_kb(), 256 * MIB);
    assert_eq!(reclaimer.trim_mib(), 900 - 256);
}

#[test]
fn nfr2_an_idle_guest_keeps_its_cache_at_the_working_set_between_full_reclaims() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    assert_eq!(
        reclaimer.decide_under(secs(0), 0, &memory(3000, 400), false),
        Some(Reason::Idle)
    );
    // Background work reads files again: the next look trims them.
    assert_eq!(
        reclaimer.decide_under(secs(5), 0, &memory(3000, 360), false),
        Some(Reason::Gradual)
    );
}

#[test]
fn nfr2_the_daemon_looks_at_the_guests_memory_every_few_seconds() {
    use dsa_guest_channeld::reclaim::CHECK_INTERVAL;
    assert!(CHECK_INTERVAL <= Duration::from_secs(5));
}

#[test]
fn nfr2_a_close_returns_the_closed_apps_memory_at_once_even_while_another_app_opens() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.decide_under(secs(0), 1, &memory(3000, 200), false);
    // A second app opens at 10 s, and the first closes 5 s later, well inside the 30 s the
    // new app's files are protected for.
    reclaimer.decide_under(secs(10), 2, &memory(2500, 500), false);
    // The close does not wait out that grace: the closed app's cache above the floor goes
    // back now, as page cache only, so the new app's own memory stays resident.
    assert_eq!(
        reclaimer.app_closed(secs(15), 1, &memory(2400, 900)),
        Some(Reason::Gradual)
    );
    assert_eq!(reclaimer.trim_mib(), 900 - 256);
}

#[test]
fn nfr2_a_close_with_nothing_opening_reclaims_everything_the_app_left_at_once() {
    let mut reclaimer = Reclaimer::new(ReclaimPolicy::default());
    reclaimer.decide_under(secs(0), 2, &memory(2000, 600), false);
    assert_eq!(
        reclaimer.app_closed(secs(60), 1, &memory(2600, 600)),
        Some(Reason::AppClosed)
    );
    // The last app closing too: at once, and it counts as the idle reclaim.
    assert_eq!(
        reclaimer.app_closed(secs(61), 0, &memory(3000, 400)),
        Some(Reason::AppClosed)
    );
    assert_eq!(
        reclaimer.decide_under(secs(65), 0, &memory(3500, 200), false),
        None
    );
}

#[test]
fn nfr2_the_close_reclaim_starts_when_the_apps_processes_have_exited() {
    use dsa_guest_channeld::reclaim::{package_running, EXIT_WAIT_LIMIT};
    let running = vec![
        "system_server\0".to_string(),
        "org.example.notes\0".to_string(),
        "org.example.notes:sync\0".to_string(),
    ];
    assert!(package_running(&running, "org.example.notes"));
    let gone = vec![
        "system_server\0".to_string(),
        "org.example.notes2\0".to_string(),
        String::new(),
    ];
    assert!(!package_running(&gone, "org.example.notes"));
    // A process the agent keeps running does not hold the reclaim back for long.
    assert!(EXIT_WAIT_LIMIT <= Duration::from_secs(2));
}

#[test]
fn nfr2_the_router_names_each_app_whose_last_task_ended() {
    let (to_host, _host) = mpsc::channel();
    let router = Router::new(to_host, |_| Err("no agent here".to_string()));
    let agent = Peer(10);
    for (task_id, package) in [
        (1, "org.example.a"),
        (2, "org.example.a"),
        (3, "org.example.b"),
    ] {
        router
            .from_guest(
                agent,
                frame(Body::TaskUpdated(TaskUpdated {
                    task_id,
                    package_name: package.to_string(),
                    ..Default::default()
                })),
            )
            .unwrap();
    }
    for task_id in [1, 3] {
        router
            .from_guest(agent, frame(Body::TaskRemoved(TaskRemoved { task_id })))
            .unwrap();
    }
    // org.example.a still has a task.
    assert_eq!(router.take_closed_packages(), ["org.example.b"]);
    assert!(router.take_closed_packages().is_empty());
}

#[test]
fn nfr2_a_kernel_file_the_reclaimer_cannot_read_is_reported_once() {
    use dsa_guest_channeld::reclaim::ReadFailures;
    use std::io::{Error, ErrorKind};
    fn denied() -> std::io::Result<String> {
        Err(Error::from(ErrorKind::PermissionDenied))
    }
    let mut failures = ReadFailures::default();
    let first = failures
        .note("/proc/vmstat", &denied(), "the trim cannot see refaults")
        .expect("the first failure is reported");
    assert!(first.contains("/proc/vmstat"), "{first}");
    assert!(first.contains("SELinux"), "{first}");
    for _ in 0..10 {
        assert_eq!(failures.note("/proc/vmstat", &denied(), "x"), None);
    }
    // Another file is a report of its own.
    assert!(failures
        .note("/proc/pressure/memory", &denied(), "x")
        .is_some());
    // Read once, then failing again, it is reported again.
    assert_eq!(failures.note("/proc/vmstat", &Ok(String::new()), "x"), None);
    assert!(failures.note("/proc/vmstat", &denied(), "x").is_some());
}

#[test]
fn nfr2_an_idle_reclaim_moves_half_the_anonymous_memory_to_zram_not_a_fixed_gigabyte() {
    use dsa_guest_channeld::reclaim::{reclaim_step_mib, RECLAIM_STEP_PROPERTY};
    assert!(RECLAIM_STEP_PROPERTY.starts_with("vendor."));
    // 640 MiB of app memory: four steps of 80 MiB, 320 MiB in all, the coldest first, so the
    // pages of the processes still running stay resident and are not swapped straight back.
    let memory = MemInfo {
        anon_pages_kb: 640 * MIB,
        ..MemInfo::default()
    };
    assert_eq!(reclaim_step_mib(&memory), 80);
    // A guest with almost none still gets a step init can write.
    assert_eq!(reclaim_step_mib(&MemInfo::default()), 1);
}
