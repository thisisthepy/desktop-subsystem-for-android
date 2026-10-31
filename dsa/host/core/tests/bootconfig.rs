//! Handing the guest kernel a bootconfig appended to its initrd.

use dsa_host_core::bootconfig::{append, CMDLINE_FLAG};
use dsa_protocol::auth::{bootconfig_entry, secret_from_bootconfig, BootSecret};

/// Reads the trailer the kernel looks for at the end of the initrd, the way the kernel
/// does: magic, then the checksum and the size before it, then the data before those.
fn kernel_reads(initrd: &[u8]) -> Option<(usize, String)> {
    let magic = b"#BOOTCONFIG\n";
    let end = initrd.len().checked_sub(magic.len())?;
    if &initrd[end..] != magic {
        return None;
    }
    let size = u32::from_le_bytes(initrd[end - 8..end - 4].try_into().unwrap()) as usize;
    let checksum = u32::from_le_bytes(initrd[end - 4..end].try_into().unwrap());
    let start = end - 8 - size;
    let data = &initrd[start..end - 8];
    let sum = data.iter().fold(0u32, |sum, &b| sum.wrapping_add(b as u32));
    assert_eq!(sum, checksum, "checksum");
    assert_eq!(size % 4, 0, "the data is padded to 4 bytes");
    let text = std::str::from_utf8(data).unwrap().trim_end_matches('\0');
    Some((start, text.to_string()))
}

#[test]
fn pr6_the_secret_reaches_the_guest_kernel_as_bootconfig() {
    let initrd = b"\x1f\x8b compressed ramdisk".to_vec();
    let secret = BootSecret::from_bytes([0xab; 32]);
    let with_config = append(&initrd, &bootconfig_entry(&secret));

    let (start, text) = kernel_reads(&with_config).expect("a bootconfig trailer");
    // The ramdisk itself is untouched, so the kernel unpacks the same files.
    assert_eq!(start, initrd.len());
    assert_eq!(&with_config[..start], &initrd[..]);
    // What the guest later reads back from /proc/bootconfig.
    assert_eq!(secret_from_bootconfig(&text), Some(secret));
    // The kernel only reads a bootconfig when its command line asks for one.
    assert_eq!(CMDLINE_FLAG, "bootconfig");
}

#[test]
fn pr6_padding_keeps_the_trailer_aligned_for_any_length() {
    for extra in 0..8 {
        let text = format!("dsa.x = \"{}\"\n", "y".repeat(extra));
        let (_, read) = kernel_reads(&append(b"rd", &text)).expect("a trailer");
        assert_eq!(read, text);
    }
}
