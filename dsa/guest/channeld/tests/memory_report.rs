//! `dsa-channeld --memory-report`: what it reads from the kernel's files and the lines it
//! writes, against text in the kernel's own formats.

use dsa_guest_channeld::memory_report::{
    kernel_thread_kind, process, report, slab_caches, thread_groups, vmalloc_users, Sources,
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
    });
    assert_eq!(lines.first().map(String::as_str), Some("begin"));
    assert_eq!(lines.last().map(String::as_str), Some("end"));
    assert!(lines.contains(&"meminfo SUnreclaim 102400".to_string()));
    assert!(lines.contains(&"meminfo KernelStack 21552".to_string()));
    assert!(lines.iter().any(|l| l.starts_with("slab-total 3 ")));
    assert!(lines.contains(&"slab task_struct 14944 1350 1400 9984".to_string()));
    assert!(lines.contains(&"cgroups 212 37".to_string()));
    assert!(lines.contains(&"bufio 41943040".to_string()));
    assert!(lines.iter().any(|l| l.starts_with("missing vmalloc ")));
    assert!(lines.iter().any(|l| l.starts_with("missing threads ")));
    // Without SLUB's debugging support the kernel has no slabinfo; the report says so.
    let without = report(&Sources::default());
    assert!(without.iter().any(|l| l.starts_with("missing slab ")));
    // Every line is one record of the kernel log.
    assert!(lines.iter().all(|l| !l.contains('\n') && l.len() < 900));
}
