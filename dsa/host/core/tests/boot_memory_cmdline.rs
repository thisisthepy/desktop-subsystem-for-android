//! What the guest kernel sets aside at boot, by the guest's architecture: the command line
//! words every host gives it.

use dsa_host_core::image_files::GuestArch;
use dsa_host_core::memory::guest_boot_memory_cmdline;

fn words(arch: GuestArch) -> Vec<String> {
    guest_boot_memory_cmdline(arch)
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn swiotlb(arch: GuestArch) -> Vec<String> {
    words(arch)
        .into_iter()
        .filter(|word| word.starts_with("swiotlb="))
        .collect()
}

#[test]
fn nfr1_an_x86_64_guest_makes_no_bounce_buffer() {
    // The x86_64 VMs' devices (VMBus on Hyper-V, virtio under crosvm and Apple's framework)
    // reach all of memory, and nothing else on x86_64 needs the buffer, so none is made.
    assert_eq!(swiotlb(GuestArch::X86_64), ["swiotlb=noforce"]);
}

#[test]
fn nfr1_an_arm64_guest_keeps_a_small_bounce_buffer() {
    // An arm64 kernel lets small allocations skip the 128-byte cache line alignment only while
    // a bounce buffer exists; without one every kmalloc would round up to 128 bytes.
    assert_eq!(swiotlb(GuestArch::Arm64), ["swiotlb=2048"]);
}

fn value_of(arch: GuestArch, key: &str) -> Vec<String> {
    words(arch)
        .into_iter()
        .filter_map(|word| word.strip_prefix(&format!("{key}=")).map(str::to_string))
        .collect()
}

#[test]
fn nfr1_slab_caches_of_large_objects_use_one_page_slabs() {
    // dev.39 with no app open: task_struct (4032 bytes) and kmalloc-4k sat in 32 KiB slabs of 8
    // objects, and a slab is freed only when all 8 are, so 5440 KiB held 867 tasks (3414 KiB)
    // and 5280 KiB held 685 kmalloc-4k objects (2740 KiB). The kernel picks a slab's order
    // from a minimum object count (16 on 4 processors); with 1 it picks the smallest order
    // that wastes at most a sixteenth, as before, so these get one page and one object each.
    for arch in [GuestArch::X86_64, GuestArch::Arm64] {
        assert_eq!(value_of(arch, "slab_min_objects"), ["1"], "{arch:?}");
        // The largest order stays the kernel's own, so no object loses a slab that fits it.
        assert!(value_of(arch, "slab_max_order").is_empty(), "{arch:?}");
        assert!(value_of(arch, "slub_max_order").is_empty(), "{arch:?}");
    }
}

#[test]
fn nfr1_loop_devices_keep_a_desktop_sized_request_pool() {
    // apexd keeps 64 loop devices (dev.39's console log), each with a pool of 128 requests
    // allocated at once whether the device is used or not (a 64 KiB block each). 32 requests
    // in flight still cover an APEX's reads.
    for arch in [GuestArch::X86_64, GuestArch::Arm64] {
        assert_eq!(value_of(arch, "loop.hw_queue_depth"), ["32"], "{arch:?}");
    }
}

#[test]
fn nfr1_both_architectures_size_the_rest_of_the_boot_memory_alike() {
    for arch in [GuestArch::X86_64, GuestArch::Arm64] {
        let words = words(arch);
        for word in [
            "kfence.sample_interval=0",
            "dhash_entries=65536",
            "ihash_entries=32768",
            "thash_entries=16384",
            "uhash_entries=1024",
            "slab_min_objects=1",
            "loop.hw_queue_depth=32",
        ] {
            assert_eq!(
                words.iter().filter(|w| *w == word).count(),
                1,
                "{word} for {arch:?}"
            );
        }
    }
}
