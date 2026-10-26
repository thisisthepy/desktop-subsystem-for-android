//! How much memory a user's Android takes: on the host, the processes that run it, and
//! inside the guest, as Android last reported it.
//!
//! The host side is measured by the platform ([`HostMemory`]). The guest side comes from the
//! guest over the control channel; until an Android reports it, [`Memory::guest`] is `None`.

/// One Android process's share of the guest's memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestProcessMemory {
    pub pid: i32,
    pub name: String,
    /// Proportional set size in KiB.
    pub pss_kb: u64,
    /// The part of it swapped out to compressed memory, proportionally, in KiB.
    pub swap_pss_kb: u64,
}

/// How Android uses the guest's memory. Sizes in KiB; 0 when Android could not read one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// The apps and the system server together.
    pub total_pss_kb: u64,
    /// Largest first, at most 10.
    pub top_processes: Vec<GuestProcessMemory>,
    /// How long ago Android sent the report, in seconds.
    pub age_seconds: u64,
}

/// What a host process does for the user's Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostRole {
    /// The process that holds the VM's memory (`vmmem` on Windows).
    Vm,
    /// The window manager.
    WindowManager,
    /// The window of one app.
    AppWindow,
}

impl HostRole {
    /// The name the local API carries.
    pub fn as_str(&self) -> &'static str {
        match self {
            HostRole::Vm => "vm",
            HostRole::WindowManager => "window manager",
            HostRole::AppWindow => "app window",
        }
    }

    pub fn parse(text: &str) -> Option<HostRole> {
        match text {
            "vm" => Some(HostRole::Vm),
            "window manager" => Some(HostRole::WindowManager),
            "app window" => Some(HostRole::AppWindow),
            _ => None,
        }
    }

    fn order(&self) -> u8 {
        match self {
            HostRole::Vm => 0,
            HostRole::WindowManager => 1,
            HostRole::AppWindow => 2,
        }
    }
}

/// One host process that runs a user's Android.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProcessMemory {
    pub role: HostRole,
    pub pid: u32,
    /// Resident in RAM now, in KiB.
    pub working_set_kb: u64,
    /// Committed to it alone, in KiB.
    pub private_kb: u64,
}

/// The memory of a user's Android.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    /// `None` until Android has sent its first report since it started.
    pub guest: Option<GuestMemory>,
    /// The VM, the window manager, then the app windows.
    pub host: Vec<HostProcessMemory>,
}

impl Memory {
    /// The working sets of the host processes added up, in KiB.
    pub fn host_total_kb(&self) -> u64 {
        self.host.iter().map(|p| p.working_set_kb).sum()
    }
}

/// Puts host processes in the order the local API promises: the VM, the window manager, the
/// app windows, each by process ID.
pub fn sort_host(processes: &mut [HostProcessMemory]) {
    processes.sort_by_key(|p| (p.role.order(), p.pid));
}

/// Measures the host processes that run a user's Android.
pub trait HostMemory: Send + Sync {
    /// The processes of `user` (an identity as the transport reports it) that run their
    /// Android, whose VM has the runtime ID `vm_id` when it is running.
    fn processes(&self, user: &str, vm_id: Option<&str>) -> Vec<HostProcessMemory>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr17_host_processes_are_listed_vm_first_then_window_manager_then_apps() {
        let p = |role, pid| HostProcessMemory {
            role,
            pid,
            working_set_kb: 1,
            private_kb: 1,
        };
        let mut list = vec![
            p(HostRole::AppWindow, 9),
            p(HostRole::WindowManager, 50),
            p(HostRole::AppWindow, 3),
            p(HostRole::Vm, 70),
        ];
        sort_host(&mut list);
        let order: Vec<(HostRole, u32)> = list.iter().map(|p| (p.role, p.pid)).collect();
        assert_eq!(
            order,
            vec![
                (HostRole::Vm, 70),
                (HostRole::WindowManager, 50),
                (HostRole::AppWindow, 3),
                (HostRole::AppWindow, 9)
            ]
        );
        let memory = Memory { guest: None, host: list };
        assert_eq!(memory.host_total_kb(), 4);
    }

    #[test]
    fn fr17_host_roles_read_back() {
        for role in [HostRole::Vm, HostRole::WindowManager, HostRole::AppWindow] {
            assert_eq!(HostRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(HostRole::parse("kernel"), None);
    }
}
