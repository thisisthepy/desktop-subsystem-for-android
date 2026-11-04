//! The guest's power HAL on Windows: how hard Windows runs the guest's virtual processors.
//!
//! On Windows with the root scheduler (the default since Windows 10), a VM's virtual
//! processors are threads of its `vmmem` process, scheduled like any other program's. So the
//! performance the guest's power hints ask for ([`Performance`]) becomes the `vmmem`
//! process's power throttling (EcoQoS) and priority class:
//!
//! - `Full`: never throttled, above normal priority, while Android launches an app, runs a
//!   game, or the user touches it;
//! - `Normal`: as Windows decides for any program;
//! - `Efficient`: throttled onto the efficient cores and clocks, below normal priority, while
//!   the guest's display is off, it is in Doze, or low power is on, and no task window is
//!   shown.
//!
//! Each press, release, scroll or key the host passes on boosts at once, before the guest's
//! own interaction boost could arrive, and each change is logged with its time and cause.
//!
//! [`settings`] is the rule and is tested on every platform; `follow` applies it.

use dsa_host_core::power_hints::Performance;

/// `PROCESS_POWER_THROTTLING_EXECUTION_SPEED`.
pub const EXECUTION_SPEED: u32 = 0x1;
/// `NORMAL_PRIORITY_CLASS`.
pub const NORMAL_PRIORITY: u32 = 0x20;
/// `ABOVE_NORMAL_PRIORITY_CLASS`.
pub const ABOVE_NORMAL_PRIORITY: u32 = 0x8000;
/// `BELOW_NORMAL_PRIORITY_CLASS`.
pub const BELOW_NORMAL_PRIORITY: u32 = 0x4000;

/// What one performance sets on the VM's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSettings {
    /// `PROCESS_POWER_THROTTLING_STATE.ControlMask`: the throttling the process decides for
    /// itself. 0 leaves it to Windows.
    pub throttling_control: u32,
    /// `PROCESS_POWER_THROTTLING_STATE.StateMask`: of those, the ones that are on.
    pub throttling_state: u32,
    /// The priority class.
    pub priority: u32,
}

/// The process settings for `performance`.
pub fn settings(performance: Performance) -> ProcessSettings {
    match performance {
        Performance::Full => ProcessSettings {
            throttling_control: EXECUTION_SPEED,
            throttling_state: 0,
            priority: ABOVE_NORMAL_PRIORITY,
        },
        Performance::Normal => ProcessSettings {
            throttling_control: 0,
            throttling_state: 0,
            priority: NORMAL_PRIORITY,
        },
        Performance::Efficient => ProcessSettings {
            throttling_control: EXECUTION_SPEED,
            throttling_state: EXECUTION_SPEED,
            priority: BELOW_NORMAL_PRIORITY,
        },
    }
}

/// Words for the log.
pub fn describe(performance: Performance) -> &'static str {
    match performance {
        Performance::Full => "at full speed",
        Performance::Normal => "as any program",
        Performance::Efficient => "frugally",
    }
}

#[cfg(windows)]
pub use system::follow;

#[cfg(windows)]
mod system {
    use std::collections::HashSet;
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::Weak;
    use std::time::Instant;

    use dsa_host_core::power_hints::{Performance, PowerHints};
    use dsa_host_core::resize_trace::{utc_stamp, wall_now};
    use dsa_host_core::session::{GuestPower, Session};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_STATE,
        PROCESS_SET_INFORMATION,
    };

    use super::{describe, settings, ProcessSettings};
    use crate::pipe::OwnedHandle;

    /// The VM's processes, found once for each VM.
    struct VmProcesses {
        vm_id: String,
        pids: Vec<u32>,
    }

    impl VmProcesses {
        fn find(vm_id: &str) -> Result<VmProcesses, String> {
            let pids = crate::host_memory::vm_processes(vm_id)
                .map_err(|e| format!("cannot list the host's processes: {e}"))?;
            if pids.is_empty() {
                return Err(format!("no process of the VM {vm_id} was found"));
            }
            Ok(VmProcesses {
                vm_id: vm_id.to_string(),
                pids,
            })
        }

        /// Applies `performance` to each process. An error says why none took it.
        fn apply(&self, performance: Performance) -> Result<(), String> {
            let wanted = settings(performance);
            let mut last_error = String::new();
            let mut set = false;
            for &pid in &self.pids {
                match apply_to(pid, wanted) {
                    Ok(()) => set = true,
                    Err(why) => last_error = why,
                }
            }
            if set {
                Ok(())
            } else {
                Err(last_error)
            }
        }
    }

    fn apply_to(pid: u32, wanted: ProcessSettings) -> Result<(), String> {
        // SAFETY: opening a process by ID to set its scheduling; closed by OwnedHandle.
        let handle = unsafe { OpenProcess(PROCESS_SET_INFORMATION, 0, pid) };
        if handle.is_null() {
            return Err(format!(
                "cannot open the VM's process {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
        let process = OwnedHandle(handle);
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: wanted.throttling_control,
            StateMask: wanted.throttling_state,
        };
        // SAFETY: the state outlives the call and has the size passed.
        let throttled = unsafe {
            SetProcessInformation(
                process.0,
                ProcessPowerThrottling,
                (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        if throttled == 0 {
            return Err(format!(
                "cannot set the power throttling of the VM's process {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: a process handle opened with PROCESS_SET_INFORMATION.
        if unsafe { SetPriorityClass(process.0, wanted.priority) } == 0 {
            return Err(format!(
                "cannot set the priority of the VM's process {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Follows the power hints of `session`'s guest until the session is gone, and runs the
    /// guest's processors as they ask. `user` names the user in the log, which says the first
    /// time each VM runs at each performance, and the first failure for each.
    pub fn follow(hints: Receiver<GuestPower>, session: Weak<Session>, user: String) {
        let mut policy = PowerHints::new();
        let mut processes: Option<VmProcesses> = None;
        let mut applied: Option<Performance> = None;
        let mut failed: HashSet<Performance> = HashSet::new();
        loop {
            let now = Instant::now();
            let event = match policy.next_change(now) {
                Some(at) => hints.recv_timeout(at.saturating_duration_since(now)),
                None => hints.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match event {
                Ok(GuestPower::Hint(hint)) => policy.hint(hint, Instant::now()),
                Ok(GuestPower::WindowsShown(shown)) => policy.windows_shown(shown),
                Ok(GuestPower::HostInput) => policy.host_input(Instant::now()),
                Ok(GuestPower::Stopped) => {
                    // The VM and its processes are gone; the next one starts as Windows runs
                    // any program.
                    policy.clear();
                    processes = None;
                    applied = None;
                    failed.clear();
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let received = Instant::now();
            let wanted = policy.performance(Instant::now());
            if applied == Some(wanted) {
                continue;
            }
            let vm_id = match session.upgrade() {
                Some(session) => session.vm_id(),
                None => return,
            };
            let Some(vm_id) = vm_id else {
                continue;
            };
            if processes.as_ref().is_none_or(|found| found.vm_id != vm_id) {
                processes = match VmProcesses::find(&vm_id) {
                    Ok(found) => Some(found),
                    Err(why) => {
                        if failed.insert(wanted) {
                            eprintln!("{user}: power: {why}");
                        }
                        None
                    }
                };
            }
            let Some(found) = processes.as_ref() else {
                continue;
            };
            match found.apply(wanted) {
                Ok(()) => {
                    applied = Some(wanted);
                    let now = Instant::now();
                    eprintln!(
                        "{user}: power: {} running Android's processors {} ({}; took {} us)",
                        utc_stamp(wall_now()),
                        describe(wanted),
                        policy.explain(now),
                        now.saturating_duration_since(received).as_micros()
                    );
                }
                Err(why) => {
                    if failed.insert(wanted) {
                        eprintln!(
                            "{user}: power: could not run Android's processors {}: {why}",
                            describe(wanted)
                        );
                    }
                    // Looked up again next time, in case the VM's process changed.
                    processes = None;
                }
            }
        }
    }
}
