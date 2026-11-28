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
        ] {
            assert_eq!(
                words.iter().filter(|w| *w == word).count(),
                1,
                "{word} for {arch:?}"
            );
        }
    }
}
