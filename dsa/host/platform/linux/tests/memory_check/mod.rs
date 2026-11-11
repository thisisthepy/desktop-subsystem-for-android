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
//! - the guest's PSS by process, as `dumpsys meminfo` lists it.
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
    })
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
    })
}

/// KiB to whole MiB, rounded to nearest.
pub fn mib(kb: u64) -> u64 {
    (kb + 512) / 1024
}
