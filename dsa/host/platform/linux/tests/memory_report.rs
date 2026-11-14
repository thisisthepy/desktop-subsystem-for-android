//! The memory measurement's numbers and its comparison with the baseline, from snapshots
//! made up here; the boot check that takes real ones is in `overlays.rs`.

mod memory_check;

use std::time::Duration;

use dsa_host_core::memory::{
    GooglePlayMemory, GuestMemory, GuestMemoryReport, HostProcessMemory, HostRole, KernelMemory,
    MemorySnapshot,
};
use memory_check::{
    app_summary, heap_settings, kernel_markdown, kernel_report, listed_services, median,
    pss_by_process, reading, regressions, report, system_server_report, thread_shapes, unrecorded,
    ServiceMemory, Setting, SystemServer,
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
                mapped_kb: 0,
            },
            HostProcessMemory {
                role: HostRole::WindowManager,
                pid: 11,
                working_set_kb: mib(30),
                private_kb: mib(20),
                mapped_kb: 0,
            },
            HostProcessMemory {
                role: HostRole::AppWindow,
                pid: 12,
                working_set_kb: mib(40),
                private_kb: mib(25),
                mapped_kb: 0,
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
fn nfr2_the_report_shows_how_much_free_memory_the_host_has_back() {
    use dsa_host_core::memory::PageReporting;
    use memory_check::idle_report;
    let with_reporting = |vm_mib: u64, used_mib: u64, reported_mib: u64| {
        let mut snapshot = snapshot(vm_mib, used_mib);
        snapshot.guest.as_mut().unwrap().memory.page_reporting = Some(PageReporting {
            free_reported_kb: reported_mib * 1024,
            reported_total_kb: 9000 * 1024,
            ..PageReporting::default()
        });
        reading(&snapshot, SERVICE).unwrap()
    };
    // 4096 MiB given: 800 in use leaves 3296 free, 480 in use leaves 3616.
    let open = vec![with_reporting(600, 800, 3200)];
    let idle = vec![with_reporting(500, 480, 3600)];
    let setting = Setting {
        google_play: "LiteGapps' core",
        app: "Settings",
        memory_mb: 4096,
        processors: 2,
        environment: "test",
    };
    let first = report(&open, &[], &serde_json::json!({}), &setting).unwrap();
    assert!(
        first.markdown.contains(
            "Of the guest's 3296 MiB free, free page reporting has handed 3200 MiB back to the \
             host (96 MiB not yet)."
        ),
        "{}",
        first.markdown
    );
    let closed = idle_report(&open, &idle, &[], 150).unwrap();
    for line in [
        "| guest free | 3296 | 3616 |",
        "| guest free handed back to the host | 3200 | 3600 |",
    ] {
        assert!(
            closed.markdown.contains(line),
            "{line}\n{}",
            closed.markdown
        );
    }
    assert_eq!(closed.json["median_kb"]["free_reported"], 3600 * 1024);
    // A guest whose kernel does not say gives no such line.
    let without = report(
        &[reading(&snapshot(600, 800), SERVICE).unwrap()],
        &[],
        &serde_json::json!({}),
        &setting,
    )
    .unwrap();
    assert!(!without.markdown.contains("free page reporting"));
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

/// A kernel log with an unfinished report, then a whole one, as `logcat -b kernel` prints
/// the guest's lines.
const KERNEL_LOG: &str = "\
11-11 10:00:00.000     0     0 I dsa-channeld: memory report: begin\n\
11-11 10:00:00.001     0     0 I dsa-channeld: memory report: slab stale 1 1 1 1\n\
11-11 10:01:00.000     0     0 I init    : starting service 'dsa_memory_report'...\n\
11-11 10:01:00.100     0     0 I dsa-channeld: memory report: begin\n\
11-11 10:01:00.101     0     0 I dsa-channeld: memory report: meminfo SUnreclaim 102400\n\
11-11 10:01:00.102     0     0 I dsa-channeld: memory report: slab-total 180 120000 30000\n\
11-11 10:01:00.103     0     0 I dsa-channeld: memory report: slab task_struct 14944 1350 1400 9984\n\
11-11 10:01:00.104     0     0 I dsa-channeld: memory report: vmalloc-total 1500 61440\n\
11-11 10:01:00.105     0     0 I dsa-channeld: memory report: vmalloc alloc_thread_stack_node 21552 1347\n\
11-11 10:01:00.106     0     0 I dsa-channeld: memory report: threads-total 1347 230 1117 16\n\
11-11 10:01:00.107     0     0 I dsa-channeld: memory report: threads kernel ext4-rsv-conver 75 75\n\
11-11 10:01:00.108     0     0 I dsa-channeld: memory report: threads user system_server 252 1\n\
11-11 10:01:00.109     0     0 I dsa-channeld: memory report: page-tables system_server 2400\n\
11-11 10:01:00.110     0     0 I dsa-channeld: memory report: cgroups 212 37\n\
11-11 10:01:00.111     0     0 I dsa-channeld: memory report: bufio 41943040\n\
11-11 10:01:00.112     0     0 I dsa-channeld: memory report: missing nothing else\n\
11-11 10:01:00.113     0     0 I dsa-channeld: memory report: end\n";

#[test]
fn nfr1_the_kernel_memory_report_is_the_last_whole_one_in_the_kernel_log() {
    let lines = kernel_report(KERNEL_LOG).unwrap();
    assert_eq!(
        lines.first().map(String::as_str),
        Some("meminfo SUnreclaim 102400")
    );
    assert!(!lines.iter().any(|l| l.contains("stale")));
    assert!(!lines.iter().any(|l| l == "begin" || l == "end"));
    // A report cut off before its end is no report.
    let cut = KERNEL_LOG.rsplit_once("memory report: end").unwrap().0;
    assert!(kernel_report(cut).is_none());
    assert!(kernel_report("").is_none());
}

#[test]
fn nfr1_the_kernel_memory_report_shows_slab_vmalloc_threads_and_page_tables() {
    let md = kernel_markdown(&kernel_report(KERNEL_LOG).unwrap());
    assert!(md.contains("| SUnreclaim | 100.0 |"), "{md}");
    assert!(
        md.contains("**Slab: 180 caches, 117.2 MiB**, of which 29.3 MiB"),
        "{md}"
    );
    assert!(
        md.contains("| task_struct | 14.6 | 1350 | 1400 | 9984 |"),
        "{md}"
    );
    assert!(md.contains("**vmalloc: 1500 areas, 60.0 MiB.**"), "{md}");
    assert!(
        md.contains("| alloc_thread_stack_node | 21.0 | 1347 |"),
        "{md}"
    );
    assert!(
        md.contains("**Threads: 1347** (230 kernel, 1117 user)"),
        "{md}"
    );
    // 75 threads of 16 KiB.
    assert!(
        md.contains("| ext4-rsv-conver | kernel | 75 | 75 | 1.2 |"),
        "{md}"
    );
    assert!(
        md.contains("| system_server | user | 252 | 1 | 3.9 |"),
        "{md}"
    );
    assert!(md.contains("| system_server | 2.3 |"), "{md}");
    assert!(md.contains("Control groups: 212 live, 37 dying"), "{md}");
    assert!(
        md.contains("dm-bufio (dm-verity's hash block cache): 40.0 MiB."),
        "{md}"
    );
    assert!(md.contains("Not read: nothing else."), "{md}");
}

/// system_server's `dumpsys meminfo <pid>`, cut to the parts the report reads.
const SYSTEM_SERVER_MEMINFO: &str = r#"
Applications Memory Usage (in Kilobytes):
** MEMINFO in pid 806 [system] **
                   Pss  Private  Private  SwapPss      Rss
                 Total    Dirty    Clean    Dirty    Total
        TOTAL   131506    61000    20000    40234   133564

 App Summary
                       Pss(KB)                        Rss(KB)
                        ------                         ------
           Java Heap:    24312                          37400
         Native Heap:    13184                          14492
                Code:    24660                          73040
               Stack:     3296                           3304
            Graphics:        0                              0
       Private Other:     9824
              System:    56230
             Unknown:                                    5328

           TOTAL PSS:   131506            TOTAL RSS:   133564       TOTAL SWAP PSS:    40234

 Objects
               Views:        0         ViewRootImpl:        0
       Local Binders:     1520        Proxy Binders:     2210
       Parcel memory:      120         Parcel count:      480
    Death Recipients:      640             WebViews:        0"#;

#[test]
fn nfr1_system_servers_app_summary_is_read_with_pss_only() {
    let summary = app_summary(SYSTEM_SERVER_MEMINFO);
    let names: Vec<&str> = summary.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "Java Heap",
            "Native Heap",
            "Code",
            "Stack",
            "Graphics",
            "Private Other",
            "System",
            "TOTAL PSS",
            "TOTAL SWAP PSS"
        ]
    );
    assert_eq!(summary[0].1, 24312);
    assert_eq!(summary[7].1, 131506);
    assert_eq!(summary[8].1, 40234);
    assert!(app_summary("no summary").is_empty());
}

#[test]
fn nfr1_system_servers_threads_services_and_heap_settings_are_reported() {
    let shapes =
        thread_shapes("binder:806_1\nbinder:806_2\nbinder:806_1F\nsystem_server\nHeapTaskDaemon\n");
    assert_eq!(shapes[0], ("binder:#_#".to_string(), 2));
    assert!(shapes.contains(&("binder:#_#F".to_string(), 1)));
    let services = "Currently running services:\n  accessibility\n  account\n  activity\n";
    assert_eq!(
        listed_services(services),
        ["accessibility", "account", "activity"]
    );
    let getprop = "[dalvik.vm.foreground-heap-growth-multiplier]: [1.0]\n\
                   [dalvik.vm.heapmaxfree]: [8m]\n[dalvik.vm.heapsize]: [512m]\n[dalvik.vm.usejit]: [true]\n";
    assert_eq!(
        heap_settings(getprop),
        "dalvik.vm.foreground-heap-growth-multiplier=1.0\ndalvik.vm.heapmaxfree=8m\ndalvik.vm.heapsize=512m"
    );
    let (md, json) = system_server_report(&SystemServer {
        meminfo: SYSTEM_SERVER_MEMINFO,
        threads: "binder:806_1\nbinder:806_2\nsystem_server\n",
        services,
        heap_settings: &heap_settings(getprop),
        heap_dump: "none (`am dumpheap` answered: Process not debuggable)",
    });
    assert!(md.contains("| Java Heap | 23.7 |"), "{md}");
    assert!(
        md.contains("local binders 1520, proxy binders 2210, death recipients 640"),
        "{md}"
    );
    assert!(md.contains("**3 threads**"), "{md}");
    assert!(md.contains("3 system services"), "{md}");
    assert!(md.contains("`dalvik.vm.heapmaxfree=8m`"), "{md}");
    assert!(
        md.contains("Heap dump: none (`am dumpheap` answered: Process not debuggable)"),
        "{md}"
    );
    assert_eq!(json["threads"], 3);
    assert_eq!(json["services"].as_array().map(Vec::len), Some(3));
}
