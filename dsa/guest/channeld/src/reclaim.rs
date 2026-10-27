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
//! Dropping caches costs reading those files again later, so it is not done often: once each
//! time the guest goes idle, and otherwise at most every few minutes while cache dominates.
//! This module is the decision alone, so it runs and is tested on any platform; the daemon
//! applies it on Android by setting [`RECLAIM_PROPERTY`], which the image's init script acts on.

use std::time::Duration;

/// The property init watches: set to `1`, init drops the caches, compacts memory and sets it
/// back to `0`.
pub const RECLAIM_PROPERTY: &str = "vendor.dsa.memory.reclaim";

/// The property init watches for the gradual trim: set to `1`, init drops only the page
/// cache and sets it back to `0`.
pub const TRIM_PROPERTY: &str = "vendor.dsa.memory.trim";

/// How often the daemon looks at the guest's memory.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// The fields of `/proc/meminfo` the decision uses, in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemInfo {
    pub mem_total_kb: u64,
    pub mem_free_kb: u64,
    pub buffers_kb: u64,
    pub cached_kb: u64,
    pub shmem_kb: u64,
    pub slab_reclaimable_kb: u64,
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

/// Why the daemon asks for a reclaim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No app task has been open for a while.
    Idle,
    /// Most of the memory in use is cache the guest can drop.
    MostlyCache,
    /// Tasks are open and cold cache has piled up past what the guest keeps for itself: a
    /// slice of page cache goes back to the host, as WSL's gradual reclaim does.
    Gradual,
}

/// The decision's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimPolicy {
    /// How long the guest must have no app task before its idle reclaim.
    pub idle_after: Duration,
    /// The least time between two reclaims.
    pub min_gap: Duration,
    /// The share of the memory in use, in percent, that droppable cache must reach for a
    /// reclaim while tasks are open.
    pub cache_percent: u64,
    /// The least droppable cache worth a reclaim, in KiB: below it the cost of reading the
    /// files again outweighs what the host gets back.
    pub min_droppable_kb: u64,
    /// The time between two gradual trims while tasks are open.
    pub trim_gap: Duration,
    /// The page cache the gradual trim leaves alone, in KiB: warm files of the open apps.
    /// A trim happens only when the droppable memory is this much plus `min_trim_kb`.
    pub keep_cache_kb: u64,
    /// The least cache above `keep_cache_kb` worth a trim, in KiB.
    pub min_trim_kb: u64,
}

impl Default for ReclaimPolicy {
    fn default() -> Self {
        ReclaimPolicy {
            idle_after: Duration::from_secs(60),
            min_gap: Duration::from_secs(10 * 60),
            cache_percent: 50,
            min_droppable_kb: 128 * 1024,
            trim_gap: Duration::from_secs(45),
            keep_cache_kb: 256 * 1024,
            min_trim_kb: 64 * 1024,
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
}

impl Reclaimer {
    pub fn new(policy: ReclaimPolicy) -> Reclaimer {
        Reclaimer {
            policy,
            idle_since: None,
            idle_reclaimed: false,
            last_reclaim: None,
            last_trim: None,
        }
    }

    /// Whether to reclaim at `now` (any monotonic clock), with `tasks` app tasks open and
    /// memory as `memory` says. A `Some` counts as done: the next one waits for its turn.
    pub fn decide(&mut self, now: Duration, tasks: usize, memory: &MemInfo) -> Option<Reason> {
        self.decide_under(now, tasks, memory, false)
    }

    /// Like [`Reclaimer::decide`], told whether the guest is under memory pressure. Under
    /// pressure the gradual trim waits, so it never competes with an app for memory.
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
        let worth_it = memory.droppable_kb() >= self.policy.min_droppable_kb;
        let gap_passed = self
            .last_reclaim
            .is_none_or(|last| now.saturating_sub(last) >= self.policy.min_gap);
        let idle_due = !self.idle_reclaimed
            && self
                .idle_since
                .is_some_and(|since| now.saturating_sub(since) >= self.policy.idle_after);
        let mostly_cache = memory.droppable_kb().saturating_mul(100)
            >= memory.used_kb().saturating_mul(self.policy.cache_percent);
        // Going idle is the moment the guest stops needing its cache, so the gap does not hold
        // the idle reclaim back.
        let reason = if idle_due && worth_it {
            self.idle_reclaimed = true;
            Some(Reason::Idle)
        } else if gap_passed && worth_it && mostly_cache {
            Some(Reason::MostlyCache)
        } else if tasks > 0
            && !pressure
            && self
                .last_trim
                .is_none_or(|last| now.saturating_sub(last) >= self.policy.trim_gap)
            && memory.droppable_kb() >= self.policy.keep_cache_kb + self.policy.min_trim_kb
        {
            self.last_trim = Some(now);
            return Some(Reason::Gradual);
        } else {
            None
        };
        if reason.is_some() {
            self.last_reclaim = Some(now);
        }
        reason
    }
}
