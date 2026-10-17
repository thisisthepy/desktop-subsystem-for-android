//! Copying Android's partitions into a user's disk when the image changes.

use std::io::{Cursor, Seek, SeekFrom, Write};

use dsa_host_windows::gpt::{copy_partitions, is_blank, partitions, SECTOR};

/// A disk image with a GPT holding `parts` (name, sectors), laid out one after another
/// from sector 2048, each filled with `fill`.
fn disk(parts: &[(&str, u64)], fill: u8) -> Cursor<Vec<u8>> {
    let total: u64 = 2048 + parts.iter().map(|p| p.1).sum::<u64>() + 34;
    let mut d = Cursor::new(vec![0u8; (total * SECTOR) as usize]);
    let mut header = vec![0u8; 512];
    header[0..8].copy_from_slice(b"EFI PART");
    header[72..80].copy_from_slice(&2u64.to_le_bytes());
    header[80..84].copy_from_slice(&128u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());
    d.seek(SeekFrom::Start(SECTOR)).unwrap();
    d.write_all(&header).unwrap();
    let mut lba: u64 = 2048;
    for (i, (name, sectors)) in parts.iter().enumerate() {
        let mut entry = vec![0u8; 128];
        entry[0] = 1; // a non-zero type GUID
        entry[16] = i as u8 + 1;
        entry[32..40].copy_from_slice(&lba.to_le_bytes());
        entry[40..48].copy_from_slice(&(lba + sectors - 1).to_le_bytes());
        for (j, u) in name.encode_utf16().enumerate() {
            entry[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
        d.seek(SeekFrom::Start(2 * SECTOR + i as u64 * 128))
            .unwrap();
        d.write_all(&entry).unwrap();
        d.seek(SeekFrom::Start(lba * SECTOR)).unwrap();
        d.write_all(&vec![fill; (sectors * SECTOR) as usize])
            .unwrap();
        lba += sectors;
    }
    d
}

fn content(d: &mut Cursor<Vec<u8>>, name: &str) -> Vec<u8> {
    let p = partitions(d)
        .unwrap()
        .into_iter()
        .find(|p| p.name == name)
        .unwrap();
    d.get_ref()[p.start as usize..(p.start + p.len) as usize].to_vec()
}

const LAYOUT: [(&str, u64); 5] = [
    ("misc", 8),
    ("metadata", 64),
    ("vbmeta_a", 8),
    ("super", 4096),
    ("userdata", 1024),
];

#[test]
fn fr14_partitions_are_found_by_name() {
    let mut d = disk(&LAYOUT, 0);
    let names: Vec<String> = partitions(&mut d)
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["misc", "metadata", "vbmeta_a", "super", "userdata"]);
    let sup = partitions(&mut d)
        .unwrap()
        .into_iter()
        .find(|p| p.name == "super")
        .unwrap();
    assert_eq!(sup.len, 4096 * SECTOR);
}

#[test]
fn fr14_a_new_image_replaces_android_and_keeps_the_users_data() {
    let mut template = disk(&LAYOUT, 0xAA);
    let mut user = disk(&LAYOUT, 0x11);
    copy_partitions(&mut template, &mut user, &["super", "vbmeta_a"]).unwrap();
    assert!(
        content(&mut user, "super").iter().all(|&b| b == 0xAA),
        "Android is replaced"
    );
    assert!(content(&mut user, "vbmeta_a").iter().all(|&b| b == 0xAA));
    for kept in ["userdata", "metadata", "misc"] {
        assert!(
            content(&mut user, kept).iter().all(|&b| b == 0x11),
            "{kept} is the user's and stays"
        );
    }
}

#[test]
fn fr14_a_larger_partition_than_the_new_image_needs_is_zero_filled_and_a_smaller_one_refused() {
    let mut small = disk(&[("misc", 8), ("super", 2048)], 0xAA);
    let mut big = disk(&[("misc", 8), ("super", 4096)], 0x11);
    copy_partitions(&mut small, &mut big, &["super"]).unwrap();
    let sup = content(&mut big, "super");
    assert!(sup[..2048 * 512].iter().all(|&b| b == 0xAA));
    assert!(sup[2048 * 512..].iter().all(|&b| b == 0));
    let mut big2 = disk(&[("misc", 8), ("super", 4096)], 0x11);
    let mut small2 = disk(&[("misc", 8), ("super", 2048)], 0x22);
    assert!(copy_partitions(&mut big2, &mut small2, &["super"]).is_err());
}

#[test]
fn fr14_a_disk_without_a_partition_table_is_refused() {
    let mut blank = Cursor::new(vec![0u8; 1 << 20]);
    assert!(partitions(&mut blank).is_err());
}

#[test]
fn fr14_a_disk_nothing_was_written_to_is_blank_and_one_with_a_table_is_not() {
    let mut empty = Cursor::new(vec![0u8; 4 << 20]);
    assert!(is_blank(&mut empty).unwrap());
    let mut formatted = Cursor::new(vec![0u8; 4 << 20]);
    // A file system written straight onto the disk, as the guest does with an empty one.
    formatted.get_mut()[1024 + 56..1024 + 58].copy_from_slice(&[0x53, 0xef]);
    assert!(!is_blank(&mut formatted).unwrap());
    assert!(!is_blank(&mut disk(&[("misc", 2048)], 0)).unwrap());
}
