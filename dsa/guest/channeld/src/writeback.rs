//! When what stays cold in zram goes on to its backing file on the guest's data disk.
//!
//! zram keeps idle apps' memory compressed, at about a third of its size, but that third is
//! still the host's memory. Much of it is never read again while Android dozes: the memory of
//! Google Play services and of cached apps that nothing wakes. zram can write such pages to a
//! backing device (writeback), after which they cost the host disk space instead of memory, and
//! a page read back is read from the disk once and leaves it. The guest's backing device is a
//! file on its data disk, `/data/per_boot/zram_swap`, which fs_mgr makes each boot.
//!
//! Writing wears the user's disk, so the writeback is narrow:
//!
//! - only while no app is in use, and only after none has been for a while
//!   ([`WritebackPolicy::idle_after`]): the guest dozes, and nothing it writes is in use;
//! - only pages no one has read or written for a long while ([`WritebackPolicy::age`]), which
//!   zram tells by the time it keeps for each page;
//! - at most [`WritebackPolicy::run_pages`] a run, so init, which does nothing else during the
//!   write, is busy for a fraction of a second, and at most one run every
//!   [`WritebackPolicy::gap`];
//! - at most [`WritebackPolicy::daily_pages`] a day, counted across restarts of Android in a
//!   persistent property ([`Day`]). zram itself stops at the budget the run is given
//!   (`writeback_limit`), so a run never writes more than it was allowed, whatever the daemon
//!   believes it wrote.
//!
//! zstd's second compression (the idle reclaim) still comes first: a page that stays in memory
//! is better small, and the writeback takes only what has stayed cold far longer than that.
//!
//! This module is the decision and the parsing of zram's statistics, so it runs and is tested on
//! any platform; the daemon applies it on Android by setting [`WRITEBACK_PROPERTY`], which the
//! image's init script acts on.

use std::time::Duration;

/// The property init watches: set to `1`, init gives zram the run's budget
/// ([`WRITEBACK_PAGES_PROPERTY`]), marks the pages unused for the age idle, writes the
/// incompressible ones and then the rest, takes the budget away and sets this back to `0`.
pub const WRITEBACK_PROPERTY: &str = "vendor.dsa.memory.writeback";

/// How many 4 KiB pages the next run may write. The daemon sets it before
/// [`WRITEBACK_PROPERTY`].
pub const WRITEBACK_PAGES_PROPERTY: &str = "vendor.dsa.memory.writeback_pages";

/// How long no app must be in use before a writeback, in seconds. init sets it at boot, from the
/// boot's `androidboot.dsa.zram_writeback_after` or its default.
pub const WRITEBACK_AFTER_PROPERTY: &str = "vendor.dsa.memory.writeback_after";

/// How long a page must have gone unused before it is written, in seconds. init sets it at
/// boot, from the boot's `androidboot.dsa.zram_writeback_age` or its default, and marks the
/// pages idle with it.
pub const WRITEBACK_AGE_PROPERTY: &str = "vendor.dsa.memory.writeback_age";

/// What the writeback wrote on which day ([`Day`]), kept across restarts of Android.
pub const WRITEBACK_DAY_PROPERTY: &str = "persist.vendor.dsa.memory.writeback_day";

/// zram's folder in sysfs.
pub const ZRAM: &str = "/sys/devices/virtual/block/zram0";

/// How often the daemon looks whether a writeback is due.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// How long the daemon waits for init to finish a run before it stops waiting.
pub const RUN_WAIT_LIMIT: Duration = Duration::from_secs(120);

/// The unit zram counts its writeback budget and its backing device's statistics in.
pub const PAGE_KB: u64 = 4;

const MIB_PAGES: u64 = 1024 / PAGE_KB;

/// The decision's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WritebackPolicy {
    /// How long no app must have been in use before a run. Ten minutes by default: the guest
    /// has dozed off, and the idle reclaim has already swapped and compressed what it holds.
    pub idle_after: Duration,
    /// How long a page must have gone unused to be written. Half an hour by default: a page
    /// untouched that long, with no app open, is one the guest is not about to read.
    pub age: Duration,
    /// The least time between two runs. Ten minutes by default.
    pub gap: Duration,
    /// The most one run writes, in 4 KiB pages: 32 MiB by default. zram writes one page at a
    /// time and waits for each, and init does nothing else meanwhile.
    pub run_pages: u64,
    /// The most written in a day, in 4 KiB pages: 512 MiB by default. A day's budget is
    /// what one first long doze needs to write out the cold part of a full zram (about 480 MiB
    /// before compression on the owner's PC); a page once written stays on the disk until it is
    /// read back, so later days write only what has newly gone cold.
    pub daily_pages: u64,
    /// The least zram must keep in memory for a run to be worth it, in KiB: below it, the
    /// scan of zram's pages and the disk's wake-up cost more than they save.
    pub min_store_kb: u64,
}

impl Default for WritebackPolicy {
    fn default() -> Self {
        WritebackPolicy {
            idle_after: Duration::from_secs(10 * 60),
            age: Duration::from_secs(30 * 60),
            gap: Duration::from_secs(10 * 60),
            run_pages: 32 * MIB_PAGES,
            daily_pages: 512 * MIB_PAGES,
            min_store_kb: 8 * 1024,
        }
    }
}

impl WritebackPolicy {
    /// The default policy with the times init set at boot ([`WRITEBACK_AFTER_PROPERTY`] and
    /// [`WRITEBACK_AGE_PROPERTY`], in seconds). A value that is empty or not a number keeps the
    /// default.
    pub fn with_times(after: &str, age: &str) -> WritebackPolicy {
        let mut policy = WritebackPolicy::default();
        if let Ok(secs) = after.trim().parse::<u64>() {
            policy.idle_after = Duration::from_secs(secs);
        }
        if let Ok(secs) = age.trim().parse::<u64>() {
            policy.age = Duration::from_secs(secs);
        }
        policy
    }
}

/// What the writeback wrote on one day, in 4 KiB pages. The day is the number of whole days
/// since 1970 by the guest's clock, which follows the host's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub day: u64,
    pub pages: u64,
}

impl Day {
    /// Reads `<day> <pages>`, as [`Day::to_property`] writes it. `None` when it is anything else.
    pub fn parse(text: &str) -> Option<Day> {
        let mut fields = text.split_whitespace();
        let day = fields.next()?.parse().ok()?;
        let pages = fields.next()?.parse().ok()?;
        fields.next().is_none().then_some(Day { day, pages })
    }

    /// The value of [`WRITEBACK_DAY_PROPERTY`].
    pub fn to_property(self) -> String {
        format!("{} {}", self.day, self.pages)
    }
}

/// The day `unix_secs` (seconds since 1970) falls on.
pub fn day_of(unix_secs: u64) -> u64 {
    unix_secs / 86_400
}

/// The fields of zram's `mm_stat` the writeback reports, in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ZramStat {
    /// What zram holds, before compression.
    pub orig_kb: u64,
    /// What zram's pages take in memory (`mem_used_total`).
    pub mem_used_kb: u64,
    /// How many of its pages are stored uncompressed (`huge_pages`).
    pub huge_pages: u64,
}

impl ZramStat {
    /// Reads `mm_stat`: `orig_data_size compr_data_size mem_used_total mem_limit mem_used_max
    /// same_pages pages_compacted huge_pages ...`, sizes in bytes. `None` when it has fewer
    /// than three numbers.
    pub fn parse(text: &str) -> Option<ZramStat> {
        let fields: Vec<u64> = text
            .split_whitespace()
            .map_while(|f| f.parse().ok())
            .collect();
        if fields.len() < 3 {
            return None;
        }
        Some(ZramStat {
            orig_kb: fields[0] / 1024,
            mem_used_kb: fields[2] / 1024,
            huge_pages: fields.get(7).copied().unwrap_or(0),
        })
    }
}

/// zram's `bd_stat`, in KiB: what is on the backing device now, and what was read from it and
/// written to it since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackingStat {
    pub stored_kb: u64,
    pub read_kb: u64,
    pub written_kb: u64,
}

impl BackingStat {
    /// Reads `bd_stat`: `bd_count bd_reads bd_writes`, in 4 KiB units. `None` when it is not
    /// three numbers.
    pub fn parse(text: &str) -> Option<BackingStat> {
        let fields: Vec<u64> = text
            .split_whitespace()
            .map(|f| f.parse().ok())
            .collect::<Option<_>>()?;
        match fields[..] {
            [stored, read, written] => Some(BackingStat {
                stored_kb: stored * PAGE_KB,
                read_kb: read * PAGE_KB,
                written_kb: written * PAGE_KB,
            }),
            _ => None,
        }
    }
}

/// Whether zram's `backing_dev` names a device: zram says `none` when it has none.
pub fn has_backing_device(backing_dev: &str) -> bool {
    let name = backing_dev.trim();
    !name.is_empty() && name != "none"
}

/// One run, as the daemon logs it: the line the boot check reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub before: ZramStat,
    pub after: ZramStat,
    pub backing_before: BackingStat,
    pub backing_after: BackingStat,
    pub took: Duration,
    /// What is left of the day's budget after the run, and the budget, in 4 KiB pages.
    pub left_pages: u64,
    pub daily_pages: u64,
}

impl Run {
    /// What the run wrote, in 4 KiB pages.
    pub fn written_pages(&self) -> u64 {
        self.backing_after
            .written_kb
            .saturating_sub(self.backing_before.written_kb)
            / PAGE_KB
    }

    /// The log line: `zram writeback: wrote N KiB ...`.
    pub fn line(&self) -> String {
        format!(
            "zram writeback: wrote {} KiB of cold pages to the backing file in {} ms; zram store \
             {} -> {} KiB (holding {} -> {} KiB, {} -> {} incompressible pages), {} KiB on the \
             disk; {} of today's {} MiB left",
            self.written_pages() * PAGE_KB,
            self.took.as_millis(),
            self.before.mem_used_kb,
            self.after.mem_used_kb,
            self.before.orig_kb,
            self.after.orig_kb,
            self.before.huge_pages,
            self.after.huge_pages,
            self.backing_after.stored_kb,
            self.left_pages / MIB_PAGES,
            self.daily_pages / MIB_PAGES,
        )
    }
}

/// Decides, each time it is asked, whether a writeback runs now and how much it may write.
#[derive(Debug, Clone)]
pub struct Writeback {
    policy: WritebackPolicy,
    /// Since when no app has been in use.
    idle_since: Option<Duration>,
    last_run: Option<Duration>,
    today: Day,
}

impl Writeback {
    /// Starts on `today`, with what `stored` (the persistent property) says was written: kept
    /// when it is today's, forgotten when it is another day's.
    pub fn new(policy: WritebackPolicy, today: u64, stored: Option<Day>) -> Writeback {
        let today = match stored {
            Some(day) if day.day == today => day,
            _ => Day {
                day: today,
                pages: 0,
            },
        };
        Writeback {
            policy,
            idle_since: None,
            last_run: None,
            today,
        }
    }

    pub fn policy(&self) -> WritebackPolicy {
        self.policy
    }

    /// What is left of today's budget, in 4 KiB pages.
    pub fn left_pages(&self) -> u64 {
        self.policy.daily_pages.saturating_sub(self.today.pages)
    }

    /// Whether to write back at `now` (any monotonic clock), on day `day`, with `tasks` app
    /// tasks in use and zram as `zram` says. `Some` is the run's budget in 4 KiB pages, and
    /// counts as a run: the next one waits for [`WritebackPolicy::gap`].
    pub fn decide(
        &mut self,
        now: Duration,
        day: u64,
        tasks: usize,
        zram: &ZramStat,
    ) -> Option<u64> {
        if tasks > 0 {
            self.idle_since = None;
            return None;
        }
        let since = *self.idle_since.get_or_insert(now);
        if day != self.today.day {
            self.today = Day { day, pages: 0 };
        }
        let p = self.policy;
        let idle_long_enough = now.saturating_sub(since) >= p.idle_after;
        let gap_passed = self
            .last_run
            .is_none_or(|last| now.saturating_sub(last) >= p.gap);
        let budget = p.run_pages.min(self.left_pages());
        if !idle_long_enough || !gap_passed || budget == 0 || zram.mem_used_kb < p.min_store_kb {
            return None;
        }
        self.last_run = Some(now);
        Some(budget)
    }

    /// Counts `pages` written by the run, and returns the day to keep in
    /// [`WRITEBACK_DAY_PROPERTY`].
    pub fn wrote(&mut self, pages: u64) -> Day {
        self.today.pages = self.today.pages.saturating_add(pages);
        self.today
    }
}
