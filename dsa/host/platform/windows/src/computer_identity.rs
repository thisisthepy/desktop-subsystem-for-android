//! What this PC says it is, for the device identity of each user's Android: the firmware's
//! system and baseboard strings, the processor's name and the computer's name.
//!
//! Windows keeps a copy of the firmware's SMBIOS strings under
//! `HKLM\HARDWARE\DESCRIPTION\System\BIOS` and the processor's under
//! `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0`; they are what WMI's
//! `Win32_ComputerSystem`, `Win32_BaseBoard` and `Win32_Processor` report, read without a COM
//! round trip. A value the registry lacks is taken from the SMBIOS tables themselves
//! (`GetSystemFirmwareTable`), which is where WMI reads them. The computer's name is the one
//! Settings shows under System, About.
//!
//! Read at every start of Android, so a renamed PC is followed from the next start.

use dsa_host_core::computer_identity::ComputerInfo;

/// What this PC says it is now.
pub fn read() -> ComputerInfo {
    let mut info = from_registry();
    let lacking = [
        &info.system_manufacturer,
        &info.system_product,
        &info.system_family,
        &info.system_version,
        &info.baseboard_product,
        &info.cpu_name,
    ]
    .iter()
    .any(|value| dsa_host_core::computer_identity::is_placeholder(value));
    if lacking {
        if let Some(tables) = smbios_tables() {
            info.fill_from(&dsa_host_core::computer_identity::from_smbios(&tables));
        }
    }
    info
}

#[cfg(windows)]
fn from_registry() -> ComputerInfo {
    let bios = r"HARDWARE\DESCRIPTION\System\BIOS";
    let cpu = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    ComputerInfo {
        system_manufacturer: registry_text(bios, "SystemManufacturer"),
        system_product: registry_text(bios, "SystemProductName"),
        system_family: registry_text(bios, "SystemFamily"),
        system_version: registry_text(bios, "SystemVersion"),
        baseboard_product: registry_text(bios, "BaseBoardProduct"),
        computer_name: computer_name(),
        cpu_vendor: registry_text(cpu, "VendorIdentifier"),
        cpu_name: registry_text(cpu, "ProcessorNameString"),
    }
}

#[cfg(not(windows))]
fn from_registry() -> ComputerInfo {
    ComputerInfo::default()
}

/// A string value under `HKEY_LOCAL_MACHINE`, or empty when there is none.
#[cfg(windows)]
fn registry_text(key: &str, name: &str) -> String {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (key, name) = (wide(key), wide(name));
    let mut buffer = vec![0u16; 512];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer and its size in bytes describe the same memory.
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if code != ERROR_SUCCESS {
        return String::new();
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len]).trim().to_string()
}

/// The computer's name as Settings shows it (its DNS host name, as the user typed it).
#[cfg(windows)]
fn computer_name() -> String {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, GetComputerNameExW,
    };
    let mut buffer = vec![0u16; 256];
    let mut size = buffer.len() as u32;
    // SAFETY: the buffer holds `size` characters, which the call is told.
    let ok = unsafe { GetComputerNameExW(ComputerNameDnsHostname, buffer.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..(size as usize).min(buffer.len())])
}

/// The SMBIOS structure tables, without the eight bytes Windows puts before them.
#[cfg(windows)]
fn smbios_tables() -> Option<Vec<u8>> {
    use windows_sys::Win32::System::SystemInformation::{GetSystemFirmwareTable, RSMB};
    // SAFETY: a null buffer of size 0 asks only for the size.
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: the buffer is `size` bytes long.
    let read = unsafe { GetSystemFirmwareTable(RSMB, 0, data.as_mut_ptr().cast(), size) };
    if read == 0 || read > size {
        return None;
    }
    data.truncate(read as usize);
    // RawSMBIOSData: a calling method byte, the major and minor version, the DMI revision,
    // the length of the tables (u32), then the tables.
    let length = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?) as usize;
    let tables = data.get(8..8 + length.min(data.len().saturating_sub(8)))?;
    Some(tables.to_vec())
}

#[cfg(not(windows))]
fn smbios_tables() -> Option<Vec<u8>> {
    None
}
