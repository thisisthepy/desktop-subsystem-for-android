//! The Linux kernel's bootconfig: `key = "value"` lines appended to the initrd.
//!
//! The kernel finds them by a trailer at the very end of the initrd and shows them in
//! `/proc/bootconfig`, which, unlike the command line, the guest's policy keeps from apps.
//! That is how the host hands the guest's control daemon the boot's secret.

/// The kernel command line word that makes the kernel read the bootconfig.
pub const CMDLINE_FLAG: &str = "bootconfig";

const MAGIC: &[u8] = b"#BOOTCONFIG\n";
const ALIGN: usize = 4;

/// `initrd` followed by `text` as bootconfig: the text, NUL-terminated and padded with NULs
/// to a multiple of four bytes, its size and checksum (little-endian 32-bit), and the magic.
pub fn append(initrd: &[u8], text: &str) -> Vec<u8> {
    let mut data = text.as_bytes().to_vec();
    data.push(0);
    data.resize(data.len().div_ceil(ALIGN) * ALIGN, 0);
    let checksum = data
        .iter()
        .fold(0u32, |sum, &byte| sum.wrapping_add(byte as u32));
    let mut out = Vec::with_capacity(initrd.len() + data.len() + 8 + MAGIC.len());
    out.extend_from_slice(initrd);
    out.extend_from_slice(&data);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&checksum.to_le_bytes());
    out.extend_from_slice(MAGIC);
    out
}
