//! A user's Android's memory from end to end: the guest's agent reports it on the control
//! channel, a real session keeps it, and `dsa memory` reads it through the local API with
//! the host processes beside it, all over the in-memory transports.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{HostApiClient, HostApiService, PerUser, UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::RunningService;
use dsa_host_core::memory::{
    held_line, snapshot_from_wire, snapshot_to_wire, summary_lines, GooglePlayMemory, GuestMemory,
    GuestMemoryReport, HeldMemory, HostMemory, HostProcessMemory, HostRole, IdleTrim,
    MemorySnapshot, ProcessMemory,
};

fn pressure(total_mb: u64, available_mb: u64, stall: f64) -> memory_pressure::GuestPressure {
    memory_pressure::GuestPressure {
        mem_total_kb: total_mb * MIB,
        mem_available_kb: available_mb * MIB,
        stall_percent: stall,
    }
}

mod memory_pressure {
    pub use dsa_host_core::memory::GuestPressure;
}

#[test]
fn nfr2_the_vm_grows_when_the_guest_runs_short_up_to_its_maximum() {
    use dsa_host_core::memory::{GrowthAction, MemoryGrowth};
    let start = Instant::now();
    let mut growth = MemoryGrowth::new(3072, 6144);
    // Plenty free: nothing to do.
    assert_eq!(growth.on_pressure(&pressure(2900, 1500, 0.0), start), None);
    // Short: grows to what it uses (2600 MiB) plus a third again, in whole 128 MiB blocks.
    assert_eq!(
        growth.on_pressure(&pressure(2900, 300, 0.0), start),
        Some(GrowthAction::Resize(3584))
    );
    growth.applied();
    // The guest sees it; still short a while later: grows on, to the maximum and no further.
    let mut later = start;
    for (total, grown) in [(3400, 4224), (4100, 5120), (4900, 6144)] {
        later += Duration::from_secs(6);
        assert_eq!(
            growth.on_pressure(&pressure(total, 300, 20.0), later),
            Some(GrowthAction::Resize(grown))
        );
        growth.applied();
    }
    let later = later + Duration::from_secs(6);
    assert_eq!(growth.on_pressure(&pressure(5900, 300, 20.0), later), None);
    assert_eq!(growth.current_mb(), 6144);
}

#[test]
fn nfr2_the_vm_grows_only_as_far_as_the_guest_needs_since_added_memory_keeps_its_page_records() {
    use dsa_host_core::memory::{GrowthAction, MemoryGrowth};
    let start = Instant::now();
    let mut growth = MemoryGrowth::new(2048, 29042);
    // As on the owner's PC: Android had grown to 2560 MiB (2483 MiB after the kernel's own)
    // and ran short with 509 MiB available. Half the size again took it to 3840 MiB; what it
    // uses plus 768 MiB of headroom, and at least 512 MiB more, is 3072 MiB.
    assert_eq!(
        growth.on_pressure(&pressure(1950, 450, 12.0), start),
        Some(GrowthAction::Resize(2560))
    );
    growth.applied();
    let t = start + Duration::from_secs(6);
    assert_eq!(
        growth.on_pressure(&pressure(2483, 509, 2.7), t),
        Some(GrowthAction::Resize(3072))
    );
    growth.applied();
    // A guest that uses almost all of it grows by about a third again.
    let t = t + Duration::from_secs(6);
    assert_eq!(
        growth.on_pressure(&pressure(2995, 100, 30.0), t),
        Some(GrowthAction::Resize(3968))
    );
}

#[test]
fn nfr2_the_vm_gives_memory_back_on_the_next_calm_report_but_not_below_its_base() {
    use dsa_host_core::memory::{GrowthAction, MemoryGrowth};
    let start = Instant::now();
    let mut growth = MemoryGrowth::new(3072, 16384);
    assert_eq!(
        growth.on_pressure(&pressure(2900, 100, 0.0), start),
        Some(GrowthAction::Resize(3840))
    );
    growth.applied();
    // Still settling right after the growth.
    let t = start + Duration::from_secs(2);
    assert_eq!(growth.on_pressure(&pressure(4400, 3000, 0.0), t), None);
    // The apps closed: the first calm report after the growth settled takes the memory
    // back, to what the guest uses plus headroom, here the base. No calm stretch to wait out.
    let t = start + Duration::from_secs(5);
    assert_eq!(
        growth.on_pressure(&pressure(4400, 3600, 0.0), t),
        Some(GrowthAction::Resize(3072))
    );
    let t = t + Duration::from_secs(5);
    assert_eq!(growth.on_pressure(&pressure(2900, 2000, 0.0), t), None);
}

#[test]
fn nfr2_growing_stops_when_added_memory_never_reaches_the_guest() {
    use dsa_host_core::memory::{GrowthAction, MemoryGrowth};
    let start = Instant::now();
    let mut growth = MemoryGrowth::new(3072, 16384);
    assert!(matches!(
        growth.on_pressure(&pressure(2900, 100, 0.0), start),
        Some(GrowthAction::Resize(_))
    ));
    growth.applied();
    let t = start + Duration::from_secs(31);
    assert!(matches!(
        growth.on_pressure(&pressure(2900, 100, 0.0), t),
        Some(GrowthAction::GiveUp(_))
    ));
    assert!(growth.stopped());
    assert_eq!(growth.on_pressure(&pressure(2900, 100, 0.0), t), None);
}

#[test]
fn nfr2_a_timed_out_shrink_is_retried_in_smaller_steps_until_the_vm_is_back_at_its_base() {
    use dsa_host_core::memory::{GrowthAction, MemoryGrowth};
    let start = Instant::now();
    let mut growth = MemoryGrowth::new(3072, 16384);
    assert_eq!(
        growth.on_pressure(&pressure(2900, 100, 0.0), start),
        Some(GrowthAction::Resize(3840))
    );
    growth.applied();
    // The apps closed: all 768 MiB is asked back at once, and Hyper-V times out.
    let t = start + Duration::from_secs(5);
    assert_eq!(
        growth.on_pressure(&pressure(4400, 3600, 0.0), t),
        Some(GrowthAction::Resize(3072))
    );
    growth.refused(t);
    assert_eq!(
        growth.current_mb(),
        3840,
        "the VM counts as still at its size"
    );
    assert!(!growth.stopped(), "a failure does not stop later resizes");
    assert_eq!(growth.retry_in(t), Some(Duration::from_secs(5)));
    // Nothing during the wait.
    let t = t + Duration::from_secs(2);
    assert_eq!(growth.on_pressure(&pressure(4400, 3600, 0.0), t), None);
    // Then half the step that failed (384 MiB, in whole 128 MiB blocks).
    let t = t + Duration::from_secs(3);
    assert_eq!(
        growth.on_pressure(&pressure(4100, 3300, 0.0), t),
        Some(GrowthAction::Resize(3456))
    );
    // It times out again: half again, after twice the wait.
    growth.refused(t);
    assert_eq!(growth.retry_in(t), Some(Duration::from_secs(10)));
    let t = t + Duration::from_secs(10);
    assert_eq!(
        growth.on_pressure(&pressure(4100, 3300, 0.0), t),
        Some(GrowthAction::Resize(3584))
    );
    growth.applied();
    // Each step that goes through lets the next double, down to the base.
    let t = t + Duration::from_secs(5);
    assert_eq!(
        growth.on_pressure(&pressure(3800, 3000, 0.0), t),
        Some(GrowthAction::Resize(3072))
    );
    growth.applied();
    assert_eq!(growth.current_mb(), 3072);
    // And it still grows when the guest runs short again.
    let t = t + Duration::from_secs(5);
    assert_eq!(
        growth.on_pressure(&pressure(2900, 100, 0.0), t),
        Some(GrowthAction::Resize(3840))
    );
}

#[test]
fn nfr2_a_refused_growth_is_retried_smaller_and_the_wait_never_passes_five_minutes() {
    use dsa_host_core::memory::{GrowthAction, MemoryGrowth};
    let start = Instant::now();
    let mut growth = MemoryGrowth::new(3072, 16384);
    let mut t = start;
    let mut asked = Vec::new();
    for _ in 0..12 {
        match growth.on_pressure(&pressure(2900, 100, 0.0), t) {
            Some(GrowthAction::Resize(mb)) => {
                asked.push(mb);
                growth.refused(t);
                assert_eq!(growth.current_mb(), 3072);
                let wait = growth.retry_in(t).expect("a wait after a failure");
                assert!(wait <= Duration::from_secs(300), "{wait:?}");
                t += wait;
            }
            other => panic!("expected another try, got {other:?}"),
        }
    }
    // Each try half the one before, down to the guest's 128 MiB blocks.
    assert_eq!(&asked[..5], &[3840, 3456, 3328, 3200, 3200]);
    assert!(asked[5..].iter().all(|&mb| mb == 3200), "{asked:?}");
    assert!(!growth.stopped());
}

#[test]
fn nfr2_the_vm_is_sized_to_the_host_less_what_the_host_keeps() {
    use dsa_host_core::memory::{automatic_vm_memory_mb, vm_memory_mb};
    // A tenth of a large host, at least 2 GiB of a small one.
    assert_eq!(automatic_vm_memory_mb(32768), 32768 - 3276);
    assert_eq!(automatic_vm_memory_mb(8192), 6144);
    // Never less than Android needs to boot.
    assert_eq!(automatic_vm_memory_mb(3072), 2048);
    // The user's limit wins, within what the host has.
    assert_eq!(vm_memory_mb(32768, 4096), 4096);
    assert_eq!(vm_memory_mb(8192, 65536), 8192);
    assert_eq!(vm_memory_mb(32768, 0), 32768 - 3276);
}

#[test]
fn nfr2_without_a_limit_the_vm_may_grow_to_half_the_host() {
    use dsa_host_core::memory::half_host_vm_memory_mb;
    // Half of any host, with no fixed ceiling.
    assert_eq!(half_host_vm_memory_mb(16384, 0), 8192);
    assert_eq!(half_host_vm_memory_mb(65536, 0), 32768);
    assert_eq!(half_host_vm_memory_mb(131072, 0), 65536);
    // Never less than Android needs to boot.
    assert_eq!(half_host_vm_memory_mb(2048, 0), 2048);
    // The user's limit wins, within what the host has.
    assert_eq!(half_host_vm_memory_mb(32768, 24576), 24576);
    assert_eq!(half_host_vm_memory_mb(8192, 65536), 8192);
}

#[test]
fn nfr1_a_give_back_logs_what_the_window_manager_still_holds() {
    let before = HeldMemory {
        working_set_kb: Some(119 * MIB),
        private_kb: Some(501 * MIB),
        gpu_local_kb: Some(300 * MIB),
        gpu_non_local_kb: None,
    };
    let after = HeldMemory {
        working_set_kb: Some(2 * MIB),
        private_kb: Some(420 * MIB),
        gpu_local_kb: Some(250 * MIB),
        gpu_non_local_kb: None,
    };
    assert_eq!(
        held_line(&before, &after),
        "working set 119 -> 2 MiB, private 501 -> 420 MiB, GPU memory 300 -> 250 MiB, \
         GPU-mapped system memory ? -> ? MiB"
    );
}
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_host_core::tasks::{Task, TaskEvent};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Role};
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::local;
use dsa_transport::memory;
use dsa_transport::{Connection, Connector};

const WAIT: Duration = Duration::from_secs(10);
const MIB: u64 = 1024;

#[derive(Clone, Default)]
struct Guest(Arc<Mutex<Option<BootSecret>>>);

impl GuestLauncher for Guest {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(secret.clone());
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
    fn vm_id(&self) -> Option<String> {
        Some("6a5f2a1e-0000-4000-8000-000000000001".into())
    }
}

/// The host side as a platform would measure it, recording what it was asked.
#[derive(Default)]
struct FakeHost {
    asked: Mutex<Vec<(String, Option<String>)>>,
}

impl HostMemory for FakeHost {
    fn processes(&self, user: &str, vm_id: Option<&str>) -> Vec<HostProcessMemory> {
        self.asked
            .lock()
            .unwrap()
            .push((user.to_string(), vm_id.map(str::to_string)));
        vec![
            host(HostRole::Vm, 4, 600),
            host(HostRole::WindowManager, 10, 120),
            host(HostRole::AppWindow, 11, 50),
            host(HostRole::AppWindow, 12, 50),
        ]
    }
}

fn host(role: HostRole, pid: u32, mib: u64) -> HostProcessMemory {
    HostProcessMemory {
        role,
        pid,
        working_set_kb: mib * MIB,
        private_kb: mib * MIB / 2,
    }
}

fn report() -> control::GuestMemory {
    control::GuestMemory {
        mem_total_kb: 4096 * MIB,
        mem_free_kb: 2048 * MIB,
        mem_available_kb: 3000 * MIB,
        buffers_kb: 10 * MIB,
        cached_kb: 900 * MIB,
        anon_pages_kb: 700 * MIB,
        shmem_kb: 20 * MIB,
        slab_reclaimable_kb: 80 * MIB,
        swap_total_kb: 2048 * MIB,
        swap_free_kb: 1948 * MIB,
        zram_original_kb: 100 * MIB,
        zram_used_kb: 30 * MIB,
        total_pss_kb: 650 * MIB,
        swap_pss_kb: 300 * MIB,
        top_processes: vec![control::ProcessMemory {
            pid: 500,
            name: "system".into(),
            pss_kb: 180 * MIB,
            swap_pss_kb: MIB,
            ..Default::default()
        }],
        google_play_processes: 2,
        google_play_pss_kb: 135 * MIB,
        google_play_swap_pss_kb: 120 * MIB,
        slab_unreclaimable_kb: 60 * MIB,
        kernel_stack_kb: 10 * MIB,
        page_tables_kb: 20 * MIB,
        vmalloc_used_kb: 30 * MIB,
        percpu_kb: 2 * MIB,
        cma_total_kb: 0,
        hugetlb_kb: 0,
        mem_online_kb: 4200 * MIB,
    }
}

struct Setup {
    session: Arc<Session>,
    agent: Box<dyn Connection>,
    api: local::MemoryLocalConnector,
    host: Arc<FakeHost>,
    _service: RunningService,
}

fn setup() -> Setup {
    let guest = Guest::default();
    let (listener, mut control) = memory::pair();
    let session = Arc::new(Session::new(
        guest.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            ..SessionConfig::default()
        },
    ));
    let (api_listener, api) = local::pair();
    let for_service = Arc::clone(&session);
    let host = Arc::new(FakeHost::default());
    let androids = Arc::new(PerUser::new(move |_: &str| {
        Ok(Arc::clone(&for_service) as Arc<dyn UserAndroid>)
    }));
    let service = HostApiService::new(androids, "test service")
        .with_host_memory(Arc::clone(&host) as Arc<dyn HostMemory>)
        .with_memory_wait(WAIT);
    let service = RunningService::start_service(Box::new(api_listener), service);
    session.start();
    let deadline = Instant::now() + WAIT;
    let secret = loop {
        if let Some(secret) = guest.0.lock().unwrap().clone() {
            break secret;
        }
        assert!(Instant::now() < deadline, "the guest was never started");
        thread::sleep(Duration::from_millis(5));
    };
    let mut agent = control.connect(WAIT).expect("the agent connects");
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        &secret,
        WAIT,
    )
    .expect("the host accepts the agent");
    // The guest has finished booting and its agent has connected, as its daemon reports.
    agent
        .send(&dsa_protocol::encode(&dsa_protocol::control::Envelope {
            body: Some(dsa_protocol::control::envelope::Body::GuestBoot(
                dsa_protocol::control::GuestBoot {
                    boot_completed: true,
                    agent_connected: true,
                },
            )),
        }))
        .expect("the host takes the report");
    assert!(session.wait_for(SessionState::Active, WAIT));
    Setup {
        session,
        agent,
        api,
        host,
        _service: service,
    }
}

fn send(agent: &mut Box<dyn Connection>, body: Body) {
    agent
        .send(&dsa_protocol::encode(&Envelope { body: Some(body) }))
        .unwrap();
}

fn wait_for_report(session: &Session) -> GuestMemoryReport {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(report) = session.guest_memory() {
            return report;
        }
        assert!(Instant::now() < deadline, "the report never arrived");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn nfr1_the_session_keeps_the_guests_last_memory_report_until_it_stops() {
    let mut setup = setup();
    assert_eq!(setup.session.guest_memory(), None);
    send(&mut setup.agent, Body::GuestMemory(report()));
    let kept = wait_for_report(&setup.session);
    assert_eq!(kept.memory, GuestMemory::from_control(report()));
    assert_eq!(kept.memory.top_processes[0].name, "system");

    setup.session.stop();
    assert!(setup.session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(setup.session.guest_memory(), None);
}

#[test]
fn nfr1_dsa_memory_asks_the_guest_and_shows_it_beside_the_host_processes() {
    let mut setup = setup();
    let api = &mut setup.api;
    let reader = thread::scope(|scope| {
        let reader = scope.spawn(|| {
            HostApiClient::connect(api, "test client", WAIT)
                .expect("connecting")
                .memory()
        });
        // The service asks the agent for a fresh report, which it answers. What else the
        // host tells a newly connected agent (whether app backups are on) comes on the same
        // channel, and is not what this test is about.
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the service never asked for a report");
            let frame = setup.agent.recv(Some(left)).unwrap();
            let asked = dsa_protocol::decode(&frame).unwrap();
            if asked.body == Some(Body::RequestGuestMemory(control::RequestGuestMemory {})) {
                break;
            }
        }
        send(&mut setup.agent, Body::GuestMemory(report()));
        reader.join().unwrap()
    });
    let snapshot = reader.expect("the memory");
    let guest = snapshot.guest.as_ref().expect("the guest's report");
    assert_eq!(guest.memory, GuestMemory::from_control(report()));
    assert_eq!(snapshot.host.len(), 4);
    assert_eq!(snapshot.host_total_kb(), 820 * MIB);
    assert_eq!(
        setup.host.asked.lock().unwrap().as_slice(),
        [(
            "test".to_string(),
            Some("6a5f2a1e-0000-4000-8000-000000000001".to_string())
        )]
        .as_slice()
    );
}

fn snapshot() -> MemorySnapshot {
    MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: GuestMemory::from_control(report()),
            age: Duration::from_secs(42),
        }),
        host: vec![
            host(HostRole::Vm, 4, 600),
            host(HostRole::WindowManager, 10, 120),
            host(HostRole::Renderer, 13, 80),
            host(HostRole::AppWindow, 11, 50),
            host(HostRole::AppWindow, 12, 50),
        ],
    }
}

#[test]
fn nfr1_the_log_line_sums_the_host_and_says_where_the_guests_memory_goes() {
    assert_eq!(
        snapshot().summary_line(),
        "memory: host 900 MiB (vm 600, window manager 120, graphics renderer 80, 2 app windows \
         100); guest 2048/4096 MiB used, cache 910, apps 700, swap 100 (zram 30), processes 440 \
         MiB (350 resident, 90 compressed in zram), Google Play services 51 MiB (15 resident, 36 \
         compressed in zram), 42 s ago"
    );
    let no_guest = MemorySnapshot {
        guest: None,
        host: vec![host(HostRole::Vm, 4, 600)],
    };
    assert_eq!(
        no_guest.summary_line(),
        "memory: host 600 MiB (vm 600, window manager 0, graphics renderer 0, 0 app windows 0); \
         guest has not reported"
    );
}

#[test]
fn nfr1_the_snapshot_crosses_the_local_api_unchanged() {
    let snapshot = snapshot();
    assert_eq!(snapshot_from_wire(snapshot_to_wire(&snapshot)), snapshot);
}

#[test]
fn nfr1_the_report_dsa_memory_prints_names_every_part() {
    let text = snapshot().to_string();
    for part in [
        "Host: 900 MiB in all",
        "vm",
        "window manager",
        "graphics renderer pid 13",
        "app window",
        "Android (42 s ago): 2048 of 4096 MiB in use",
        "page cache 900 MiB",
        "swap 100 of 2048 MiB used; zram holds 100 MiB in 30 MiB",
        "processes: 440 MiB (350 resident, 90 compressed in zram) in all (300 MiB swapped out \
         before compression)",
        "Google Play services: 51 MiB (15 resident, 36 compressed in zram) in 2 processes",
        "kernel and everything else 358 MiB",
        "unreclaimable slab 60, kernel stacks 10, page tables 20, vmalloc 30, per-CPU 2",
        "zram store 30, unaccounted (drivers, GPU and DMA buffers) 206 MiB",
        "reserved by the kernel at boot 104 MiB of 4200 MiB given",
        "system",
    ] {
        assert!(text.contains(part), "{part:?} missing from:\n{text}");
    }
}

#[test]
fn nfr2_swapped_out_memory_counts_at_the_size_zram_holds_it_in() {
    // The owner's PC, an hour after boot with Google Play and no window: Google Play services'
    // processes held 81 MiB of PSS, 74.6 MiB of it in zram, which compressed 3.14 to 1.
    let guest = GuestMemory {
        zram_original_kb: 314 * MIB,
        zram_used_kb: 100 * MIB,
        swap_total_kb: 2048 * MIB,
        swap_free_kb: 1734 * MIB,
        ..GuestMemory::default()
    };
    let gms = guest.share(81 * MIB, 74_600);
    assert_eq!(gms.resident_kb, 81 * MIB - 74_600);
    assert_eq!(gms.swapped_out_kb, 74_600);
    assert_eq!(gms.compressed_kb, Some(23_758));
    assert_eq!(gms.total_kb(), 8_344 + 23_758);
    // Never the swapped-out size itself: that is what made the guest look 2.7 times bigger.
    assert!(gms.total_kb() < 81 * MIB / 2);

    // zram never holds a page in more than its size, whatever its statistics say.
    let odd = GuestMemory {
        zram_original_kb: MIB,
        zram_used_kb: 2 * MIB,
        ..GuestMemory::default()
    };
    assert_eq!(odd.compressed_kb(512), Some(512));
    // Without the size zram stores, the swap in use from /proc/meminfo stands for it.
    let from_meminfo = GuestMemory {
        zram_used_kb: 100 * MIB,
        swap_total_kb: 2048 * MIB,
        swap_free_kb: 1748 * MIB,
        ..GuestMemory::default()
    };
    assert_eq!(from_meminfo.compressed_kb(300 * MIB), Some(100 * MIB));
}

#[test]
fn nfr2_a_guest_that_cannot_measure_zram_says_so_instead_of_counting_full_size() {
    let guest = GuestMemory {
        total_pss_kb: 200 * MIB,
        swap_pss_kb: 150 * MIB,
        ..GuestMemory::default()
    };
    let apps = guest.apps();
    assert_eq!(apps.resident_kb, 50 * MIB);
    assert_eq!(apps.compressed_kb, None);
    assert_eq!(apps.total_kb(), 50 * MIB);
    // Nothing swapped out needs no ratio.
    assert_eq!(guest.share(10 * MIB, 0).compressed_kb, Some(0));
    let snapshot = MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: guest,
            age: Duration::from_secs(1),
        }),
        host: Vec::new(),
    };
    let text = snapshot.to_string();
    assert!(
        text.contains("processes: 50 MiB (50 resident, ? compressed in zram)"),
        "{text}"
    );
    assert!(text.contains("Google Play services: not running"), "{text}");
}

#[test]
fn nfr2_a_swap_total_larger_than_the_pss_does_not_make_every_total_zero_resident() {
    // The owner's PC on v0.2.2: the agent summed each process's count of every page it had
    // swapped out, 781 MiB against 577 MiB of PSS, and `dsa memory` printed "0 resident" for
    // every total while the Play Store alone had 38 MiB resident.
    let process = |pid, name: &str, pss_kb, swap_pss_kb, google_play| ProcessMemory {
        pid,
        name: name.into(),
        pss_kb,
        swap_pss_kb,
        google_play,
    };
    let guest = GuestMemory {
        zram_original_kb: 432 * MIB,
        zram_used_kb: 146 * MIB,
        total_pss_kb: 577_420,
        swap_pss_kb: 781_252,
        top_processes: vec![
            process(5225, "com.android.vending", 169_087, 129_844, true),
            process(806, "system", 123_747, 96_488, false),
            process(3712, "com.google.android.gms", 109_468, 93_932, true),
            process(1347, "android.ext.services", 35_395, 61_008, false),
        ],
        google_play: Some(GooglePlayMemory {
            processes: 2,
            pss_kb: 278_555,
            swap_pss_kb: 322_556,
        }),
        ..GuestMemory::default()
    };
    let apps = guest.apps();
    assert_eq!(apps.swapped_out_kb, 129_844 + 96_488 + 93_932 + 35_395);
    assert_eq!(apps.resident_kb, 577_420 - apps.swapped_out_kb);
    assert!(apps.resident_kb > 0);
    let play = guest.google_play_share().unwrap();
    assert_eq!(play.swapped_out_kb, 129_844 + 93_932);
    assert_eq!(play.resident_kb, 278_555 - 129_844 - 93_932);
    // A consistent total is taken as the agent sends it.
    let consistent = GuestMemory {
        swap_pss_kb: 300_000,
        ..guest.clone()
    };
    assert_eq!(consistent.apps().swapped_out_kb, 300_000);
    let snapshot = MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: guest,
            age: Duration::from_secs(1),
        }),
        host: Vec::new(),
    };
    let text = snapshot.to_string();
    // The totals read as resident memory. One process can still be wholly swapped out: the
    // agent counted 61 MiB swapped of android.ext.services' 35 MiB, so its own row says
    // "0 resident", and rightly.
    for total in ["processes: ", "Google Play services: "] {
        let line = text
            .lines()
            .find(|line| line.trim_start().starts_with(total))
            .unwrap_or_else(|| panic!("no {total:?} line in {text}"));
        assert!(!line.contains("(0 resident"), "{text}");
    }
    assert!(text.contains("(38 resident, 43 compressed)"), "{text}");
}

#[test]
fn nfr2_google_play_services_has_a_line_of_its_own_that_crosses_the_local_api() {
    let snapshot = snapshot();
    let guest = &snapshot.guest.as_ref().unwrap().memory;
    let play = guest.google_play_share().unwrap();
    assert_eq!(play.resident_kb, 15 * MIB);
    assert_eq!(play.compressed_kb, Some(36 * MIB));
    let wire = snapshot_to_wire(&snapshot);
    let wired = wire.guest.as_ref().unwrap();
    assert_eq!(wired.google_play_processes, 2);
    assert_eq!(wired.google_play_pss_kb, 135 * MIB);
    assert_eq!(wired.google_play_swap_pss_kb, 120 * MIB);
    assert_eq!(wired.swap_pss_kb, 300 * MIB);
    assert_eq!(snapshot_from_wire(wire), snapshot);
    // A process of Google Play services among the largest is marked.
    let mut marked = report();
    marked.top_processes[0].google_play = true;
    let marked = MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: GuestMemory::from_control(marked),
            age: Duration::from_secs(1),
        }),
        host: Vec::new(),
    };
    assert!(marked.to_string().contains("system [Google Play services]"));
}

/// One user's running Android, as the service's log sees it.
struct OneUser(Arc<Session>);

impl UserAndroids for OneUser {
    fn android(&self, _user: &str) -> Result<Arc<dyn UserAndroid>, String> {
        Ok(Arc::clone(&self.0) as Arc<dyn UserAndroid>)
    }
    fn running(&self) -> Vec<(String, SessionState)> {
        vec![("S-1-5-21-7".to_string(), self.0.state())]
    }
    fn stop_all(&self, _timeout: Duration) -> Vec<(String, SessionState)> {
        Vec::new()
    }
}

#[test]
fn nfr1_the_service_logs_one_memory_line_for_each_running_android() {
    let mut setup = setup();
    send(&mut setup.agent, Body::GuestMemory(report()));
    wait_for_report(&setup.session);
    let host = FakeHost::default();
    let lines = summary_lines(&OneUser(Arc::clone(&setup.session)), &host);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].starts_with("S-1-5-21-7: memory: host 820 MiB (vm 600,"),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].contains("guest 2048/4096 MiB used"),
        "{}",
        lines[0]
    );

    setup.session.stop();
    assert!(setup.session.wait_for(SessionState::Stopped, WAIT));
    assert!(summary_lines(&OneUser(Arc::clone(&setup.session)), &host).is_empty());
}

fn opened(task_id: i32) -> TaskEvent {
    TaskEvent::Updated(Task {
        task_id,
        ..Task::default()
    })
}

#[test]
fn nfr1_the_window_manager_gives_memory_back_once_every_window_is_minimized() {
    let start = Instant::now();
    let at = |s: u64| start + Duration::from_secs(s);
    let mut trim = IdleTrim::new(Duration::from_secs(30), start);
    trim.sync(&[opened(1)], at(0));
    assert!(!trim.due(at(100)));
    trim.windows_shown(false, at(100));
    assert!(!trim.due(at(110)));
    assert!(trim.due(at(130)));
    assert!(!trim.due(at(200)));
    // Shown again, then minimized again: a new idle stretch.
    trim.windows_shown(true, at(210));
    assert!(!trim.due(at(300)));
    trim.windows_shown(false, at(300));
    assert!(trim.due(at(330)));
}

#[test]
fn nfr1_a_shown_window_with_a_still_screen_gets_the_gpu_driver_trim_once_per_quiet_stretch() {
    use dsa_host_core::memory::QUIET_TRIM_DELAY;
    let start = Instant::now();
    let mut trim = IdleTrim::new(Duration::from_secs(30), start);
    // No window shown: the idle trim owns that case, not this one.
    assert!(!trim.quiet_due(QUIET_TRIM_DELAY));
    trim.sync(&[opened(1)], start);
    // Frames are arriving: nothing.
    assert!(!trim.quiet_due(Duration::from_secs(1)));
    // A still screen: once.
    assert!(trim.quiet_due(QUIET_TRIM_DELAY));
    assert!(!trim.quiet_due(QUIET_TRIM_DELAY * 2));
    // A frame starts a new quiet stretch.
    assert!(!trim.quiet_due(Duration::ZERO));
    assert!(trim.quiet_due(QUIET_TRIM_DELAY));
    // Minimized: not this trim's.
    trim.windows_shown(false, start);
    assert!(!trim.quiet_due(QUIET_TRIM_DELAY * 3));
}

#[test]
fn nfr2_the_window_manager_gives_memory_back_once_no_window_has_been_open_a_while() {
    let start = Instant::now();
    let delay = Duration::from_secs(30);
    let at = |s: u64| start + Duration::from_secs(s);
    let mut trim = IdleTrim::new(delay, start);
    // Idle from start-up: once, after the delay.
    assert!(!trim.due(at(10)));
    assert!(trim.due(at(30)));
    assert!(!trim.due(at(60)));

    trim.sync(&[opened(1), opened(2)], at(70));
    assert!(!trim.due(at(200)));
    trim.task_event(Some(&TaskEvent::Removed { task_id: 1 }), at(210));
    assert!(!trim.due(at(300)), "a window is still open");
    trim.task_event(Some(&TaskEvent::Removed { task_id: 2 }), at(310));
    assert!(!trim.due(at(320)));
    assert!(trim.due(at(340)));
    assert!(!trim.due(at(400)));

    // The service out of reach counts as no window.
    trim.task_event(Some(&opened(3)), at(410));
    trim.task_event(None, at(420));
    assert!(trim.due(at(450)));
}

#[test]
fn nfr2_memory_the_kernel_reserved_at_boot_is_not_counted_as_free_memory_held() {
    // The guest uses 2048 MiB of its 4096 MiB total, and reserved 104 MiB of the 4200 MiB it
    // was given at boot: 2152 MiB the host cannot take back while the VM runs.
    let mut snapshot = snapshot();
    snapshot.host[0].working_set_kb = 2160 * MIB;
    assert_eq!(snapshot.vm_beyond_guest_kb(), Some(8 * MIB));
    // A VM that has returned every free page holds nothing beyond.
    snapshot.host[0].working_set_kb = 2152 * MIB;
    assert_eq!(snapshot.vm_beyond_guest_kb(), Some(0));
    // A guest that could not count what it was given still reports what it uses.
    let mut uncounted = report();
    uncounted.mem_online_kb = 0;
    snapshot.guest = Some(GuestMemoryReport {
        memory: GuestMemory::from_control(uncounted),
        age: Duration::ZERO,
    });
    assert_eq!(snapshot.vm_beyond_guest_kb(), Some(104 * MIB));
}

#[test]
fn nfr2_memory_given_is_the_vms_size_when_the_host_set_it_below_the_online_blocks() {
    // Hot-added and since ballooned back: the blocks stay online, the VM is smaller.
    // As on the owner's PC: grown to 3840 MiB, shrunk back to 2560 MiB, 2483 MiB in MemTotal.
    let mut grown = report();
    grown.mem_online_kb = 3840 * MIB;
    grown.mem_total_kb = 2483 * MIB;
    let memory = GuestMemory::from_control(grown);
    assert_eq!(memory.reserved_at_boot_kb(), Some(1357 * MIB));
    let given = memory.given(Some(2560));
    assert_eq!(given.kernel.mem_online_kb, 2560 * MIB);
    assert_eq!(given.reserved_at_boot_kb(), Some(77 * MIB));
    // Without a size from the host, or with more online than the VM was set to (memory
    // hot-added but not yet onlined), the guest's own count stands.
    assert_eq!(memory.given(None), memory);
    assert_eq!(memory.given(Some(8192)), memory);
}

#[test]
fn nfr2_the_window_manager_gives_memory_back_again_while_no_window_is_shown() {
    use dsa_host_core::memory::{IDLE_TRIM_DELAY, IDLE_TRIM_REPEAT};
    let start = Instant::now();
    let mut trim = IdleTrim::new(IDLE_TRIM_DELAY, start);
    assert!(trim.due(start));
    // What the idle window manager touches meanwhile goes back on the next round, not hours
    // later.
    assert!(!trim.due(start + IDLE_TRIM_REPEAT - Duration::from_secs(1)));
    assert!(trim.due(start + IDLE_TRIM_REPEAT));
    assert!(!trim.due(start + IDLE_TRIM_REPEAT + Duration::from_secs(1)));
    assert!(trim.due(start + IDLE_TRIM_REPEAT * 2));
    // A shown window ends the rounds; closing it starts a new stretch at once.
    let shown = start + IDLE_TRIM_REPEAT * 2 + Duration::from_secs(10);
    trim.sync(&[opened(1)], shown);
    assert!(!trim.due(shown + IDLE_TRIM_REPEAT * 3));
    let closed = shown + IDLE_TRIM_REPEAT * 3;
    trim.task_event(Some(&TaskEvent::Removed { task_id: 1 }), closed);
    assert!(trim.due(closed));
    assert!(IDLE_TRIM_REPEAT <= Duration::from_secs(5 * 60));
}

#[test]
fn nfr2_the_window_manager_gives_memory_back_as_soon_as_no_window_is_open() {
    use dsa_host_core::memory::IDLE_TRIM_DELAY;
    let start = Instant::now();
    let mut trim = IdleTrim::new(IDLE_TRIM_DELAY, start);
    trim.sync(&[opened(1)], start);
    assert!(!trim.due(start));
    trim.task_event(Some(&TaskEvent::Removed { task_id: 1 }), start);
    // The trim's next check, a second later at most, gives it back.
    assert!(trim.due(start));
}

#[test]
fn nfr2_the_window_manager_gives_back_what_the_last_video_left_while_windows_stay_shown() {
    use dsa_host_core::memory::{IDLE_TRIM_DELAY, LET_GO_TRIM_DELAY};
    let start = Instant::now();
    let mut trim = IdleTrim::new(IDLE_TRIM_DELAY, start);
    trim.sync(&[opened(1), opened(2), opened(3)], start);
    // The windows animate (no quiet trim) while a video plays and then closes.
    assert!(!trim.spare_due(start));
    let closed = start + Duration::from_secs(60);
    trim.let_go(closed);
    assert!(!trim.spare_due(closed + LET_GO_TRIM_DELAY - Duration::from_secs(1)));
    assert!(trim.spare_due(closed + LET_GO_TRIM_DELAY));
    // Once only for each let-go.
    assert!(!trim.spare_due(closed + LET_GO_TRIM_DELAY + Duration::from_secs(1)));
    assert!(LET_GO_TRIM_DELAY <= Duration::from_secs(10));
}

#[test]
fn nfr2_shown_windows_that_keep_drawing_still_get_a_give_back_every_round() {
    use dsa_host_core::memory::{IDLE_TRIM_DELAY, IDLE_TRIM_REPEAT};
    let start = Instant::now();
    let mut trim = IdleTrim::new(IDLE_TRIM_DELAY, start);
    trim.sync(&[opened(1)], start);
    assert!(!trim.spare_due(start));
    assert!(!trim.spare_due(start + IDLE_TRIM_REPEAT - Duration::from_secs(1)));
    assert!(trim.spare_due(start + IDLE_TRIM_REPEAT));
    assert!(!trim.spare_due(start + IDLE_TRIM_REPEAT + Duration::from_secs(1)));
    assert!(trim.spare_due(start + IDLE_TRIM_REPEAT * 2));
}

#[test]
fn nfr2_with_no_window_shown_the_idle_give_back_takes_what_a_video_let_go() {
    use dsa_host_core::memory::{IDLE_TRIM_DELAY, LET_GO_TRIM_DELAY};
    let start = Instant::now();
    let mut trim = IdleTrim::new(IDLE_TRIM_DELAY, start);
    // Before the idle give-back has run, it takes it: no second give-back.
    trim.let_go(start);
    assert!(!trim.spare_due(start + LET_GO_TRIM_DELAY));
    assert!(trim.due(start + LET_GO_TRIM_DELAY));
    // After it ran, the next one is minutes away: the let-go gets its own.
    let later = start + Duration::from_secs(30);
    trim.let_go(later);
    assert!(trim.spare_due(later + LET_GO_TRIM_DELAY));
    assert!(!trim.spare_due(later + LET_GO_TRIM_DELAY * 2));
}

#[test]
fn nfr2_a_still_screen_gets_the_gpu_driver_trim_within_seconds() {
    use dsa_host_core::memory::QUIET_TRIM_DELAY;
    assert!(QUIET_TRIM_DELAY <= Duration::from_secs(5));
}

#[test]
fn nfr2_the_window_managers_committed_memory_is_broken_down_by_owner() {
    use dsa_host_core::memory::{committed_line, CommittedMemory};
    let committed = CommittedMemory {
        private_kb: Some(453 * MIB),
        walked_private_kb: 450 * MIB,
        process_heap_kb: 20 * MIB,
        other_heaps_kb: 30 * MIB,
        other_heaps: 4,
        gpu_mapped_kb: 100 * MIB,
        direct_kb: 300 * MIB,
        largest_private_kb: vec![64 * MIB, 3 * MIB / 2],
        own_live_kb: Some(12 * MIB),
        gpu_local_kb: Some(220 * MIB),
        gpu_non_local_kb: None,
    };
    assert_eq!(
        committed_line(&committed),
        "committed memory by owner: private 453 MiB (450 found); default heap 20 (the window \
         manager's own allocations 12); 4 other heaps 30; GPU-mapped 100; allocated directly 300 \
         (largest allocations 64.0, 1.5 MiB); GPU memory 220 MiB, GPU-mapped system memory ? MiB"
    );
    let unknown = CommittedMemory::default();
    assert!(committed_line(&unknown).contains("(largest allocations none MiB)"));
}

#[test]
fn nfr5_the_renderers_memory_is_logged_under_its_own_process() {
    use dsa_host_core::memory::{HeldMemory, RendererMemory};
    use dsa_host_core::renderer::{renderer_line, RendererProcessMemory};
    let memory = RendererProcessMemory {
        process_id: 4242,
        held: HeldMemory {
            working_set_kb: Some(310 * MIB),
            private_kb: Some(290 * MIB),
            gpu_local_kb: None,
            gpu_non_local_kb: None,
        },
        counted: Some(RendererMemory {
            device_local_kb: 200 * MIB,
            device_local_count: 40,
            host_visible_kb: 96 * MIB,
            host_visible_count: 6,
            imported_kb: 0,
            imported_count: 0,
            largest_kb: 32 * MIB,
            guest_processes: 7,
        }),
    };
    assert_eq!(
        renderer_line(Some(&memory)),
        "graphics renderer (dsa-gfx, pid 4242): working set 310 MiB, private 290 MiB; GPU-only \
         200 MiB in 40, CPU-mapped 96 MiB in 6, imported 0 MiB in 0, largest 32 MiB, 7 guest \
         processes connected"
    );
    // The report the renderer sends carries all of it to the window manager.
    let carried = RendererProcessMemory::from_report(&memory.to_report(9));
    assert_eq!(carried, memory);
    // A renderer that cannot count says so rather than showing zeros, and one that does not
    // answer says that.
    let uncounted = RendererProcessMemory {
        counted: None,
        ..memory
    };
    assert!(renderer_line(Some(&uncounted)).contains("its GPU allocations: no count"));
    assert!(renderer_line(None).contains("not running or not answering"));
}
