//! What the guest's idle reclaim costs while Google apps are installed and no app window is
//! open: how often it runs, the kernel time each run takes, the guest's whole processor time
//! while idle, and what fills the page cache again between runs.
//!
//! dsa-channeld asks init for two kinds of reclaim. The idle reclaim moves cold anonymous
//! pages to zram in four steps, drops the clean caches (`drop_caches`) and compacts memory;
//! the working set trim reclaims cold page cache and a little cold anonymous memory (`memory.reclaim` with `swappiness=30`)
//! and compacts. init's subcontext runs the vendor script's writes, and the kernel does each
//! write's work in the writer, so the kernel time of the processes named `init` is the cost of
//! a run. A sampler ([SAMPLER]) reads it once a second in the guest, with `/proc/stat`,
//! `/proc/vmstat` and `/proc/meminfo`, using only the shell's builtins so that a look starts
//! no process; every few looks it also reads every process's major faults (file pages read in
//! through a mapping: APKs, dex files and libraries) and each top-level cgroup's refaults.
//! `/proc/<pid>/io` is readable only for the shell's own processes on a user build, so the
//! bytes a process read are there only where the shell could read them.
//!
//! This module only reads what the sampler and the guest log said and writes the report; the
//! boot check in `overlays.rs` runs it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The sampler, run as the guest's shell: `sh <file> <seconds> <every>` looks once a second for
/// `seconds`, and every `every` looks reads every process and cgroup too. It prints:
///   `W <pid> <name>`  a thread whose ticks each look reads (init and its subcontexts,
///                     kswapd0, kcompactd0)
///   `S <uptime> cpu=<user,nice,system,idle,iowait,irq,softirq,steal> t<pid>=<utime,stime>
///      <counter>=<value>... self=<utime,stime,cutime,cstime>`  one look
///   `P <uptime> <pid> <major faults> <read_bytes or -> <comm>`  a process in a snapshot
///   `C <uptime> <cgroup> <workingset_refault_file or -> <file bytes or ->`  a cgroup in one
///   `N <pid> <name>`  the process names `ps` gives once the looks are done
pub const SAMPLER: &str = r##"# The idle reclaim sampler (dsa/host/platform/linux/tests/idle_reclaim). Only the shell's
# builtins read, so a look starts no process but the sleep after it.
seconds=$1
every=$2
watch=
for d in /proc/[0-9]*; do
	{ read -r stat < "$d/stat"; } 2>/dev/null || continue
	case "$stat" in
	*" (init) "*) name=init ;;
	*" (kswapd0) "*) name=kswapd0 ;;
	*" (kcompactd0) "*) name=kcompactd0 ;;
	*) continue ;;
	esac
	watch="$watch ${d#/proc/}"
	echo "W ${d#/proc/} $name"
done
read -r up _ < /proc/uptime
end=$(( ${up%.*} + seconds ))
n=0
while :; do
	read -r up _ < /proc/uptime
	[ "${up%.*}" -ge "$end" ] && break
	read -r _ u ni s i w q sq st _ < /proc/stat
	line="S $up cpu=$u,$ni,$s,$i,$w,$q,$sq,$st"
	for p in $watch; do
		{ read -r stat < "/proc/$p/stat"; } 2>/dev/null || continue
		rest=${stat##*\)}
		set -- $rest
		line="$line t$p=${12},${13}"
	done
	while read -r k v; do
		case $k in
		workingset_refault_file|pgpgin|pswpin|pswpout|pgmajfault|compact_stall) line="$line $k=$v" ;;
		esac
	done < /proc/vmstat
	while read -r k v _; do
		case $k in
		Cached:|MemFree:|AnonPages:|SwapTotal:|SwapFree:) line="$line ${k%:}=$v" ;;
		esac
	done < /proc/meminfo
	read -r stat < "/proc/$$/stat"
	rest=${stat##*\)}
	set -- $rest
	line="$line self=${12},${13},${14},${15}"
	echo "$line"
	if [ $(( n % every )) -eq 0 ]; then
		for d in /proc/[0-9]*; do
			{ read -r stat < "$d/stat"; } 2>/dev/null || continue
			name=${stat#*\(}
			name=${name%\)*}
			rest=${stat##*\)}
			set -- $rest
			rb=-
			{ while read -r k v; do [ "$k" = read_bytes: ] && rb=$v; done < "$d/io"; } 2>/dev/null
			echo "P $up ${d#/proc/} ${10} $rb $name"
		done
		for f in /sys/fs/cgroup/*/memory.stat; do
			r=-
			c=-
			{ while read -r k v; do
				case $k in
				workingset_refault_file) r=$v ;;
				file) c=$v ;;
				esac
			done < "$f"; } 2>/dev/null || continue
			g=${f#/sys/fs/cgroup/}
			echo "C $up ${g%/memory.stat} $r $c"
		done
	fi
	n=$(( n + 1 ))
	sleep 1
done
ps -A -o PID,NAME | while read -r p name; do echo "N $p $name"; done
"##;

/// The ticks of `/proc/stat` and `/proc/<pid>/stat` (USER_HZ), in milliseconds.
pub const TICK_MS: u64 = 10;
/// The kernel ticks init must spend in one look for the look to count as part of a reclaim. An
/// idle init spends none; the lightest trim spends tens of milliseconds.
pub const RUN_TICKS: u64 = 3;
/// How far from a run's looks, in seconds, a line of the guest log may be and still be about
/// that run: the log's clock and the sampler's are read apart.
const LOG_SLACK: f64 = 3.0;
/// How many processes and cgroups the report lists.
const TOP: usize = 12;

/// One look: the guest's uptime, the processors' ticks, the user and kernel ticks of each
/// watched thread by pid, the counters it read, and the sampler's own ticks with those of what
/// it waited for (utime, stime, cutime, cstime).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Look {
    pub at: f64,
    pub cpu: Vec<u64>,
    pub threads: BTreeMap<u32, (u64, u64)>,
    pub counters: BTreeMap<String, u64>,
    pub own: Vec<u64>,
}

/// A process in a snapshot: its major faults, the bytes it read from storage where the shell
/// could read them, and its short name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLook {
    pub pid: u32,
    pub major_faults: u64,
    pub read_bytes: Option<u64>,
    pub comm: String,
}

/// A top-level cgroup in a snapshot: its file pages read back after eviction. The page cache
/// charged to it stays in the sampler's raw output only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupLook {
    pub group: String,
    pub refaults: Option<u64>,
}

/// Everything the sampler printed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub watched: BTreeMap<u32, String>,
    pub looks: Vec<Look>,
    pub processes: Vec<(f64, Vec<ProcessLook>)>,
    pub groups: Vec<(f64, Vec<GroupLook>)>,
    pub names: BTreeMap<u32, String>,
}

fn numbers(text: &str) -> Vec<u64> {
    text.split(',').map(|n| n.parse().unwrap_or(0)).collect()
}

/// Reads the sampler's output. A line it does not know is skipped.
pub fn parse(text: &str) -> Record {
    let mut record = Record::default();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("W") => {
                if let (Some(Ok(pid)), Some(name)) =
                    (words.next().map(str::parse::<u32>), words.next())
                {
                    record.watched.insert(pid, name.to_string());
                }
            }
            Some("S") => {
                let Some(Ok(at)) = words.next().map(str::parse::<f64>) else {
                    continue;
                };
                let mut look = Look {
                    at,
                    ..Look::default()
                };
                for word in words {
                    let Some((key, value)) = word.split_once('=') else {
                        continue;
                    };
                    if key == "cpu" {
                        look.cpu = numbers(value);
                    } else if key == "self" {
                        look.own = numbers(value);
                    } else if let Some(Ok(pid)) = key.strip_prefix('t').map(str::parse::<u32>) {
                        let ticks = numbers(value);
                        if ticks.len() == 2 {
                            look.threads.insert(pid, (ticks[0], ticks[1]));
                        }
                    } else if let Ok(value) = value.parse::<u64>() {
                        look.counters.insert(key.to_string(), value);
                    }
                }
                record.looks.push(look);
            }
            Some("P") => {
                let (Some(Ok(at)), Some(Ok(pid)), Some(Ok(major_faults)), Some(read)) = (
                    words.next().map(str::parse::<f64>),
                    words.next().map(str::parse::<u32>),
                    words.next().map(str::parse::<u64>),
                    words.next(),
                ) else {
                    continue;
                };
                let look = ProcessLook {
                    pid,
                    major_faults,
                    read_bytes: read.parse().ok(),
                    comm: words.collect::<Vec<_>>().join(" "),
                };
                match record.processes.last_mut() {
                    Some((when, all)) if when.to_bits() == at.to_bits() => all.push(look),
                    _ => record.processes.push((at, vec![look])),
                }
            }
            Some("C") => {
                let (Some(Ok(at)), Some(group), Some(refaults)) = (
                    words.next().map(str::parse::<f64>),
                    words.next(),
                    words.next(),
                ) else {
                    continue;
                };
                let look = GroupLook {
                    group: group.to_string(),
                    refaults: refaults.parse().ok(),
                };
                match record.groups.last_mut() {
                    Some((when, all)) if when.to_bits() == at.to_bits() => all.push(look),
                    _ => record.groups.push((at, vec![look])),
                }
            }
            Some("N") => {
                if let Some(Ok(pid)) = words.next().map(str::parse::<u32>) {
                    let name = words.collect::<Vec<_>>().join(" ");
                    if !name.is_empty() {
                        record.names.insert(pid, name);
                    }
                }
            }
            _ => {}
        }
    }
    record
}

/// What the guest log says about the reclaim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    /// dsa-channeld asked for the idle reclaim, with its reason and the tasks in use.
    Reclaim { reason: String, tasks: Option<u32> },
    /// dsa-channeld asked for the working set trim.
    Trim,
    /// A step of init's that took 50 ms or more (init logs no shorter one): which, in ms.
    Step(&'static str, u64),
}

/// The reclaim's lines of a guest log printed with `logcat -v monotonic` (each line starts
/// with the seconds since boot), with their times.
pub fn log_events(log: &str) -> Vec<(f64, LogEvent)> {
    let mut events = Vec::new();
    for line in log.lines() {
        let Some(Ok(at)) = line.split_whitespace().next().map(str::parse::<f64>) else {
            continue;
        };
        if let Some((_, rest)) = line.split_once("dsa-channeld: reclaiming the guest's memory (") {
            let reason = rest
                .split([',', ')'])
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let tasks = rest
                .split_once(", ")
                .and_then(|(_, after)| after.split_whitespace().next())
                .and_then(|n| n.parse().ok());
            events.push((at, LogEvent::Reclaim { reason, tasks }));
        } else if line.contains("dsa-channeld: trimming ") {
            events.push((at, LogEvent::Trim));
        } else if let Some((_, rest)) = line.split_once("Command 'write ") {
            // The command alone: the action after it names the reclaim's property, whose name
            // holds "memory.reclaim" too.
            let Some((command, after)) = rest.split_once("' action=") else {
                continue;
            };
            let Some(took) = after
                .split_once(" took ")
                .and_then(|(_, after)| after.split_once("ms"))
                .and_then(|(ms, _)| ms.trim().parse::<u64>().ok())
            else {
                continue;
            };
            let step = if command.contains("memory.reclaim") && command.contains("swappiness=200") {
                "zram"
            } else if command.contains("memory.reclaim") {
                "trim"
            } else if command.contains("drop_caches") {
                "drop_caches"
            } else if command.contains("compact_memory") {
                "compaction"
            } else {
                continue;
            };
            events.push((at, LogEvent::Step(step, took)));
        }
    }
    events
}

/// One run of the reclaim: `before` is the look just before init started spending kernel time,
/// `last` the last look it was still spending it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub before: usize,
    pub last: usize,
}

/// The ticks the watched threads named `name` spent between look `i - 1` and look `i`: kernel
/// ticks, or user ticks when `kernel` is false.
fn thread_ticks(record: &Record, i: usize, name: &str, kernel: bool) -> u64 {
    let (a, b) = (&record.looks[i - 1], &record.looks[i]);
    record
        .watched
        .iter()
        .filter(|(_, watched)| watched.as_str() == name)
        .filter_map(|(pid, _)| {
            let (a_user, a_kernel) = a.threads.get(pid)?;
            let (b_user, b_kernel) = b.threads.get(pid)?;
            Some(if kernel {
                b_kernel.saturating_sub(*a_kernel)
            } else {
                b_user.saturating_sub(*a_user)
            })
        })
        .sum()
}

/// The ticks the threads named `name` spent from look `from` to look `to`.
fn ticks_between(record: &Record, from: usize, to: usize, name: &str, kernel: bool) -> u64 {
    (from + 1..=to)
        .map(|i| thread_ticks(record, i, name, kernel))
        .sum()
}

/// The runs of the reclaim in `record`: the looks in which init spent [RUN_TICKS] or more of
/// kernel time, one quiet look inside a run not ending it.
pub fn runs(record: &Record) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for i in 1..record.looks.len() {
        if thread_ticks(record, i, "init", true) < RUN_TICKS {
            continue;
        }
        match runs.last_mut() {
            Some(run) if i <= run.last + 2 => run.last = i,
            _ => runs.push(Run {
                before: i - 1,
                last: i,
            }),
        }
    }
    runs
}

fn counter(look: &Look, name: &str) -> Option<u64> {
    look.counters.get(name).copied()
}

/// How much the counter `name` grew from look `from` to look `to`.
fn grew(record: &Record, from: usize, to: usize, name: &str) -> Option<u64> {
    Some(counter(&record.looks[to], name)?.saturating_sub(counter(&record.looks[from], name)?))
}

/// Pages (4 KiB) as MiB.
fn pages_mib(pages: u64) -> f64 {
    pages as f64 * 4.0 / 1024.0
}

/// KiB as MiB.
fn kib_mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn cell(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |v| format!("{v:.1}"))
}

/// What each process, by name, faulted in and read from the snapshot taken at or after
/// `from` to the last one taken at or before `to`. A process that started in between counts
/// from nothing.
pub fn process_growth(record: &Record, from: f64, to: f64) -> BTreeMap<String, (u64, Option<u64>)> {
    let mut grown = BTreeMap::new();
    let first = record.processes.iter().find(|(at, _)| *at >= from);
    let last = record.processes.iter().rev().find(|(at, _)| *at <= to);
    let (Some((first_at, first)), Some((last_at, last))) = (first, last) else {
        return grown;
    };
    if first_at >= last_at {
        return grown;
    }
    let before: BTreeMap<u32, &ProcessLook> = first.iter().map(|p| (p.pid, p)).collect();
    for process in last {
        let (faults, read) = match before.get(&process.pid).filter(|p| p.comm == process.comm) {
            Some(earlier) => (
                process.major_faults.saturating_sub(earlier.major_faults),
                process
                    .read_bytes
                    .zip(earlier.read_bytes)
                    .map(|(now, then)| now.saturating_sub(then)),
            ),
            None => (process.major_faults, process.read_bytes),
        };
        if faults == 0 && read.unwrap_or(0) == 0 {
            continue;
        }
        let name = record
            .names
            .get(&process.pid)
            .cloned()
            .unwrap_or_else(|| process.comm.clone());
        let entry: &mut (u64, Option<u64>) = grown.entry(name).or_insert((0, None));
        entry.0 += faults;
        if let Some(read) = read {
            entry.1 = Some(entry.1.unwrap_or(0) + read);
        }
    }
    grown
}

/// The file pages each top-level cgroup read back after eviction, between the same snapshots
/// as [process_growth].
pub fn group_growth(record: &Record, from: f64, to: f64) -> BTreeMap<String, u64> {
    let mut grown = BTreeMap::new();
    let first = record.groups.iter().find(|(at, _)| *at >= from);
    let last = record.groups.iter().rev().find(|(at, _)| *at <= to);
    let (Some((first_at, first)), Some((last_at, last))) = (first, last) else {
        return grown;
    };
    if first_at >= last_at {
        return grown;
    }
    for group in last {
        let Some(now) = group.refaults else { continue };
        let then = first
            .iter()
            .find(|g| g.group == group.group)
            .and_then(|g| g.refaults)
            .unwrap_or(0);
        let refaults = now.saturating_sub(then);
        if refaults > 0 {
            *grown.entry(group.group.clone()).or_insert(0) += refaults;
        }
    }
    grown
}

/// The measurement's report: Markdown for the job summary and the same numbers as JSON.
pub struct Report {
    pub markdown: String,
    pub json: serde_json::Value,
}

/// The kind of a run: the idle reclaim swaps to zram, the trim does not. The guest log's word,
/// when a line of it is about the run, decides.
fn run_kind(record: &Record, run: Run, logged: &[&(f64, LogEvent)]) -> String {
    for (_, event) in logged {
        match event {
            LogEvent::Reclaim { reason, .. } => return format!("idle reclaim ({reason})"),
            LogEvent::Trim => return "working set trim".into(),
            LogEvent::Step(..) => {}
        }
    }
    match grew(record, run.before, run.last, "pswpout") {
        Some(swapped) if swapped > 0 => "idle reclaim".into(),
        Some(_) => "working set trim".into(),
        None => "unknown".into(),
    }
}

/// The report of the measurement in `record`, with what the guest log `events` said, the
/// Google apps build `gapps` it ran with, and the guest's `memory_mb` and `processors`.
pub fn report(
    record: &Record,
    events: &[(f64, LogEvent)],
    gapps: &str,
    memory_mb: u64,
    processors: u64,
) -> Report {
    let mut md = String::new();
    let _ = writeln!(
        md,
        "## Idle reclaim with Google apps installed and no app window\n"
    );
    let _ = writeln!(
        md,
        "Google apps: {gapps}. The guest had {memory_mb} MiB and {processors} processors."
    );
    let looks = &record.looks;
    if looks.len() < 2 {
        let _ = writeln!(
            md,
            "\nThe sampler gave {} looks, too few to measure anything.",
            looks.len()
        );
        return Report {
            markdown: md,
            json: serde_json::json!({ "gapps": gapps, "looks": looks.len() }),
        };
    }
    let (first, last) = (0, looks.len() - 1);
    let elapsed = looks[last].at - looks[first].at;
    let cpu = |i: usize, column: usize| looks[i].cpu.get(column).copied().unwrap_or(0);
    let delta = |column: usize| cpu(last, column).saturating_sub(cpu(first, column));
    let busy_ticks = [0, 1, 2, 5, 6].iter().map(|&c| delta(c)).sum::<u64>();
    let all_ticks = (0..8).map(delta).sum::<u64>();
    let iowait_ticks = delta(4);
    let own = |i: usize| looks[i].own.iter().sum::<u64>();
    let sampler_ticks = own(last).saturating_sub(own(first));
    let busy_s = (busy_ticks * TICK_MS) as f64 / 1000.0;
    let one_cpu = if elapsed > 0.0 {
        busy_s / elapsed * 100.0
    } else {
        0.0
    };
    let all_cpus = if all_ticks > 0 {
        busy_ticks as f64 / all_ticks as f64 * 100.0
    } else {
        0.0
    };
    let kswapd_ms = ticks_between(record, first, last, "kswapd0", true) * TICK_MS;
    let kcompactd_ms = ticks_between(record, first, last, "kcompactd0", true) * TICK_MS;
    let refaults = grew(record, first, last, "workingset_refault_file");
    let pgpgin = grew(record, first, last, "pgpgin");

    let found = runs(record);
    let window = looks[first].at - LOG_SLACK..=looks[last].at + LOG_SLACK;
    let in_window = |at: f64| window.contains(&at);
    let reclaim_requests = events
        .iter()
        .filter(|(at, e)| in_window(*at) && matches!(e, LogEvent::Reclaim { .. }))
        .count();
    let trim_requests = events
        .iter()
        .filter(|(at, e)| in_window(*at) && matches!(e, LogEvent::Trim))
        .count();

    let mut rows = Vec::new();
    let mut json_runs = Vec::new();
    let mut refills: Vec<(f64, f64)> = Vec::new();
    let mut previous_end = first;
    let mut previous_start: Option<f64> = None;
    for (number, run) in found.iter().enumerate() {
        let start = looks[run.before].at;
        let end = looks[run.last].at;
        let logged: Vec<&(f64, LogEvent)> = events
            .iter()
            .filter(|(at, _)| (start - LOG_SLACK..=end + LOG_SLACK).contains(at))
            .collect();
        let kind = run_kind(record, *run, &logged);
        let tasks = logged.iter().find_map(|(_, e)| match e {
            LogEvent::Reclaim { tasks, .. } => *tasks,
            _ => None,
        });
        let mut steps: BTreeMap<&str, u64> = BTreeMap::new();
        for (_, event) in &logged {
            if let LogEvent::Step(step, took) = event {
                *steps.entry(*step).or_insert(0) += *took;
            }
        }
        let steps_text = if steps.is_empty() {
            "-".to_string()
        } else {
            steps
                .iter()
                .map(|(step, ms)| format!("{step} {ms}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let kernel_ms = ticks_between(record, run.before, run.last, "init", true) * TICK_MS;
        let user_ms = ticks_between(record, run.before, run.last, "init", false) * TICK_MS;
        let since = previous_start.map(|at| start - at);
        let refill_refaults = grew(record, previous_end, run.before, "workingset_refault_file");
        let refill_pgpgin = grew(record, previous_end, run.before, "pgpgin");
        let cached_before = counter(&looks[run.before], "Cached");
        let cached_after = counter(&looks[run.last], "Cached");
        let to_zram = grew(record, run.before, run.last, "pswpout");
        let at = start - looks[first].at;
        rows.push(format!(
            "| {} | {at:.0} | {kind} | {} | {kernel_ms} | {steps_text} | {} | {} | {} to {} | {} |",
            number + 1,
            since.map_or_else(|| "-".to_string(), |s| format!("{s:.0}")),
            cell(refill_refaults.map(pages_mib)),
            cell(refill_pgpgin.map(kib_mib)),
            cell(cached_before.map(kib_mib)),
            cell(cached_after.map(kib_mib)),
            cell(to_zram.map(pages_mib)),
        ));
        json_runs.push(serde_json::json!({
            "at_s": at,
            "uptime_s": start,
            "kind": kind,
            "tasks_in_use": tasks,
            "since_last_s": since,
            "wall_s_at_most": end - start,
            "init_kernel_ms": kernel_ms,
            "init_user_ms": user_ms,
            "init_steps_ms": steps,
            "refill_before": {
                "workingset_refault_file_pages": refill_refaults,
                "pgpgin_kib": refill_pgpgin,
            },
            "during": {
                "workingset_refault_file_pages": grew(record, run.before, run.last, "workingset_refault_file"),
                "pgpgin_kib": grew(record, run.before, run.last, "pgpgin"),
                "pswpout_pages": to_zram,
                "kswapd_kernel_ms": ticks_between(record, run.before, run.last, "kswapd0", true) * TICK_MS,
                "kcompactd_kernel_ms": ticks_between(record, run.before, run.last, "kcompactd0", true) * TICK_MS,
            },
            "cached_kib_before": cached_before,
            "cached_kib_after": cached_after,
        }));
        if run.before > previous_end {
            refills.push((looks[previous_end].at, start));
        }
        previous_end = run.last;
        previous_start = Some(start);
    }

    let idle_runs = rows
        .iter()
        .filter(|row| row.contains("| idle reclaim"))
        .count();
    let trims = rows
        .iter()
        .filter(|row| row.contains("| working set trim"))
        .count();
    let kernel_each: Vec<u64> = found
        .iter()
        .map(|run| ticks_between(record, run.before, run.last, "init", true) * TICK_MS)
        .collect();
    let kernel_total: u64 = kernel_each.iter().sum();
    let kernel_most = kernel_each.iter().copied().max().unwrap_or(0);
    let starts: Vec<f64> = found.iter().map(|run| looks[run.before].at).collect();
    let mean_interval = if starts.len() > 1 {
        Some((starts[starts.len() - 1] - starts[0]) / (starts.len() - 1) as f64)
    } else {
        None
    };

    let _ = writeln!(
        md,
        "The sampler looked {} times over {elapsed:.0} s, from {:.0} s after the guest's boot.\n",
        looks.len(),
        looks[first].at
    );
    let _ = writeln!(md, "| What | Measured |\n|---|---|");
    let _ = writeln!(
        md,
        "| Runs of the reclaim | {} ({idle_runs} idle reclaims with zram, drop_caches and compaction; {trims} working set trims) |",
        found.len()
    );
    let _ = writeln!(
        md,
        "| Time between runs | {} |",
        mean_interval.map_or_else(|| "-".to_string(), |s| format!("{s:.0} s on average"))
    );
    let _ = writeln!(
        md,
        "| Requests dsa-channeld logged | {reclaim_requests} idle reclaims, {trim_requests} trims |"
    );
    let _ = writeln!(
        md,
        "| init's kernel time in the runs | {kernel_total} ms in all, {} ms a run on average, {kernel_most} ms at most |",
        if found.is_empty() { 0 } else { kernel_total / found.len() as u64 }
    );
    let _ = writeln!(
        md,
        "| kswapd0 and kcompactd0 kernel time | {kswapd_ms} ms and {kcompactd_ms} ms |"
    );
    let _ = writeln!(
        md,
        "| Guest processor time while idle | {busy_s:.1} s busy in {elapsed:.0} s: {one_cpu:.1} % of one processor, {all_cpus:.1} % of all; iowait {:.1} s |",
        (iowait_ticks * TICK_MS) as f64 / 1000.0
    );
    let _ = writeln!(
        md,
        "| Of that, the sampler's own | {:.1} s |",
        (sampler_ticks * TICK_MS) as f64 / 1000.0
    );
    let _ = writeln!(
        md,
        "| File pages read back after eviction (workingset_refault_file) | {} MiB |",
        cell(refaults.map(pages_mib))
    );
    let _ = writeln!(
        md,
        "| Read from storage (pgpgin) | {} MiB |",
        cell(pgpgin.map(kib_mib))
    );

    let _ = writeln!(
        md,
        "\n### Each run\n\n\"Before it\" is what was read between the end of the run before (or the first look) and this run's start, so it is what the run had to reclaim again. \"Cache\" is the page cache at the run's start and at its end. init logs only steps of 50 ms or more. A run's kernel time is counted in whole looks, so a run that ends just after a look shows its last ticks in the look after.\n"
    );
    let _ = writeln!(
        md,
        "| # | At (s) | Kind | Since the last (s) | init kernel ms | init steps (ms) | Refaults before it (MiB) | pgpgin before it (MiB) | Cache (MiB) | To zram (MiB) |\n|---|---|---|---|---|---|---|---|---|---|"
    );
    for row in &rows {
        let _ = writeln!(md, "{row}");
    }

    let mut refill_processes: BTreeMap<String, (u64, Option<u64>)> = BTreeMap::new();
    let mut refill_groups: BTreeMap<String, u64> = BTreeMap::new();
    for (from, to) in &refills {
        for (name, (faults, read)) in process_growth(record, *from, *to) {
            let entry = refill_processes.entry(name).or_insert((0, None));
            entry.0 += faults;
            if let Some(read) = read {
                entry.1 = Some(entry.1.unwrap_or(0) + read);
            }
        }
        for (group, refaults) in group_growth(record, *from, *to) {
            *refill_groups.entry(group).or_insert(0) += refaults;
        }
    }
    let whole = process_growth(record, looks[first].at, looks[last].at);
    let mut top: Vec<(&String, &(u64, Option<u64>))> = refill_processes.iter().collect();
    top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(b.0)));
    top.truncate(TOP);
    let mut whole_top: Vec<(&String, &(u64, Option<u64>))> = whole.iter().collect();
    whole_top.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(b.0)));
    whole_top.truncate(TOP);
    let mut groups: Vec<(&String, &u64)> = refill_groups.iter().collect();
    groups.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    groups.truncate(TOP);

    let _ = writeln!(
        md,
        "\n### What read the cache back in between runs\n\nMajor faults are file pages a process read in through a mapping (APKs, dex files, libraries), counted between the snapshots nearest each gap between runs (one every few looks). A process's bytes read are shown only where the shell could read its `/proc/<pid>/io`, which on a user build is only for the shell's own processes.\n"
    );
    if top.is_empty() {
        let _ = writeln!(md, "No process faulted in a file page between runs, or no two snapshots fell inside a gap.\n");
    } else {
        let _ = writeln!(md, "| Process | Major faults | Read (MiB) |\n|---|---|---|");
        for (name, (faults, read)) in &top {
            let _ = writeln!(
                md,
                "| {name} | {faults} | {} |",
                cell(read.map(|b| b as f64 / 1048576.0))
            );
        }
    }
    let _ = writeln!(
        md,
        "\nOver the whole window:\n\n| Process | Major faults | Read (MiB) |\n|---|---|---|"
    );
    for (name, (faults, read)) in &whole_top {
        let _ = writeln!(
            md,
            "| {name} | {faults} | {} |",
            cell(read.map(|b| b as f64 / 1048576.0))
        );
    }
    if !groups.is_empty() {
        let _ = writeln!(md, "\nFile pages read back after eviction between runs, by top-level cgroup:\n\n| cgroup | MiB |\n|---|---|");
        for (group, refaults) in &groups {
            let _ = writeln!(md, "| {group} | {:.1} |", pages_mib(**refaults));
        }
    }

    let json = serde_json::json!({
        "gapps": gapps,
        "memory_mb": memory_mb,
        "processors": processors,
        "window": {
            "uptime_s_first": looks[first].at,
            "elapsed_s": elapsed,
            "looks": looks.len(),
        },
        "runs": json_runs,
        "totals": {
            "runs": found.len(),
            "idle_reclaims": idle_runs,
            "trims": trims,
            "mean_interval_s": mean_interval,
            "logged_reclaim_requests": reclaim_requests,
            "logged_trim_requests": trim_requests,
            "init_kernel_ms": kernel_total,
            "init_kernel_ms_most": kernel_most,
            "kswapd_kernel_ms": kswapd_ms,
            "kcompactd_kernel_ms": kcompactd_ms,
            "guest_busy_s": busy_s,
            "guest_busy_percent_of_one_processor": one_cpu,
            "guest_busy_percent_of_all": all_cpus,
            "iowait_s": (iowait_ticks * TICK_MS) as f64 / 1000.0,
            "sampler_s": (sampler_ticks * TICK_MS) as f64 / 1000.0,
            "workingset_refault_file_pages": refaults,
            "pgpgin_kib": pgpgin,
        },
        "refill_processes": top.iter().map(|(name, (faults, read))| serde_json::json!({
            "process": name, "major_faults": faults, "read_bytes": read,
        })).collect::<Vec<_>>(),
        "window_processes": whole_top.iter().map(|(name, (faults, read))| serde_json::json!({
            "process": name, "major_faults": faults, "read_bytes": read,
        })).collect::<Vec<_>>(),
        "refill_cgroups": groups.iter().map(|(group, refaults)| serde_json::json!({
            "cgroup": group, "workingset_refault_file_pages": refaults,
        })).collect::<Vec<_>>(),
    });
    Report { markdown: md, json }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A look at `at` s: init's kernel ticks, the refaults, pgpgin, pswpout and the cache.
    fn look(
        at: u32,
        init_kernel: u64,
        refaults: u64,
        pgpgin: u64,
        pswpout: u64,
        cached: u64,
    ) -> String {
        let busy = u64::from(at) * 2;
        let idle = u64::from(at) * 198;
        format!(
            "S {at}.00 cpu={busy},0,0,{idle},0,0,0,0 t1=5,{init_kernel} t300=0,{} t40=0,1 \
             workingset_refault_file={refaults} pgpgin={pgpgin} pswpout={pswpout} Cached={cached} \
             self={at},0,0,0\n",
            init_kernel / 2
        )
    }

    fn sample() -> String {
        let mut text = String::from("W 1 init\nW 300 init\nW 40 kswapd0\nW 41 kcompactd0\n");
        // Quiet; a run at looks 3 and 4; quiet; a trim at look 9.
        let rows = [
            (100, 0, 0, 0, 0, 400_000),
            (101, 0, 10, 100, 0, 400_100),
            (102, 0, 20, 200, 0, 400_200),
            (103, 50, 20, 200, 0, 400_200),
            (104, 100, 20, 300, 512, 100_000),
            (105, 100, 25, 400, 512, 110_000),
            (106, 100, 1_024, 4_096, 512, 150_000),
            (107, 100, 2_048, 8_192, 512, 200_000),
            (108, 100, 2_560, 10_240, 512, 300_000),
            (109, 120, 2_560, 10_240, 512, 150_000),
            (110, 120, 2_560, 10_240, 512, 150_000),
        ];
        for (at, init_kernel, refaults, pgpgin, pswpout, cached) in rows {
            text.push_str(&look(at, init_kernel, refaults, pgpgin, pswpout, cached));
            if at == 104 || at == 108 {
                text.push_str(&format!(
                    "P {at}.00 1 0 - init\nP {at}.00 500 {} - id.gms.persistent\nP {at}.00 600 7 4096 sh\n",
                    if at == 104 { 100 } else { 900 }
                ));
                text.push_str(&format!(
                    "C {at}.00 uid_10100 {} 0\nC {at}.00 system - -\n",
                    if at == 104 { 0 } else { 256 }
                ));
            }
        }
        text.push_str("N PID NAME\nN 500 com.google.android.gms.persistent\n");
        text
    }

    #[test]
    fn nfr3_idle_sampler_output_is_read_into_looks_snapshots_and_names() {
        let record = parse(&sample());
        assert_eq!(record.looks.len(), 11);
        assert_eq!(record.watched.get(&300).map(String::as_str), Some("init"));
        assert_eq!(record.looks[4].threads.get(&1), Some(&(5, 100)));
        assert_eq!(record.looks[4].threads.get(&300), Some(&(0, 50)));
        assert_eq!(counter(&record.looks[4], "pswpout"), Some(512));
        assert_eq!(record.looks[0].cpu.len(), 8);
        assert_eq!(record.processes.len(), 2);
        assert_eq!(record.processes[0].1.len(), 3);
        assert_eq!(record.processes[0].1[2].read_bytes, Some(4096));
        assert_eq!(record.processes[0].1[1].read_bytes, None);
        assert_eq!(record.groups[0].1[1].refaults, None);
        assert_eq!(
            record.names.get(&500).map(String::as_str),
            Some("com.google.android.gms.persistent")
        );
        // The header of ps is not a process.
        assert_eq!(record.names.len(), 1);
    }

    #[test]
    fn nfr3_a_reclaim_run_is_the_looks_in_which_init_spends_kernel_time() {
        let record = parse(&sample());
        // init (both processes) spends 75 ticks at look 3, 75 at look 4, and 30 at look 9.
        assert_eq!(
            runs(&record),
            vec![Run { before: 2, last: 4 }, Run { before: 8, last: 9 }]
        );
        assert_eq!(ticks_between(&record, 2, 4, "init", true), 150);
        // A thread that is not init does not make a run.
        let quiet = sample().replace(" t1=5,", " t2=5,");
        let record = parse(&quiet);
        assert_eq!(runs(&record).len(), 2);
    }

    #[test]
    fn nfr3_the_guest_log_says_which_reclaim_ran_and_how_long_its_steps_took() {
        let log = "   103.40   700   700 I dsa-channeld: reclaiming the guest's memory (Idle, 0 tasks in use): 300 MiB droppable and 200 MiB anonymous of 900 MiB in use\n\
                   103.90     1     1 I init    : Command 'write /sys/fs/cgroup/memory.reclaim ${vendor.dsa.memory.reclaim_step}M swappiness=200' action=property:vendor.dsa.memory.reclaim=1 && property:sys.boot_completed=1 (/vendor/etc/init/dsa-channeld.rc:33) took 412ms and succeeded\n\
                   104.20     1     1 I init    : Command 'write /proc/sys/vm/drop_caches 3' action=property:vendor.dsa.memory.reclaim=1 && property:sys.boot_completed=1 (/vendor/etc/init/dsa-channeld.rc:36) took 88ms and succeeded\n\
                   104.50     1     1 I init    : Command 'write /proc/sys/vm/compact_memory 1' action=property:vendor.dsa.memory.reclaim=1 && property:sys.boot_completed=1 (/vendor/etc/init/dsa-channeld.rc:37) took 230ms and succeeded\n\
                   108.70   700   700 I dsa-channeld: trimming 128 MiB of cold page cache (working set target 256 MiB)\n\
                   109.10     1     1 I init    : Command 'write /sys/fs/cgroup/memory.reclaim ${vendor.dsa.memory.trim_size}M swappiness=30' action=y (/vendor/etc/init/dsa-channeld.rc:52) took 140ms and succeeded\n\
                   109.20     1     1 I init    : Command 'start foo' action=z (/x.rc:1) took 70ms and succeeded\n\
                   not a line\n";
        let events = log_events(log);
        assert_eq!(
            events.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(),
            vec![
                LogEvent::Reclaim {
                    reason: "Idle".into(),
                    tasks: Some(0)
                },
                LogEvent::Step("zram", 412),
                LogEvent::Step("drop_caches", 88),
                LogEvent::Step("compaction", 230),
                LogEvent::Trim,
                LogEvent::Step("trim", 140),
            ]
        );

        let record = parse(&sample());
        let report = report(&record, &events, "LiteGapps test", 4096, 2);
        let md = &report.markdown;
        assert!(md.contains("| Runs of the reclaim | 2 (1 idle reclaims with zram, drop_caches and compaction; 1 working set trims) |"), "{md}");
        assert!(
            md.contains("| Requests dsa-channeld logged | 1 idle reclaims, 1 trims |"),
            "{md}"
        );
        assert!(md.contains("| 1 | 2 | idle reclaim (Idle) | - | 1500 | compaction 230, drop_caches 88, zram 412 |"), "{md}");
        // The refill before the trim: from the first run's end (look 4) to its start (look 8).
        assert!(
            md.contains("| 2 | 8 | working set trim | 6 | 300 | trim 140 | 9.9 | 9.7 |"),
            "{md}"
        );
        // init spent 1500 + 300 ms; kswapd's ticks did not move.
        assert!(md.contains("| init's kernel time in the runs | 1800 ms in all, 900 ms a run on average, 1500 ms at most |"), "{md}");
        assert!(
            md.contains("| kswapd0 and kcompactd0 kernel time | 0 ms and 0 ms |"),
            "{md}"
        );
        // 20 busy ticks of 2000 in 10 s: two hundredths of a second each second.
        assert!(
            md.contains("0.2 s busy in 10 s: 2.0 % of one processor, 1.0 % of all"),
            "{md}"
        );
        // Between the runs, the Play services process faulted in 800 pages and its cgroup read
        // back 256.
        assert!(
            md.contains("| com.google.android.gms.persistent | 800 | - |"),
            "{md}"
        );
        assert!(md.contains("| uid_10100 | 1.0 |"), "{md}");
        assert_eq!(report.json["totals"]["runs"], 2);
        assert_eq!(report.json["runs"][0]["init_steps_ms"]["zram"], 412);
        assert_eq!(
            report.json["runs"][1]["refill_before"]["workingset_refault_file_pages"],
            2_540
        );
    }

    #[test]
    fn nfr3_too_few_looks_say_so_instead_of_measuring() {
        let report = report(&parse("S 1.0 cpu=1,0,0,1,0,0,0,0\n"), &[], "none", 4096, 2);
        assert!(report.markdown.contains("too few to measure anything"));
    }
}
