//! Hyper-V socket service GUID registration under `GuestCommunicationServices`.
//!
//! Windows Hyper-V sockets (`AF_HYPERV`) require that every custom service GUID the host
//! listens on be registered in the machine registry under
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices`.
//!
//! Registering here requires administrator privileges, so the DSA Windows service registers
//! the graphics service GUIDs at startup. Once registered, non-admin interactive users whose
//! VM security descriptor grants socket access can bind an `AF_HYPERV` listener for that
//! service ID and their VM ID.

use std::io;
use std::ptr::{null, null_mut};

use dsa_host_core::compositor::{GRAPHICS_COMMAND_PORT, GRAPHICS_PROCESS_PIPE_PORT};
use windows_sys::Win32::Foundation::{LocalFree, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegGetValueW, RegOpenKeyExW, RegSetKeySecurity,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ,
    REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};

use crate::guid::Guid;

/// The Windows registry hive to operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryHive {
    LocalMachine,
    CurrentUser,
}

impl RegistryHive {
    fn hkey(self) -> HKEY {
        match self {
            RegistryHive::LocalMachine => HKEY_LOCAL_MACHINE,
            RegistryHive::CurrentUser => HKEY_CURRENT_USER,
        }
    }
}

/// The registry key where Hyper-V guest communication services are registered.
pub const GUEST_COMMUNICATION_SERVICES_KEY: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices";

/// The default SDDL for registered Hyper-V socket services.
///
/// Grants SYSTEM, Administrators and Owner full control, and Interactive Users and
/// Authenticated Users read access.
pub const DEFAULT_SERVICE_SDDL: &str =
    "D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)(A;OICI;GA;;;OW)(A;OICI;GR;;;IU)(A;OICI;GR;;;AU)";

/// Friendly name for the graphics command stream service (port 5566).
pub const GRAPHICS_COMMAND_ELEMENT_NAME: &str =
    "Desktop Subsystem for Android Graphics Command Stream";

/// Friendly name for the graphics process pipe service (port 5567).
pub const GRAPHICS_PROCESS_PIPE_ELEMENT_NAME: &str =
    "Desktop Subsystem for Android Graphics Process Pipe";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

fn parse_sddl(sddl: &str) -> Result<SecurityDescriptor, String> {
    let wide_sddl = wide(sddl);
    let mut descriptor = null_mut();
    // SAFETY: standard Win32 call with valid wide string and output pointer.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "invalid security descriptor {sddl:?}: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(SecurityDescriptor(descriptor))
}

/// Registers a Hyper-V socket service GUID under `root` at `base_path`.
pub fn register_service_guid_under(
    root: RegistryHive,
    base_path: &str,
    guid: Guid,
    element_name: &str,
    sddl: Option<&str>,
) -> Result<(), String> {
    let subkey_name = format!("{base_path}\\{guid}");
    let subkey_wide = wide(&subkey_name);
    let mut key: HKEY = null_mut();
    let mut disposition = 0u32;

    // SAFETY: Win32 registry creation.
    let code = unsafe {
        RegCreateKeyExW(
            root.hkey(),
            subkey_wide.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            null(),
            &mut key,
            &mut disposition,
        )
    };
    if code != ERROR_SUCCESS {
        return Err(format!(
            "creating registry key {subkey_name}: {}",
            io::Error::from_raw_os_error(code as i32)
        ));
    }

    let result = (|| {
        if let Some(sddl_text) = sddl {
            let desc = parse_sddl(sddl_text)?;
            // SAFETY: setting security on an open registry key.
            let sec_code = unsafe { RegSetKeySecurity(key, DACL_SECURITY_INFORMATION, desc.0) };
            if sec_code != ERROR_SUCCESS {
                return Err(format!(
                    "setting DACL on {subkey_name}: {}",
                    io::Error::from_raw_os_error(sec_code as i32)
                ));
            }
        }

        let name_wide = wide(element_name);
        let value_name_wide = wide("ElementName");
        let bytes = name_wide.len() * 2;
        // SAFETY: writing string value to open registry key.
        let val_code = unsafe {
            RegSetValueExW(
                key,
                value_name_wide.as_ptr(),
                0,
                REG_SZ,
                name_wide.as_ptr().cast(),
                bytes as u32,
            )
        };
        if val_code != ERROR_SUCCESS {
            return Err(format!(
                "setting ElementName on {subkey_name}: {}",
                io::Error::from_raw_os_error(val_code as i32)
            ));
        }
        Ok(())
    })();

    // SAFETY: closing key handle.
    unsafe { RegCloseKey(key) };
    result
}

/// Unregisters a Hyper-V socket service GUID under `root` at `base_path`.
pub fn unregister_service_guid_under(
    root: RegistryHive,
    base_path: &str,
    guid: Guid,
) -> Result<(), String> {
    let subkey_name = format!("{base_path}\\{guid}");
    let subkey_wide = wide(&subkey_name);
    // SAFETY: deleting registry subkey.
    let code = unsafe { RegDeleteKeyW(root.hkey(), subkey_wide.as_ptr()) };
    if code == ERROR_SUCCESS || code == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "deleting registry key {subkey_name}: {}",
            io::Error::from_raw_os_error(code as i32)
        ))
    }
}

/// Checks whether a service GUID is registered under `root` at `base_path`.
pub fn is_service_guid_registered_under(root: RegistryHive, base_path: &str, guid: Guid) -> bool {
    let subkey_name = format!("{base_path}\\{guid}");
    let subkey_wide = wide(&subkey_name);
    let mut key: HKEY = null_mut();
    // SAFETY: opening registry key for read access.
    let code = unsafe { RegOpenKeyExW(root.hkey(), subkey_wide.as_ptr(), 0, KEY_READ, &mut key) };
    if code == ERROR_SUCCESS {
        unsafe { RegCloseKey(key) };
        true
    } else {
        false
    }
}

/// Reads the ElementName value of a registered service GUID, if present.
pub fn read_service_element_name_under(
    root: RegistryHive,
    base_path: &str,
    guid: Guid,
) -> Option<String> {
    let subkey_name = format!("{base_path}\\{guid}");
    let subkey_wide = wide(&subkey_name);
    let value_wide = wide("ElementName");
    let mut size = 0u32;
    // SAFETY: query size of string value.
    let code = unsafe {
        RegGetValueW(
            root.hkey(),
            subkey_wide.as_ptr(),
            value_wide.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        )
    };
    if code != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; size as usize / 2 + 1];
    let mut size_bytes = (buffer.len() * 2) as u32;
    // SAFETY: read string value into buffer.
    let code = unsafe {
        RegGetValueW(
            root.hkey(),
            subkey_wide.as_ptr(),
            value_wide.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size_bytes,
        )
    };
    if code != ERROR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// Registers the service GUID under `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Virtualization\GuestCommunicationServices`.
pub fn register_service_guid(
    guid: Guid,
    element_name: &str,
    sddl: Option<&str>,
) -> Result<(), String> {
    register_service_guid_under(
        RegistryHive::LocalMachine,
        GUEST_COMMUNICATION_SERVICES_KEY,
        guid,
        element_name,
        sddl,
    )
}

/// Unregisters the service GUID from `HKLM`.
pub fn unregister_service_guid(guid: Guid) -> Result<(), String> {
    unregister_service_guid_under(
        RegistryHive::LocalMachine,
        GUEST_COMMUNICATION_SERVICES_KEY,
        guid,
    )
}

/// Registers the graphics command stream (port 5566) and process pipe (port 5567) service GUIDs under `root` at `base_path`.
pub fn register_graphics_service_guids_under(
    root: RegistryHive,
    base_path: &str,
) -> Result<(), String> {
    let command_guid = Guid::vsock_service(GRAPHICS_COMMAND_PORT);
    let process_pipe_guid = Guid::vsock_service(GRAPHICS_PROCESS_PIPE_PORT);
    register_service_guid_under(
        root,
        base_path,
        command_guid,
        GRAPHICS_COMMAND_ELEMENT_NAME,
        Some(DEFAULT_SERVICE_SDDL),
    )?;
    register_service_guid_under(
        root,
        base_path,
        process_pipe_guid,
        GRAPHICS_PROCESS_PIPE_ELEMENT_NAME,
        Some(DEFAULT_SERVICE_SDDL),
    )?;
    Ok(())
}

/// Registers the graphics command stream (port 5566) and process pipe (port 5567) service GUIDs.
///
/// This function is intended to be called by the DSA Windows service during startup so that
/// user-session compositor processes can listen on these ports without requiring administrator rights.
pub fn register_graphics_service_guids() -> Result<(), String> {
    if let Ok(test_key) = std::env::var("DSA_TEST_GUEST_COMMUNICATION_SERVICES_KEY") {
        if !test_key.is_empty() {
            return register_graphics_service_guids_under(RegistryHive::CurrentUser, &test_key);
        }
    }
    register_graphics_service_guids_under(
        RegistryHive::LocalMachine,
        GUEST_COMMUNICATION_SERVICES_KEY,
    )
}
