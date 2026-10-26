//! The host processes that run a user's Android, and how much memory each takes.
//!
//! Three kinds of process belong to a user's Android:
//!
//! - the VM's memory process, `vmmem`, which holds the guest's RAM. Hyper-V runs it as the
//!   VM's own account, `NT VIRTUAL MACHINE\<VM ID>`, whose SID is made from the VM's runtime
//!   ID, so each user's VM is told apart from every other VM on the machine (WSL's too);
//! - the window manager, `dsa-wm.exe`, running as the user;
//! - each app's window program, running as the user: `dsa-app-window.exe`, or its copy named
//!   after the app under `%LOCALAPPDATA%\DSA\apps\`.
//!
//! The service runs as SYSTEM, so it can read the memory counters of every one of them. The
//! rules that pick the processes are plain data and are tested on every platform; reading the
//! process list is Windows only.

use dsa_host_core::memory::HostRole;

use crate::guid::Guid;

/// The SID of the account Hyper-V runs the VM with runtime ID `vm_id` as:
/// `S-1-5-83-1-` and the 16 bytes of the GUID, in its in-memory layout, as four
/// little-endian numbers.
pub fn vm_account_sid(vm_id: &str) -> Option<String> {
    let guid: Guid = vm_id.trim_matches(|c| c == '{' || c == '}').parse().ok()?;
    let mut bytes = [0u8; 16];
    bytes[..4].copy_from_slice(&guid.data1.to_le_bytes());
    bytes[4..6].copy_from_slice(&guid.data2.to_le_bytes());
    bytes[6..8].copy_from_slice(&guid.data3.to_le_bytes());
    bytes[8..].copy_from_slice(&guid.data4);
    let part = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Some(format!(
        "S-1-5-83-1-{}-{}-{}-{}",
        part(0),
        part(4),
        part(8),
        part(12)
    ))
}

/// What one process on the machine is, as far as it can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFacts<'a> {
    /// The program's file name, such as `dsa-wm.exe`.
    pub name: &'a str,
    /// The program's full path, when it could be read.
    pub path: Option<&'a str>,
    /// The SID of the account it runs as, when it could be read.
    pub owner: Option<&'a str>,
}

/// What the process does for the Android of `user`, whose VM runs as `vm_sid`; `None` when
/// it is not one of theirs.
pub fn classify(facts: &ProcessFacts<'_>, user: &str, vm_sid: Option<&str>) -> Option<HostRole> {
    let name = facts.name.to_ascii_lowercase();
    let owner = facts.owner?;
    if vm_sid.is_some_and(|sid| sid.eq_ignore_ascii_case(owner)) && name.starts_with("vmmem") {
        return Some(HostRole::Vm);
    }
    if !owner.eq_ignore_ascii_case(user) {
        return None;
    }
    if name == "dsa-wm.exe" {
        return Some(HostRole::WindowManager);
    }
    let in_apps = facts.path.is_some_and(|path| {
        path.to_ascii_lowercase()
            .replace('/', "\\")
            .contains("\\dsa\\apps\\")
    });
    if name == "dsa-app-window.exe" || in_apps {
        return Some(HostRole::AppWindow);
    }
    None
}

#[cfg(windows)]
pub use windows_impl::ProcessMemory;

#[cfg(windows)]
mod windows_impl {
    use dsa_host_core::memory::{HostMemory, HostProcessMemory};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::{classify, vm_account_sid, ProcessFacts};
    use crate::pipe::user_sid_of_token;

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle this module opened, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn wide_to_string(wide: &[u16]) -> String {
        let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..len])
    }

    fn image_path(process: &Handle) -> Option<String> {
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        // SAFETY: the buffer holds `len` characters.
        let ok = unsafe {
            QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut len)
        };
        (ok != 0).then(|| String::from_utf16_lossy(&buffer[..len as usize]))
    }

    fn owner(process: &Handle) -> Option<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: a process handle opened for query; the token is closed below.
        if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let token = Handle(token);
        user_sid_of_token(token.0).ok()
    }

    fn counters(process: &Handle) -> Option<(u64, u64)> {
        // SAFETY: an all-zero PROCESS_MEMORY_COUNTERS_EX is valid.
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        // SAFETY: the structure is as large as `cb` says.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                process.0,
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
        (ok != 0).then(|| {
            (
                counters.WorkingSetSize as u64 / 1024,
                counters.PrivateUsage as u64 / 1024,
            )
        })
    }

    /// Reads the process list and the memory counters of the processes that run each user's
    /// Android.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct ProcessMemory;

    impl HostMemory for ProcessMemory {
        fn processes(&self, user: &str, vm_id: Option<&str>) -> Vec<HostProcessMemory> {
            let vm_sid = vm_id.and_then(vm_account_sid);
            let mut found = Vec::new();
            // SAFETY: a snapshot of the process list; closed when `snapshot` drops.
            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snapshot == INVALID_HANDLE_VALUE {
                eprintln!(
                    "memory: the process list could not be read: {}",
                    std::io::Error::last_os_error()
                );
                return found;
            }
            let snapshot = Handle(snapshot);
            // SAFETY: an all-zero PROCESSENTRY32W is valid once its size is set.
            let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            // SAFETY: a valid snapshot and an entry whose size is set.
            let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
            while more {
                let name = wide_to_string(&entry.szExeFile);
                let pid = entry.th32ProcessID;
                let interesting = {
                    let lower = name.to_ascii_lowercase();
                    lower.starts_with("vmmem") || lower.ends_with(".exe")
                };
                if pid != 0 && interesting {
                    // SAFETY: opening for limited query only; closed when `process` drops.
                    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
                    if !raw.is_null() {
                        let process = Handle(raw);
                        let owner = owner(&process);
                        let path = image_path(&process);
                        let facts = ProcessFacts {
                            name: &name,
                            path: path.as_deref(),
                            owner: owner.as_deref(),
                        };
                        if let Some(role) = classify(&facts, user, vm_sid.as_deref()) {
                            if let Some((working_set_kb, private_kb)) = counters(&process) {
                                found.push(HostProcessMemory {
                                    role,
                                    pid,
                                    working_set_kb,
                                    private_kb,
                                });
                            }
                        }
                    }
                }
                // SAFETY: as above.
                more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
            }
            found
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn fr17_the_vm_account_sid_is_made_from_the_vm_id() {
        let sid = vm_account_sid("01020304-0506-0708-090a-0b0c0d0e0f10").unwrap();
        // data1 0x01020304 is stored 04 03 02 01, and read back as one number.
        assert_eq!(
            sid,
            format!(
                "S-1-5-83-1-{}-{}-{}-{}",
                0x0102_0304u32,
                u32::from_le_bytes([0x06, 0x05, 0x08, 0x07]),
                u32::from_le_bytes([0x09, 0x0a, 0x0b, 0x0c]),
                u32::from_le_bytes([0x0d, 0x0e, 0x0f, 0x10])
            )
        );
        assert_eq!(
            vm_account_sid("{01020304-0506-0708-090a-0b0c0d0e0f10}"),
            Some(sid)
        );
        assert_eq!(vm_account_sid("not a guid"), None);
    }

    #[test]
    fn fr17_only_the_callers_vm_window_manager_and_app_windows_are_counted() {
        let vm = vm_account_sid("01020304-0506-0708-090a-0b0c0d0e0f10").unwrap();
        let other_vm = vm_account_sid("11111111-0506-0708-090a-0b0c0d0e0f10").unwrap();
        let of = |name, path, owner| ProcessFacts { name, path, owner };
        let role = |facts: ProcessFacts<'_>| classify(&facts, USER, Some(&vm));

        assert_eq!(role(of("vmmem", None, Some(&vm))), Some(HostRole::Vm));
        assert_eq!(role(of("vmmem", None, Some(&other_vm))), None, "another VM");
        assert_eq!(role(of("vmmemWSL", None, Some(&other_vm))), None, "WSL's VM");
        assert_eq!(
            role(of("dsa-wm.exe", Some("C:\\Program Files\\DSA\\dsa-wm.exe"), Some(USER))),
            Some(HostRole::WindowManager)
        );
        assert_eq!(
            role(of("dsa-wm.exe", None, Some("S-1-5-21-1-2-3-1002"))),
            None,
            "another user's window manager"
        );
        assert_eq!(
            role(of(
                "Settings.exe",
                Some("C:\\Users\\u\\AppData\\Local\\DSA\\apps\\com.android.settings\\Settings.exe"),
                Some(USER)
            )),
            Some(HostRole::AppWindow)
        );
        assert_eq!(
            role(of("DSA-APP-WINDOW.EXE", None, Some(USER))),
            Some(HostRole::AppWindow)
        );
        assert_eq!(
            role(of("notepad.exe", Some("C:\\Windows\\notepad.exe"), Some(USER))),
            None
        );
        assert_eq!(role(of("dsa-wm.exe", None, None)), None, "owner unknown");
        // Without a running VM, no vmmem process is the caller's.
        assert_eq!(
            classify(&of("vmmem", None, Some(&vm)), USER, None),
            None
        );
    }
}
