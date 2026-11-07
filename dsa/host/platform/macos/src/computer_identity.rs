//! What this Mac says it is, for the device identity of the user's Android: its model
//! identifier (`hw.model`, such as `Mac15,3`), the name it is sold under from the I/O
//! Registry's device tree when the Mac has one there (such as
//! `MacBook Pro (14-inch, M3, Nov 2023)`), the processor (`machdep.cpu.brand_string`) and the
//! computer's name, as System Settings shows it under General, About.
//!
//! Read at every start of Android, so a renamed Mac is followed from the next start.

use dsa_host_core::computer_identity::ComputerInfo;

/// What this Mac says it is now.
pub fn read() -> ComputerInfo {
    let manufacturer = registry_text(&["-rd1", "-c", "IOPlatformExpertDevice"], "manufacturer");
    ComputerInfo {
        system_manufacturer: if manufacturer.is_empty() {
            "Apple Inc.".to_string()
        } else {
            manufacturer
        },
        system_product: sysctl_text("hw.model"),
        system_family: registry_text(
            &["-p", "IODeviceTree", "-rd1", "-n", "product"],
            "product-name",
        ),
        system_version: String::new(),
        baseboard_product: String::new(),
        computer_name: computer_name(),
        cpu_vendor: {
            let vendor = sysctl_text("machdep.cpu.vendor");
            if vendor.is_empty() {
                "Apple".to_string()
            } else {
                vendor
            }
        },
        cpu_name: sysctl_text("machdep.cpu.brand_string"),
    }
}

/// The value of `key` in what `ioreg <args>` prints, or empty.
fn registry_text(args: &[&str], key: &str) -> String {
    std::process::Command::new("/usr/sbin/ioreg")
        .args(args)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| ioreg_value(&String::from_utf8_lossy(&out.stdout), key))
        .unwrap_or_default()
}

/// The string value of `key` in `ioreg` output, where a line reads `"key" = <"value">` (data)
/// or `"key" = "value"` (a string). Data may end with a zero byte, which goes.
pub fn ioreg_value(output: &str, key: &str) -> String {
    let wanted = format!("\"{key}\" = ");
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(&wanted).map(str::to_string))
        .map(|value| {
            let value = value.trim();
            let value = value
                .strip_prefix('<')
                .and_then(|v| v.strip_suffix('>'))
                .unwrap_or(value);
            value
                .trim_matches('"')
                .trim_end_matches('\0')
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// The computer's name, as System Settings shows it.
fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A string sysctl, or empty when there is none.
#[cfg(target_os = "macos")]
fn sysctl_text(name: &str) -> String {
    let Ok(name) = std::ffi::CString::new(name) else {
        return String::new();
    };
    let mut buffer = vec![0u8; 256];
    let mut len = buffer.len();
    // SAFETY: the buffer is `len` bytes long, and the call writes at most that many.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return String::new();
    }
    buffer.truncate(len.min(buffer.len()));
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).trim().to_string()
}

#[cfg(not(target_os = "macos"))]
fn sysctl_text(_name: &str) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr17_the_macs_marketing_name_is_read_from_the_device_tree() {
        let product = "+-o product  <class IOPlatformDevice, id 0x100000110>\n    {\n      \
                       \"product-name\" = <\"MacBook Pro (14-inch, M3, Nov 2023)\">\n      \
                       \"compatible\" = <\"product\">\n    }\n";
        assert_eq!(
            ioreg_value(product, "product-name"),
            "MacBook Pro (14-inch, M3, Nov 2023)"
        );
        let platform = "    \"manufacturer\" = <\"Apple Inc.\">\n    \"model\" = <\"Mac15,3\">\n";
        assert_eq!(ioreg_value(platform, "manufacturer"), "Apple Inc.");
        assert_eq!(ioreg_value(platform, "board-id"), "");

        let identity = ComputerInfo {
            system_manufacturer: "Apple Inc.".into(),
            system_product: "Mac15,3".into(),
            system_family: "MacBook Pro (14-inch, M3, Nov 2023)".into(),
            computer_name: "BREW's MacBook Pro".into(),
            cpu_vendor: "Apple".into(),
            cpu_name: "Apple M3".into(),
            ..ComputerInfo::default()
        }
        .identity();
        assert_eq!(identity.manufacturer.value, "Apple");
        assert_eq!(identity.model.value, "Mac15,3");
        assert_eq!(
            identity.model_name.value,
            "MacBook Pro (14-inch, M3, Nov 2023)"
        );
        assert_eq!(identity.device_name.value, "BREW's MacBook Pro");
        assert_eq!(identity.soc_manufacturer.value, "Apple");
        assert_eq!(identity.soc_model.value, "M3");
    }
}
