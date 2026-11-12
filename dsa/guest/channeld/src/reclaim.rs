//! When the guest hands its idle memory back to the host.
//!
//! The host gets memory back only when the guest's kernel frees it: free page reporting then
//! tells Hyper-V which pages it may take. A guest that has been running a while holds most of
//! its memory as page cache (file contents it read once), which the kernel frees only when
//! something needs the memory, and inside the guest nothing does. So while no app task is
//! open, or when most of what the guest holds is cache, the daemon asks init to drop the
//! clean caches and compact memory (`/proc/sys/vm/drop_caches`, `/proc/sys/vm/compact_memory`),
//! which frees whole 2 MiB blocks that page reporting can return.
//!
//! Page cache is only part of it. A guest with memory to spare is never short, so the kernel
//! never swaps on its own and the anonymous memory of idle processes stays resident, however
//! much zram is free. So the idle reclaim also asks the kernel to move cold anonymous pages to
//! zram (cgroup `memory.reclaim`), where they take about a third of their size.
//!
//! Memory the guest no longer uses goes back at once, not over minutes: the first look after
//! the last task closes reclaims, and while idle every look that finds memory grown back
//! reclaims again. Dropping caches costs reading those files again later, so the full drop
//! with tasks open waits for cache to dominate, at most every few minutes.
//! When the host says it is short of memory, the guest gives back what it can at once, as a
//! phone's low memory killer would: the agent ends the cached app processes first, and the
//! daemon's next look reclaims whatever that and the caches freed, whether tasks are open or
//! not, and again while the host stays short each time memory has grown back since.
//!
//! Between those, and whether or not the host is short, the guest keeps only its working set:
//! all the page cache beyond a target goes back in one trim of the coldest pages, on the next
//! look that finds it, while the guest is not under memory pressure. Only an app that has just
//! opened holds the trim back for a while, because it is still reading its files, and that
//! protects only its own memory: an app that closes has what it left returned the moment its
//! processes exit ([`Reclaimer::app_closed`]), whatever else is opening. The target follows the
//! guest's own signal, the evicted file pages it reads back (`workingset_refault_file`): cache
//! that returns soon after a trim was in use, so the target rises; while none returns, it falls
//! back to its floor.
//!
//! This module is the decision alone, so it runs and is tested on any platform; the daemon
//! applies it on Android by setting [`RECLAIM_PROPERTY`], which the image's init script acts on.

use std::time::Duration;

/// The property init watches: set to `1`, init drops the caches, compacts memory and sets it
/// back to `0`.
pub const RECLAIM_PROPERTY: &str = "vendor.dsa.memory.reclaim";

/// The property init watches for the working set trim: set to `1`, init asks the kernel to
/// reclaim [`TRIM_SIZE_PROPERTY`] MiB of the coldest page cache (cgroup `memory.reclaim`) and
/// sets it back to `0`.
pub const TRIM_PROPERTY: &str = "vendor.dsa.memory.trim";

/// How much the next working set trim takes, in MiB. The daemon sets it before [`TRIM_PROPERTY`].
pub const TRIM_SIZE_PROPERTY: &str = "vendor.dsa.memory.trim_size";

/// The property init reads for the size of each of the idle reclaim's four steps that move
/// cold anonymous pages to zram, in MiB. The daemon sets it before [`RECLAIM_PROPERTY`].
pub const RECLAIM_STEP_PROPERTY: &str = "vendor.dsa.memory.reclaim_step";

/// The size of each of the idle reclaim's four zram steps, in MiB: together, half of the
/// guest's anonymous memory, the coldest first. A fixed gigabyte took more than the guest
/// had, so the hot pages of the processes still running (the system server, the display)
/// went to zram too and came straight back, each time as a compressed copy and then a
/// decompressed one, both in pages the host had already been given back. The host's commit
/// for the VM then rose for seconds after the windows closed instead of falling.
pub fn reclaim_step_mib(memory: &MemInfo) -> u64 {
    (memory.anon_pages_kb / 2 / 4 / 1024).max(1)
}

/// How often the daemon looks at the guest's memory. Free memory reaches the host on the
/// look after it is freed, so this is how long it can sit in the guest.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// The longest the reclaim after an app closes waits for that app's processes to exit. The
/// agent ends them as the window closes, so they are normally gone within milliseconds; one
/// it keeps running (a widget, a foreground service) does not hold the reclaim back longer.
pub const EXIT_WAIT_LIMIT: Duration = Duration::from_secs(2);

/// How often the wait for a closed app's processes looks again.
pub const EXIT_POLL: Duration = Duration::from_millis(20);

/// Whether one of `cmdlines` (the contents of `/proc/<pid>/cmdline`) is a process of
/// `package`: Android names an app's processes after its package, and its other processes
/// `<package>:<name>`.
pub fn package_running(cmdlines: &[String], package: &str) -> bool {
    cmdlines.iter().any(|line| {
        let name = line.split('\0').next().unwrap_or("");
        name == package
            || name
                .strip_prefix(package)
                .is_some_and(|rest| rest.starts_with(':'))
    })
}

/// The fields of `/proc/meminfo` the decision uses, in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub mem_total_kb: u64,
    pub mem_free_kb: u64,
    pub buffers_kb: u64,
    pub cached_kb: u64,
    pub shmem_kb: u64,
    pub slab_reclaimable_kb: u64,
    pub anon_pages_kb: u64,
}

impl MemInfo {
    /// Reads `/proc/meminfo`'s `Name: value kB` lines. A line it does not know or cannot
    /// read is skipped.
    pub fn parse(text: &str) -> MemInfo {
        let mut info = MemInfo::default();
        for line in text.lines() {
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let Some(Ok(value)) = rest.split_whitespace().next().map(str::parse::<u64>) else {
                continue;
            };
            match name.trim() {
                "MemTotal" => info.mem_total_kb = value,
                "MemFree" => info.mem_free_kb = value,
                "Buffers" => info.buffers_kb = value,
                "Cached" => info.cached_kb = value,
                "Shmem" => info.shmem_kb = value,
                "SReclaimable" => info.slab_reclaimable_kb = value,
                "AnonPages" => info.anon_pages_kb = value,
                _ => {}
            }
        }
        info
    }

    /// The memory in use that dropping caches can free: the page cache without shared
    /// memory (which lives in it but cannot be dropped), buffers, and reclaimable slab.
    pub fn droppable_kb(&self) -> u64 {
        self.cached_kb
            .saturating_sub(self.shmem_kb)
            .saturating_add(self.buffers_kb)
            .saturating_add(self.slab_reclaimable_kb)
    }

    /// What an idle reclaim can act on: the droppable memory, and anonymous memory it can
    /// swap to zram.
    pub fn reclaimable_kb(&self) -> u64 {
        self.droppable_kb().saturating_add(self.anon_pages_kb)
    }

    /// The memory in use, free memory aside.
    pub fn used_kb(&self) -> u64 {
        self.mem_total_kb.saturating_sub(self.mem_free_kb)
    }
}

/// Whether `/proc/pressure/memory` shows the guest short of memory: tasks stalled on it for
/// `threshold` percent or more of the last 10 seconds (the `some` line's `avg10`). Text that
/// cannot be read counts as pressure, so the trim stays out of the way when unsure.
pub fn memory_pressure(text: &str, threshold: f64) -> bool {
    let avg10 = text
        .lines()
        .find_map(|line| line.strip_prefix("some "))
        .and_then(|rest| {
            rest.split_whitespace()
                .find_map(|f| f.strip_prefix("avg10="))
        })
        .and_then(|v| v.parse::<f64>().ok());
    avg10.is_none_or(|v| v >= threshold)
}

/// How much page cache the kernel has read back soon after evicting it, in KiB: the
/// `workingset_refault_file` count of `/proc/vmstat` (in pages of `page_kb` KiB). It only
/// grows; the reclaimer looks at how much it grew between two looks. `None` when the line is
/// missing.
pub fn file_refaults_kb(vmstat: &str, page_kb: u64) -> Option<u64> {
    vmstat.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("workingset_refault_file") {
            return None;
        }
        fields
            .next()?
            .parse::<u64>()
            .ok()
            .map(|pages| pages.saturating_mul(page_kb))
    })
}

/// Why the daemon asks for a reclaim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No app task is open.
    Idle,
    /// Most of the memory in use is cache the guest can drop.
    MostlyCache,
    /// Cold cache has piled up past the guest's working set: all of it
    /// ([`Reclaimer::trim_mib`]) goes back to the host at once. It runs whether or not the
    /// host is short of memory, so the guest keeps only what it uses.
    Gradual,
    /// The host is short of memory.
    HostShort,
    /// An app's last window closed: what it held goes back now, whatever else is opening.
    AppClosed,
}

/// The decision's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimPolicy {
    /// How long the guest must have no app task before its idle reclaim. None by default:
    /// the first look after the last task closes reclaims.
    pub idle_after: Duration,
    /// The least time between two reclaims.
    pub min_gap: Duration,
    /// The share of the memory in use, in percent, that droppable cache must reach for a
    /// reclaim while tasks are open.
    pub cache_percent: u64,
    /// The least droppable cache worth a reclaim, in KiB: below it the cost of reading the
    /// files again outweighs what the host gets back. While idle, anonymous memory counts too.
    pub min_droppable_kb: u64,
    /// The least time between two idle reclaims in one idle stretch.
    pub idle_repeat: Duration,
    /// How much reclaimable memory must have come back since the last idle reclaim for
    /// another, in KiB. A guest that stays small is left alone.
    pub idle_growth_kb: u64,
    /// The least time between two working set trims.
    pub trim_gap: Duration,
    /// The least page cache the working set trim leaves alone, in KiB: warm files of the open
    /// apps. It is where the working set target starts and the lowest it goes.
    pub keep_cache_kb: u64,
    /// The most the working set target rises to, in KiB, however often evicted files are read
    /// back.
    pub keep_cache_max_kb: u64,
    /// The least cache above the working set target worth a trim, in KiB.
    pub min_trim_kb: u64,
    /// The most one trim takes, in KiB. Unbounded by default: an excess that went back in
    /// slices would sit in the guest for several looks.
    pub max_trim_kb: u64,
    /// How long after an app opens the trims and cache drops wait: an app that has just
    /// started is reading its files, and trimming them then would make it read them twice.
    /// An app closing does not wait: what it read is no longer anyone's.
    pub settle_after: Duration,
    /// How much evicted page cache read back between two looks, in KiB, says the cache trimmed
    /// was in use: the target rises by `min_trim_kb` and that look trims nothing. A quarter of
    /// it or less lets the target fall back by `min_trim_kb`.
    pub refault_limit_kb: u64,
    /// The least time between two reclaims while the host is short of memory. The second also
    /// waits for [`ReclaimPolicy::idle_growth_kb`] to come back since the first.
    pub host_short_gap: Duration,
}

impl Default for ReclaimPolicy {
    fn default() -> Self {
        ReclaimPolicy {
            idle_after: Duration::ZERO,
            min_gap: Duration::from_secs(10 * 60),
            cache_percent: 50,
            min_droppable_kb: 128 * 1024,
            idle_repeat: Duration::ZERO,
            idle_growth_kb: 64 * 1024,
            trim_gap: Duration::ZERO,
            keep_cache_kb: 256 * 1024,
            keep_cache_max_kb: 1024 * 1024,
            min_trim_kb: 64 * 1024,
            max_trim_kb: u64::MAX,
            settle_after: Duration::from_secs(30),
            refault_limit_kb: 16 * 1024,
            host_short_gap: Duration::from_secs(60),
        }
    }
}

/// Decides, each time it is asked, whether the guest should drop its caches now.
#[derive(Debug, Clone)]
pub struct Reclaimer {
    policy: ReclaimPolicy,
    /// When the guest last had no task after having one (or since start).
    idle_since: Option<Duration>,
    /// Whether the current idle stretch has had its reclaim.
    idle_reclaimed: bool,
    last_reclaim: Option<Duration>,
    last_trim: Option<Duration>,
    /// Whether the next look at memory is the first since a reclaim, which sets `idle_floor_kb`.
    measure_floor: bool,
    /// What was left reclaimable just after the last reclaim, in KiB.
    idle_floor_kb: Option<u64>,
    /// The host said it is short of memory.
    host_short: bool,
    /// The page cache the apps are taken to need, in KiB: the working set trim leaves this much.
    /// It follows the refaults (see [`Reclaimer::refaults`]).
    target_kb: u64,
    /// The refault count at the last look, in KiB.
    last_refaults_kb: Option<u64>,
    /// Evicted cache came back since the last look: this look trims nothing.
    refaulting: bool,
    /// The number of open tasks at the last look, and when it last changed.
    last_tasks: Option<usize>,
    tasks_changed: Option<Duration>,
    /// What the last working set trim asks for, in KiB.
    trim_kb: u64,
    /// The least the target falls to now, in KiB: `keep_cache_kb`, raised when what a trim
    /// took came back (see [`Reclaimer::refaults`]), and back to `keep_cache_kb` when an app
    /// closes.
    floor_kb: u64,
    /// The last trim not yet judged: the refault count when it ran, what it took and the
    /// target it left, in KiB.
    trim_mark: Option<TrimMark>,
    /// Looks left in which refaults say nothing about the working set: a full reclaim dropped
    /// every cache, and what the running apps read back right after is their own working set
    /// coming back, not cache a trim took by mistake.
    refault_settle: u32,
    /// A reclaim for the host's shortage ran in this stretch of it: the next waits for memory
    /// to come back first ([`ReclaimPolicy::idle_growth_kb`]).
    host_short_reclaimed: bool,
}

/// How many looks after a full reclaim the refaults are not counted (half a minute at
/// [`CHECK_INTERVAL`]). On the owner's PC each cache drop for a host short of memory was
/// followed by some 400 MiB read back within seconds; counted, it raised the working set target
/// 64 MiB a look toward 1 GiB, and the trim then left 500 to 600 MiB of cache in place.
pub const REFAULT_SETTLE_LOOKS: u32 = 6;

/// A working set trim, as the next looks judge it by what of it comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrimMark {
    refaults_kb: u64,
    took_kb: u64,
    left_kb: u64,
}

impl Reclaimer {
    pub fn new(policy: ReclaimPolicy) -> Reclaimer {
        Reclaimer {
            policy,
            idle_since: None,
            idle_reclaimed: false,
            last_reclaim: None,
            last_trim: None,
            measure_floor: false,
            idle_floor_kb: None,
            host_short: false,
            target_kb: policy.keep_cache_kb,
            last_refaults_kb: None,
            refaulting: false,
            last_tasks: None,
            tasks_changed: None,
            trim_kb: 0,
            floor_kb: policy.keep_cache_kb,
            trim_mark: None,
            refault_settle: 0,
            host_short_reclaimed: false,
        }
    }

    /// The apps the working set was learnt for changed: it starts again from `keep_cache_kb`.
    fn forget_working_set(&mut self) {
        self.target_kb = self.policy.keep_cache_kb;
        self.floor_kb = self.policy.keep_cache_kb;
        self.trim_mark = None;
        self.refaulting = false;
    }

    /// Notes a working set trim of `took_kb` that leaves the target: the looks after it judge
    /// it by what of it is read back. The trim before it, if no look found a quarter of it
    /// back, took cache no app used, so the floor steps down toward `keep_cache_kb`: half the
    /// way, at least `min_trim_kb`. A fixed 64 MiB a step took a floor learnt at 1 GiB more than
    /// ten trims to come back down while the apps that needed it were long gone.
    fn mark_trim(&mut self, took_kb: u64) {
        if self.trim_mark.take().is_some() {
            let keep = self.policy.keep_cache_kb;
            let step = (self.floor_kb.saturating_sub(keep) / 2).max(self.policy.min_trim_kb);
            self.floor_kb = self.floor_kb.saturating_sub(step).max(keep);
        }
        if let Some(refaults_kb) = self.last_refaults_kb {
            self.trim_mark = Some(TrimMark {
                refaults_kb,
                took_kb,
                left_kb: self.target_kb,
            });
        }
    }

    /// Takes in the guest's file refault count ([`file_refaults_kb`]), for the decision that
    /// follows. Cache read back soon after a trim was cache an app still uses, so the working
    /// set target rises and the trim holds off; while little comes back, the target falls back
    /// toward its floor, so cache no app uses any more goes back to the host.
    ///
    /// A look at a time sees only a few seconds of refaults, and an app that reads its files
    /// back slowly never reached the limit of one look: on the owner's PC, with only the Play
    /// Store shown, a trim of about 100 MiB came back at 8 MiB a look, 60 % of it within
    /// 20 s, and the next trim took it again, every 10 to 30 s for as long as the window was
    /// shown (16 MiB read back every 10 s). So each trim is also judged by what of it came
    /// back since it ran: once a quarter of it has, it took part of the working set, and the
    /// floor rises to what it left plus what came back. The floor steps down again only after
    /// a trim none of the looks found a quarter of back.
    pub fn refaults(&mut self, total_kb: u64) {
        let grown = self
            .last_refaults_kb
            .map(|last| total_kb.saturating_sub(last));
        self.last_refaults_kb = Some(total_kb);
        if self.refault_settle > 0 {
            self.refault_settle -= 1;
            return;
        }
        let Some(grown) = grown else {
            return;
        };
        let p = self.policy;
        let mut learnt = false;
        if let Some(mark) = self.trim_mark {
            let back = total_kb.saturating_sub(mark.refaults_kb);
            if mark.took_kb > 0 && back.saturating_mul(4) >= mark.took_kb {
                self.trim_mark = None;
                self.floor_kb = self
                    .floor_kb
                    .max(mark.left_kb.saturating_add(back))
                    .min(p.keep_cache_max_kb);
                self.target_kb = self.target_kb.max(self.floor_kb);
                learnt = true;
            }
        }
        if grown >= p.refault_limit_kb {
            self.refaulting = true;
            self.target_kb = (self.target_kb + p.min_trim_kb).min(p.keep_cache_max_kb);
        } else {
            self.refaulting = learnt;
            if !learnt && grown.saturating_mul(4) <= p.refault_limit_kb {
                self.target_kb = self
                    .target_kb
                    .saturating_sub(p.min_trim_kb)
                    .max(self.floor_kb);
            }
        }
    }

    /// The page cache the working set trim leaves now, in KiB.
    pub fn working_set_target_kb(&self) -> u64 {
        self.target_kb
    }

    /// How much the last [`Reason::Gradual`] asks for, in whole MiB.
    pub fn trim_mib(&self) -> u64 {
        self.trim_kb / 1024
    }

    /// What to do the moment an app's last window has closed and its processes have exited,
    /// with `tasks` tasks still open. It never waits. An app that opened a moment ago keeps
    /// its own working set: the trim then takes page cache only, above the floor, so the new
    /// app's memory stays resident. Otherwise everything the closed app left goes back in a
    /// full reclaim ([`Reason::AppClosed`]). `None` only when there is nothing to take.
    pub fn app_closed(&mut self, now: Duration, tasks: usize, memory: &MemInfo) -> Option<Reason> {
        // The closed app's cache is not the working set any more, and the refaults it caused
        // say nothing about the apps left.
        self.forget_working_set();
        self.last_tasks = Some(tasks);
        let opening = tasks > 0
            && self
                .tasks_changed
                .is_some_and(|at| now.saturating_sub(at) < self.policy.settle_after);
        if opening {
            let excess = memory.droppable_kb().saturating_sub(self.target_kb);
            self.trim_kb = excess.min(self.policy.max_trim_kb) / 1024 * 1024;
            if self.trim_kb == 0 {
                return None;
            }
            self.last_trim = Some(now);
            self.mark_trim(self.trim_kb);
            return Some(Reason::Gradual);
        }
        if tasks == 0 {
            self.idle_since.get_or_insert(now);
            self.idle_reclaimed = true;
        }
        self.last_reclaim = Some(now);
        self.last_trim = Some(now);
        self.measure_floor = true;
        self.refault_settle = REFAULT_SETTLE_LOOKS;
        Some(Reason::AppClosed)
    }

    /// Takes in whether the host is short of memory, for the decisions that follow.
    pub fn host_short(&mut self, short: bool) {
        if !short {
            self.host_short_reclaimed = false;
        }
        self.host_short = short;
    }

    /// Whether to reclaim at `now` (any monotonic clock), with `tasks` app tasks open and
    /// memory as `memory` says. A `Some` counts as done: the next one waits for its turn.
    pub fn decide(&mut self, now: Duration, tasks: usize, memory: &MemInfo) -> Option<Reason> {
        self.decide_under(now, tasks, memory, false)
    }

    /// Like [`Reclaimer::decide`], told whether the guest is under memory pressure. Under
    /// pressure the working set trim waits, so it never competes with an app for memory.
    pub fn decide_under(
        &mut self,
        now: Duration,
        tasks: usize,
        memory: &MemInfo,
        pressure: bool,
    ) -> Option<Reason> {
        if tasks > 0 {
            self.idle_since = None;
            self.idle_reclaimed = false;
        } else if self.idle_since.is_none() {
            self.idle_since = Some(now);
        }
        match self.last_tasks {
            Some(last) if tasks > last => self.tasks_changed = Some(now),
            Some(last) if tasks < last => {
                // An app closed: the cache it used is not the working set any more, and the
                // refaults it caused say nothing about the apps left.
                self.forget_working_set();
            }
            _ => {}
        }
        self.last_tasks = Some(tasks);
        let settled = self
            .tasks_changed
            .is_none_or(|at| now.saturating_sub(at) >= self.policy.settle_after);
        if self.measure_floor {
            self.measure_floor = false;
            self.idle_floor_kb = Some(memory.reclaimable_kb());
        }
        let worth_it = memory.droppable_kb() >= self.policy.min_droppable_kb;
        let idle_worth_it = memory.reclaimable_kb() >= self.policy.min_droppable_kb;
        let gap_passed = self
            .last_reclaim
            .is_none_or(|last| now.saturating_sub(last) >= self.policy.min_gap);
        let idle_due = !self.idle_reclaimed
            && self
                .idle_since
                .is_some_and(|since| now.saturating_sub(since) >= self.policy.idle_after);
        // Background work during a long idle stretch, and a boot still under way at the first
        // reclaim, bring memory back that nothing would otherwise return.
        let idle_grew = self.idle_reclaimed
            && self.idle_since.is_some()
            && self
                .last_reclaim
                .is_some_and(|last| now.saturating_sub(last) >= self.policy.idle_repeat)
            && self
                .idle_floor_kb
                .is_some_and(|floor| memory.reclaimable_kb() >= floor + self.policy.idle_growth_kb);
        let mostly_cache = memory.droppable_kb().saturating_mul(100)
            >= memory.used_kb().saturating_mul(self.policy.cache_percent);
        // Going idle is the moment the guest stops needing its cache, so the gap does not hold
        // the idle reclaim back.
        // While the host stays short, the reclaim runs again only once memory has come back
        // since the last one. Dropping the caches again when nothing grew only drops the open
        // apps' working set, which they read straight back: on the owner's PC a drop every
        // minute found 580 to 870 MiB droppable each time, refilled within seconds.
        let host_short_grew = !self.host_short_reclaimed
            || self
                .idle_floor_kb
                .is_some_and(|floor| memory.reclaimable_kb() >= floor + self.policy.idle_growth_kb);
        let host_short_due = self.host_short
            && idle_worth_it
            && host_short_grew
            && self
                .last_reclaim
                .is_none_or(|last| now.saturating_sub(last) >= self.policy.host_short_gap);
        let reason = if (idle_due || idle_grew) && idle_worth_it {
            self.idle_reclaimed = true;
            Some(Reason::Idle)
        } else if host_short_due {
            self.host_short_reclaimed = true;
            Some(Reason::HostShort)
        } else if settled && gap_passed && worth_it && mostly_cache {
            Some(Reason::MostlyCache)
        } else if settled
            && (tasks > 0 || self.idle_reclaimed)
            && !pressure
            && !self.refaulting
            && self
                .last_trim
                .is_none_or(|last| now.saturating_sub(last) >= self.policy.trim_gap)
            && memory.droppable_kb() >= self.target_kb + self.policy.min_trim_kb
        {
            // Whether or not tasks are open or the host is short: cache beyond the working
            // set is the host's. Idle stretches get their full reclaim above; between those,
            // and with apps open, the trim keeps the cache at the target.
            self.last_trim = Some(now);
            let excess = memory.droppable_kb() - self.target_kb;
            // Whole MiB, as init's script writes it.
            self.trim_kb = excess.min(self.policy.max_trim_kb) / 1024 * 1024;
            self.mark_trim(self.trim_kb);
            return Some(Reason::Gradual);
        } else {
            None
        };
        if reason.is_some() {
            self.last_reclaim = Some(now);
            // A full reclaim also counts as the latest trim: no second one right behind it.
            self.last_trim = Some(now);
            self.measure_floor = true;
            // It dropped every cache, so what is read back now says nothing about the trim
            // before it, nor about the working set until the apps have read theirs back.
            self.trim_mark = None;
            self.refault_settle = REFAULT_SETTLE_LOOKS;
        }
        reason
    }
}

/// The kernel files the reclaimer reads on every check, and which of them could not be read
/// last time. A file that cannot be read (SELinux refuses it, or the kernel lacks it) is
/// reported once, loudly, with what the reclaimer loses without it, and again only after it
/// has been read in between. Reporting it on every check would bury the log; never reporting
/// it would leave idle memory in the guest with no word of why.
#[derive(Debug, Default)]
pub struct ReadFailures {
    failing: std::collections::BTreeSet<String>,
}

impl ReadFailures {
    /// The line to log for this read of `path`, if any: `Some` the first time it fails (at the
    /// start, or after it was read), `None` while it keeps failing and when it succeeds.
    /// `loses` says what the reclaimer cannot do without the file.
    pub fn note<T>(
        &mut self,
        path: &str,
        result: &std::io::Result<T>,
        loses: &str,
    ) -> Option<String> {
        match result {
            Ok(_) => {
                self.failing.remove(path);
                None
            }
            Err(error) => {
                if !self.failing.insert(path.to_string()) {
                    return None;
                }
                let cause = if error.kind() == std::io::ErrorKind::PermissionDenied {
                    "the SELinux policy does not let dsa-channeld read it (look for an avc \
                     denial in the kernel log)"
                        .to_string()
                } else {
                    error.to_string()
                };
                Some(format!(
                    "WARNING: cannot read {path}: {cause}. Until it can, {loses}. This is \
                     reported once."
                ))
            }
        }
    }
}
