//! Which host processes belong to a user's Android, for `dsa memory` and the service's log.

use dsa_host_core::memory::HostRole;
use dsa_host_windows::host_memory::{role_of, select, ProcessInfo};

const USER: &str = "S-1-5-21-1000-2000-3000-1001";
const OTHER_USER: &str = "S-1-5-21-1000-2000-3000-1002";
const VM: &str = "6A5F2A1E-0000-4000-8000-000000000001";
const WINDOW_MANAGER: &str = r"C:\Program Files\DSA\dsa-wm.exe";
const TEMPLATE: &str = r"C:\Program Files\DSA\dsa-app-window.exe";
const CLOCK: &str = r"C:\Users\me\AppData\Local\DSA\apps\a\Clock.exe";
const SETTINGS: &str = r"C:\Users\me\AppData\Local\DSA\apps\b\Settings.exe";
const EXPLORER: &str = r"C:\Windows\explorer.exe";

fn process(pid: u32, image: &str, account: &str, mib: u64) -> ProcessInfo {
    ProcessInfo {
        pid,
        image: image.into(),
        account: account.into(),
        working_set_kb: mib * 1024,
        private_kb: mib * 512,
    }
}

#[test]
fn nfr1_the_vms_memory_process_is_told_apart_by_its_vm_id() {
    let vmmem = process(4, "vmmem", &format!("NT VIRTUAL MACHINE\\{VM}"), 2444);
    let vm_id = VM.to_ascii_lowercase();
    assert_eq!(role_of(&vmmem, USER, Some(&vm_id)), Some(HostRole::Vm));
    assert_eq!(
        role_of(&vmmem, USER, Some(&format!("{{{vm_id}}}"))),
        Some(HostRole::Vm)
    );
    // Another user's VM, or no VM running for this user.
    let other_vm = "11111111-2222-3333-4444-555555555555";
    assert_eq!(role_of(&vmmem, USER, Some(other_vm)), None);
    assert_eq!(role_of(&vmmem, USER, None), None);
    // WSL's own VM runs under the same domain with its own ID.
    let wsl = process(5, "vmmem.exe", "NT VIRTUAL MACHINE\\AAAA", 900);
    assert_eq!(role_of(&wsl, USER, Some(&vm_id)), None);
}

#[test]
fn nfr1_the_vms_memory_process_matches_with_any_suffix() {
    let vm_id = VM.to_ascii_lowercase();
    for name in ["vmmemDSA", "vmmemDSA.exe", "VmmemDSA"] {
        let vmmem = process(4, name, &format!("NT VIRTUAL MACHINE\\{VM}"), 2444);
        assert_eq!(
            role_of(&vmmem, USER, Some(&vm_id)),
            Some(HostRole::Vm),
            "{name}"
        );
    }
    // WSL's VM has its own ID, so its suffix does not matter.
    let wsl = process(5, "VmmemWSL", "NT VIRTUAL MACHINE\\AAAA", 900);
    assert_eq!(role_of(&wsl, USER, Some(&vm_id)), None);
}

#[test]
fn nfr1_the_window_processes_are_the_users_own() {
    assert_eq!(
        role_of(&process(10, WINDOW_MANAGER, USER, 508), USER, Some(VM)),
        Some(HostRole::WindowManager)
    );
    assert_eq!(
        role_of(&process(11, TEMPLATE, USER, 53), USER, Some(VM)),
        Some(HostRole::AppWindow)
    );
    assert_eq!(
        role_of(&process(12, SETTINGS, USER, 53), USER, Some(VM)),
        Some(HostRole::AppWindow)
    );
    assert_eq!(
        role_of(
            &process(13, WINDOW_MANAGER, OTHER_USER, 508),
            USER,
            Some(VM)
        ),
        None
    );
    assert_eq!(
        role_of(&process(14, EXPLORER, USER, 100), USER, Some(VM)),
        None
    );
}

#[test]
fn nfr1_the_processes_are_listed_vm_first_then_window_manager_then_app_windows() {
    let processes = [
        process(30, CLOCK, USER, 53),
        process(20, WINDOW_MANAGER, USER, 508),
        process(12, SETTINGS, USER, 53),
        process(4, "vmmem", &format!("NT VIRTUAL MACHINE\\{VM}"), 2444),
        process(9, EXPLORER, USER, 100),
    ];
    let found = select(&processes, USER, Some(VM));
    let order: Vec<(HostRole, u32)> = found.iter().map(|p| (p.role, p.pid)).collect();
    assert_eq!(
        order,
        [
            (HostRole::Vm, 4),
            (HostRole::WindowManager, 20),
            (HostRole::AppWindow, 12),
            (HostRole::AppWindow, 30),
        ]
    );
    assert_eq!(found[0].working_set_kb, 2444 * 1024);
}
