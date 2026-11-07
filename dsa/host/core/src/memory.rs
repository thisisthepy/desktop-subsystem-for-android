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
    /// What the kernel itself holds.
    pub kernel: KernelMemory,
}

/// What the guest's kernel itself holds, in KiB, from /proc/meminfo, and the memory it was
/// given. 0 for what the guest could not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KernelMemory {
    pub slab_unreclaimable_kb: u64,
    pub kernel_stack_kb: u64,
    pub page_tables_kb: u64,
    pub vmalloc_used_kb: u64,
    pub percpu_kb: u64,
    pub cma_total_kb: u64,
    pub hugetlb_kb: u64,
    /// The memory the kernel was given (its online memory blocks).
    pub mem_online_kb: u64,
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

    /// The rest of the memory in use: kernel stacks, page tables, unreclaimable slab, zram's
    /// own store, and buffers pinned by drivers.
    pub fn other_kb(&self) -> u64 {
        self.used_kb().saturating_sub(
            self.cached_kb + self.buffers_kb + self.anon_pages_kb + self.slab_reclaimable_kb,
        )
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
            kernel: KernelMemory {
                slab_unreclaimable_kb: memory.slab_unreclaimable_kb,
                kernel_stack_kb: memory.kernel_stack_kb,
                page_tables_kb: memory.page_tables_kb,
                vmalloc_used_kb: memory.vmalloc_used_kb,
                percpu_kb: memory.percpu_kb,
                cma_total_kb: memory.cma_total_kb,
                hugetlb_kb: memory.hugetlb_kb,
                mem_online_kb: memory.mem_online_kb,
            },
        }
    }

    /// What of [`other_kb`](Self::other_kb) no counter names: memory drivers took for
    /// themselves, such as GPU and DMA buffers.
    pub fn unaccounted_kb(&self) -> u64 {
        let k = &self.kernel;
        self.other_kb().saturating_sub(
            k.slab_unreclaimable_kb
                + k.kernel_stack_kb
                + k.page_tables_kb
                + k.vmalloc_used_kb
                + k.percpu_kb
                + k.hugetlb_kb
                + self.zram_used_kb,
        )
    }

    /// What the kernel reserved at boot, before MemTotal: its code and data, the page
    /// structures, swiotlb, pstore. `None` when the guest could not count its memory.
    pub fn reserved_at_boot_kb(&self) -> Option<u64> {
        (self.kernel.mem_online_kb > 0)
            .then(|| self.kernel.mem_online_kb.saturating_sub(self.mem_total_kb))
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

    /// How much more the VM holds on the host than the guest says it uses, in KiB: free guest
    /// memory the host has not got back. `None` until the guest has reported or without a VM.
    ///
    /// What the kernel reserved at boot (its code, the page structures, swiotlb) is outside
    /// MemTotal, so it is not in the guest's use either, but it is in use for as long as the
    /// VM runs and the host can never take it back. It counts as used here; counting it as
    /// free memory not returned would show a gap that no reclaim can close.
    pub fn vm_beyond_guest_kb(&self) -> Option<u64> {
        let guest = &self.guest.as_ref()?.memory;
        let (vm, count) = self.role_total_kb(HostRole::Vm);
        let held = guest.used_kb() + guest.reserved_at_boot_kb().unwrap_or(0);
        (count > 0).then(|| vm.saturating_sub(held))
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
        writeln!(f, "  kernel and everything else {} MiB", mib(g.other_kb()))?;
        let k = &g.kernel;
        writeln!(
            f,
            "    unreclaimable slab {}, kernel stacks {}, page tables {}, vmalloc {}, per-CPU {}, \
             huge pages {}, zram store {}, unaccounted (drivers, GPU and DMA buffers) {} MiB",
            mib(k.slab_unreclaimable_kb),
            mib(k.kernel_stack_kb),
            mib(k.page_tables_kb),
            mib(k.vmalloc_used_kb),
            mib(k.percpu_kb),
            mib(k.hugetlb_kb),
            mib(g.zram_used_kb),
            mib(g.unaccounted_kb())
        )?;
        if let Some(reserved) = g.reserved_at_boot_kb() {
            writeln!(
                f,
                "  reserved by the kernel at boot {} MiB of {} MiB given (code, page structures, \
                 swiotlb, pstore); contiguous area (CMA) {} MiB",
                mib(reserved),
                mib(k.mem_online_kb),
                mib(k.cma_total_kb)
            )?;
        }
        if let Some(beyond) = self.vm_beyond_guest_kb() {
            writeln!(
                f,
                "  the VM holds {} MiB beyond what Android uses (free memory not yet returned)",
                mib(beyond)
            )?;
        }
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
                slab_unreclaimable_kb: g.kernel.slab_unreclaimable_kb,
                kernel_stack_kb: g.kernel.kernel_stack_kb,
                page_tables_kb: g.kernel.page_tables_kb,
                vmalloc_used_kb: g.kernel.vmalloc_used_kb,
                percpu_kb: g.kernel.percpu_kb,
                cma_total_kb: g.kernel.cma_total_kb,
                hugetlb_kb: g.kernel.hugetlb_kb,
                mem_online_kb: g.kernel.mem_online_kb,
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
                kernel: KernelMemory {
                    slab_unreclaimable_kb: g.slab_unreclaimable_kb,
                    kernel_stack_kb: g.kernel_stack_kb,
                    page_tables_kb: g.page_tables_kb,
                    vmalloc_used_kb: g.vmalloc_used_kb,
                    percpu_kb: g.percpu_kb,
                    cma_total_kb: g.cma_total_kb,
                    hugetlb_kb: g.hugetlb_kb,
                    mem_online_kb: g.mem_online_kb,
                },
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

/// The least memory the host's own system keeps when the VM is sized to the host, in MiB.
pub const HOST_RESERVE_MIN_MB: u64 = 2048;

/// The least memory a VM is given, in MiB: Android does not boot in much less.
pub const VM_MEMORY_MIN_MB: u64 = 2048;

/// The memory the VM may grow to on a host with `host_total_mb` of RAM, in MiB, when the user
/// set no limit: all of it but what the host's own system keeps, a tenth of it and at least
/// [`HOST_RESERVE_MIN_MB`]. The VM holds only what the guest uses (deferred commit, free page
/// reporting), so a large maximum costs nothing while the guest does not use it.
pub fn automatic_vm_memory_mb(host_total_mb: u64) -> u64 {
    let reserve = HOST_RESERVE_MIN_MB.max(host_total_mb / 10);
    host_total_mb.saturating_sub(reserve).max(VM_MEMORY_MIN_MB)
}

/// The VM's memory, in MiB: the user's limit when they set one (`limit_mb` above 0), never
/// more than the host has, else [`automatic_vm_memory_mb`].
pub fn vm_memory_mb(host_total_mb: u64, limit_mb: u32) -> u64 {
    if limit_mb > 0 {
        u64::from(limit_mb)
            .min(host_total_mb.max(VM_MEMORY_MIN_MB))
            .max(VM_MEMORY_MIN_MB)
    } else {
        automatic_vm_memory_mb(host_total_mb)
    }
}

/// How far the VM may grow on a host with `host_total_mb` of RAM, in MiB, for a host whose
/// default is half its memory: the user's limit when they set one (`limit_mb` above 0), never
/// more than the host has, else half the host's memory, with no fixed ceiling. Either way the
/// VM starts small and holds only what [`MemoryGrowth`] gives it, so a large maximum costs
/// the host nothing until the guest needs it.
pub fn half_host_vm_memory_mb(host_total_mb: u64, limit_mb: u32) -> u64 {
    if limit_mb > 0 {
        vm_memory_mb(host_total_mb, limit_mb)
    } else {
        (host_total_mb / 2).max(VM_MEMORY_MIN_MB)
    }
}

/// How much the guest has and how hard it works to find memory, as its control daemon reports
/// it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuestPressure {
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    /// The share of the last 10 seconds some task waited for memory, in percent (PSI `some
    /// avg10`).
    pub stall_percent: f64,
}

impl GuestPressure {
    pub fn from_control(p: &control::GuestMemoryPressure) -> GuestPressure {
        GuestPressure {
            mem_total_kb: p.mem_total_kb,
            mem_available_kb: p.mem_available_kb,
            stall_percent: f64::from(p.stall_centipercent) / 100.0,
        }
    }

    fn used_mb(&self) -> u64 {
        self.mem_total_kb.saturating_sub(self.mem_available_kb) / 1024
    }
}

/// What [`MemoryGrowth`] asks the platform to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrowthAction {
    /// Set the VM's memory to this many MiB: more is hot-added, less is ballooned out.
    Resize(u64),
    /// The memory added last did not reach the guest: stop growing, for the reason given.
    GiveUp(String),
}

/// Grows the VM's memory when the guest runs short and gives it back when the guest no longer
/// needs it, between a small base and a large maximum.
///
/// A VM made at its maximum size from the start costs the host the kernel's page records for
/// all of it, used or not, about 1.6% of the maximum. Starting at the base and adding memory
/// while running costs that only for memory actually added, and the guest still has no fixed
/// ceiling below the maximum.
///
/// - Grow when the guest's available memory falls under a fifth of its total (at least
///   512 MiB), or when its tasks stall on memory for 10% of the time: by half the current size,
///   at least 1 GiB, up to the maximum, at most every 5 seconds.
/// - Shrink on the first report that finds the guest not short, once 5 seconds have passed
///   since the last change: to what it uses plus half again (at least 1 GiB of headroom),
///   never below the base. Memory the guest no longer needs goes back at once; the headroom,
///   not a wait, keeps the size from swinging.
/// - If memory added does not show in the guest's total within 30 seconds, give up growing:
///   the platform or the guest cannot hot-add.
#[derive(Debug, Clone)]
pub struct MemoryGrowth {
    base_mb: u64,
    max_mb: u64,
    current_mb: u64,
    last_change: Option<Instant>,
    /// The guest total before the last growth, and when it was asked for.
    awaiting: Option<(u64, Instant)>,
    stopped: bool,
}

/// Memory is added and removed in steps of this, in MiB, the guest's memory block size.
const GROWTH_GRANULE_MB: u64 = 128;
const GROW_COOLDOWN: Duration = Duration::from_secs(5);
const SHRINK_COOLDOWN: Duration = Duration::from_secs(5);
const HOT_ADD_DEADLINE: Duration = Duration::from_secs(30);

fn round_up_mb(mb: u64) -> u64 {
    mb.div_ceil(GROWTH_GRANULE_MB) * GROWTH_GRANULE_MB
}

impl MemoryGrowth {
    /// A VM that starts at `base_mb` and may grow to `max_mb`.
    pub fn new(base_mb: u64, max_mb: u64) -> MemoryGrowth {
        MemoryGrowth {
            base_mb,
            max_mb: max_mb.max(base_mb),
            current_mb: base_mb,
            last_change: None,
            awaiting: None,
            stopped: false,
        }
    }

    /// The size the VM was last set to, in MiB.
    pub fn current_mb(&self) -> u64 {
        self.current_mb
    }

    /// Whether growing was given up.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Takes in a pressure report and says what to do now, if anything.
    pub fn on_pressure(&mut self, p: &GuestPressure, now: Instant) -> Option<GrowthAction> {
        if self.stopped {
            return None;
        }
        let total_mb = p.mem_total_kb / 1024;
        if let Some((before_mb, asked)) = self.awaiting {
            if total_mb > before_mb {
                self.awaiting = None;
            } else if now.saturating_duration_since(asked) >= HOT_ADD_DEADLINE {
                self.stopped = true;
                self.current_mb = self.current_mb.min(before_mb.max(self.base_mb));
                return Some(GrowthAction::GiveUp(format!(
                    "the guest's memory stayed at {total_mb} MiB {} s after more was added",
                    HOT_ADD_DEADLINE.as_secs()
                )));
            } else {
                return None;
            }
        }
        let available_mb = p.mem_available_kb / 1024;
        let short = available_mb < (total_mb / 5).max(512) || p.stall_percent >= 10.0;
        let since_change = |cooldown| {
            self.last_change
                .is_none_or(|at| now.saturating_duration_since(at) >= cooldown)
        };
        if short {
            if self.current_mb >= self.max_mb || !since_change(GROW_COOLDOWN) {
                return None;
            }
            let step = (self.current_mb / 2).max(1024);
            let target = round_up_mb(self.current_mb + step).min(self.max_mb);
            self.awaiting = Some((total_mb, now));
            return Some(self.resize(target, now));
        }
        if self.current_mb <= self.base_mb
            || p.stall_percent >= 1.0
            || !since_change(SHRINK_COOLDOWN)
        {
            return None;
        }
        let used_mb = p.used_mb();
        let want = round_up_mb(used_mb + (used_mb / 2).max(1024)).max(self.base_mb);
        // Not worth a balloon for less than half a gibibyte.
        if want + 512 > self.current_mb {
            return None;
        }
        Some(self.resize(want, now))
    }

    fn resize(&mut self, mb: u64, now: Instant) -> GrowthAction {
        self.current_mb = mb;
        self.last_change = Some(now);
        GrowthAction::Resize(mb)
    }

    /// The platform refused the last resize: stay at the size before it, and stop growing.
    pub fn refused(&mut self, previous_mb: u64) {
        self.current_mb = previous_mb;
        self.awaiting = None;
        self.stopped = true;
    }
}

/// What a host process holds, in KiB, as far as the platform can tell; `None` where it
/// cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeldMemory {
    /// Resident in RAM.
    pub working_set_kb: Option<u64>,
    /// Committed to the process alone, resident or not.
    pub private_kb: Option<u64>,
    /// The GPU's own memory the process uses.
    pub gpu_local_kb: Option<u64>,
    /// System memory the GPU maps for the process.
    pub gpu_non_local_kb: Option<u64>,
}

/// The log line a give-back writes: what the process held before it and holds after, in
/// MiB, with `?` for what could not be read.
pub fn held_line(before: &HeldMemory, after: &HeldMemory) -> String {
    let show = |kb: Option<u64>| kb.map_or("?".to_string(), |kb| mib(kb).to_string());
    let part = |b: Option<u64>, a: Option<u64>| format!("{} -> {} MiB", show(b), show(a));
    format!(
        "working set {}, private {}, GPU memory {}, GPU-mapped system memory {}",
        part(before.working_set_kb, after.working_set_kb),
        part(before.private_kb, after.private_kb),
        part(before.gpu_local_kb, after.gpu_local_kb),
        part(before.gpu_non_local_kb, after.gpu_non_local_kb),
    )
}

/// What the graphics renderer holds, as it counts it: its live GPU allocations by kind, in
/// KiB, and how many guest processes are connected to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RendererMemory {
    /// Memory only the GPU sees: the guest's images and buffers, and the color buffers
    /// behind every gralloc buffer.
    pub device_local_kb: u64,
    pub device_local_count: u32,
    /// Memory the host's CPU maps: what the guest's host-visible memory is copied into.
    pub host_visible_kb: u64,
    pub host_visible_count: u32,
    /// Memory imported from another graphics API: the task windows' textures.
    pub imported_kb: u64,
    pub imported_count: u32,
    pub largest_kb: u64,
    /// Guest processes with an open connection to the renderer. Each keeps its own
    /// allocations until it exits.
    pub guest_processes: i32,
}

/// Where a host process's committed (private) memory goes, by owner, in KiB, as far as the
/// platform can tell; `None` where it cannot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommittedMemory {
    /// All committed private memory, from the platform's counters.
    pub private_kb: Option<u64>,
    /// Committed private pages found by walking the address space.
    pub walked_private_kb: u64,
    /// Of those: the process's default heap, which the process's own allocations use.
    pub process_heap_kb: u64,
    /// Of those: every other heap (drivers' and libraries' own), and how many there are.
    pub other_heaps_kb: u64,
    pub other_heaps: u32,
    /// Of those: write-combined or uncached pages, which only GPU drivers map.
    pub gpu_mapped_kb: u64,
    /// Of those: the rest, allocated straight from the system (drivers, the renderer, thread
    /// stacks).
    pub direct_kb: u64,
    /// The largest private allocations of any owner (heap segments included), largest first:
    /// a renderer's or a driver's pool shows up here by its size.
    pub largest_private_kb: Vec<u64>,
    /// What the process's own allocator has handed out and not freed.
    pub own_live_kb: Option<u64>,
    /// The renderer's own count, when it can give one.
    pub renderer: Option<RendererMemory>,
    pub gpu_local_kb: Option<u64>,
    pub gpu_non_local_kb: Option<u64>,
}

/// The log line that breaks a host process's committed memory down by owner, in MiB.
pub fn committed_line(c: &CommittedMemory) -> String {
    let show = |kb: Option<u64>| kb.map_or("?".to_string(), |kb| mib(kb).to_string());
    let largest: Vec<String> = c
        .largest_private_kb
        .iter()
        .map(|kb| format!("{:.1}", *kb as f64 / 1024.0))
        .collect();
    let mut line = format!(
        "committed memory by owner: private {} MiB ({} found); default heap {} (the window \
         manager's own allocations {}); {} other heaps {}; GPU-mapped {}; allocated directly {} \
         (largest allocations {} MiB)",
        show(c.private_kb),
        mib(c.walked_private_kb),
        mib(c.process_heap_kb),
        show(c.own_live_kb),
        c.other_heaps,
        mib(c.other_heaps_kb),
        mib(c.gpu_mapped_kb),
        mib(c.direct_kb),
        if largest.is_empty() {
            "none".to_string()
        } else {
            largest.join(", ")
        },
    );
    match &c.renderer {
        Some(r) => line.push_str(&format!(
            "; renderer: GPU-only {} MiB in {}, CPU-mapped {} MiB in {}, imported {} MiB in {}, \
             largest {} MiB, {} guest processes connected",
            mib(r.device_local_kb),
            r.device_local_count,
            mib(r.host_visible_kb),
            r.host_visible_count,
            mib(r.imported_kb),
            r.imported_count,
            mib(r.largest_kb),
            r.guest_processes,
        )),
        None => line.push_str("; renderer: no count"),
    }
    line.push_str(&format!(
        "; GPU memory {} MiB, GPU-mapped system memory {} MiB",
        show(c.gpu_local_kb),
        show(c.gpu_non_local_kb)
    ));
    line
}

/// How long the window manager waits after the last task window closed before it gives back
/// the memory it is not using: not at all. Memory no window uses goes back on the trim's next
/// check, within a second; pages touched again come back from the standby list on their own.
pub const IDLE_TRIM_DELAY: Duration = Duration::ZERO;

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
    /// The current quiet stretch (windows shown, no new frame) has had its trim.
    quiet_trimmed: bool,
}

/// How long shown windows must go without a new frame before the window manager gives back the
/// GPU driver's spare memory. An app that shows a still screen draws nothing; one that is
/// animating never reaches it, so nothing it renders waits for memory the driver let go.
pub const QUIET_TRIM_DELAY: Duration = Duration::from_secs(5);

impl IdleTrim {
    /// Starts idle, as at start-up, when no window is open yet.
    pub fn new(delay: Duration, now: Instant) -> IdleTrim {
        IdleTrim {
            delay,
            open: BTreeSet::new(),
            all_hidden: false,
            idle_since: Some(now),
            trimmed: false,
            quiet_trimmed: false,
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

    /// Whether windows are shown but have shown no new frame for `quiet_for`, at least
    /// [`QUIET_TRIM_DELAY`]: the GPU driver's spare memory can go back then without touching
    /// what the next frame needs. True once per quiet stretch; a frame starts a new one.
    pub fn quiet_due(&mut self, quiet_for: Duration) -> bool {
        let shown = !self.open.is_empty() && !self.all_hidden;
        if !shown || quiet_for < QUIET_TRIM_DELAY {
            self.quiet_trimmed = false;
            return false;
        }
        let due = !self.quiet_trimmed;
        self.quiet_trimmed = true;
        due
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
