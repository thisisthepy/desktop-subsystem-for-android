//! The image's VHDX disks read into raw disks the Virtualization framework attaches.

use std::path::PathBuf;

use dsa_host_macos::vhdx::{convert_to_raw, is_vhdx, layout};

mod common;
use common::{small_vhdx, MIB};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("vhdx");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{}-{name}", std::process::id()))
}

#[test]
fn fr2_a_vhdx_disk_becomes_a_raw_disk_with_the_same_contents() {
    let vhdx = scratch("disk.vhdx");
    std::fs::write(&vhdx, small_vhdx(false)).unwrap();
    assert!(is_vhdx(&vhdx).unwrap());
    let raw = scratch("disk.img");
    convert_to_raw(&vhdx, &raw).unwrap();
    let disk = std::fs::read(&raw).unwrap();
    assert_eq!(disk.len(), 3 * MIB);
    assert!(disk[..MIB].iter().all(|&b| b == 0xa5));
    assert!(disk[MIB..2 * MIB].iter().all(|&b| b == 0));
    assert_eq!(disk[2 * MIB + 100], 0x5a);
    assert_eq!(disk[2 * MIB..].iter().filter(|&&b| b != 0).count(), 1);
    assert!(!is_vhdx(&raw).unwrap());
}

#[test]
fn fr2_the_layout_names_only_the_blocks_that_hold_data() {
    let file = small_vhdx(false);
    let found = layout(&mut std::io::Cursor::new(file)).unwrap();
    assert_eq!(found.virtual_size, 3 * MIB as u64);
    assert_eq!(found.block_size, MIB as u64);
    assert_eq!(found.logical_sector_size, 512);
    assert_eq!(
        found.blocks,
        vec![Some(3 * MIB as u64), None, Some(4 * MIB as u64)]
    );
}

#[test]
fn nfr5_a_vhdx_with_a_log_to_replay_or_no_signature_is_refused() {
    let with_log = small_vhdx(true);
    assert!(layout(&mut std::io::Cursor::new(with_log)).is_err());
    let mut unsigned = small_vhdx(false);
    unsigned[0] = b'X';
    assert!(layout(&mut std::io::Cursor::new(unsigned)).is_err());
    let truncated = small_vhdx(false)[..300 * 1024].to_vec();
    assert!(layout(&mut std::io::Cursor::new(truncated)).is_err());
}
