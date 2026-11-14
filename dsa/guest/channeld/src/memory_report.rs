//! What the guest kernel's own memory is made of: `dsa-channeld --memory-report`.
//!
//! The guest's memory report to the host gives the kernel's costs as sums (unreclaimable slab,
//! kernel stacks, page tables, vmalloc). Which slab caches, which vmalloc users and which
//! threads make them up can only be read as root: `/proc/slabinfo` and `/proc/vmallocinfo`
//! are root's alone, and the shell of a user build is not root. init runs this as root, in a
//! domain of its own that may only read, when developer mode is on and the shell asks for it
//! (`setprop debug.dsa.memory_report 1`, `dsa-channeld.rc`). It writes the report to the kernel
//! log, one record per line, each starting with [`MARK`], between a `begin` and an `end` line,
//! where `logcat -b kernel` shows it.
//!
//! The report names caches, functions and process names, never an address: a vmalloc area's
//! address would tell where the kernel lies in memory.

/// What starts every line of the report in the kernel log, after the daemon's own prefix.
pub const MARK: &str = "memory report: ";

/// The property the shell sets to ask for a report.
pub const PROPERTY: &str = "debug.dsa.memory_report";

/// The size of each thread's kernel stack, in KiB, on both architectures the guest runs on.
pub const KERNEL_STACK_KIB: u64 = 16;

/// How many of the largest of each kind the report lists.
pub const SLAB_LINES: usize = 40;
pub const VMALLOC_LINES: usize = 30;
pub const THREAD_LINES: usize = 40;
pub const PAGE_TABLE_LINES: usize = 15;

/// The `/proc/meminfo` lines the report repeats, so its parts can be read against them.
pub const MEMINFO_KEYS: [&str; 11] = [
    "MemTotal",
    "MemFree",
    "Slab",
    "SReclaimable",
    "SUnreclaim",
    "KernelStack",
    "PageTables",
    "SecPageTables",
    "VmallocUsed",
    "Percpu",
    "Shmem",
];

/// One slab cache, as `/proc/slabinfo` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlabCache {
    pub name: String,
    pub active_objects: u64,
    pub objects: u64,
    pub object_size: u64,
    /// The memory its slabs take, in KiB.
    pub kib: u64,
}

impl SlabCache {
    /// What its slabs hold beyond the objects in use, in KiB: free objects and the slack of
    /// each slab.
    pub fn unused_kib(&self) -> u64 {
        self.kib
            .saturating_sub(self.active_objects * self.object_size / 1024)
    }
}

/// The caches of a `/proc/slabinfo` (version 2.1), largest first. A line that does not parse
/// is skipped.
pub fn slab_caches(slabinfo: &str) -> Vec<SlabCache> {
    let mut caches: Vec<SlabCache> = slabinfo
        .lines()
        .filter(|line| !line.starts_with("slabinfo") && !line.starts_with('#'))
        .filter_map(|line| {
            // name active_objs num_objs objsize objperslab pagesperslab : tunables ... :
            // slabdata active_slabs num_slabs sharedavail
            let (head, rest) = line.split_once(':')?;
            let mut fields = head.split_whitespace();
            let name = fields.next()?.to_string();
            let numbers: Vec<u64> = fields.map(|f| f.parse().ok()).collect::<Option<_>>()?;
            let [active_objects, objects, object_size, _per_slab, pages_per_slab] = numbers[..]
            else {
                return None;
            };
            let slabdata = rest.split_once("slabdata")?.1;
            let num_slabs: u64 = slabdata.split_whitespace().nth(1)?.parse().ok()?;
            Some(SlabCache {
                name,
                active_objects,
                objects,
                object_size,
                kib: num_slabs * pages_per_slab * 4,
            })
        })
        .collect();
    caches.sort_by(|a, b| b.kib.cmp(&a.kib).then_with(|| a.name.cmp(&b.name)));
    caches
}

/// The memory one function's vmalloc areas hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmallocUser {
    /// The function that asked for them, without its offset.
    pub caller: String,
    pub kib: u64,
    pub areas: u64,
}

/// The users of a `/proc/vmallocinfo`, largest first, by the pages each area holds. Areas
/// with no pages of their own (an `ioremap` of a device, a `vmap` of pages counted elsewhere)
/// are left out, as the kernel's `VmallocUsed` leaves them out.
pub fn vmalloc_users(vmallocinfo: &str) -> Vec<VmallocUser> {
    let mut users: Vec<VmallocUser> = Vec::new();
    for line in vmallocinfo.lines() {
        let mut fields = line.split_whitespace();
        // The address range, then the size; neither goes into the report.
        if fields.next().is_none() || fields.next().is_none() {
            continue;
        }
        let rest: Vec<&str> = fields.collect();
        let Some(pages) = rest
            .iter()
            .find_map(|f| f.strip_prefix("pages="))
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        let caller = rest
            .first()
            .filter(|f| !f.contains('='))
            .map_or("unknown", |f| f.split('+').next().unwrap_or(f));
        let caller = if caller.is_empty() { "unknown" } else { caller };
        match users.iter_mut().find(|u| u.caller == caller) {
            Some(user) => {
                user.kib += pages * 4;
                user.areas += 1;
            }
            None => users.push(VmallocUser {
                caller: caller.to_string(),
                kib: pages * 4,
                areas: 1,
            }),
        }
    }
    users.sort_by(|a, b| b.kib.cmp(&a.kib).then_with(|| a.caller.cmp(&b.caller)));
    users
}

/// One process as its `/proc/<pid>/status` and command line describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub name: String,
    pub threads: u64,
    /// A kernel thread: a child of `kthreadd`, or `kthreadd` itself.
    pub kernel: bool,
    /// Its page tables, in KiB (`VmPTE`); 0 for a kernel thread.
    pub page_tables_kib: u64,
}

/// The process of one `/proc/<pid>/status`, named by the first word of its command line when
/// it has one (the status gives only the first 15 characters). `None` when the status does
/// not parse.
pub fn process(pid: u32, status: &str, cmdline: &[u8]) -> Option<Process> {
    let field = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .map(str::trim)
    };
    let short = field("Name")?;
    let threads = field("Threads")?.parse().ok()?;
    let parent: u32 = field("PPid")?.parse().ok()?;
    let page_tables_kib = field("VmPTE")
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0);
    let kernel = pid == 2 || parent == 2;
    let long = cmdline
        .split(|b| *b == 0)
        .next()
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .unwrap_or_default();
    // A command line's first word is a path for a native program, a package name for an app.
    let long = long.rsplit('/').next().unwrap_or(&long).to_string();
    let name = if kernel || long.is_empty() {
        short.to_string()
    } else {
        long
    };
    Some(Process {
        name: name.split_whitespace().collect::<Vec<_>>().join("_"),
        threads,
        kernel,
        page_tables_kib,
    })
}

/// The name kernel threads of one kind share: the part before any `/` (`kworker/u8:3-events`,
/// `kdmflush/253:4`), without a trailing number (`loop12`).
pub fn kernel_thread_kind(name: &str) -> String {
    let head = name.split('/').next().unwrap_or(name);
    let kind = head.trim_end_matches(|c: char| c.is_ascii_digit());
    let kind = if kind.is_empty() { head } else { kind };
    kind.to_string()
}

/// Threads of one kind: kernel threads by [`kernel_thread_kind`], user threads by process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadGroup {
    pub name: String,
    pub kernel: bool,
    pub threads: u64,
    pub processes: u64,
}

/// The threads of `processes` in groups, most threads first.
pub fn thread_groups(processes: &[Process]) -> Vec<ThreadGroup> {
    let mut groups: Vec<ThreadGroup> = Vec::new();
    for p in processes {
        let name = if p.kernel {
            kernel_thread_kind(&p.name)
        } else {
            p.name.clone()
        };
        match groups
            .iter_mut()
            .find(|g| g.kernel == p.kernel && g.name == name)
        {
            Some(group) => {
                group.threads += p.threads;
                group.processes += 1;
            }
            None => groups.push(ThreadGroup {
                name,
                kernel: p.kernel,
                threads: p.threads,
                processes: 1,
            }),
        }
    }
    groups.sort_by(|a, b| b.threads.cmp(&a.threads).then_with(|| a.name.cmp(&b.name)));
    groups
}

/// The value of `key` in a `/proc/meminfo`, in KiB.
pub fn meminfo_kib(meminfo: &str, key: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?;
        value.split_whitespace().next()?.parse().ok()
    })
}

/// The value of `key` in a cgroup's `cgroup.stat`.
pub fn cgroup_stat(stat: &str, key: &str) -> Option<u64> {
    stat.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == key).then(|| value.trim().parse().ok())?
    })
}

/// What the report is made from. A file the guest could not read is `None`, and the report
/// says so in its place.
#[derive(Debug, Default, Clone)]
pub struct Sources {
    pub meminfo: Option<String>,
    pub slabinfo: Option<String>,
    pub vmallocinfo: Option<String>,
    pub processes: Vec<Process>,
    /// The root cgroup's `cgroup.stat`: live and dying descendants.
    pub cgroup_stat: Option<String>,
    /// dm-bufio's `current_allocated_bytes`: the dm-verity hash blocks it caches.
    pub bufio_bytes: Option<String>,
}

/// The report's lines, without [`MARK`]: `begin`, then each part, then `end`. Each line is
/// words separated by spaces, its kind first:
///
/// - `meminfo <key> <KiB>`
/// - `slab-total <caches> <KiB> <unused KiB>`, then `slab <name> <KiB> <active> <objects> <size>`
/// - `vmalloc-total <areas> <KiB>`, then `vmalloc <caller> <KiB> <areas>`
/// - `threads-total <threads> <kernel threads> <user threads> <stack KiB each>`, then
///   `threads kernel|user <name> <threads> <processes>`
/// - `page-tables <process> <KiB>`
/// - `cgroups <descendants> <dying>`
/// - `bufio <bytes>`
/// - `missing <what> <why>` for a part that could not be read
pub fn report(sources: &Sources) -> Vec<String> {
    let mut lines = vec!["begin".to_string()];
    match &sources.meminfo {
        Some(meminfo) => {
            for key in MEMINFO_KEYS {
                if let Some(kib) = meminfo_kib(meminfo, key) {
                    lines.push(format!("meminfo {key} {kib}"));
                }
            }
        }
        None => lines.push("missing meminfo /proc/meminfo could not be read".into()),
    }
    match &sources.slabinfo {
        Some(slabinfo) => {
            let caches = slab_caches(slabinfo);
            let total: u64 = caches.iter().map(|c| c.kib).sum();
            let unused: u64 = caches.iter().map(SlabCache::unused_kib).sum();
            lines.push(format!("slab-total {} {total} {unused}", caches.len()));
            for c in caches.iter().take(SLAB_LINES) {
                lines.push(format!(
                    "slab {} {} {} {} {}",
                    c.name, c.kib, c.active_objects, c.objects, c.object_size
                ));
            }
        }
        None => lines.push(
            "missing slab /proc/slabinfo could not be read (a kernel without SLUB_DEBUG has none)"
                .into(),
        ),
    }
    match &sources.vmallocinfo {
        Some(vmallocinfo) => {
            let users = vmalloc_users(vmallocinfo);
            let total: u64 = users.iter().map(|u| u.kib).sum();
            let areas: u64 = users.iter().map(|u| u.areas).sum();
            lines.push(format!("vmalloc-total {areas} {total}"));
            for u in users.iter().take(VMALLOC_LINES) {
                lines.push(format!("vmalloc {} {} {}", u.caller, u.kib, u.areas));
            }
        }
        None => lines.push("missing vmalloc /proc/vmallocinfo could not be read".into()),
    }
    if sources.processes.is_empty() {
        lines.push("missing threads no process could be read in /proc".into());
    } else {
        let kernel: u64 = sources
            .processes
            .iter()
            .filter(|p| p.kernel)
            .map(|p| p.threads)
            .sum();
        let user: u64 = sources
            .processes
            .iter()
            .filter(|p| !p.kernel)
            .map(|p| p.threads)
            .sum();
        lines.push(format!(
            "threads-total {} {kernel} {user} {KERNEL_STACK_KIB}",
            kernel + user
        ));
        let groups = thread_groups(&sources.processes);
        for kind in [true, false] {
            for g in groups
                .iter()
                .filter(|g| g.kernel == kind)
                .take(THREAD_LINES)
            {
                lines.push(format!(
                    "threads {} {} {} {}",
                    if kind { "kernel" } else { "user" },
                    g.name,
                    g.threads,
                    g.processes
                ));
            }
        }
        let mut by_tables: Vec<&Process> = sources
            .processes
            .iter()
            .filter(|p| p.page_tables_kib > 0)
            .collect();
        by_tables.sort_by_key(|p| std::cmp::Reverse(p.page_tables_kib));
        for p in by_tables.iter().take(PAGE_TABLE_LINES) {
            lines.push(format!("page-tables {} {}", p.name, p.page_tables_kib));
        }
    }
    match sources.cgroup_stat.as_deref().map(|stat| {
        (
            cgroup_stat(stat, "nr_descendants"),
            cgroup_stat(stat, "nr_dying_descendants"),
        )
    }) {
        Some((Some(live), Some(dying))) => lines.push(format!("cgroups {live} {dying}")),
        _ => lines.push("missing cgroups /sys/fs/cgroup/cgroup.stat could not be read".into()),
    }
    match sources
        .bufio_bytes
        .as_deref()
        .and_then(|b| b.trim().parse::<u64>().ok())
    {
        Some(bytes) => lines.push(format!("bufio {bytes}")),
        None => {
            lines.push("missing bufio dm-bufio's current_allocated_bytes could not be read".into())
        }
    }
    lines.push("end".into());
    lines
}

/// Reads what the report is made from on this guest.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn read_sources() -> Sources {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let mut processes = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = entry.path();
            let Ok(status) = std::fs::read_to_string(dir.join("status")) else {
                continue;
            };
            let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
            if let Some(p) = process(pid, &status, &cmdline) {
                processes.push(p);
            }
        }
    }
    Sources {
        meminfo: read("/proc/meminfo"),
        slabinfo: read("/proc/slabinfo"),
        vmallocinfo: read("/proc/vmallocinfo"),
        processes,
        cgroup_stat: read("/sys/fs/cgroup/cgroup.stat"),
        bufio_bytes: read("/sys/module/dm_bufio/parameters/current_allocated_bytes"),
    }
}

/// Writes the report through `say`, one line each, and returns the exit status.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn run(say: &dyn Fn(&str)) -> i32 {
    for line in report(&read_sources()) {
        say(&format!("{MARK}{line}"));
    }
    0
}
