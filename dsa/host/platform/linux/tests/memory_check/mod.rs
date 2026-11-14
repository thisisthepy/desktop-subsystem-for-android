//! The memory measurement's numbers, its report and its comparison with the recorded baseline.
//!
//! The boot check (`overlays.rs`) boots the image with Google Play, opens one app window,
//! leaves it untouched for a minute and then reads the memory several times, as `dsa memory`
//! reads it. This module turns those readings into the numbers every release shows:
//!
//! - the host's total: the resident memory of every host process of the user's Android (the
//!   VM's crosvm process, the window manager, the graphics renderer, the app windows) and of
//!   the service, the number the one-app-window target is about;
//! - the same in private memory, which on the VM counts all of the guest's memory it holds;
//! - in the guest: what it uses, page cache, app memory, the kernel and the rest, zram, and what
//!   its processes cost (resident plus their share of zram at the ratio zram measures, not the
//!   size before compression), with Google Play services on a line of its own;
//! - the guest's PSS by process, as `dumpsys meminfo` lists it;
//! - how much more the VM holds than the guest uses (resident and private), the memory the
//!   guest has freed but the host has not yet taken back;
//! - after the window has closed and the guest has had its idle reclaim and its zram writeback
//!   ([`idle_report`]): the same numbers again, with what each writeback wrote and how the zram
//!   store changed around it, read from the guest's log ([`writeback_runs`]).
//!
//! Each number is the median of the readings, so one reading taken during a reclaim does not
//! decide it. The check fails only on a large regression: a number more than a quarter above
//! its baseline and more than 100 MiB above it. A number the baseline does not record yet is
//! reported and never fails.
// Each test program that includes this module uses its own part of it.
#![allow(dead_code)]

use dsa_host_core::memory::{HostRole, MemorySnapshot};

pub mod direct_mapped;

/// The recorded baseline the measurement is compared with, checked in beside this module.
pub const BASELINE: &str = include_str!("baseline.json");

/// How far above its baseline a number may go before the check fails: a quarter, and also
/// this many MiB, so a small baseline does not fail on noise.
pub const REGRESSION_SHARE: f64 = 0.25;
pub const REGRESSION_MIB: u64 = 100;

/// The service's own process, which the host's snapshot does not list: resident and private,
/// in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServiceMemory {
    pub resident_kb: u64,
    pub private_kb: u64,
}

/// One reading's numbers, in KiB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reading {
    pub host_total_kb: u64,
    pub host_private_kb: u64,
    pub vm_kb: u64,
    pub vm_private_kb: u64,
    pub window_manager_kb: u64,
    pub renderer_kb: u64,
    pub app_windows_kb: u64,
    pub app_windows: u64,
    pub service_kb: u64,
    pub guest_total_kb: u64,
    pub guest_used_kb: u64,
    pub guest_cache_kb: u64,
    pub guest_anon_kb: u64,
    pub guest_kernel_kb: u64,
    pub slab_unreclaimable_kb: u64,
    pub kernel_stack_kb: u64,
    pub page_tables_kb: u64,
    pub vmalloc_kb: u64,
    pub zram_used_kb: u64,
    pub zram_stored_kb: u64,
    /// What the guest's processes cost: resident plus their share of zram.
    pub processes_kb: u64,
    /// The same for Google Play services' processes; 0 when none runs.
    pub google_play_kb: u64,
    /// The guest's free memory, and the part of it free page reporting has handed back to the
    /// host (0 when the guest's kernel does not say).
    pub guest_free_kb: u64,
    pub free_reported_kb: u64,
}

/// The numbers of one snapshot, with the service's own process. `None` when the guest has not
/// reported or no VM process was found, as nothing can be measured then.
pub fn reading(snapshot: &MemorySnapshot, service: ServiceMemory) -> Option<Reading> {
    let guest = &snapshot.guest.as_ref()?.memory;
    let role = |role: HostRole| {
        snapshot
            .host
            .iter()
            .filter(|p| p.role == role)
            .fold((0u64, 0u64, 0u64), |(kb, private, n), p| {
                (kb + p.working_set_kb, private + p.private_kb, n + 1)
            })
    };
    let (vm_kb, vm_private_kb, vms) = role(HostRole::Vm);
    if vms == 0 {
        return None;
    }
    let (window_manager_kb, window_manager_private, _) = role(HostRole::WindowManager);
    let (renderer_kb, renderer_private, _) = role(HostRole::Renderer);
    let (app_windows_kb, app_windows_private, app_windows) = role(HostRole::AppWindow);
    let k = &guest.kernel;
    Some(Reading {
        host_total_kb: snapshot.host_total_kb() + service.resident_kb,
        host_private_kb: vm_private_kb
            + window_manager_private
            + renderer_private
            + app_windows_private
            + service.private_kb,
        vm_kb,
        vm_private_kb,
        window_manager_kb,
        renderer_kb,
        app_windows_kb,
        app_windows,
        service_kb: service.resident_kb,
        guest_total_kb: guest.mem_total_kb,
        guest_used_kb: guest.used_kb(),
        guest_cache_kb: guest.cached_kb + guest.buffers_kb,
        guest_anon_kb: guest.anon_pages_kb,
        guest_kernel_kb: guest.other_kb(),
        slab_unreclaimable_kb: k.slab_unreclaimable_kb,
        kernel_stack_kb: k.kernel_stack_kb,
        page_tables_kb: k.page_tables_kb,
        vmalloc_kb: k.vmalloc_used_kb,
        zram_used_kb: guest.zram_used_kb,
        zram_stored_kb: guest.zram_original_kb,
        processes_kb: guest.apps().total_kb(),
        google_play_kb: guest.google_play_share().map_or(0, |play| play.total_kb()),
        guest_free_kb: guest.mem_free_kb,
        free_reported_kb: guest.page_reporting.map_or(0, |r| r.free_reported_kb),
    })
}

/// The median of each number across `readings` (the lower of the two middle ones for an even
/// count). `None` without readings.
pub fn median(readings: &[Reading]) -> Option<Reading> {
    if readings.is_empty() {
        return None;
    }
    let pick = |field: fn(&Reading) -> u64| {
        let mut values: Vec<u64> = readings.iter().map(field).collect();
        values.sort_unstable();
        values[(values.len() - 1) / 2]
    };
    Some(Reading {
        host_total_kb: pick(|r| r.host_total_kb),
        host_private_kb: pick(|r| r.host_private_kb),
        vm_kb: pick(|r| r.vm_kb),
        vm_private_kb: pick(|r| r.vm_private_kb),
        window_manager_kb: pick(|r| r.window_manager_kb),
        renderer_kb: pick(|r| r.renderer_kb),
        app_windows_kb: pick(|r| r.app_windows_kb),
        app_windows: pick(|r| r.app_windows),
        service_kb: pick(|r| r.service_kb),
        guest_total_kb: pick(|r| r.guest_total_kb),
        guest_used_kb: pick(|r| r.guest_used_kb),
        guest_cache_kb: pick(|r| r.guest_cache_kb),
        guest_anon_kb: pick(|r| r.guest_anon_kb),
        guest_kernel_kb: pick(|r| r.guest_kernel_kb),
        slab_unreclaimable_kb: pick(|r| r.slab_unreclaimable_kb),
        kernel_stack_kb: pick(|r| r.kernel_stack_kb),
        page_tables_kb: pick(|r| r.page_tables_kb),
        vmalloc_kb: pick(|r| r.vmalloc_kb),
        zram_used_kb: pick(|r| r.zram_used_kb),
        zram_stored_kb: pick(|r| r.zram_stored_kb),
        processes_kb: pick(|r| r.processes_kb),
        google_play_kb: pick(|r| r.google_play_kb),
        guest_free_kb: pick(|r| r.guest_free_kb),
        free_reported_kb: pick(|r| r.free_reported_kb),
    })
}

/// How much more the VM holds than the guest uses, in KiB, resident and private; negative
/// when the host has trimmed the VM below what the guest uses.
pub fn vm_beyond_guest_kb(r: &Reading) -> (i64, i64) {
    (
        r.vm_kb as i64 - r.guest_used_kb as i64,
        r.vm_private_kb as i64 - r.guest_used_kb as i64,
    )
}

/// The median of each of [`vm_beyond_guest_kb`]'s two numbers across `readings`, in KiB (the
/// lower of the two middle ones for an even count). `None` without readings.
pub fn median_gap_kb(readings: &[Reading]) -> Option<(i64, i64)> {
    if readings.is_empty() {
        return None;
    }
    let pick = |which: fn((i64, i64)) -> i64| {
        let mut values: Vec<i64> = readings
            .iter()
            .map(|r| which(vm_beyond_guest_kb(r)))
            .collect();
        values.sort_unstable();
        values[(values.len() - 1) / 2]
    };
    Some((pick(|g| g.0), pick(|g| g.1)))
}

/// KiB to whole MiB, rounded to nearest, keeping the sign.
pub fn signed_mib(kb: i64) -> i64 {
    if kb < 0 {
        -(mib(kb.unsigned_abs()) as i64)
    } else {
        mib(kb as u64) as i64
    }
}

/// One zram writeback as the guest's daemon logged it (`zram writeback: wrote N KiB ...`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WritebackRun {
    pub written_kb: u64,
    pub took_ms: u64,
    pub store_before_kb: u64,
    pub store_after_kb: u64,
    pub on_disk_kb: u64,
}

/// The zram writebacks in the guest's log `log`, in order. A line it cannot read is skipped.
pub fn writeback_runs(log: &str) -> Vec<WritebackRun> {
    fn number_after(text: &str, marker: &str) -> Option<(u64, usize)> {
        let at = text.find(marker)? + marker.len();
        let digits: String = text[at..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        Some((digits.parse().ok()?, at + digits.len()))
    }
    log.lines()
        .filter_map(|line| {
            let line = &line[line.find("zram writeback: wrote ")?..];
            let (written_kb, at) = number_after(line, "wrote ")?;
            let line = &line[at..];
            let (took_ms, at) = number_after(line, " in ")?;
            let line = &line[at..];
            let (store_before_kb, at) = number_after(line, "zram store ")?;
            let line = &line[at..];
            let (store_after_kb, at) = number_after(line, " -> ")?;
            let line = &line[at..];
            let (on_disk_kb, _) = number_after(line, "), ")?;
            Some(WritebackRun {
                written_kb,
                took_ms,
                store_before_kb,
                store_after_kb,
                on_disk_kb,
            })
        })
        .collect()
}

/// Whether the guest's daemon said zram has no backing file, so no writeback can run.
pub fn no_backing_file(log: &str) -> bool {
    log.contains("zram has no backing file") || log.contains("cannot read zram's backing device")
}

/// The second part of the report: the memory with the window open (`open`) and after it closed
/// (`idle`), and the zram writebacks that ran in between (`runs`), `waited_secs` after the
/// window closed. `None` without readings of both.
pub fn idle_report(
    open: &[Reading],
    idle: &[Reading],
    runs: &[WritebackRun],
    waited_secs: u64,
) -> Option<Report> {
    let before = median(open)?;
    let after = median(idle)?;
    let (gap_before, private_gap_before) = median_gap_kb(open)?;
    let (gap_after, private_gap_after) = median_gap_kb(idle)?;
    let mut md = String::new();
    md.push_str("## After the window closes\n\n");
    md.push_str(&format!(
        "The window closed and no app was in use for {waited_secs} s: the idle reclaim, which \
         also reports every free page to the host again, and the zram writeback (the boot \
         shortens its wait and its page age for this check). The median of {} readings, beside \
         the ones with the window open.\n\n",
        idle.len()
    ));
    md.push_str("| | Window open MiB | Closed MiB |\n|---|---|---|\n");
    for (name, b, a) in [
        ("VM resident", before.vm_kb, after.vm_kb),
        ("VM private", before.vm_private_kb, after.vm_private_kb),
        ("guest in use", before.guest_used_kb, after.guest_used_kb),
        ("guest free", before.guest_free_kb, after.guest_free_kb),
        (
            "guest free handed back to the host",
            before.free_reported_kb,
            after.free_reported_kb,
        ),
        ("zram store", before.zram_used_kb, after.zram_used_kb),
        (
            "zram holds (before compression)",
            before.zram_stored_kb,
            after.zram_stored_kb,
        ),
    ] {
        md.push_str(&format!("| {name} | {} | {} |\n", mib(b), mib(a)));
    }
    for (name, b, a) in [
        ("VM resident beyond guest in use", gap_before, gap_after),
        (
            "VM private beyond guest in use",
            private_gap_before,
            private_gap_after,
        ),
    ] {
        md.push_str(&format!(
            "| {name} | {} | {} |\n",
            signed_mib(b),
            signed_mib(a)
        ));
    }
    md.push('\n');
    let written_kb: u64 = runs.iter().map(|r| r.written_kb).sum();
    match (runs.first(), runs.last()) {
        (Some(first), Some(last)) => md.push_str(&format!(
            "zram writeback: {} run(s) wrote {} KiB; the zram store went from {} to {} KiB \
             around them, and the backing file holds {} KiB.\n",
            runs.len(),
            written_kb,
            first.store_before_kb,
            last.store_after_kb,
            last.on_disk_kb
        )),
        _ => md.push_str("No zram writeback ran.\n"),
    }
    let json = serde_json::json!({
        "waited_secs": waited_secs,
        "readings": idle.len(),
        "median_kb": reading_json(&after),
        "each_kb": idle.iter().map(reading_json).collect::<Vec<_>>(),
        "vm_beyond_guest_kb": {
            "open": {"resident": gap_before, "private": private_gap_before},
            "closed": {"resident": gap_after, "private": private_gap_after},
        },
        "writeback": {
            "runs": runs.len(),
            "written_kb": written_kb,
            "each": runs
                .iter()
                .map(|r| serde_json::json!({
                    "written_kb": r.written_kb,
                    "took_ms": r.took_ms,
                    "store_before_kb": r.store_before_kb,
                    "store_after_kb": r.store_after_kb,
                    "on_disk_kb": r.on_disk_kb,
                }))
                .collect::<Vec<_>>(),
        },
    });
    Some(Report { markdown: md, json })
}

/// One number of a reading, in MiB.
type Number = fn(&Reading) -> u64;

/// The numbers the baseline records, in MiB, by the name the report gives them.
const COMPARED: &[(&str, Number)] = &[
    ("host_total_mib", |r| mib(r.host_total_kb)),
    ("vm_mib", |r| mib(r.vm_kb)),
    ("guest_used_mib", |r| mib(r.guest_used_kb)),
    ("guest_kernel_mib", |r| mib(r.guest_kernel_kb)),
    ("processes_mib", |r| mib(r.processes_kb)),
];

/// What the measurement finds wrong against `baseline` (the JSON of [`BASELINE`]): one line for
/// each number more than [`REGRESSION_SHARE`] and [`REGRESSION_MIB`] above what the baseline
/// records. A number the baseline leaves `null` is not compared.
pub fn regressions(reading: &Reading, baseline: &serde_json::Value) -> Vec<String> {
    COMPARED
        .iter()
        .filter_map(|(name, value)| {
            let base = baseline[name].as_u64()?;
            let now = value(reading);
            let limit = base
                .saturating_add(REGRESSION_MIB)
                .max((base as f64 * (1.0 + REGRESSION_SHARE)).ceil() as u64);
            (now > limit).then(|| {
                format!(
                    "{name} is {now} MiB, more than {limit} MiB: the baseline recorded {base} MiB \
                     ({}), and a regression is more than {}% and {REGRESSION_MIB} MiB above it",
                    baseline["recorded"]
                        .as_str()
                        .unwrap_or("no record of where"),
                    (REGRESSION_SHARE * 100.0) as u64
                )
            })
        })
        .collect()
}

/// The numbers the baseline does not record yet, to say so in the report.
pub fn unrecorded(baseline: &serde_json::Value) -> Vec<&'static str> {
    COMPARED
        .iter()
        .filter(|(name, _)| baseline[name].as_u64().is_none())
        .map(|(name, _)| *name)
        .collect()
}

/// One process of `dumpsys meminfo`'s "Total PSS by process" list: its PSS in KiB and its
/// name with its pid, as the list gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPss {
    pub pss_kb: u64,
    pub name: String,
}

/// The "Total PSS by process" list of a `dumpsys meminfo`, largest first, as it is printed:
/// `    177,868K: com.android.vending (pid 5225 / activities)`. Empty when the text has none.
pub fn pss_by_process(dumpsys: &str) -> Vec<ProcessPss> {
    let mut lines = dumpsys.lines();
    if !lines.any(|line| line.trim_start().starts_with("Total PSS by process")) {
        return Vec::new();
    }
    lines
        .map(str::trim)
        .take_while(|line| !line.is_empty() && !line.starts_with("Total "))
        .filter_map(|line| {
            let (size, name) = line.split_once("K: ")?;
            let pss_kb = size.replace(',', "").trim().parse().ok()?;
            Some(ProcessPss {
                pss_kb,
                name: name.trim().to_string(),
            })
        })
        .collect()
}

/// The report: Markdown for the job's page, and JSON for the artifact and for recording a new
/// baseline.
pub struct Report {
    pub markdown: String,
    pub json: serde_json::Value,
}

/// Where and how the measurement ran, for the report.
pub struct Setting<'a> {
    /// The Google apps build installed, and the image it is mounted from.
    pub google_play: &'a str,
    /// The app whose window was open.
    pub app: &'a str,
    pub memory_mb: u64,
    pub processors: u64,
    /// The runner: its kernel, processor and memory.
    pub environment: &'a str,
}

/// The report of `readings` against `baseline`, with the guest's PSS by process.
pub fn report(
    readings: &[Reading],
    pss: &[ProcessPss],
    baseline: &serde_json::Value,
    setting: &Setting<'_>,
) -> Option<Report> {
    let m = median(readings)?;
    let problems = regressions(&m, baseline);
    let missing = unrecorded(baseline);
    let mut md = String::new();
    md.push_str("## Memory with Google Play and one app window\n\n");
    md.push_str(&format!(
        "{} with {}, one window of {} open and untouched for a minute; the median of {} readings. \
         The VM has {} MiB and {} processors. Runner: {}.\n\n",
        if m.google_play_kb > 0 {
            "Google Play services running"
        } else {
            "No Google Play services process running"
        },
        setting.google_play,
        setting.app,
        readings.len(),
        setting.memory_mb,
        setting.processors,
        setting.environment
    ));
    md.push_str(&format!(
        "**Host total: {} MiB resident** ({} MiB private)\n\n",
        mib(m.host_total_kb),
        mib(m.host_private_kb)
    ));
    md.push_str("| Host process | Resident MiB |\n|---|---|\n");
    for (name, kb) in [
        ("VM (crosvm)".to_string(), m.vm_kb),
        ("window manager".to_string(), m.window_manager_kb),
        ("graphics renderer".to_string(), m.renderer_kb),
        (format!("app windows ({})", m.app_windows), m.app_windows_kb),
        ("service".to_string(), m.service_kb),
    ] {
        md.push_str(&format!("| {name} | {} |\n", mib(kb)));
    }
    md.push_str(&format!(
        "\nVM private (all the guest memory it holds): {} MiB.\n\n",
        mib(m.vm_private_kb)
    ));
    let (gap, private_gap) = median_gap_kb(readings)?;
    md.push_str(&format!(
        "The VM holds {} MiB resident and {} MiB private beyond what the guest uses (memory the \
         guest freed that the host has not taken back yet).\n\n",
        signed_mib(gap),
        signed_mib(private_gap)
    ));
    if m.free_reported_kb > 0 {
        md.push_str(&format!(
            "Of the guest's {} MiB free, free page reporting has handed {} MiB back to the host \
             ({} MiB not yet).\n\n",
            mib(m.guest_free_kb),
            mib(m.free_reported_kb),
            mib(m.guest_free_kb.saturating_sub(m.free_reported_kb))
        ));
    }
    md.push_str("| Guest | MiB |\n|---|---|\n");
    for (name, kb) in [
        ("given", m.guest_total_kb),
        ("in use", m.guest_used_kb),
        ("page cache and buffers", m.guest_cache_kb),
        ("app memory (anonymous)", m.guest_anon_kb),
        ("kernel and everything else", m.guest_kernel_kb),
        ("unreclaimable slab", m.slab_unreclaimable_kb),
        ("kernel stacks", m.kernel_stack_kb),
        ("page tables", m.page_tables_kb),
        ("vmalloc", m.vmalloc_kb),
        ("zram store", m.zram_used_kb),
        ("zram holds (before compression)", m.zram_stored_kb),
        ("processes (resident plus zram share)", m.processes_kb),
        (
            "Google Play services (resident plus zram share)",
            m.google_play_kb,
        ),
    ] {
        md.push_str(&format!("| {name} | {} |\n", mib(kb)));
    }
    if !pss.is_empty() {
        md.push_str("\n| Guest process | PSS MiB |\n|---|---|\n");
        for p in pss.iter().take(25) {
            md.push_str(&format!("| {} | {} |\n", p.name, mib(p.pss_kb)));
        }
        md.push_str("\nPSS counts what is in zram at its size before compression.\n");
    }
    md.push('\n');
    if problems.is_empty() {
        md.push_str("No regression against the baseline.\n");
    } else {
        md.push_str("**Regressions against the baseline:**\n\n");
        for problem in &problems {
            md.push_str(&format!("- {problem}\n"));
        }
    }
    if !missing.is_empty() {
        md.push_str(&format!(
            "\nThe baseline records no value yet for {}; record this run's `baseline` values in \
             `dsa/host/platform/linux/tests/memory_check/baseline.json`.\n",
            missing.join(", ")
        ));
    }
    let mut measured = serde_json::Map::new();
    for (name, value) in COMPARED {
        measured.insert((*name).to_string(), value(&m).into());
    }
    let json = serde_json::json!({
        "setting": {
            "google_play": setting.google_play,
            "app": setting.app,
            "memory_mb": setting.memory_mb,
            "processors": setting.processors,
            "environment": setting.environment,
        },
        "readings": readings.len(),
        "baseline": measured,
        "median_kb": reading_json(&m),
        "vm_beyond_guest_kb": {"resident": gap, "private": private_gap},
        "each_kb": readings.iter().map(reading_json).collect::<Vec<_>>(),
        "pss_by_process_kb": pss
            .iter()
            .map(|p| serde_json::json!({"name": p.name, "pss_kb": p.pss_kb}))
            .collect::<Vec<_>>(),
        "regressions": problems,
        "unrecorded": missing,
    });
    Some(Report { markdown: md, json })
}

fn reading_json(r: &Reading) -> serde_json::Value {
    serde_json::json!({
        "host_total": r.host_total_kb,
        "host_private": r.host_private_kb,
        "vm": r.vm_kb,
        "vm_private": r.vm_private_kb,
        "window_manager": r.window_manager_kb,
        "renderer": r.renderer_kb,
        "app_windows": r.app_windows_kb,
        "app_window_count": r.app_windows,
        "service": r.service_kb,
        "guest_total": r.guest_total_kb,
        "guest_used": r.guest_used_kb,
        "guest_cache": r.guest_cache_kb,
        "guest_anon": r.guest_anon_kb,
        "guest_kernel": r.guest_kernel_kb,
        "slab_unreclaimable": r.slab_unreclaimable_kb,
        "kernel_stack": r.kernel_stack_kb,
        "page_tables": r.page_tables_kb,
        "vmalloc": r.vmalloc_kb,
        "zram_used": r.zram_used_kb,
        "zram_stored": r.zram_stored_kb,
        "processes": r.processes_kb,
        "google_play": r.google_play_kb,
        "guest_free": r.guest_free_kb,
        "free_reported": r.free_reported_kb,
    })
}

/// KiB to whole MiB, rounded to nearest.
pub fn mib(kb: u64) -> u64 {
    (kb + 512) / 1024
}

/// KiB to MiB with one decimal, for the smaller kernel parts.
fn mib1(kb: u64) -> String {
    format!("{:.1}", kb as f64 / 1024.0)
}

/// What starts each line of the guest's kernel memory report in the kernel log, after the
/// daemon's prefix: `dsa-channeld: memory report: <line>`.
pub const KERNEL_REPORT_MARK: &str = "memory report: ";

/// The property the shell sets to ask the guest for its kernel memory report.
pub const KERNEL_REPORT_PROPERTY: &str = "debug.dsa.memory_report";

/// The last whole kernel memory report in `log` (a `logcat -b kernel`), its lines without the
/// mark, `begin` and `end` left out. `None` when no report has both.
pub fn kernel_report(log: &str) -> Option<Vec<String>> {
    let mut current: Option<Vec<String>> = None;
    let mut last = None;
    for line in log.lines() {
        let Some((_, rest)) = line.split_once(KERNEL_REPORT_MARK) else {
            continue;
        };
        match rest.trim() {
            "begin" => current = Some(Vec::new()),
            "end" => {
                if let Some(lines) = current.take() {
                    last = Some(lines);
                }
            }
            other => {
                if let Some(lines) = current.as_mut() {
                    lines.push(other.to_string());
                }
            }
        }
    }
    last
}

/// The words of the report's lines of `kind`, after the kind.
fn report_lines<'a>(lines: &'a [String], kind: &'a str) -> impl Iterator<Item = Vec<&'a str>> {
    lines.iter().filter_map(move |line| {
        let mut words = line.split_whitespace();
        (words.next() == Some(kind)).then(|| words.collect())
    })
}

fn number(word: Option<&&str>) -> u64 {
    word.and_then(|w| w.parse().ok()).unwrap_or(0)
}

/// The kernel memory report as Markdown for the job's page: what the guest kernel's
/// unreclaimable slab, vmalloc, kernel stacks and page tables are made of.
pub fn kernel_markdown(lines: &[String]) -> String {
    let mut md = String::from("## The guest kernel's memory\n\n");
    let meminfo: Vec<(String, u64)> = report_lines(lines, "meminfo")
        .filter_map(|w| Some((w.first()?.to_string(), w.get(1)?.parse().ok()?)))
        .collect();
    if !meminfo.is_empty() {
        md.push_str("| /proc/meminfo | MiB |\n|---|---|\n");
        for (key, kib) in &meminfo {
            md.push_str(&format!("| {key} | {} |\n", mib1(*kib)));
        }
        md.push('\n');
    }
    if let Some(total) = report_lines(lines, "slab-total").next() {
        md.push_str(&format!(
            "**Slab: {} caches, {} MiB**, of which {} MiB holds no object in use.\n\n",
            number(total.first()),
            mib1(number(total.get(1))),
            mib1(number(total.get(2)))
        ));
        md.push_str("| Slab cache | MiB | Objects in use | Objects | Object size |\n|---|---|---|---|---|\n");
        for w in report_lines(lines, "slab").take(25) {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                w.first().unwrap_or(&"?"),
                mib1(number(w.get(1))),
                number(w.get(2)),
                number(w.get(3)),
                number(w.get(4))
            ));
        }
        md.push('\n');
    }
    if let Some(total) = report_lines(lines, "vmalloc-total").next() {
        md.push_str(&format!(
            "**vmalloc: {} areas, {} MiB.**\n\n| Caller | MiB | Areas |\n|---|---|---|\n",
            number(total.first()),
            mib1(number(total.get(1)))
        ));
        for w in report_lines(lines, "vmalloc").take(15) {
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                w.first().unwrap_or(&"?"),
                mib1(number(w.get(1))),
                number(w.get(2))
            ));
        }
        md.push('\n');
    }
    if let Some(total) = report_lines(lines, "threads-total").next() {
        let stack = number(total.get(3));
        md.push_str(&format!(
            "**Threads: {}** ({} kernel, {} user), each with a {stack} KiB kernel stack.\n\n",
            number(total.first()),
            number(total.get(1)),
            number(total.get(2))
        ));
        md.push_str("| Threads of | Kind | Threads | Processes | Kernel stacks MiB |\n|---|---|---|---|---|\n");
        for kind in ["kernel", "user"] {
            for w in report_lines(lines, "threads")
                .filter(|w| w.first() == Some(&kind))
                .take(15)
            {
                let threads = number(w.get(2));
                md.push_str(&format!(
                    "| {} | {kind} | {threads} | {} | {} |\n",
                    w.get(1).unwrap_or(&"?"),
                    number(w.get(3)),
                    mib1(threads * stack)
                ));
            }
        }
        md.push('\n');
    }
    let tables: Vec<Vec<&str>> = report_lines(lines, "page-tables").take(10).collect();
    if !tables.is_empty() {
        md.push_str("| Page tables of | MiB |\n|---|---|\n");
        for w in tables {
            md.push_str(&format!(
                "| {} | {} |\n",
                w.first().unwrap_or(&"?"),
                mib1(number(w.get(1)))
            ));
        }
        md.push('\n');
    }
    if let Some(w) = report_lines(lines, "cgroups").next() {
        md.push_str(&format!(
            "Control groups: {} live, {} dying (removed, still holding memory charged to them).\n\n",
            number(w.first()),
            number(w.get(1))
        ));
    }
    if let Some(w) = report_lines(lines, "bufio").next() {
        md.push_str(&format!(
            "dm-bufio (dm-verity's hash block cache): {} MiB.\n\n",
            mib1(number(w.first()) / 1024)
        ));
    }
    for w in report_lines(lines, "missing") {
        md.push_str(&format!("Not read: {}.\n\n", w.join(" ")));
    }
    md
}

/// One line of the App Summary of a `dumpsys meminfo <pid>`: its name and its PSS in KiB.
/// A line with no PSS of its own (`Unknown`, which only has an RSS) is left out.
pub fn app_summary(meminfo: &str) -> Vec<(String, u64)> {
    let mut lines = meminfo.lines();
    if !lines.by_ref().any(|l| l.trim() == "App Summary") {
        return Vec::new();
    }
    let Some(columns) = lines.next() else {
        return Vec::new();
    };
    // The PSS column ends where its heading does; a number that ends further right is RSS.
    let Some(pss_end) = columns.find("Pss(KB)").map(|at| at + "Pss(KB)".len()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("TOTAL") {
            let words: Vec<&str> = trimmed.split_whitespace().collect();
            for (at, pair) in words.windows(2).enumerate() {
                if pair[0] == "PSS:" {
                    let name = if at >= 2 && words[at - 1] == "SWAP" {
                        "TOTAL SWAP PSS"
                    } else {
                        "TOTAL PSS"
                    };
                    if let Ok(kb) = pair[1].parse() {
                        out.push((name.to_string(), kb));
                    }
                }
            }
            break;
        }
        let Some((name, values)) = line.split_once(':') else {
            continue;
        };
        let offset = name.len() + 1;
        let Some(start) = values.find(|c: char| c.is_ascii_digit()) else {
            continue;
        };
        let digits: String = values[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if offset + start + digits.len() > pss_end + 2 {
            continue;
        }
        if let Ok(kb) = digits.parse() {
            out.push((name.trim().to_string(), kb));
        }
    }
    out
}

/// The value after `label` in a `dumpsys meminfo <pid>`'s Objects section
/// (`Local Binders:`, `Proxy Binders:`, `Death Recipients:`).
pub fn meminfo_object(meminfo: &str, label: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let (_, after) = line.split_once(label)?;
        after.split_whitespace().next()?.parse().ok()
    })
}

/// Thread names grouped by their shape, every run of digits made `#` (`Binder:1234_3` is
/// `Binder:#_#`), most threads first.
pub fn thread_shapes(names: &str) -> Vec<(String, u64)> {
    let mut groups: Vec<(String, u64)> = Vec::new();
    for name in names.lines().map(str::trim).filter(|n| !n.is_empty()) {
        let mut shape = String::new();
        let mut in_digits = false;
        for c in name.chars() {
            if c.is_ascii_digit() {
                if !in_digits {
                    shape.push('#');
                }
                in_digits = true;
            } else {
                shape.push(c);
                in_digits = false;
            }
        }
        match groups.iter_mut().find(|(s, _)| *s == shape) {
            Some((_, count)) => *count += 1,
            None => groups.push((shape, 1)),
        }
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    groups
}

/// The services `dumpsys -l` lists.
pub fn listed_services(list: &str) -> Vec<&str> {
    list.lines()
        .filter(|line| line.starts_with("  "))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

/// What the measurement found about system_server, for the report.
pub struct SystemServer<'a> {
    /// Its `dumpsys meminfo <pid>`.
    pub meminfo: &'a str,
    /// Its threads' names, one a line.
    pub threads: &'a str,
    /// `dumpsys -l`.
    pub services: &'a str,
    /// The ART heap settings, `<name>=<value>` a line.
    pub heap_settings: &'a str,
    /// What asking for a heap dump said.
    pub heap_dump: &'a str,
}

/// The system_server part of the report, as Markdown and JSON.
pub fn system_server_report(s: &SystemServer<'_>) -> (String, serde_json::Value) {
    let summary = app_summary(s.meminfo);
    let shapes = thread_shapes(s.threads);
    let threads: u64 = shapes.iter().map(|(_, n)| n).sum();
    let services = listed_services(s.services);
    let mut md = String::from("## system_server\n\n");
    if summary.is_empty() {
        md.push_str("Its `dumpsys meminfo` had no App Summary.\n\n");
    } else {
        md.push_str("| App Summary | PSS MiB |\n|---|---|\n");
        for (name, kb) in &summary {
            md.push_str(&format!("| {name} | {} |\n", mib1(*kb)));
        }
        md.push('\n');
    }
    let binders: Vec<String> = ["Local Binders:", "Proxy Binders:", "Death Recipients:"]
        .iter()
        .filter_map(|label| {
            meminfo_object(s.meminfo, label)
                .map(|n| format!("{} {n}", label.trim_end_matches(':').to_lowercase()))
        })
        .collect();
    if !binders.is_empty() {
        md.push_str(&format!("Binder objects: {}.\n\n", binders.join(", ")));
    }
    md.push_str(&format!(
        "**{threads} threads** (a 16 KiB kernel stack each, about {} MiB), {} system services \
         listed by `dumpsys -l`.\n\n",
        mib1(threads * 16),
        services.len()
    ));
    if !shapes.is_empty() {
        md.push_str("| Thread name | Threads |\n|---|---|\n");
        for (shape, n) in shapes.iter().take(20) {
            md.push_str(&format!("| `{shape}` | {n} |\n"));
        }
        md.push('\n');
    }
    if !s.heap_settings.trim().is_empty() {
        md.push_str("ART heap settings: ");
        let settings: Vec<String> = s
            .heap_settings
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| format!("`{l}`"))
            .collect();
        md.push_str(&settings.join(", "));
        md.push_str(".\n\n");
    }
    md.push_str(&format!("Heap dump: {}\n", s.heap_dump.trim()));
    let json = serde_json::json!({
        "app_summary_kb": summary
            .iter()
            .map(|(name, kb)| serde_json::json!({"name": name, "pss_kb": kb}))
            .collect::<Vec<_>>(),
        "threads": threads,
        "thread_names": shapes
            .iter()
            .map(|(shape, n)| serde_json::json!({"name": shape, "threads": n}))
            .collect::<Vec<_>>(),
        "services": services,
        "heap_settings": s.heap_settings.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>(),
        "heap_dump": s.heap_dump.trim(),
    });
    (md, json)
}

/// The ART heap settings in a `getprop`'s output, as `<name>=<value>` lines: the heap's sizes
/// and free space, and the factor a foreground process's free space is multiplied by.
pub fn heap_settings(getprop: &str) -> String {
    getprop
        .lines()
        .filter_map(|line| {
            let (name, value) = line.trim().split_once("]: [")?;
            let name = name.strip_prefix('[')?;
            let value = value.strip_suffix(']')?;
            (name.starts_with("dalvik.vm.heap")
                || name == "dalvik.vm.foreground-heap-growth-multiplier")
                .then(|| format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
