//! The memory measurement's numbers and its comparison with the baseline, from snapshots
//! made up here; the boot check that takes real ones is in `overlays.rs`.

mod memory_check;

use std::time::Duration;

use dsa_host_core::memory::{
    GooglePlayMemory, GuestMemory, GuestMemoryReport, HostProcessMemory, HostRole, KernelMemory,
    MemorySnapshot,
};
use memory_check::{
    median, pss_by_process, reading, regressions, report, unrecorded, ServiceMemory, Setting,
};

fn snapshot(vm_mib: u64, guest_used_mib: u64) -> MemorySnapshot {
    let mib = |m: u64| m * 1024;
    MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: GuestMemory {
                mem_total_kb: mib(4096),
                mem_free_kb: mib(4096 - guest_used_mib),
                mem_available_kb: mib(3000),
                cached_kb: mib(200),
                anon_pages_kb: mib(150),
                zram_original_kb: mib(300),
                zram_used_kb: mib(100),
                swap_total_kb: mib(2048),
                swap_free_kb: mib(2048 - 300),
                total_pss_kb: mib(400),
                swap_pss_kb: mib(300),
                google_play: Some(GooglePlayMemory {
                    processes: 3,
                    pss_kb: mib(90),
                    swap_pss_kb: mib(60),
                }),
                kernel: KernelMemory {
                    slab_unreclaimable_kb: mib(100),
                    kernel_stack_kb: mib(20),
                    page_tables_kb: mib(40),
                    vmalloc_used_kb: mib(60),
                    ..KernelMemory::default()
                },
                ..GuestMemory::default()
            },
            age: Duration::from_secs(1),
        }),
        host: vec![
            HostProcessMemory {
                role: HostRole::Vm,
                pid: 10,
                working_set_kb: mib(vm_mib),
                private_kb: mib(vm_mib + 100),
            },
            HostProcessMemory {
                role: HostRole::WindowManager,
                pid: 11,
                working_set_kb: mib(30),
                private_kb: mib(20),
            },
            HostProcessMemory {
                role: HostRole::AppWindow,
                pid: 12,
                working_set_kb: mib(40),
                private_kb: mib(25),
            },
        ],
    }
}

const SERVICE: ServiceMemory = ServiceMemory {
    resident_kb: 10 * 1024,
    private_kb: 5 * 1024,
};

#[test]
fn nfr1_a_reading_adds_the_service_to_the_hosts_processes_and_counts_zram_at_its_ratio() {
    let r = reading(&snapshot(600, 800), SERVICE).expect("a VM and a guest report");
    assert_eq!(r.host_total_kb, (600 + 30 + 40 + 10) * 1024);
    assert_eq!(r.host_private_kb, (700 + 20 + 25 + 5) * 1024);
    assert_eq!(r.app_windows, 1);
    assert_eq!(r.guest_used_kb, 800 * 1024);
    // 100 resident and 300 swapped out, which zram holds in a third: 200 MiB.
    assert_eq!(r.processes_kb, 200 * 1024);
    // 30 resident and 60 swapped out: 50 MiB.
    assert_eq!(r.google_play_kb, 50 * 1024);
}

#[test]
fn nfr1_nothing_is_measured_without_a_vm_or_a_guest_report() {
    let mut no_vm = snapshot(600, 800);
    no_vm.host.retain(|p| p.role != HostRole::Vm);
    assert_eq!(reading(&no_vm, SERVICE), None);
    let mut no_guest = snapshot(600, 800);
    no_guest.guest = None;
    assert_eq!(reading(&no_guest, SERVICE), None);
}

#[test]
fn nfr1_the_median_of_the_readings_is_reported_so_one_taken_in_a_reclaim_does_not_decide() {
    let readings: Vec<_> = [(600, 800), (2000, 1900), (610, 790)]
        .into_iter()
        .map(|(vm, used)| reading(&snapshot(vm, used), SERVICE).unwrap())
        .collect();
    let m = median(&readings).unwrap();
    assert_eq!(m.vm_kb, 610 * 1024);
    assert_eq!(m.guest_used_kb, 800 * 1024);
    assert_eq!(median(&[]), None);
}

#[test]
fn nfr1_only_a_large_regression_against_the_baseline_fails() {
    let r = reading(&snapshot(600, 800), SERVICE).unwrap();
    // 680 MiB host total.
    let baseline = |host: u64| serde_json::json!({"recorded": "run 1", "host_total_mib": host});
    // Within a quarter, or within 100 MiB: not a regression.
    assert!(regressions(&r, &baseline(560)).is_empty());
    assert!(regressions(&r, &baseline(600)).is_empty());
    // A quarter and 100 MiB above: one.
    let found = regressions(&r, &baseline(500));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("host_total_mib is 680 MiB"),
        "{}",
        found[0]
    );
    // The kernel and the rest are 450 MiB. Against 355 that is more than a quarter above, but
    // within 100 MiB: a small baseline needs both.
    let small = serde_json::json!({"guest_kernel_mib": 355});
    assert!(regressions(&r, &small).is_empty());
    assert_eq!(
        regressions(&r, &serde_json::json!({"guest_kernel_mib": 340})).len(),
        1
    );
}

#[test]
fn nfr1_a_number_the_baseline_does_not_record_is_reported_and_never_fails() {
    let r = reading(&snapshot(5000, 3000), SERVICE).unwrap();
    let baseline: serde_json::Value =
        serde_json::from_str(memory_check::BASELINE).expect("the baseline is JSON");
    let missing = unrecorded(&baseline);
    if missing.len() == 5 {
        assert!(regressions(&r, &baseline).is_empty());
    }
    let none = serde_json::json!({});
    assert_eq!(unrecorded(&none).len(), 5);
    assert!(regressions(&r, &none).is_empty());
}

#[test]
fn nfr1_the_guest_pss_by_process_is_read_from_dumpsys_meminfo() {
    let dumpsys = "Applications Memory Usage (in Kilobytes):\n\
                   Uptime: 1 Realtime: 1\n\n\
                   Total PSS by process:\n\
                   \x20   177,868K: com.android.vending (pid 5225 / activities)\n\
                   \x20   131,149K: system (pid 806)\n\
                   \x20       951K: android.hardware.drm-service.clearkey (pid 626)\n\n\
                   Total PSS by OOM adjustment:\n\
                   \x20   159,787K: Native\n";
    let list = pss_by_process(dumpsys);
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].pss_kb, 177_868);
    assert_eq!(list[0].name, "com.android.vending (pid 5225 / activities)");
    assert_eq!(list[2].pss_kb, 951);
    assert!(pss_by_process("no list here").is_empty());
}

#[test]
fn nfr1_the_report_shows_the_host_total_and_the_values_to_record_as_the_baseline() {
    let readings = vec![reading(&snapshot(600, 800), SERVICE).unwrap()];
    let setting = Setting {
        google_play: "LiteGapps' core",
        app: "Settings",
        memory_mb: 4096,
        processors: 2,
        environment: "test",
    };
    let report = report(&readings, &[], &serde_json::json!({}), &setting).unwrap();
    assert!(
        report.markdown.contains("**Host total: 680 MiB resident**"),
        "{}",
        report.markdown
    );
    assert!(
        report.markdown.contains("records no value yet"),
        "{}",
        report.markdown
    );
    assert_eq!(report.json["baseline"]["host_total_mib"], 680);
    assert_eq!(report.json["baseline"]["guest_used_mib"], 800);
    assert!(report
        .markdown
        .contains("| Google Play services (resident plus zram share) | 50 |"));
}

#[test]
fn nfr2_the_report_shows_how_much_more_the_vm_holds_than_the_guest_uses() {
    use memory_check::{median_gap_kb, signed_mib, vm_beyond_guest_kb};
    // The VM holds 600 resident and 700 private; the guest uses 550.
    let r = reading(&snapshot(600, 550), SERVICE).unwrap();
    assert_eq!(vm_beyond_guest_kb(&r), (50 * 1024, 150 * 1024));
    // A host that trimmed the VM below what the guest uses: negative.
    let trimmed = reading(&snapshot(500, 550), SERVICE).unwrap();
    assert_eq!(signed_mib(vm_beyond_guest_kb(&trimmed).0), -50);
    let readings = vec![r, trimmed, reading(&snapshot(560, 550), SERVICE).unwrap()];
    assert_eq!(median_gap_kb(&readings), Some((10 * 1024, 110 * 1024)));
    assert_eq!(median_gap_kb(&[]), None);
    let setting = Setting {
        google_play: "LiteGapps' core",
        app: "Settings",
        memory_mb: 4096,
        processors: 2,
        environment: "test",
    };
    let report = report(&readings, &[], &serde_json::json!({}), &setting).unwrap();
    assert!(
        report.markdown.contains(
            "The VM holds 10 MiB resident and 110 MiB private beyond what the guest uses"
        ),
        "{}",
        report.markdown
    );
    assert_eq!(report.json["vm_beyond_guest_kb"]["resident"], 10 * 1024);
}

#[test]
fn nfr1_the_zram_writebacks_are_read_from_the_guests_log() {
    use memory_check::{no_backing_file, writeback_runs, WritebackRun};
    let log = "[  812.1] dsa-channeld: reclaiming the guest's memory (Idle, 0 tasks in use)\n\
               [  843.5] dsa-channeld: zram writeback: wrote 32768 KiB of cold pages to the \
               backing file in 640 ms; zram store 163840 -> 153600 KiB (holding 491520 -> \
               458752 KiB, 512 -> 0 incompressible pages), 32000 KiB on the disk; 480 of \
               today's 512 MiB left\n\
               [  900.0] dsa-channeld: zram writeback: wrote garbled\n";
    assert_eq!(
        writeback_runs(log),
        vec![WritebackRun {
            written_kb: 32768,
            took_ms: 640,
            store_before_kb: 163840,
            store_after_kb: 153600,
            on_disk_kb: 32000,
        }]
    );
    assert!(!no_backing_file(log));
    assert!(no_backing_file(
        "dsa-channeld: zram has no backing file, so what stays cold in it stays in memory"
    ));
}

#[test]
fn nfr1_the_report_after_the_window_closes_shows_the_zram_store_and_what_was_written() {
    use memory_check::{idle_report, WritebackRun};
    let open = vec![reading(&snapshot(600, 800), SERVICE).unwrap()];
    let idle = vec![reading(&snapshot(500, 480), SERVICE).unwrap()];
    let runs = [WritebackRun {
        written_kb: 32768,
        took_ms: 640,
        store_before_kb: 163840,
        store_after_kb: 153600,
        on_disk_kb: 32000,
    }];
    let report = idle_report(&open, &idle, &runs, 150).unwrap();
    for line in [
        "| VM resident | 600 | 500 |",
        "| guest in use | 800 | 480 |",
        "| VM resident beyond guest in use | -200 | 20 |",
        "| VM private beyond guest in use | -100 | 120 |",
        "zram writeback: 1 run(s) wrote 32768 KiB; the zram store went from 163840 to 153600 KiB",
    ] {
        assert!(
            report.markdown.contains(line),
            "{line}\n{}",
            report.markdown
        );
    }
    assert_eq!(report.json["writeback"]["written_kb"], 32768);
    assert_eq!(
        report.json["vm_beyond_guest_kb"]["closed"]["resident"],
        20 * 1024
    );
    let none = idle_report(&open, &idle, &[], 150).unwrap();
    assert!(none.markdown.contains("No zram writeback ran."));
    assert!(idle_report(&open, &[], &runs, 150).is_none());
}
