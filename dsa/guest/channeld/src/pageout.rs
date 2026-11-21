//! Paging an idle app's memory back to zram soon after something woke it.
//!
//! The idle reclaim ([`crate::reclaim`]) moves cold anonymous memory to zram for the whole
//! guest, but only when the guest goes idle or grows. Between those, a background process that
//! wakes (Google Play services checking in, a broadcast, even `dumpsys` asking it for its
//! state) reads its swapped-out pages back, and they stay resident until the next global
//! reclaim. Google Play services went from 30 to 73 MiB resident on each wake that way.
//!
//! So the daemon watches the app processes that no window shows, and when one has used the CPU
//! and then gone quiet for a few seconds, it asks the kernel to page that process's anonymous
//! memory out (`process_madvise` with `MADV_PAGEOUT`). The kernel moves only the pages nothing
//! else maps, to zram, where they take about a third of their size; what the process touches
//! again comes back on demand.
//!
//! Which processes: an app's (its app ID at least 10000), never one whose package has a task
//! window open, never the foreground app or one the user can see (`oom_score_adj` below
//! [`VISIBLE_APP_ADJ`]), and otherwise one at perceptible importance or lower
//! (`oom_score_adj` [`PERCEPTIBLE_APP_ADJ`] and up). Google Play services' processes, which
//! other apps and the system bind and so often sit just above perceptible without anything to
//! show, are taken from [`VISIBLE_APP_ADJ`] up. Each process is paged out at most every
//! [`PageoutPolicy::min_interval`], only after it has grown by [`PageoutPolicy::min_growth_kb`]
//! since the last time, and one process at most per look, so the daemon never competes with
//! the apps for the CPU.
//!
//! Never a process that is exiting (state `X` or `Z`, or no address space left): paging out
//! the memory of a process the kernel is tearing down raced with its exit, and the kernel then
//! reported a bad rss-counter and later panicked in binder. Never one that used the CPU within
//! [`QUIET_AFTER_WAKE`] either, checked again right before the pageout, so a process that
//! woke (or began to exit) after the look is left alone.
//!
//! The decision runs and is tested on any platform; [`system`] reads `/proc` and asks the
//! kernel on Linux and Android.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// How often the daemon looks at the app processes.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// The activity manager's `oom_score_adj` for a process the user can see (a visible activity,
/// or something a visible app is bound to). Below it is the foreground app and the system.
pub const VISIBLE_APP_ADJ: i32 = 100;

/// The activity manager's `oom_score_adj` for a process the user can perceive without seeing
/// it (music, a foreground service's notification). From it up are the services, the previous
/// app and the cached processes.
pub const PERCEPTIBLE_APP_ADJ: i32 = 200;

/// The first app ID; below it are the system's own users.
pub const FIRST_APPLICATION_UID: u32 = 10_000;

/// Android gives each user's apps a range of this many UIDs: a UID's app ID is its remainder.
pub const PER_USER_RANGE: u32 = 100_000;

/// How long a process must have used no CPU, after it woke, before it is paged out. A process
/// that has just run may be in the middle of something (a binder call, an exit, the runtime's
/// collector) whose pages it is about to touch again; paging them out then only brings them
/// back at once, and paging out a process that is exiting raced with the kernel tearing its
/// memory down. Six seconds is three looks: a process quiet that long has gone back to sleep.
pub const QUIET_AFTER_WAKE: Duration = Duration::from_secs(6);

/// What the daemon reads of one process at a look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSample {
    pub pid: i32,
    /// The process name, as `/proc/<pid>/cmdline` gives it.
    pub name: String,
    pub uid: u32,
    pub oom_score_adj: i32,
    /// The CPU time it has used, user and system, in clock ticks.
    pub cpu_ticks: u64,
    /// Its anonymous memory resident in RAM, in KiB (`RssAnon`).
    pub rss_anon_kb: u64,
    /// It is exiting: dead or a zombie (state `X` or `Z` in `/proc/<pid>/stat`), or its address
    /// space is already gone (no `VmSize` in `/proc/<pid>/status`).
    pub exiting: bool,
}

/// A process to page out now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub pid: i32,
    pub name: String,
    pub oom_score_adj: i32,
    pub google_play: bool,
    /// How much its resident anonymous memory grew since it was last paged out (or since the
    /// daemon first saw it), in KiB.
    pub grown_kb: u64,
    /// The CPU time it had used at the look, in clock ticks: any more right before the
    /// pageout means it woke since, and it is left alone.
    pub cpu_ticks: u64,
}

/// Whether a process of this name belongs to Google Play services: Google's apps
/// (`com.google.android.*`), the Play Store (`com.android.vending`), and the shared processes
/// the services framework runs (`com.google.process.*`).
pub fn is_google_play(name: &str) -> bool {
    let base = name.split(':').next().unwrap_or("");
    base.starts_with("com.google.android.")
        || base == "com.android.vending"
        || base.starts_with("com.google.process.")
}

/// Whether a process of this name runs the code of one of `packages` (those with a task
/// window open): Android names an app's processes after its package, its other processes
/// `<package>:<name>` or `<package>.<name>`.
pub fn has_window(name: &str, packages: &BTreeSet<String>) -> bool {
    let base = name.split(':').next().unwrap_or("");
    packages.iter().any(|package| {
        base == package
            || base
                .strip_prefix(package.as_str())
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Whether the process may be paged out at all, whatever its recent activity.
pub fn eligible(sample: &ProcessSample, windows: &BTreeSet<String>) -> bool {
    if sample.exiting {
        return false;
    }
    if sample.uid % PER_USER_RANGE < FIRST_APPLICATION_UID {
        return false;
    }
    if sample.oom_score_adj < VISIBLE_APP_ADJ || has_window(&sample.name, windows) {
        return false;
    }
    sample.oom_score_adj >= PERCEPTIBLE_APP_ADJ || is_google_play(&sample.name)
}

/// The decision's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageoutPolicy {
    /// How long a process must use no CPU after a wake before it is paged out.
    pub quiet: Duration,
    /// The least time between two pageouts of one process.
    pub min_interval: Duration,
    /// How much its resident anonymous memory must have grown since the last pageout, in KiB:
    /// below it the wake brought too little back to be worth a pageout.
    pub min_growth_kb: u64,
    /// The most processes paged out at one look.
    pub per_look: usize,
}

impl Default for PageoutPolicy {
    fn default() -> Self {
        PageoutPolicy {
            quiet: QUIET_AFTER_WAKE,
            min_interval: Duration::from_secs(30),
            min_growth_kb: 2 * 1024,
            per_look: 1,
        }
    }
}

#[derive(Debug, Clone)]
struct Tracked {
    name: String,
    cpu_ticks: u64,
    /// When it last used the CPU (or was first seen).
    last_active: Duration,
    /// It has woken since it was last paged out, or since it was first seen.
    woke: bool,
    /// Its resident anonymous memory just after the last pageout, in KiB.
    rss_after_kb: u64,
    last_pageout: Option<Duration>,
}

/// Decides, at each look, which quiet process to page out.
#[derive(Debug, Clone)]
pub struct Pageout {
    policy: PageoutPolicy,
    tracked: BTreeMap<i32, Tracked>,
}

impl Pageout {
    pub fn new(policy: PageoutPolicy) -> Pageout {
        Pageout {
            policy,
            tracked: BTreeMap::new(),
        }
    }

    /// Takes in a look at `now` (any monotonic clock) at the processes in `samples`, with
    /// `windows` the packages that have a task window open, and says which to page out now,
    /// most grown first. Each one returned counts as paged out; [`Pageout::paged`] then says
    /// how much it kept.
    pub fn look(
        &mut self,
        now: Duration,
        samples: &[ProcessSample],
        windows: &BTreeSet<String>,
    ) -> Vec<Target> {
        // A process that is gone, or a pid used again by another process, starts over.
        self.tracked.retain(|pid, tracked| {
            samples
                .iter()
                .any(|s| s.pid == *pid && s.name == tracked.name)
        });
        let mut due = Vec::new();
        for sample in samples {
            let tracked = self.tracked.entry(sample.pid).or_insert_with(|| Tracked {
                name: sample.name.clone(),
                cpu_ticks: sample.cpu_ticks,
                last_active: now,
                // Whatever it did before the daemon looked counts as one wake.
                woke: true,
                rss_after_kb: 0,
                last_pageout: None,
            });
            if sample.cpu_ticks != tracked.cpu_ticks {
                tracked.cpu_ticks = sample.cpu_ticks;
                tracked.last_active = now;
                tracked.woke = true;
            }
            if !tracked.woke || !eligible(sample, windows) {
                continue;
            }
            if now.saturating_sub(tracked.last_active) < self.policy.quiet {
                continue;
            }
            if tracked
                .last_pageout
                .is_some_and(|at| now.saturating_sub(at) < self.policy.min_interval)
            {
                continue;
            }
            let grown_kb = sample.rss_anon_kb.saturating_sub(tracked.rss_after_kb);
            if grown_kb < self.policy.min_growth_kb {
                // The wake brought little back: nothing to do until the next one.
                tracked.woke = false;
                continue;
            }
            due.push(Target {
                pid: sample.pid,
                name: sample.name.clone(),
                oom_score_adj: sample.oom_score_adj,
                google_play: is_google_play(&sample.name),
                grown_kb,
                cpu_ticks: sample.cpu_ticks,
            });
        }
        due.sort_by(|a, b| b.grown_kb.cmp(&a.grown_kb).then(a.pid.cmp(&b.pid)));
        due.truncate(self.policy.per_look);
        for target in &due {
            if let Some(tracked) = self.tracked.get_mut(&target.pid) {
                tracked.woke = false;
                tracked.last_pageout = Some(now);
            }
        }
        due
    }

    /// The process `pid` was paged out and keeps `rss_anon_kb` resident: the next pageout waits
    /// until it has grown past that again.
    pub fn paged(&mut self, pid: i32, rss_anon_kb: u64) {
        if let Some(tracked) = self.tracked.get_mut(&pid) {
            tracked.rss_after_kb = rss_anon_kb;
        }
    }
}

/// How `target` is right before its pageout, read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recheck {
    pub oom_score_adj: i32,
    pub cpu_ticks: u64,
    pub exiting: bool,
}

/// Why `target`, chosen at the last look, is not to be paged out after all as it is `now`:
/// it began to exit, it used the CPU since the look (it woke again), or it became more
/// important. `None` when the pageout may go ahead.
pub fn skip_reason(target: &Target, now: &Recheck) -> Option<String> {
    if now.exiting {
        return Some(format!("{} (pid {}) is exiting", target.name, target.pid));
    }
    if now.cpu_ticks != target.cpu_ticks {
        return Some(format!(
            "{} (pid {}) used the CPU since the look",
            target.name, target.pid
        ));
    }
    let adj = now.oom_score_adj;
    if adj < VISIBLE_APP_ADJ || (!target.google_play && adj < PERCEPTIBLE_APP_ADJ) {
        return Some(format!(
            "{} became more important (oom_score_adj {adj})",
            target.name
        ));
    }
    None
}

/// The process state in `/proc/<pid>/stat` (`R`, `S`, `Z`, `X`, ...), the field after the name.
pub fn state(stat: &str) -> Option<char> {
    stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .next()?
        .chars()
        .next()
}

/// Whether a process whose `/proc/<pid>/stat` and `/proc/<pid>/status` read so is exiting: dead
/// or a zombie, or with no address space left.
pub fn is_exiting(stat: &str, status: &Status) -> bool {
    matches!(state(stat), Some('X' | 'Z' | 'x')) || !status.has_mm
}

/// The user and system CPU time in `/proc/<pid>/stat`, in clock ticks. The name in brackets
/// may hold spaces and brackets of its own, so the fields are counted from the last `)`.
pub fn cpu_ticks(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name: state (field 3), ..., utime (field 14) and stime (field 15).
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// What `/proc/<pid>/status` says of a process, in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    /// The real UID.
    pub uid: u32,
    pub rss_anon_kb: u64,
    /// What it has in swap, at its size before compression.
    pub swap_kb: u64,
    /// It still has an address space (a `VmSize` line): an exiting process loses it before it
    /// is gone.
    pub has_mm: bool,
}

impl Status {
    /// Reads the `Uid`, `RssAnon` and `VmSwap` lines; a line it cannot read stays 0.
    pub fn parse(text: &str) -> Status {
        let mut status = Status::default();
        for line in text.lines() {
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let value = rest.split_whitespace().next().unwrap_or("");
            match name {
                "Uid" => status.uid = value.parse().unwrap_or(0),
                "RssAnon" => status.rss_anon_kb = value.parse().unwrap_or(0),
                "VmSwap" => status.swap_kb = value.parse().unwrap_or(0),
                "VmSize" => status.has_mm = true,
                _ => {}
            }
        }
        status
    }
}

/// The address ranges of `/proc/<pid>/maps` worth paging out, as (start, length): private
/// mappings no file backs (the heaps, the runtime's own, named `[anon:...]`), which go to zram.
/// Mappings with no access at all (guard pages), the stack, file mappings and shared memory
/// are left: there is nothing in them to page out, or paging them out is not this process's
/// alone to decide.
pub fn anonymous_ranges(maps: &str) -> Vec<(usize, usize)> {
    maps.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let range = fields.next()?;
            let perms = fields.next()?;
            let _offset = fields.next()?;
            let _device = fields.next()?;
            let inode = fields.next()?;
            let path = fields.collect::<Vec<_>>().join(" ");
            if perms.len() < 4 || !perms.ends_with('p') || perms.starts_with("---") {
                return None;
            }
            if inode != "0" {
                return None;
            }
            if !(path.is_empty() || path == "[heap]" || path.starts_with("[anon:")) {
                return None;
            }
            let (start, end) = range.split_once('-')?;
            let start = usize::from_str_radix(start, 16).ok()?;
            let end = usize::from_str_radix(end, 16).ok()?;
            (end > start).then_some((start, end - start))
        })
        .collect()
}

/// How the daemon reads the processes and asks the kernel to page one out, on Linux and
/// Android. The daemon holds `CAP_SYS_NICE` (which `process_madvise` asks of a caller acting on
/// another process) and `CAP_SYS_PTRACE` (which reading another app's address space asks), and
/// the policy lets it read an app's `/proc` entries.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod system {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, Instant};

    use super::{anonymous_ranges, cpu_ticks, is_exiting, skip_reason, ProcessSample, Recheck};
    use super::{Status, Target};
    use super::{FIRST_APPLICATION_UID, PER_USER_RANGE};

    /// The system call numbers, the same on every architecture since Linux 5.1.
    const SYS_PIDFD_OPEN: libc::c_long = 434;
    const SYS_PROCESS_MADVISE: libc::c_long = 440;
    const MADV_PAGEOUT: libc::c_int = 21;
    /// The most ranges one `process_madvise` takes (`UIO_MAXIOV`).
    const MOST_RANGES: usize = 1024;

    /// The app processes now. A process is an app's when the folder its `/proc` entry is owned
    /// by an app's UID, which the daemon reads without opening anything; only those are read.
    pub fn samples() -> std::io::Result<Vec<ProcessSample>> {
        let mut samples = Vec::new();
        for entry in std::fs::read_dir("/proc")?.flatten() {
            let file_name = entry.file_name();
            let Some(pid) = file_name.to_str().and_then(|n| n.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.uid() % PER_USER_RANGE < FIRST_APPLICATION_UID {
                continue;
            }
            let dir = entry.path();
            let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
            let (Some(cmdline), Some(adj), Some(stat), Some(status)) = (
                read("cmdline"),
                read("oom_score_adj"),
                read("stat"),
                read("status"),
            ) else {
                continue;
            };
            let name = cmdline.split('\0').next().unwrap_or("").to_string();
            let (Ok(oom_score_adj), Some(cpu)) = (adj.trim().parse::<i32>(), cpu_ticks(&stat))
            else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let status = Status::parse(&status);
            samples.push(ProcessSample {
                pid,
                name,
                uid: status.uid,
                oom_score_adj,
                cpu_ticks: cpu,
                rss_anon_kb: status.rss_anon_kb,
                exiting: is_exiting(&stat, &status),
            });
        }
        Ok(samples)
    }

    /// What one pageout did.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Outcome {
        pub before: Status,
        pub after: Status,
        /// The bytes the kernel was asked to page out, and how many of the ranges it refused.
        pub advised_bytes: u64,
        pub refused_ranges: usize,
        pub took: Duration,
    }

    /// Why a pageout did not happen.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Refused {
        /// The process is gone, or no longer one to page out (it came to the foreground).
        Skipped(String),
        /// The kernel or the policy does not let the daemon do it at all: no pageout will work.
        NotAllowed(String),
    }

    fn status(pid: i32) -> Option<Status> {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .map(|text| Status::parse(&text))
    }

    /// Pages `target`'s anonymous memory out to zram, if it is still the process it was and
    /// still not shown: the foreground app's memory is never touched, however the look raced.
    pub fn page_out(target: &Target) -> Result<Outcome, Refused> {
        let started = Instant::now();
        let gone = || Refused::Skipped(format!("pid {} is gone", target.pid));
        let adj: i32 = std::fs::read_to_string(format!("/proc/{}/oom_score_adj", target.pid))
            .map_err(|_| gone())?
            .trim()
            .parse()
            .map_err(|_| gone())?;
        let stat =
            std::fs::read_to_string(format!("/proc/{}/stat", target.pid)).map_err(|_| gone())?;
        let before = status(target.pid).ok_or_else(gone)?;
        let now = Recheck {
            oom_score_adj: adj,
            cpu_ticks: cpu_ticks(&stat).ok_or_else(gone)?,
            exiting: is_exiting(&stat, &before),
        };
        if let Some(why) = skip_reason(target, &now) {
            return Err(Refused::Skipped(why));
        }
        // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
        let pidfd = unsafe { libc::syscall(SYS_PIDFD_OPEN, target.pid, 0) };
        if pidfd < 0 {
            return Err(gone());
        }
        let pidfd = pidfd as libc::c_int;
        let result = advise(pidfd, target);
        // SAFETY: the descriptor is this function's own.
        unsafe { libc::close(pidfd) };
        let (advised_bytes, refused_ranges) = result?;
        let after = status(target.pid).unwrap_or(before);
        Ok(Outcome {
            before,
            after,
            advised_bytes,
            refused_ranges,
            took: started.elapsed(),
        })
    }

    fn advise(pidfd: libc::c_int, target: &Target) -> Result<(u64, usize), Refused> {
        // The maps are read after the pidfd is open, so they are this process's own.
        let maps = std::fs::read_to_string(format!("/proc/{}/maps", target.pid)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                Refused::NotAllowed(format!(
                    "cannot read the address space of {} (pid {}): {e}",
                    target.name, target.pid
                ))
            } else {
                Refused::Skipped(format!("pid {} is gone", target.pid))
            }
        })?;
        let ranges: Vec<libc::iovec> = anonymous_ranges(&maps)
            .into_iter()
            .map(|(start, len)| libc::iovec {
                iov_base: start as *mut libc::c_void,
                iov_len: len,
            })
            .collect();
        let mut advised = 0u64;
        let mut refused = 0usize;
        let mut next = 0usize;
        while next < ranges.len() {
            let batch = &ranges[next..(next + MOST_RANGES).min(ranges.len())];
            // SAFETY: the ranges are addresses in the other process, which the kernel checks;
            // this process only passes the vector, which lives through the call.
            let done = unsafe {
                libc::syscall(
                    SYS_PROCESS_MADVISE,
                    pidfd,
                    batch.as_ptr(),
                    batch.len(),
                    MADV_PAGEOUT,
                    0u32,
                )
            };
            if done < 0 {
                let error = std::io::Error::last_os_error();
                match error.raw_os_error() {
                    Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOSYS) => {
                        return Err(Refused::NotAllowed(format!(
                            "process_madvise on {} (pid {}) was refused: {error}",
                            target.name, target.pid
                        )));
                    }
                    Some(libc::ESRCH) => {
                        return Err(Refused::Skipped(format!("pid {} is gone", target.pid)));
                    }
                    // A range the kernel will not page out (locked, or unmapped since the maps
                    // were read) ends the call at it: the rest goes on without it.
                    _ => {
                        refused += 1;
                        next += 1;
                        continue;
                    }
                }
            }
            // The kernel stops at the first range it refuses and says how far it got.
            let mut left = done as u64;
            advised += left;
            let mut consumed = 0;
            for range in batch {
                if left < range.iov_len as u64 {
                    break;
                }
                left -= range.iov_len as u64;
                consumed += 1;
            }
            if consumed < batch.len() {
                // The range it stopped at is the one it refused.
                refused += 1;
                consumed += 1;
            }
            next += consumed;
        }
        Ok((advised, refused))
    }
}
