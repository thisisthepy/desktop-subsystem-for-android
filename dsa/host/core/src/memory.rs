//! How much memory a user's Android takes: inside the guest, as the guest's agent reports
//! it over the control channel, and on the host, the processes that run it (the VM's
//! memory process, the window manager and each app window), as the platform measures them.
//!
//! The guest's report says where its memory goes (page cache, app memory, compressed swap,
//! the largest processes); the host's numbers say what that costs the host. Together they
//! show whether memory the guest is not using went back to the host.
//!
//! A process's proportional set size (PSS) counts what it has swapped out to zram at the size
//! the pages had before compression. zram holds them in about a third of that, so a PSS taken
//! as it is shows an idle guest two or three times bigger than it costs. Every process figure
//! here is what it costs: what is resident, plus its share of zram at the compression ratio
//! zram measures ([`MemoryShare`]). Google Play services, which the user chose to install, is
//! counted on a line of its own, so its cost is not taken for DSA's.

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

/// One guest process's share of the guest's memory, in KiB, as the guest counts it:
/// `swap_pss_kb` is the part of `pss_kb` swapped out to zram, at its size before compression.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessMemory {
    pub pid: i32,
    pub name: String,
    pub pss_kb: u64,
    pub swap_pss_kb: u64,
    /// It belongs to Google Play services (the Google apps the user installed).
    pub google_play: bool,
}

impl ProcessMemory {
    /// Its swapped-out part, never more than its PSS, which it is part of.
    pub fn swapped_out_kb(&self) -> u64 {
        self.swap_pss_kb.min(self.pss_kb)
    }
}

/// Every process of Google Play services together, as the guest counts them, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GooglePlayMemory {
    pub processes: u32,
    pub pss_kb: u64,
    pub swap_pss_kb: u64,
}

/// What some of the guest's processes cost the guest's memory, in KiB: their pages resident
/// in RAM, and their share of what zram holds compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryShare {
    pub resident_kb: u64,
    /// `None` when they have pages in zram but the guest could not say how well zram
    /// compresses (its statistics were unreadable), so their compressed size is not known.
    pub compressed_kb: Option<u64>,
    /// What they have in zram at the size before compression, as their PSS counts it.
    pub swapped_out_kb: u64,
}

impl MemoryShare {
    /// What they cost: resident plus compressed, the compressed part left out when unknown.
    pub fn total_kb(&self) -> u64 {
        self.resident_kb + self.compressed_kb.unwrap_or(0)
    }
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
    /// The part of `total_pss_kb` swapped out to zram, at its size before compression.
    pub swap_pss_kb: u64,
    /// Largest first.
    pub top_processes: Vec<ProcessMemory>,
    /// What the kernel itself holds.
    pub kernel: KernelMemory,
    /// Google Play services' processes; `None` when none runs (or the Google apps are not
    /// installed).
    pub google_play: Option<GooglePlayMemory>,
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
    /// The memory the kernel was given: its online memory blocks, or the VM's size when the
    /// host set it smaller ([`GuestMemory::given`]).
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
    /// The graphics renderer process (`dsa-gfx`), which holds the guest's GPU resources.
    Renderer,
    AppWindow,
}

impl HostRole {
    pub fn name(self) -> &'static str {
        match self {
            HostRole::Vm => "vm",
            HostRole::WindowManager => "window manager",
            HostRole::Renderer => "graphics renderer",
            HostRole::AppWindow => "app window",
        }
    }

    fn from_name(name: &str) -> Option<HostRole> {
        [
            HostRole::Vm,
            HostRole::WindowManager,
            HostRole::Renderer,
            HostRole::AppWindow,
        ]
        .into_iter()
        .find(|role| role.name() == name)
    }

    /// Where the role's processes go in a list: the VM, the window manager, the renderer,
    /// then the app windows.
    pub fn rank(self) -> u8 {
        match self {
            HostRole::Vm => 0,
            HostRole::WindowManager => 1,
            HostRole::Renderer => 2,
            HostRole::AppWindow => 3,
        }
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
    /// In the order: the VM, the window manager, the graphics renderer, the app windows.
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

    /// How zram compresses, as `(memory it uses, size of what it stores)` in KiB: from zram's
    /// own statistics (mm_stat), or, without the size it stores, the swap in use from
    /// /proc/meminfo. `None` when either is unknown.
    pub fn zram_ratio(&self) -> Option<(u64, u64)> {
        let stored = if self.zram_original_kb > 0 {
            self.zram_original_kb
        } else {
            self.swap_used_kb()
        };
        (stored > 0 && self.zram_used_kb > 0).then_some((self.zram_used_kb, stored))
    }

    /// What `swapped_out_kb` of pages swapped out (at their size before compression) take in
    /// zram, at the ratio zram measures; `None` when that ratio is unknown. zram never stores
    /// a page in more than its own size, so the share is never more than `swapped_out_kb`.
    pub fn compressed_kb(&self, swapped_out_kb: u64) -> Option<u64> {
        if swapped_out_kb == 0 {
            return Some(0);
        }
        let (used, stored) = self.zram_ratio()?;
        let share = (u128::from(swapped_out_kb) * u128::from(used) + u128::from(stored) / 2)
            / u128::from(stored);
        Some(u64::try_from(share).unwrap_or(u64::MAX).min(swapped_out_kb))
    }

    /// What processes with this PSS, `swap_pss_kb` of it swapped out, cost.
    pub fn share(&self, pss_kb: u64, swap_pss_kb: u64) -> MemoryShare {
        let swapped_out_kb = swap_pss_kb.min(pss_kb);
        MemoryShare {
            resident_kb: pss_kb - swapped_out_kb,
            compressed_kb: self.compressed_kb(swapped_out_kb),
            swapped_out_kb,
        }
    }

    /// What the processes the report counts (the apps and the system server) cost together.
    /// A report from an agent that does not send the total swapped out counts the largest
    /// processes' instead, which is less than all.
    ///
    /// The agent of DSA 0.2.2 and before sent each process's count of every page it had swapped out,
    /// shared pages whole, instead of its swapped-out PSS. That count was larger than the PSS
    /// for most processes, so a total of it exceeded the total PSS, and every total read
    /// "0 resident". Such a total is not used: the largest processes' parts, each no more
    /// than its PSS, stand for it.
    pub fn apps(&self) -> MemoryShare {
        let swapped_out = if self.swap_pss_kb > 0 && self.swap_pss_kb <= self.total_pss_kb {
            self.swap_pss_kb
        } else {
            self.top_processes
                .iter()
                .map(ProcessMemory::swapped_out_kb)
                .sum()
        };
        self.share(self.total_pss_kb, swapped_out)
    }

    /// What Google Play services costs, `None` when none of its processes runs. A swapped-out
    /// total larger than its PSS is replaced as in [`GuestMemory::apps`], by its processes
    /// among the largest.
    pub fn google_play_share(&self) -> Option<MemoryShare> {
        self.google_play.map(|play| {
            let swapped_out = if play.swap_pss_kb <= play.pss_kb {
                play.swap_pss_kb
            } else {
                self.top_processes
                    .iter()
                    .filter(|p| p.google_play)
                    .map(ProcessMemory::swapped_out_kb)
                    .sum()
            };
            self.share(play.pss_kb, swapped_out)
        })
    }

    /// What one of the largest processes costs.
    pub fn process_share(&self, process: &ProcessMemory) -> MemoryShare {
        self.share(process.pss_kb, process.swap_pss_kb)
    }

    pub fn from_control(memory: control::GuestMemory) -> GuestMemory {
        let google_play = (memory.google_play_processes > 0).then_some(GooglePlayMemory {
            processes: memory.google_play_processes,
            pss_kb: memory.google_play_pss_kb,
            swap_pss_kb: memory.google_play_swap_pss_kb,
        });
        GuestMemory {
            google_play,
            swap_pss_kb: memory.swap_pss_kb,
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
                    google_play: p.google_play,
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

    /// This report with the memory the guest was given taken as the VM's size, `vm_mb` MiB,
    /// when the host knows it and it is less than the guest's online memory blocks. Memory
    /// hot-added while the guest ran stays online after the host has ballooned it back, so
    /// the blocks alone count it as given, and [`GuestMemory::reserved_at_boot_kb`] would take
    /// all of it for the kernel's own.
    pub fn given(&self, vm_mb: Option<u64>) -> GuestMemory {
        let mut memory = self.clone();
        if let Some(vm_kb) = vm_mb.map(|mb| mb * 1024) {
            if memory.kernel.mem_online_kb > vm_kb {
                memory.kernel.mem_online_kb = vm_kb;
            }
        }
        memory
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
        let (renderer, _) = self.role_total_kb(HostRole::Renderer);
        let (apps, app_count) = self.role_total_kb(HostRole::AppWindow);
        let mut line = format!(
            "memory: host {} MiB (vm {}, window manager {}, graphics renderer {}, {app_count} \
             app windows {})",
            mib(self.host_total_kb()),
            mib(vm),
            mib(wm),
            mib(renderer),
            mib(apps)
        );
        match &self.guest {
            None => line.push_str("; guest has not reported"),
            Some(report) => {
                let g = &report.memory;
                let apps = g.apps();
                let play = g
                    .google_play_share()
                    .map_or("none".to_string(), |play| share_mib(&play));
                line.push_str(&format!(
                    "; guest {}/{} MiB used, cache {}, apps {}, swap {} (zram {}), processes {}, \
                     Google Play services {}, {} s ago",
                    mib(g.used_kb()),
                    mib(g.mem_total_kb),
                    mib(g.cached_kb + g.buffers_kb),
                    mib(g.anon_pages_kb),
                    mib(g.swap_used_kb()),
                    mib(g.zram_used_kb),
                    share_mib(&apps),
                    play,
                    report.age.as_secs()
                ));
            }
        }
        line
    }
}

/// KiB to whole MiB, rounded to nearest.
pub(crate) fn mib(kb: u64) -> u64 {
    (kb + 512) / 1024
}

/// A share in MiB as the log and `dsa memory` write it: what it costs, then its resident and
/// compressed parts, `?` for a compressed size zram could not measure.
fn share_mib(share: &MemoryShare) -> String {
    format!(
        "{} MiB ({} resident, {} compressed in zram)",
        mib(share.total_kb()),
        mib(share.resident_kb),
        share
            .compressed_kb
            .map_or("?".to_string(), |kb| mib(kb).to_string())
    )
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
                "  {:<17} pid {:<6} {:>6} MiB resident, {:>6} MiB private",
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
        let apps = g.apps();
        writeln!(
            f,
            "  processes: {} in all ({} MiB swapped out before compression)",
            share_mib(&apps),
            mib(apps.swapped_out_kb)
        )?;
        match g.google_play_share() {
            Some(play) => writeln!(
                f,
                "  Google Play services: {} in {} processes ({} MiB swapped out before \
                 compression)",
                share_mib(&play),
                g.google_play.map_or(0, |p| p.processes),
                mib(play.swapped_out_kb)
            )?,
            None => writeln!(f, "  Google Play services: not running")?,
        }
        for p in &g.top_processes {
            let share = g.process_share(p);
            writeln!(
                f,
                "    {:>6} MiB  {:<6} {}{} ({} resident, {} compressed)",
                mib(share.total_kb()),
                p.pid,
                p.name,
                if p.google_play {
                    " [Google Play services]"
                } else {
                    ""
                },
                mib(share.resident_kb),
                share
                    .compressed_kb
                    .map_or("?".to_string(), |kb| mib(kb).to_string())
            )?;
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
                        google_play: p.google_play,
                    })
                    .collect(),
                swap_pss_kb: g.swap_pss_kb,
                google_play_processes: g.google_play.map_or(0, |p| p.processes),
                google_play_pss_kb: g.google_play.map_or(0, |p| p.pss_kb),
                google_play_swap_pss_kb: g.google_play.map_or(0, |p| p.swap_pss_kb),
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
                        google_play: p.google_play,
                    })
                    .collect(),
                swap_pss_kb: g.swap_pss_kb,
                google_play: (g.google_play_processes > 0).then_some(GooglePlayMemory {
                    processes: g.google_play_processes,
                    pss_kb: g.google_play_pss_kb,
                    swap_pss_kb: g.google_play_swap_pss_kb,
                }),
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
///   512 MiB), or when its tasks stall on memory for 10% of the time: to what it uses plus half
///   again (at least 1 GiB of headroom), the same size a shrink would leave, and by at least
///   512 MiB, up to the maximum, at most every 5 seconds. Memory added once costs the guest its
///   page records (about 1.6% of it) for as long as it runs, even after it has been ballooned
///   back to the host, so the VM grows only as far as the guest needs: a step of half the
///   current size took a guest with 2560 MiB and 500 MiB available to 3840 MiB, where 3072 MiB
///   was enough.
/// - Shrink on the first report that finds the guest not short, once 5 seconds have passed
///   since the last change: to what it uses plus half again (at least 1 GiB of headroom),
///   never below the base. Memory the guest no longer needs goes back at once; the headroom,
///   not a wait, keeps the size from swinging.
/// - If memory added does not show in the guest's total within 30 seconds, give up growing:
///   the platform or the guest cannot hot-add.
/// - A resize the platform refuses or that times out ([`MemoryGrowth::refused`]) leaves the VM
///   at the size before it and is tried again, in steps half the size of the one that failed
///   (down to 128 MiB), after a wait that doubles with each failure in a row (5 seconds up to
///   5 minutes). A failure never stops later resizes: taking 1.5 GiB back in one request timed
///   out in Hyper-V while the guest still held the pages, and the VM then kept the memory for
///   the rest of the session. Each resize that goes through ([`MemoryGrowth::applied`]) ends
///   the wait and lets the steps double back toward their full size.
#[derive(Debug, Clone)]
pub struct MemoryGrowth {
    base_mb: u64,
    max_mb: u64,
    current_mb: u64,
    last_change: Option<Instant>,
    /// The guest total before the last growth, and when it was asked for.
    awaiting: Option<(u64, Instant)>,
    stopped: bool,
    /// The size before the last resize asked for, while the platform has not answered it.
    previous_mb: Option<u64>,
    /// The most one growth or one shrink may change, after a resize of that kind failed.
    grow_limit_mb: Option<u64>,
    shrink_limit_mb: Option<u64>,
    /// Where a shrink that goes back in steps is headed.
    shrink_goal_mb: Option<u64>,
    /// Resizes that failed in a row, and when the next may be tried.
    failures: u32,
    retry_at: Option<Instant>,
}

/// Memory is added and removed in steps of this, in MiB, the guest's memory block size.
const GROWTH_GRANULE_MB: u64 = 128;
/// The least one growth adds, in MiB: a guest short of memory gets at least this much more.
const GROW_STEP_MIN_MB: u64 = 512;
const GROW_COOLDOWN: Duration = Duration::from_secs(5);
const SHRINK_COOLDOWN: Duration = Duration::from_secs(5);
const HOT_ADD_DEADLINE: Duration = Duration::from_secs(30);
/// The wait before the first retry of a failed resize; it doubles with each failure in a row.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_MOST: Duration = Duration::from_secs(5 * 60);

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
            previous_mb: None,
            grow_limit_mb: None,
            shrink_limit_mb: None,
            shrink_goal_mb: None,
            failures: 0,
            retry_at: None,
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
        // One resize at a time: the platform has not answered the last one yet.
        if self.in_flight() {
            return None;
        }
        if self.retry_at.is_some_and(|at| now < at) {
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
            let used_mb = p.used_mb();
            let wanted = round_up_mb(used_mb + (used_mb / 2).max(1024))
                .max(self.current_mb + GROW_STEP_MIN_MB);
            let mut step = wanted - self.current_mb;
            if let Some(limit) = self.grow_limit_mb {
                step = step.min(limit);
            }
            let target = round_up_mb(self.current_mb + step).min(self.max_mb);
            self.awaiting = Some((total_mb, now));
            self.shrink_goal_mb = None;
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
        // Not worth a balloon for less than half a gibibyte, unless it is the rest of a shrink
        // that goes back in steps.
        let resuming = self.shrink_goal_mb.is_some();
        if want >= self.current_mb || (!resuming && want + 512 > self.current_mb) {
            self.shrink_goal_mb = None;
            return None;
        }
        // After a shrink failed, the memory goes back in smaller steps.
        let target = match self.shrink_limit_mb {
            Some(limit) => want.max(self.current_mb.saturating_sub(limit)),
            None => want,
        };
        self.shrink_goal_mb = (target > want).then_some(want);
        Some(self.resize(target, now))
    }

    fn resize(&mut self, mb: u64, now: Instant) -> GrowthAction {
        self.previous_mb = Some(self.current_mb);
        self.current_mb = mb;
        self.last_change = Some(now);
        GrowthAction::Resize(mb)
    }

    /// The platform set the VM to the size the last [`GrowthAction::Resize`] asked for. Later
    /// resizes need not wait, and a step that was cut down after a failure doubles again.
    pub fn applied(&mut self) {
        self.previous_mb = None;
        self.failures = 0;
        self.retry_at = None;
        let double = |limit: Option<u64>| limit.and_then(|mb| mb.checked_mul(2));
        self.grow_limit_mb = double(self.grow_limit_mb);
        self.shrink_limit_mb = double(self.shrink_limit_mb);
    }

    /// The platform refused the last resize, or it did not finish in time, at `now`. The VM
    /// counts as still at the size before it. The next resize of that kind is at most half as
    /// large, down to 128 MiB, and nothing is asked for until the retry wait has passed.
    pub fn refused(&mut self, now: Instant) {
        let Some(previous_mb) = self.previous_mb.take() else {
            return;
        };
        let failed_mb = self.current_mb.abs_diff(previous_mb);
        let half = round_up_mb((failed_mb / 2).max(GROWTH_GRANULE_MB));
        if self.current_mb > previous_mb {
            self.grow_limit_mb = Some(half);
            self.awaiting = None;
        } else {
            self.shrink_limit_mb = Some(half);
            // The rest still goes back, in the smaller steps.
            self.shrink_goal_mb = Some(self.current_mb);
        }
        self.current_mb = previous_mb;
        self.failures = self.failures.saturating_add(1);
        let wait = RETRY_FIRST
            .saturating_mul(1 << self.failures.saturating_sub(1).min(16))
            .min(RETRY_MOST);
        self.retry_at = Some(now + wait);
    }

    /// Whether the last [`GrowthAction::Resize`] has not been answered yet
    /// ([`MemoryGrowth::applied`] or [`MemoryGrowth::refused`]). No other resize is asked for
    /// meanwhile.
    pub fn in_flight(&self) -> bool {
        self.previous_mb.is_some()
    }

    /// How long until a resize may be tried again after a failure, if one is being waited out.
    pub fn retry_in(&self, now: Instant) -> Option<Duration> {
        self.retry_at
            .map(|at| at.saturating_duration_since(now))
            .filter(|wait| !wait.is_zero())
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
    /// Of those: the rest, allocated straight from the system (drivers, thread stacks).
    pub direct_kb: u64,
    /// The largest private allocations of any owner (heap segments included), largest first:
    /// a driver's pool shows up here by its size.
    pub largest_private_kb: Vec<u64>,
    /// What the process's own allocator has handed out and not freed.
    pub own_live_kb: Option<u64>,
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

/// How long after a give-back the window manager gives memory back again while still no task
/// window is shown. While Android dozes the window manager keeps working (task and memory
/// events, the guest's notifications), and what that touches stays in its working set until
/// Windows runs short; one give-back per idle stretch left it there for hours. Five minutes
/// keeps the cost (a working set emptied and a few log lines) small.
pub const IDLE_TRIM_REPEAT: Duration = Duration::from_secs(5 * 60);

/// Decides when a host process that serves task windows (the window manager) should give
/// back the memory it is not using: each time no task window has been open for a while, and
/// again every [`IDLE_TRIM_REPEAT`] while none is.
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
    /// When the current idle stretch last gave memory back.
    last_trim: Option<Instant>,
    repeat: Duration,
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
            last_trim: None,
            repeat: IDLE_TRIM_REPEAT,
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
            self.last_trim = None;
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

    /// Whether to give memory back now: once the idle stretch is `delay` old, and again every
    /// [`IDLE_TRIM_REPEAT`] after that until a window is shown.
    pub fn due(&mut self, now: Instant) -> bool {
        let idle = self
            .idle_since
            .is_some_and(|since| now.saturating_duration_since(since) >= self.delay);
        let again = self
            .last_trim
            .is_some_and(|at| now.saturating_duration_since(at) >= self.repeat);
        let due = idle && (!self.trimmed || again);
        if due {
            self.trimmed = true;
            self.last_trim = Some(now);
        }
        due
    }
}
