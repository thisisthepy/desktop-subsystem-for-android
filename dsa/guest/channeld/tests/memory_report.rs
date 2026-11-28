//! `dsa-channeld --memory-report`: what it reads from the kernel's files and the lines it
//! writes, against text in the kernel's own formats.

use std::cell::RefCell;
use std::time::Duration;

use dsa_guest_channeld::memory_report::{
    allocation_sites, io_lines, kernel_thread_kind, last_report, process, process_io, report,
    slab_caches, thread_groups, vmalloc_users, vmstat_files, Sources, MARK, REPORT_FILE,
    SNAPSHOT_PROPERTY, VMSTAT_FILES, VMSTAT_INTERVAL,
};

const SLABINFO: &str = "slabinfo - version: 2.1\n\
# name            <active_objs> <num_objs> <objsize> <objperslab> <pagesperslab> : tunables <limit> <batchcount> <sharedfactor> : slabdata <active_slabs> <num_slabs> <sharedavail>\n\
vm_area_struct     41800  42120    184   22    1 : tunables    0    0    0 : slabdata   1915   1915      0\n\
task_struct         1350   1400   9984    3    8 : tunables    0    0    0 : slabdata    467    467      0\n\
kmalloc-64         20000  20480     64   64    1 : tunables    0    0    0 : slabdata    320    320      0\n\
broken line without the separator\n";

const VMALLOCINFO: &str = "\
0x0000000000000000-0x0000000000000000   20480 alloc_thread_stack_node+0x52/0x1d0 pages=4 vmalloc N0=4\n\
0x0000000000000000-0x0000000000000000   20480 alloc_thread_stack_node+0x52/0x1d0 pages=4 vmalloc N0=4\n\
0x0000000000000000-0x0000000000000000 6295552 zram_meta_alloc+0x37/0x100 pages=1536 vmalloc vpages N0=1536\n\
0x0000000000000000-0x0000000000000000    8192 acpi_os_map_iomem+0x1a/0x30 phys=0x00000000fed00000 ioremap\n\
0x0000000000000000-0x0000000000000000   12288 pages=2 vmalloc\n";

fn status(name: &str, ppid: u32, threads: u64, pte: Option<u64>) -> String {
    let mut s =
        format!("Name:\t{name}\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t9\nPPid:\t{ppid}\n");
    if let Some(kb) = pte {
        s.push_str(&format!("VmSize:\t 100000 kB\nVmPTE:\t     {kb} kB\n"));
    }
    s.push_str(&format!("Threads:\t{threads}\n"));
    s
}

#[test]
fn nfr1_slab_caches_are_sized_by_their_slabs_largest_first() {
    let caches = slab_caches(SLABINFO);
    let names: Vec<&str> = caches.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["task_struct", "vm_area_struct", "kmalloc-64"]);
    // 467 slabs of 8 pages.
    assert_eq!(caches[0].kib, 467 * 8 * 4);
    assert_eq!(caches[0].active_objects, 1350);
    assert_eq!(caches[0].object_size, 9984);
    assert_eq!(caches[1].kib, 1915 * 4);
    // What the slabs hold beyond the objects in use.
    assert_eq!(caches[1].unused_kib(), 1915 * 4 - 41800 * 184 / 1024);
}

#[test]
fn nfr1_vmalloc_users_are_summed_by_function_without_addresses() {
    let users = vmalloc_users(VMALLOCINFO);
    assert_eq!(users[0].caller, "zram_meta_alloc");
    assert_eq!(users[0].kib, 1536 * 4);
    let stacks = users
        .iter()
        .find(|u| u.caller == "alloc_thread_stack_node")
        .unwrap();
    assert_eq!((stacks.kib, stacks.areas), (32, 2));
    // An ioremap holds no memory of its own; an area with no caller is counted as unknown.
    assert!(!users.iter().any(|u| u.caller == "acpi_os_map_iomem"));
    assert!(users.iter().any(|u| u.caller == "unknown" && u.kib == 8));
    let lines = report(&Sources {
        vmallocinfo: Some(VMALLOCINFO.into()),
        ..Sources::default()
    });
    assert!(!lines.iter().any(|l| l.contains("0x")), "{lines:?}");
}

#[test]
fn nfr1_kernel_threads_are_grouped_by_kind_and_user_threads_by_process() {
    assert_eq!(kernel_thread_kind("kworker/u8:3-events_unbound"), "kworker");
    assert_eq!(kernel_thread_kind("kdmflush/253:4"), "kdmflush");
    assert_eq!(kernel_thread_kind("loop12"), "loop");
    assert_eq!(kernel_thread_kind("ext4-rsv-conver"), "ext4-rsv-conver");
    assert_eq!(kernel_thread_kind("jbd2/dm-12-8"), "jbd2");

    let processes = vec![
        process(2, &status("kthreadd", 0, 1, None), b"").unwrap(),
        process(40, &status("ext4-rsv-conver", 2, 1, None), b"").unwrap(),
        process(41, &status("ext4-rsv-conver", 2, 1, None), b"").unwrap(),
        process(42, &status("kdmflush/253:1", 2, 1, None), b"").unwrap(),
        process(
            900,
            &status("system_server", 600, 252, Some(2400)),
            b"system_server\0",
        )
        .unwrap(),
        // The status's name is cut at 15 characters; the command line's is whole.
        process(
            950,
            &status("droid.vending", 600, 113, Some(2000)),
            b"com.android.vending\0",
        )
        .unwrap(),
        process(960, &status("lmkd", 1, 1, Some(190)), b"/system/bin/lmkd\0").unwrap(),
    ];
    assert!(processes[1].kernel && processes[0].kernel);
    assert!(!processes[4].kernel);
    assert_eq!(processes[5].name, "com.android.vending");
    assert_eq!(processes[6].name, "lmkd");
    let groups = thread_groups(&processes);
    let rsv = groups.iter().find(|g| g.name == "ext4-rsv-conver").unwrap();
    assert!(rsv.kernel);
    assert_eq!((rsv.threads, rsv.processes), (2, 2));
    assert_eq!(groups[0].name, "system_server");
    assert_eq!(groups[0].threads, 252);

    let lines = report(&Sources {
        processes,
        ..Sources::default()
    });
    assert!(
        lines.contains(&"threads-total 370 4 366 16".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(&"threads kernel ext4-rsv-conver 2 2".to_string()));
    assert!(lines.contains(&"threads user com.android.vending 113 1".to_string()));
    assert!(lines.contains(&"page-tables system_server 2400".to_string()));
}

#[test]
fn nfr1_the_report_has_every_part_between_begin_and_end_and_says_what_it_could_not_read() {
    let lines = report(&Sources {
        meminfo: Some(
            "MemTotal:  3000000 kB\nSUnreclaim:  102400 kB\nKernelStack:  21552 kB\n".into(),
        ),
        slabinfo: Some(SLABINFO.into()),
        vmallocinfo: None,
        processes: Vec::new(),
        cgroup_stat: Some("nr_descendants 212\nnr_dying_descendants 37\n".into()),
        bufio_bytes: Some("41943040\n".into()),
        allocinfo: None,
        slab_reclaim: Vec::new(),
    });
    assert_eq!(lines.first().map(String::as_str), Some("begin"));
    assert_eq!(lines.last().map(String::as_str), Some("end"));
    assert!(lines.contains(&"meminfo SUnreclaim 102400".to_string()));
    assert!(lines.contains(&"meminfo KernelStack 21552".to_string()));
    assert!(lines.iter().any(|l| l.starts_with("slab-total 3 ")));
    // Objects per slab and pages per slab follow; whether the cache is reclaimable is unknown
    // when /sys/kernel/slab could not be read, and the report says so once.
    assert!(lines.contains(&"slab task_struct 14944 1350 1400 9984 3 8 unknown".to_string()));
    assert!(lines.iter().any(|l| l.starts_with("missing slab-reclaim ")));
    assert!(lines.contains(&"cgroups 212 37".to_string()));
    assert!(lines.contains(&"bufio 41943040".to_string()));
    assert!(lines.iter().any(|l| l.starts_with("missing vmalloc ")));
    assert!(lines.iter().any(|l| l.starts_with("missing threads ")));
    // The shipped kernel has no allocation profiling; the report says how to get it.
    assert!(lines
        .iter()
        .any(|l| l.starts_with("missing allocinfo ") && l.contains("--memory-profile")));
    // Without SLUB's debugging support the kernel has no slabinfo; the report says so.
    let without = report(&Sources::default());
    assert!(without.iter().any(|l| l.starts_with("missing slab ")));
    // Every line is one record of the kernel log.
    assert!(lines.iter().all(|l| !l.contains('\n') && l.len() < 900));
}

/// Three caches of the dev.39 snapshot taken 10 minutes after a boot with no app open
/// (`android/kernel-memory-report.txt`), in `/proc/slabinfo`'s own format: task_struct and
/// kmalloc-4k in order 3 slabs of 8 objects, radix_tree_node in order 2 slabs of 28.
const DEV39_SLABINFO: &str = "slabinfo - version: 2.1\n\
# name            <active_objs> <num_objs> <objsize> <objperslab> <pagesperslab> : tunables <limit> <batchcount> <sharedfactor> : slabdata <active_slabs> <num_slabs> <sharedavail>\n\
radix_tree_node     6591  11592    584   28    4 : tunables    0    0    0 : slabdata    414    414      0\n\
task_struct          867   1360   4032    8    8 : tunables    0    0    0 : slabdata    170    170      0\n\
kmalloc-4k           685   1320   4096    8    8 : tunables    0    0    0 : slabdata    165    165      0\n";

#[test]
fn nfr1_the_report_splits_the_slab_into_reclaimable_and_unreclaimable_caches() {
    // The dev.39 report listed only the 40 largest caches and could not say which of them made
    // up SUnreclaim (81 MiB with no app open): /proc/slabinfo has no reclaim flag. The flag is
    // each cache's /sys/kernel/slab/<cache>/reclaim_account.
    let caches = slab_caches(DEV39_SLABINFO);
    let task = caches.iter().find(|c| c.name == "task_struct").unwrap();
    assert_eq!(task.kib, 5440);
    assert_eq!((task.objects_per_slab, task.pages_per_slab), (8, 8));
    let lines = report(&Sources {
        slabinfo: Some(DEV39_SLABINFO.into()),
        slab_reclaim: vec![
            ("radix_tree_node".into(), true),
            ("task_struct".into(), false),
            ("kmalloc-4k".into(), false),
        ],
        ..Sources::default()
    });
    assert!(
        lines.contains(&"slab radix_tree_node 6624 6591 11592 584 28 4 reclaimable".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(&"slab task_struct 5440 867 1360 4032 8 8 unreclaimable".to_string()));
    assert!(lines.contains(&"slab kmalloc-4k 5280 685 1320 4096 8 8 unreclaimable".to_string()));
    // The unreclaimable caches' sum, and what their slabs hold beyond the objects in use:
    // 5440 - 867 * 4032 / 1024 and 5280 - 685 * 4096 / 1024.
    assert!(
        lines.contains(&"slab-unreclaimable 2 10720 4567".to_string()),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("missing slab-reclaim ")));
}

#[test]
fn nfr1_the_report_lists_every_slab_cache_not_only_the_largest() {
    // dev.39 had 199 caches and the report named 40 of them; the other 159 held 9 MiB that no
    // line explained. Every cache now has its line, largest first.
    let mut slabinfo = String::from("slabinfo - version: 2.1\n");
    for i in 0..60 {
        slabinfo.push_str(&format!(
            "cache-{i:02}   10   16   256   16    1 : tunables    0    0    0 : slabdata    {n}    {n}      0\n",
            n = i + 1
        ));
    }
    let lines = report(&Sources {
        slabinfo: Some(slabinfo),
        ..Sources::default()
    });
    let slabs: Vec<&String> = lines.iter().filter(|l| l.starts_with("slab ")).collect();
    assert_eq!(slabs.len(), 60);
    assert!(slabs[0].starts_with("slab cache-59 240 "), "{slabs:?}");
    assert!(lines.iter().any(|l| l.starts_with("slab-total 60 ")));
}

const ALLOCINFO: &str = "allocinfo - version: 2.0\n\
#     <size>  <calls> <tag info>\n\
    58720256    14336 mm/sparse-vmemmap.c:85 func:vmemmap_alloc_block\n\
     3145728      768 drivers/gpu/drm/drm_gem_shmem_helper.c:180 [drm] func:drm_gem_shmem_get_pages\n\
           0        0 init/main.c:1310 func:do_initcalls\n\
   104857600    25600 mm/slub.c:2462 func:alloc_slab_page\n";

#[test]
fn nfr1_allocation_sites_say_where_the_unaccounted_kernel_memory_was_allocated() {
    let sites = allocation_sites(ALLOCINFO);
    // Sites that hold nothing now are left out; the rest are largest first.
    let names: Vec<&str> = sites.iter().map(|s| s.function.as_str()).collect();
    assert_eq!(
        names,
        [
            "alloc_slab_page",
            "vmemmap_alloc_block",
            "drm_gem_shmem_get_pages"
        ]
    );
    assert_eq!(sites[1].site, "mm/sparse-vmemmap.c:85");
    assert_eq!((sites[1].kib, sites[1].calls), (57344, 14336));
    let lines = report(&Sources {
        allocinfo: Some(ALLOCINFO.into()),
        ..Sources::default()
    });
    assert!(
        lines.contains(&"allocinfo-total 3 162816".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(
        &"allocinfo drivers/gpu/drm/drm_gem_shmem_helper.c:180 drm_gem_shmem_get_pages 3072 768"
            .to_string()
    ));
    assert!(!lines.iter().any(|l| l.starts_with("missing allocinfo ")));
}

const IO: &str = "rchar: 912345678\nwchar: 2048\nsyscr: 40\nsyscw: 2\nread_bytes: 314572800\n\
write_bytes: 4096\ncancelled_write_bytes: 0\n";

#[test]
fn nfr1_the_report_lists_the_processes_that_read_the_most_from_devices() {
    // Who fills the page cache: /proc/<pid>/io of every process, the ones that read the most
    // from a device first (read_bytes is what the page cache did not already hold).
    let io = process_io(IO).unwrap();
    assert_eq!(io.read_chars, 912_345_678);
    assert_eq!(io.read_bytes, 314_572_800);
    assert_eq!(io.write_bytes, 4096);
    assert_eq!(process_io("no counts here\n"), None);

    let mut gms = process(
        1141,
        &status("droid.gms", 600, 90, Some(900)),
        b"com.google.android.gms\0",
    )
    .unwrap();
    gms.io = Some(io);
    let mut artd = process(2200, &status("artd", 1, 8, Some(300)), b"/apex/bin/artd\0").unwrap();
    artd.io = process_io("rchar: 50\nwchar: 9\nread_bytes: 900000000\nwrite_bytes: 70\n");
    let quiet = process(3000, &status("logd", 1, 4, Some(50)), b"/system/bin/logd\0").unwrap();
    let lines = io_lines(&[gms.clone(), artd.clone(), quiet.clone()]);
    assert_eq!(
        lines,
        [
            "io-total 2 1214572800 4166",
            "io artd 2200 50 9 900000000 70 0",
            "io com.google.android.gms 1141 912345678 2048 314572800 4096 0",
        ]
    );
    // In the report, after the threads and page tables.
    let report = report(&Sources {
        processes: vec![gms, artd, quiet.clone()],
        ..Sources::default()
    });
    let at = |prefix: &str| report.iter().position(|l| l.starts_with(prefix)).unwrap();
    assert!(at("page-tables ") < at("io-total "), "{report:?}");
    // Without root's capabilities no counts are read, and the report says so.
    assert_eq!(
        io_lines(&[quiet]),
        ["missing io /proc/<pid>/io could not be read for any process"]
    );
}

#[test]
fn nfr1_the_diagnostics_carry_vmstat_twice_ten_seconds_apart() {
    let reads = RefCell::new(0);
    let waited = RefCell::new(Vec::new());
    let files = vmstat_files(
        &|| {
            *reads.borrow_mut() += 1;
            Ok(format!("pgpgin {}\n", *reads.borrow()).into_bytes())
        },
        &|wait| waited.borrow_mut().push(wait),
    );
    assert_eq!(VMSTAT_INTERVAL, Duration::from_secs(10));
    assert_eq!(*waited.borrow(), [Duration::from_secs(10)]);
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, VMSTAT_FILES);
    assert_eq!(files[0].1.as_deref(), Ok(&b"pgpgin 1\n"[..]));
    assert_eq!(files[1].1.as_deref(), Ok(&b"pgpgin 2\n"[..]));
    // A file that cannot be read says why, in its place.
    let refused = vmstat_files(&|| Err("/proc/vmstat: denied".into()), &|_| {});
    assert_eq!(refused[1].1, Err("/proc/vmstat: denied".to_string()));
}

#[test]
fn nfr1_the_diagnostics_take_the_last_whole_kernel_memory_report_from_the_log() {
    // The kernel log's records as the diagnostics read them: the daemon's prefix, the mark,
    // and between two reports a line of something else.
    let record =
        |text: &str| format!("11-28 10:00:00.000     0     0 I dsa-channeld: {MARK}{text}\n");
    let log = vec![
        record("begin"),
        record("slab-total 1 10 1"),
        record("end"),
        "11-28 10:00:01.000     0     0 I init: something else\n".to_string(),
        record("begin"),
        record("slab-total 2 20 2"),
        record("io-total 1 4096 0"),
        record("end"),
        // A report still being written when the log was read is not taken.
        record("begin"),
        record("slab-total 3 30 3"),
    ];
    assert_eq!(
        last_report(&log).as_deref(),
        Some("begin\nslab-total 2 20 2\nio-total 1 4096 0\nend\n")
    );
    assert_eq!(last_report(&log[..1]), None);
    assert_eq!(last_report(&[]), None);
    assert_eq!(REPORT_FILE, "android/kernel-memory-report.txt");
    // The property only the daemon may set, which init's trigger for the diagnostics reads.
    assert_eq!(SNAPSHOT_PROPERTY, "vendor.dsa.memory.report");
}
