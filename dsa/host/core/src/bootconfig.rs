//! The Linux kernel's bootconfig: `key = "value"` lines appended to the initrd.
//!
//! The kernel finds them by a trailer at the very end of the initrd and shows them in
//! `/proc/bootconfig`, which, unlike the command line, the guest's policy keeps from apps.
//! That is how the host hands the guest's control daemon the boot's secret.

use std::path::Path;

use crate::addon_disk::{self, Addon};
use crate::host_facts::HostFacts;
use crate::settings::SettingsStore;

/// The kernel command line word that makes the kernel read the bootconfig.
pub const CMDLINE_FLAG: &str = "bootconfig";

const MAGIC: &[u8] = b"#BOOTCONFIG\n";
const ALIGN: usize = 4;

/// The bootconfig text of one boot of `user`'s Android, which every host builds the same way:
/// what the add-on disk carries (`attached` is what the boot attaches of it), the ARM
/// translator (the package the user installed, else the default one of the image in
/// `image_dir`) while the user's setting is on, the Google apps and Widevine the user installed
/// while their images are on the add-on disk, the memory the VM may grow to (`vm_max_mb`),
/// which apps see as the device's memory, and the computer's physical memory and the memory it
/// is installed with (both read from `facts` at each start), which Google Play sees as the
/// device's memory and the advertised memory.
pub fn for_boot(
    store: &dyn SettingsStore,
    user: &str,
    image_dir: Option<&Path>,
    attached: &[Addon],
    facts: &dyn HostFacts,
    vm_max_mb: Option<u64>,
) -> String {
    addon_disk::bootconfig(attached).to_string()
        + &crate::arm_translation::bootconfig_for(store, user, image_dir, attached)
        + &crate::gapps::bootconfig_for(store, user, attached)
        + &crate::widevine::bootconfig_for(store, user, attached)
        + &crate::host_memory::bootconfig(
            facts.physical_memory_kb(),
            vm_max_mb,
            facts.installed_memory_kb(),
        )
}

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
