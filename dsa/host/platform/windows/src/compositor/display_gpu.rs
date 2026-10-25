//! The Windows calls behind the render GPU choice: which adapter drives each display, and
//! what each adapter is called. The choice itself is in [`crate::render_gpu`].
//!
//! The displays come from the display configuration (`QueryDisplayConfig`) rather than from
//! DXGI's outputs. On a hybrid laptop DXGI can list the built-in panel under the discrete
//! adapter too, so that a program running there can find an output; the display
//! configuration names the adapter that actually scans the desktop out.

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows_sys::Win32::Devices::Display::{
    GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO, QDC_ONLY_ACTIVE_PATHS,
};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};

use crate::render_gpu::{AdapterName, DisplaySource, Luid};

/// The active display sources: each adapter's desktop area and where it is.
pub fn display_sources() -> Result<Vec<DisplaySource>, String> {
    // The configuration can change between asking for the sizes and reading it.
    for _ in 0..3 {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        // SAFETY: both counts are valid out pointers.
        let status = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if status != ERROR_SUCCESS {
            return Err(format!(
                "could not size the display configuration (error {status})"
            ));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: the arrays hold the counts passed with them; no topology is asked for
        // with QDC_ONLY_ACTIVE_PATHS.
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != ERROR_SUCCESS {
            return Err(format!(
                "could not read the display configuration (error {status})"
            ));
        }
        modes.truncate(mode_count as usize);
        return Ok(modes
            .iter()
            .filter(|mode| mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
            .map(|mode| {
                // SAFETY: a source mode's union holds its source mode.
                let position = unsafe { mode.Anonymous.sourceMode.position };
                DisplaySource {
                    adapter: Luid::from_parts(mode.adapterId.LowPart, mode.adapterId.HighPart),
                    x: position.x,
                    y: position.y,
                }
            })
            .collect());
    }
    Err("the display configuration kept changing while it was read".into())
}

/// The hardware adapters DXGI knows, by LUID, so the log can name them.
pub fn adapter_names() -> Vec<AdapterName> {
    // SAFETY: creating a DXGI factory has no preconditions.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for index in 0.. {
        // SAFETY: enumeration ends with an error past the last adapter.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        // SAFETY: the adapter is valid.
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        names.push(AdapterName {
            luid: Luid::from_parts(desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
            name: String::from_utf16_lossy(&desc.Description[..len]),
        });
    }
    names
}
