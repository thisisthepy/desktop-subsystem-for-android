//! How much memory a user's Android takes: inside the guest, as the guest's agent reports
//! it over the control channel, and on the host, the processes that run it (the VM's
//! memory process, the window manager and each app window), as the platform measures them.
//!
//! The guest's report says where its memory goes (page cache, app memory, compressed swap,
//! the largest processes); the host's numbers say what that costs the host. Together they
//! show whether memory the guest is not using went back to the host.

use std::collections::BTreeSet;
use std::fmt;
use std::time::{Duration, Instant};

use dsa_protocol::control;
use dsa_protocol::host_api::types;

use crate::host_api::UserAndroids;
use crate::session::SessionState;
use crate::tasks::TaskEvent;

/// How often the service logs each running Android's memory: as often as the guest reports.
pub const LOG_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// One guest process's share of the guest's memory, in KiB.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessMemory {
    pub pid: i32,
    pub name: String,
    pub pss_kb: u64,
    pub swap_pss_kb: u64,
}

/// How the guest uses its memory, in KiB, as its agent last reported it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuestMemory {
    pub mem_total_kb: u64,
    pub mem_free_kb: u64,
    pub mem_available_kb: u64,
    pub buffers_kb: u64,
    pub cached_kb: u64,
    pub anon_pages_kb: u64,
    pub shmem_kb: u64,
    pub slab_reclaimable_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
    pub zram_original_kb: u64,
    pub zram_used_kb: u64,
    pub total_pss_kb: u64,
    /// Largest first.
    pub top_processes: Vec<ProcessMemory>,
}

/// The guest's last report and how old it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMemoryReport {
    pub memory: GuestMemory,
    pub age: Duration,
}

/// What a host process does for the user's Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRole {
    /// The VM's memory: on Windows, its vmmem process.
    Vm,
    WindowManager,
    AppWindow,
}

impl HostRole {
    pub fn name(self) -> &'static str {
        match self {
            HostRole::Vm => "vm",
            HostRole::WindowManager => "window manager",
            HostRole::AppWindow => "app window",
        }
    }

    fn from_name(name: &str) -> Option<HostRole> {
        [HostRole::Vm, HostRole::WindowManager, HostRole::AppWindow]
            .into_iter()
            .find(|role| role.name() == name)
    }
}

/// One host process's memory, in KiB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProcessMemory {
    pub role: HostRole,
    pub pid: u32,
    /// Resident in RAM now: what the host's task manager shows as the process's memory.
    pub working_set_kb: u64,
    /// Committed to it alone.
    pub private_kb: u64,
}

/// Everything known about a user's Android's memory at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemorySnapshot {
    pub guest: Option<GuestMemoryReport>,
    /// In the order: the VM, the window manager, the app windows.
    pub host: Vec<HostProcessMemory>,
}

/// Measures the host processes that run a user's Android. The platform supplies it.
pub trait HostMemory: Send + Sync {
    /// The processes of `user`'s Android, whose VM has the runtime ID `vm_id` while it runs.
    fn processes(&self, user: &str, vm_id: Option<&str>) -> Vec<HostProcessMemory>;
}

impl GuestMemory {
    /// The memory in use in the guest, free memory aside.
    pub fn used_kb(&self) -> u64 {
        self.mem_total_kb.saturating_sub(self.mem_free_kb)
    }

    /// The swap in use.
    pub fn swap_used_kb(&self) -> u64 {
        self.swap_total_kb.saturating_sub(self.swap_free_kb)
    }

    pub fn from_control(memory: control::GuestMemory) -> GuestMemory {
        GuestMemory {
            mem_total_kb: memory.mem_total_kb,
            mem_free_kb: memory.mem_free_kb,
            mem_available_kb: memory.mem_available_kb,
            buffers_kb: memory.buffers_kb,
            cached_kb: memory.cached_kb,
            anon_pages_kb: memory.anon_pages_kb,
            shmem_kb: memory.shmem_kb,
            slab_reclaimable_kb: memory.slab_reclaimable_kb,
            swap_total_kb: memory.swap_total_kb,
            swap_free_kb: memory.swap_free_kb,
            zram_original_kb: memory.zram_original_kb,
            zram_used_kb: memory.zram_used_kb,
            total_pss_kb: memory.total_pss_kb,
            top_processes: memory
                .top_processes
                .into_iter()
                .map(|p| ProcessMemory {
                    pid: p.pid,
                    name: p.name,
                    pss_kb: p.pss_kb,
                    swap_pss_kb: p.swap_pss_kb,
                })
                .collect(),
        }
    }
}

impl MemorySnapshot {
    /// The host's memory for the user's Android: every process's working set together.
    pub fn host_total_kb(&self) -> u64 {
        self.host.iter().map(|p| p.working_set_kb).sum()
    }

    fn role_total_kb(&self, role: HostRole) -> (u64, usize) {
        self.host
            .iter()
            .filter(|p| p.role == role)
            .fold((0, 0), |(kb, n), p| (kb + p.working_set_kb, n + 1))
    }

    /// One line for the service's log: the host's total and its parts, then what the guest
    /// last said about its memory.
    pub fn summary_line(&self) -> String {
        let (vm, _) = self.role_total_kb(HostRole::Vm);
        let (wm, _) = self.role_total_kb(HostRole::WindowManager);
        let (apps, app_count) = self.role_total_kb(HostRole::AppWindow);
        let mut line = format!(
            "memory: host {} MiB (vm {}, window manager {}, {app_count} app windows {})",
            mib(self.host_total_kb()),
            mib(vm),
            mib(wm),
            mib(apps)
        );
        match &self.guest {
            None => line.push_str("; guest has not reported"),
            Some(report) => {
                let g = &report.memory;
                line.push_str(&format!(
                    "; guest {}/{} MiB used, cache {}, apps {}, swap {} (zram {}), pss {} MiB, \
                     {} s ago",
                    mib(g.used_kb()),
                    mib(g.mem_total_kb),
                    mib(g.cached_kb + g.buffers_kb),
                    mib(g.anon_pages_kb),
                    mib(g.swap_used_kb()),
                    mib(g.zram_used_kb),
                    mib(g.total_pss_kb),
                    report.age.as_secs()
                ));
            }
        }
        line
    }
}

/// KiB to whole MiB, rounded to nearest.
fn mib(kb: u64) -> u64 {
    (kb + 512) / 1024
}

impl fmt::Display for MemorySnapshot {
    /// The report `dsa memory` prints: the summary, then the host processes and the guest's
    /// details.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Host: {} MiB in all", mib(self.host_total_kb()))?;
        if self.host.is_empty() {
            writeln!(f, "  no process of this Android is running")?;
        }
        for p in &self.host {
            writeln!(
                f,
                "  {:<15} pid {:<6} {:>6} MiB resident, {:>6} MiB private",
                p.role.name(),
                p.pid,
                mib(p.working_set_kb),
                mib(p.private_kb)
            )?;
        }
        let Some(report) = &self.guest else {
            return writeln!(
                f,
                "Android: no report yet (it sends one a minute after it starts)"
            );
        };
        let g = &report.memory;
        writeln!(
            f,
            "Android ({} s ago): {} of {} MiB in use, {} MiB available",
            report.age.as_secs(),
            mib(g.used_kb()),
            mib(g.mem_total_kb),
            mib(g.mem_available_kb)
        )?;
        writeln!(
            f,
            "  page cache {} MiB (shared memory {}), buffers {}, reclaimable kernel caches {}",
            mib(g.cached_kb),
            mib(g.shmem_kb),
            mib(g.buffers_kb),
            mib(g.slab_reclaimable_kb)
        )?;
        writeln!(f, "  app memory (anonymous) {} MiB", mib(g.anon_pages_kb))?;
        writeln!(
            f,
            "  swap {} of {} MiB used; zram holds {} MiB in {} MiB",
            mib(g.swap_used_kb()),
            mib(g.swap_total_kb),
            mib(g.zram_original_kb),
            mib(g.zram_used_kb)
        )?;
        writeln!(f, "  processes: {} MiB PSS in all", mib(g.total_pss_kb))?;
        for p in &g.top_processes {
            writeln!(f, "    {:>6} MiB  {:<6} {}", mib(p.pss_kb), p.pid, p.name)?;
        }
        Ok(())
    }
}

/// The snapshot as the local API carries it.
pub fn snapshot_to_wire(snapshot: &MemorySnapshot) -> types::Memory {
    types::Memory {
        guest: snapshot.guest.as_ref().map(|report| {
            let g = &report.memory;
            types::GuestMemory {
                mem_total_kb: g.mem_total_kb,
                mem_free_kb: g.mem_free_kb,
                mem_available_kb: g.mem_available_kb,
                buffers_kb: g.buffers_kb,
                cached_kb: g.cached_kb,
                anon_pages_kb: g.anon_pages_kb,
                shmem_kb: g.shmem_kb,
                slab_reclaimable_kb: g.slab_reclaimable_kb,
                swap_total_kb: g.swap_total_kb,
                swap_free_kb: g.swap_free_kb,
                zram_original_kb: g.zram_original_kb,
                zram_used_kb: g.zram_used_kb,
                total_pss_kb: g.total_pss_kb,
                top_processes: g
                    .top_processes
                    .iter()
                    .map(|p| types::GuestProcessMemory {
                        pid: p.pid,
                        name: p.name.clone(),
                        pss_kb: p.pss_kb,
                        swap_pss_kb: p.swap_pss_kb,
                    })
                    .collect(),
                age_seconds: report.age.as_secs(),
            }
        }),
        host: snapshot
            .host
            .iter()
            .map(|p| types::HostProcessMemory {
                role: p.role.name().into(),
                pid: p.pid,
                working_set_kb: p.working_set_kb,
                private_kb: p.private_kb,
            })
            .collect(),
    }
}

/// The snapshot the local API carried. A host process of a role this build does not know
/// is left out.
pub fn snapshot_from_wire(memory: types::Memory) -> MemorySnapshot {
    MemorySnapshot {
        guest: memory.guest.map(|g| GuestMemoryReport {
            age: Duration::from_secs(g.age_seconds),
            memory: GuestMemory {
                mem_total_kb: g.mem_total_kb,
                mem_free_kb: g.mem_free_kb,
                mem_available_kb: g.mem_available_kb,
                buffers_kb: g.buffers_kb,
                cached_kb: g.cached_kb,
                anon_pages_kb: g.anon_pages_kb,
                shmem_kb: g.shmem_kb,
                slab_reclaimable_kb: g.slab_reclaimable_kb,
                swap_total_kb: g.swap_total_kb,
                swap_free_kb: g.swap_free_kb,
                zram_original_kb: g.zram_original_kb,
                zram_used_kb: g.zram_used_kb,
                total_pss_kb: g.total_pss_kb,
                top_processes: g
                    .top_processes
                    .into_iter()
                    .map(|p| ProcessMemory {
                        pid: p.pid,
                        name: p.name,
                        pss_kb: p.pss_kb,
                        swap_pss_kb: p.swap_pss_kb,
                    })
                    .collect(),
            },
        }),
        host: memory
            .host
            .into_iter()
            .filter_map(|p| {
                Some(HostProcessMemory {
                    role: HostRole::from_name(&p.role)?,
                    pid: p.pid,
                    working_set_kb: p.working_set_kb,
                    private_kb: p.private_kb,
                })
            })
            .collect(),
    }
}

/// One line for each running Android of `androids`, for the service's log: the user, then
/// the summary of its memory on the host and in the guest. The guest is not asked for a new
/// report; it sends one every few minutes by itself.
pub fn summary_lines(androids: &dyn UserAndroids, host: &dyn HostMemory) -> Vec<String> {
    androids
        .running()
        .into_iter()
        .filter(|(_, state)| *state != SessionState::Stopped)
        .filter_map(|(user, _)| {
            let android = androids.android(&user).ok()?;
            let vm_id = android.vm_id();
            let snapshot = MemorySnapshot {
                guest: android.guest_memory(),
                host: host.processes(&user, vm_id.as_deref()),
            };
            Some(format!("{user}: {}", snapshot.summary_line()))
        })
        .collect()
}

/// How long the window manager waits after the last task window closed before it gives back
/// the memory it is not using.
pub const IDLE_TRIM_DELAY: Duration = Duration::from_secs(30);

/// Decides when a host process that serves task windows (the window manager) should give
/// back the memory it is not using: once each time no task window has been open for a while.
///
/// While windows are open its memory is in use (the renderer, each task's frames); once the
/// last one closes, most of it is not, but the system takes it back only under pressure, so
/// the process shows it as its own until then. Giving it back when idle is what makes the
/// host's memory for an idle Android small.
#[derive(Debug, Clone)]
pub struct IdleTrim {
    delay: Duration,
    open: BTreeSet<i32>,
    /// No open window is shown: they are all minimized, and the guest dozes.
    all_hidden: bool,
    idle_since: Option<Instant>,
    trimmed: bool,
}

impl IdleTrim {
    /// Starts idle, as at start-up, when no window is open yet.
    pub fn new(delay: Duration, now: Instant) -> IdleTrim {
        IdleTrim {
            delay,
            open: BTreeSet::new(),
            all_hidden: false,
            idle_since: Some(now),
            trimmed: false,
        }
    }

    /// Takes in a change to the guest's tasks. `None` says the tasks are unknown (the service
    /// cannot be reached), which counts as none.
    pub fn task_event(&mut self, event: Option<&TaskEvent>, now: Instant) {
        match event {
            Some(TaskEvent::Updated(task)) => {
                self.open.insert(task.task_id);
            }
            Some(TaskEvent::Removed { task_id }) => {
                self.open.remove(task_id);
            }
            Some(_) => {}
            None => self.open.clear(),
        }
        self.settle(now);
    }

    /// Replaces what it knows with the tasks there are now.
    pub fn sync(&mut self, events: &[TaskEvent], now: Instant) {
        self.open.clear();
        for event in events {
            if let TaskEvent::Updated(task) = event {
                self.open.insert(task.task_id);
            }
        }
        self.settle(now);
    }

    /// Takes in whether any window is shown. With every window minimized there is no frame to
    /// show, so the memory behind them is as idle as if they had closed.
    pub fn windows_shown(&mut self, shown: bool, now: Instant) {
        self.all_hidden = !shown;
        self.settle(now);
    }

    fn settle(&mut self, now: Instant) {
        if !self.open.is_empty() && !self.all_hidden {
            self.idle_since = None;
            self.trimmed = false;
        } else if self.idle_since.is_none() {
            self.idle_since = Some(now);
        }
    }

    /// Whether to give memory back now. True once per idle stretch.
    pub fn due(&mut self, now: Instant) -> bool {
        let due = !self.trimmed
            && self
                .idle_since
                .is_some_and(|since| now.saturating_duration_since(since) >= self.delay);
        if due {
            self.trimmed = true;
        }
        due
    }
}
