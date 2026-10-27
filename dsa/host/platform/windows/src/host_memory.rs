//! The host's memory for each user's Android: the VM's memory process and the user's window
//! processes, as Task Manager counts them.
//!
//! - The VM: Hyper-V gives each VM a `vmmem` process that holds the guest's memory. Its
//!   account is `NT VIRTUAL MACHINE\<VM ID>`, which tells one user's VM from another's.
//! - The window manager (`dsa-wm.exe`) and the app window hosts (`dsa-app-window.exe`, and
//!   each app's own copy under `%LOCALAPPDATA%\DSA\apps\`) run in the user's session, under
//!   the user's own account.
//!
//! The service runs as SYSTEM, which may open every one of them for a memory query. The
//! rules that pick the processes are plain functions, tested on every platform; reading the
//! processes is Windows only.

use dsa_host_core::memory::{HostProcessMemory, HostRole};

/// The window manager's program file.
pub const WINDOW_MANAGER_EXE: &str = "dsa-wm.exe";
/// The app window host as installed; each app runs a copy of it named after the app.
pub const APP_WINDOW_EXE: &str = "dsa-app-window.exe";
/// The account domain of every Hyper-V VM's processes.
pub const VM_DOMAIN: &str = "NT VIRTUAL MACHINE";

/// One process as the system lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    /// Its program file's full path, or its file name when the path cannot be read.
    pub image: String,
    /// Its account: a SID (`S-1-...`) for a user, `<domain>\<name>` for a VM.
    pub account: String,
    pub working_set_kb: u64,
    pub private_kb: u64,
}

fn file_name(image: &str) -> &str {
    image.rsplit(['\\', '/']).next().unwrap_or(image)
}

/// Whether `name` is a VM's memory process: `vmmem` with any suffix (`vmmemDSA`, `vmmemWSL`)
/// and with or without `.exe`. Whose VM it is stays the VM ID check's job.
fn is_vmmem(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("vmmem")
}

/// What `process` does for the Android of the user `user_sid`, whose VM has the ID `vm_id`
/// while it runs. `None` for any other process, and for another user's or another VM's.
pub fn role_of(process: &ProcessInfo, user_sid: &str, vm_id: Option<&str>) -> Option<HostRole> {
    let name = file_name(&process.image);
    if is_vmmem(name) {
        let vm_id = vm_id?.trim_matches(['{', '}']);
        let (domain, account) = process.account.split_once('\\')?;
        let matches = domain.eq_ignore_ascii_case(VM_DOMAIN)
            && account.trim_matches(['{', '}']).eq_ignore_ascii_case(vm_id);
        return matches.then_some(HostRole::Vm);
    }
    if !process.account.eq_ignore_ascii_case(user_sid) {
        return None;
    }
    if name.eq_ignore_ascii_case(WINDOW_MANAGER_EXE) {
        return Some(HostRole::WindowManager);
    }
    let in_apps_folder = process
        .image
        .to_ascii_lowercase()
        .replace('/', "\\")
        .contains("\\dsa\\apps\\");
    let app_window = name.eq_ignore_ascii_case(APP_WINDOW_EXE) || in_apps_folder;
    app_window.then_some(HostRole::AppWindow)
}

/// The processes of the user's Android among `processes`: the VM first, then the window
/// manager, then the app windows.
pub fn select(
    processes: &[ProcessInfo],
    user_sid: &str,
    vm_id: Option<&str>,
) -> Vec<HostProcessMemory> {
    let mut found: Vec<HostProcessMemory> = processes
        .iter()
        .filter_map(|p| {
            Some(HostProcessMemory {
                role: role_of(p, user_sid, vm_id)?,
                pid: p.pid,
                working_set_kb: p.working_set_kb,
                private_kb: p.private_kb,
            })
        })
        .collect();
    let rank = |role: HostRole| match role {
        HostRole::Vm => 0,
        HostRole::WindowManager => 1,
        HostRole::AppWindow => 2,
    };
    found.sort_by_key(|p| (rank(p.role), p.pid));
    found
}

/// Measures the processes of each user's Android from the system's process list.
#[cfg(windows)]
pub struct WindowsHostMemory;

#[cfg(windows)]
impl dsa_host_core::memory::HostMemory for WindowsHostMemory {
    fn processes(&self, user: &str, vm_id: Option<&str>) -> Vec<HostProcessMemory> {
        match system::processes() {
            Ok(list) => select(&list, user, vm_id),
            Err(error) => {
                eprintln!("memory: cannot list the host's processes: {error}");
                Vec::new()
            }
        }
    }
}

#[cfg(windows)]
mod system {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, LookupAccountSidW, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    use super::{file_name, ProcessInfo};
    use crate::pipe::{token_user_sid, OwnedHandle};

    /// The access a memory query needs. A protected process (a VM's) may grant only the
    /// first part, which is tried next.
    const ACCESS: u32 = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;

    fn wide_to_string(wide: &[u16]) -> String {
        let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..len])
    }

    /// Whether a process of this file name may be one this module reports: a VM's memory
    /// process, or a program (an app's window host has the app's name). Anything else is
    /// passed over without being opened.
    fn interesting(name: &str) -> bool {
        is_vmmem(name) || name.to_ascii_lowercase().ends_with(".exe")
    }

    /// The account of a process's token: `<domain>\<name>` for a VM, whose name is its ID,
    /// and the SID for anyone else.
    fn account(token: &OwnedHandle) -> io::Result<String> {
        let sid_text = token_user_sid(token)?;
        // NT VIRTUAL MACHINE's SIDs (S-1-5-83-...) are looked up for the VM's ID.
        if !sid_text.starts_with("S-1-5-83-") {
            return Ok(sid_text);
        }
        let mut needed = 0u32;
        // SAFETY: a size query.
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
        // SAFETY: the buffer holds at least `needed` bytes, aligned for TOKEN_USER.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the buffer holds a TOKEN_USER.
        let sid = unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut name = [0u16; 256];
        let mut domain = [0u16; 256];
        let (mut name_len, mut domain_len, mut kind) = (256u32, 256u32, 0);
        // SAFETY: valid buffers of the sizes given.
        if unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                sid,
                name.as_mut_ptr(),
                &mut name_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut kind,
            )
        } == 0
        {
            return Ok(sid_text);
        }
        Ok(format!(
            "{}\\{}",
            wide_to_string(&domain),
            wide_to_string(&name)
        ))
    }

    fn measure(pid: u32, exe: &str) -> Option<ProcessInfo> {
        // SAFETY: opening a process by ID for queries only; closed by OwnedHandle.
        let mut handle: HANDLE = unsafe { OpenProcess(ACCESS, 0, pid) };
        if handle.is_null() {
            // SAFETY: as above.
            handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        }
        if handle.is_null() {
            return None;
        }
        let process = OwnedHandle(handle);
        let mut path = [0u16; 1024];
        let mut path_len = path.len() as u32;
        // SAFETY: a valid buffer of the size given.
        let named = unsafe {
            QueryFullProcessImageNameW(
                process.0,
                PROCESS_NAME_WIN32,
                path.as_mut_ptr(),
                &mut path_len,
            )
        };
        let image = if named != 0 {
            String::from_utf16_lossy(&path[..path_len as usize])
        } else {
            exe.to_string()
        };
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the process's token, opened for query only.
        let opened = unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } != 0;
        let account = if opened {
            account(&OwnedHandle(token)).unwrap_or_default()
        } else {
            String::new()
        };
        // SAFETY: plain counters, zero until filled.
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let counters_ptr = std::ptr::addr_of_mut!(counters).cast::<PROCESS_MEMORY_COUNTERS>();
        // SAFETY: the counters' size is passed with them.
        if unsafe { K32GetProcessMemoryInfo(process.0, counters_ptr, counters.cb) } == 0 {
            return None;
        }
        Some(ProcessInfo {
            pid,
            image,
            account,
            working_set_kb: counters.WorkingSetSize as u64 / 1024,
            private_kb: counters.PrivateUsage as u64 / 1024,
        })
    }

    /// Every process that may be one of a user's Android, measured.
    pub fn processes() -> io::Result<Vec<ProcessInfo>> {
        // SAFETY: a process snapshot, closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut found = Vec::new();
        // SAFETY: plain data, zero until filled; its size is set as the API needs.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: a valid snapshot and entry.
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while more {
            let exe = wide_to_string(&entry.szExeFile);
            if interesting(file_name(&exe)) {
                if let Some(info) = measure(entry.th32ProcessID, &exe) {
                    found.push(info);
                }
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        // SAFETY: the snapshot handle, closed once.
        unsafe { CloseHandle(snapshot) };
        Ok(found)
    }
}
