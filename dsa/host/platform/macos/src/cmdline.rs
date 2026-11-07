//! The kernel command line of a VM on Apple's Virtualization framework.
//!
//! An image's command line is written for Hyper-V: its console is a UART (`ttyS0` or
//! `ttyAMA0`), and Android finds its partitions on the disks below VMBus
//! (`androidboot.boot_devices=vmbus`). On the framework the console is a virtio console
//! (`hvc0`) and every disk is a virtio-blk device on PCI bus 0. Android's first stage init
//! names a PCI device `pci0000:00/0000:00:<slot>.0`, and the framework places its devices
//! on slots it chooses, so every slot of the bus is named: the by-name links are made for
//! the disks wherever they sit, and only disks are looked at.
//!
//! Everything else the image says is kept, in its order.

/// The console of the framework's VM: the first virtio console port.
pub const CONSOLE: &str = "console=hvc0";

/// Free page reporting of every free 4 KiB page, so memory the guest frees goes back at once
/// whenever the VM's balloon offers reporting, without waiting for whole 2 MiB blocks.
pub const PAGE_REPORTING: &str = "page_reporting.page_reporting_order=0";

/// The slots a PCI bus has.
const PCI_SLOTS: u32 = 32;

/// `androidboot.boot_devices` for every slot of PCI bus 0.
pub fn boot_devices() -> String {
    let slots: Vec<String> = (1..PCI_SLOTS)
        .map(|slot| format!("pci0000:00/0000:00:{slot:02x}.0"))
        .collect();
    format!("androidboot.boot_devices={}", slots.join(","))
}

/// Whether `word` is a setting the macOS host replaces.
fn replaced(word: &str) -> bool {
    const KEYS: [&str; 7] = [
        "console=",
        "earlycon=",
        "earlycon",
        "8250.nr_uarts=",
        "androidboot.boot_devices=",
        "panic=",
        "page_reporting.page_reporting_order=",
    ];
    KEYS.iter()
        .any(|key| word == *key || (key.ends_with('=') && word.starts_with(key)))
        || word.starts_with("pstore_blk.")
}

/// The command line for the framework's VM, from the image's command line and the host's
/// own settings `extra`. With `pstore`, the kernel keeps its last log on the pstore disk.
pub fn for_virtualization_framework(image: &str, extra: &str, pstore: bool) -> String {
    let mut words = vec![
        CONSOLE.to_string(),
        "panic=-1".to_string(),
        PAGE_REPORTING.to_string(),
        boot_devices(),
    ];
    if pstore {
        words.extend(
            crate::pstore_disk::KERNEL_CMDLINE
                .split_whitespace()
                .map(str::to_string),
        );
    }
    words.extend(
        image
            .split_whitespace()
            .chain(extra.split_whitespace())
            .filter(|word| !replaced(word))
            .map(str::to_string),
    );
    // The boot's secret is bootconfig appended to the initrd, and the kernel reads that only
    // when the command line says so: without it the guest's daemon finds no secret.
    let flag = dsa_host_core::bootconfig::CMDLINE_FLAG;
    if !words.iter().any(|word| word == flag) {
        words.push(flag.to_string());
    }
    words.join(" ")
}
