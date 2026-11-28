//! The second zram device for memory the guest gains after boot.

use dsa_guest_channeld::zram_grow::{
    extra_mib, step, stored_bytes, swap_header, swapped_on, Step, ZRAM_EXTRA_PROPERTY, ZRAM_PERCENT,
};

const MIB: u64 = 1024 * 1024;

#[test]
fn nfr1_memory_added_after_boot_gets_a_second_zram_device() {
    // Booted with 2 GiB (zram0 half of it), grown to 4 GiB: zram1 swaps half of what was added.
    assert_eq!(extra_mib(4096 * 1024, 1024 * MIB), 1024);
    // Rounded down to 64 MiB steps: 1000 MiB added, a 500 MiB device, 448.
    assert_eq!(extra_mib((2048 + 1000) * 1024, 1024 * MIB), 448);
    assert_eq!(ZRAM_EXTRA_PROPERTY, "vendor.dsa.memory.zram_extra");
}

#[test]
fn nfr1_zram0_swaps_half_the_memory_at_boot_and_asks_for_no_second_device_then() {
    // fstab.dsa sizes zram0 as half of the memory at boot: the boot's own memory is covered,
    // and no zram1 is asked for until memory is added.
    assert_eq!(ZRAM_PERCENT, 50);
    assert_eq!(extra_mib(2048 * 1024, 1024 * MIB), 0);
    // MemTotal is a little under the VM's size, and fs_mgr sizes zram0 from it.
    assert_eq!(extra_mib(1_950_000, 1_950_000 * 1024 / 2), 0);
}

#[test]
fn nfr1_small_changes_in_memory_ask_for_no_device() {
    assert_eq!(extra_mib(2048 * 1024, 1024 * MIB), 0);
    assert_eq!(extra_mib((2048 + 255) * 1024, 1024 * MIB), 0);
    // A guest smaller than at boot asks for none.
    assert_eq!(extra_mib(1024 * 1024, 1024 * MIB), 0);
}

#[test]
fn nfr1_the_second_device_grows_and_goes_only_when_empty() {
    assert_eq!(step(0, 0, 0), Step::Leave);
    assert_eq!(step(512, 0, 0), Step::Make(512));
    // More memory again: remade larger, even with pages in it.
    assert_eq!(step(1024, 512, 100 * MIB), Step::Make(1024));
    // Memory gone: the device stays while it holds pages, goes once empty.
    assert_eq!(step(0, 512, 100 * MIB), Step::Leave);
    assert_eq!(step(0, 512, 0), Step::Remove);
    assert_eq!(step(256, 512, 0), Step::Make(256));
    assert_eq!(step(256, 512, 100 * MIB), Step::Leave);
}

#[test]
fn nfr2_zram1_goes_once_it_holds_nothing_and_is_asked_for_again_until_then() {
    use dsa_guest_channeld::zram_grow::{ask_again, EMPTY_BELOW_BYTES, REASK_INTERVAL};
    // The owner's PC, 1.0.1-dev.37: memory added at 5832 s went back 18 s later, and zram1 was
    // left with "it still holds 0 MiB of swapped pages", under a MiB. The daemon asked only when
    // its target changed, so zram1 stayed for the rest of the boot.
    assert_eq!(EMPTY_BELOW_BYTES, MIB);
    // Under a MiB is nothing: taking it off reads that little back.
    assert_eq!(step(0, 512, 600 * 1024), Step::Remove);
    assert_eq!(step(0, 512, MIB - 1), Step::Remove);
    assert_eq!(step(0, 512, MIB), Step::Leave);
    assert_eq!(step(256, 512, 600 * 1024), Step::Make(256));
    // The daemon asks again for a device larger than it needs once it is empty, and not while
    // it holds pages, nor for a device that is gone or the size it should be.
    assert!(ask_again(0, 512, 0));
    assert!(ask_again(0, 512, 600 * 1024));
    assert!(ask_again(256, 512, 0));
    assert!(!ask_again(0, 512, 100 * MIB));
    assert!(!ask_again(0, 0, 0));
    assert!(!ask_again(512, 512, 0));
    // A larger target is asked for when it changes, not again.
    assert!(!ask_again(1024, 512, 0));
    // It looks at most once a minute.
    assert_eq!(REASK_INTERVAL, std::time::Duration::from_secs(60));
}

#[test]
fn nfr1_the_second_device_has_a_version_1_swap_header() {
    let page = swap_header(4096, 512 * MIB);
    assert_eq!(page.len(), 4096);
    assert_eq!(&page[4086..], b"SWAPSPACE2");
    assert_eq!(u32::from_ne_bytes(page[1024..1028].try_into().unwrap()), 1);
    assert_eq!(
        u32::from_ne_bytes(page[1028..1032].try_into().unwrap()),
        (512 * MIB / 4096 - 1) as u32
    );
    assert_eq!(u32::from_ne_bytes(page[1032..1036].try_into().unwrap()), 0);
}

#[test]
fn nfr1_the_helper_reads_what_zram1_holds_and_whether_it_is_on() {
    assert_eq!(
        stored_bytes("  4096000  1000000  1200000 0 0 0 0 0 0\n"),
        4096000
    );
    assert_eq!(stored_bytes(""), 0);
    let swaps = "Filename\tType\tSize\tUsed\tPriority\n\
                 /dev/block/zram0 partition 2097148 0 -2\n\
                 /dev/block/zram1 partition 524284 0 -3\n";
    assert!(swapped_on(swaps, "/dev/block/zram1"));
    assert!(!swapped_on(swaps, "/dev/block/zram2"));
}
