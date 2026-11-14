//! The host's share of the user's Android on macOS.
//!
//! The Virtualization framework runs each VM in an XPC service process,
//! `com.apple.Virtualization.VirtualMachine`, which launchd starts and Activity Monitor
//! shows. Its parent is launchd, not the agent, so the agent tells its VM's process apart by
//! which one appeared when it started the VM ([`vm_processes`] before and after). The user's
//! window manager (`dsa-wm`) and the graphics renderer it starts (`dsa-gfx`) are found by
//! name among the user's processes.

use std::sync::{Arc, Mutex};

use dsa_host_core::memory::{HostMemory, HostProcessMemory, HostRole};

/// The name of the framework's VM process.
pub const VM_PROCESS: &str = "com.apple.Virtualization.VirtualMachine";

/// Every process of this user, by process ID.
fn own_processes() -> Vec<i32> {
    // SAFETY: a null buffer asks for the count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0i32; count as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
    // SAFETY: the buffer holds `bytes` bytes.
    let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if filled <= 0 {
        return Vec::new();
    }
    pids.truncate(filled as usize);
    // SAFETY: no preconditions.
    let me = unsafe { libc::getuid() };
    pids.into_iter()
        .filter(|&pid| pid > 0 && process_uid(pid) == Some(me))
        .collect()
}

/// The process IDs of every VM process of this user.
pub fn vm_processes() -> Vec<i32> {
    own_processes()
        .into_iter()
        .filter(|&pid| process_name(pid).is_some_and(|n| n == VM_PROCESS))
        .collect()
}

/// What a process of the user's session named `name` does for their Android: the window
/// manager, or the graphics renderer it starts.
pub fn role_of(name: &str) -> Option<HostRole> {
    match name {
        "dsa-wm" => Some(HostRole::WindowManager),
        "dsa-gfx" => Some(HostRole::Renderer),
        _ => None,
    }
}

fn process_name(pid: i32) -> Option<String> {
    let mut name = [0u8; 256];
    // SAFETY: the buffer holds its length in bytes.
    let len = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
    (len > 0).then(|| String::from_utf8_lossy(&name[..len as usize]).into_owned())
}

fn process_uid(pid: i32) -> Option<u32> {
    // SAFETY: zero is a valid proc_bsdinfo; proc_pidinfo fills it.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the buffer holds one proc_bsdinfo.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (got == size).then_some(info.pbi_uid)
}

/// What a process holds: resident in RAM, and its own footprint (what Activity Monitor
/// shows as its memory), in KiB.
pub fn process_memory_kb(pid: i32) -> Option<(u64, u64)> {
    // SAFETY: zero is a valid rusage_info_v2; proc_pid_rusage fills it.
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: the flavour matches the structure passed.
    let result = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            (&mut info as *mut libc::rusage_info_v2).cast(),
        )
    };
    (result == 0).then_some((info.ri_resident_size / 1024, info.ri_phys_footprint / 1024))
}

/// The VM process of the running VM, which the launcher records.
#[derive(Clone, Default)]
pub struct VmProcess(pub Arc<Mutex<Option<i32>>>);

impl VmProcess {
    pub fn set(&self, pid: Option<i32>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = pid;
    }
    pub fn get(&self) -> Option<i32> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The host memory of this agent's user: their VM's process.
pub struct MacHostMemory {
    pub vm: VmProcess,
}

impl HostMemory for MacHostMemory {
    fn processes(&self, _user: &str, _vm_id: Option<&str>) -> Vec<HostProcessMemory> {
        let measure = |pid: i32, role: HostRole| {
            process_memory_kb(pid).map(|(resident, footprint)| HostProcessMemory {
                role,
                pid: pid as u32,
                working_set_kb: resident,
                private_kb: footprint,
                // The VM maps no disk directly here: Virtualization.framework has no
                // persistent memory device.
                mapped_kb: 0,
            })
        };
        let mut found: Vec<HostProcessMemory> = self
            .vm
            .get()
            .and_then(|pid| measure(pid, HostRole::Vm))
            .into_iter()
            .collect();
        // The agent runs as the user, so the user's own window manager and renderer are
        // among its processes.
        for pid in own_processes() {
            if let Some(role) = process_name(pid).as_deref().and_then(role_of) {
                found.extend(measure(pid, role));
            }
        }
        found.sort_by_key(|p| (p.role.rank(), p.pid));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr5_the_renderers_memory_shows_under_its_own_process() {
        assert_eq!(role_of("dsa-gfx"), Some(HostRole::Renderer));
        assert_eq!(role_of("dsa-wm"), Some(HostRole::WindowManager));
        assert_eq!(role_of("Finder"), None);
    }
}
