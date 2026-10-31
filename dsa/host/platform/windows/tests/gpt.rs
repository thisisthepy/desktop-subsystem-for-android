//! Copying Android's partitions into a user's disk when the image changes.

use std::io::{Cursor, Seek, SeekFrom, Write};

use dsa_host_windows::gpt::{
    copy_partitions, fits, is_blank, keep_only, partition_is_blank, partitions, SECTOR,
};

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

#[test]
fn fr14_a_disk_without_a_partition_the_new_image_has_does_not_fit_it() {
    let old = &LAYOUT;
    let new = [
        ("misc", 8),
        ("metadata", 64),
        ("vbmeta_a", 8),
        ("boot_a", 64),
        ("super", 4096),
        ("userdata", 1024),
    ];
    let names = ["super", "vbmeta_a", "boot_a"];
    assert!(!fits(&mut disk(&new, 0), &mut disk(old, 0), &names).unwrap());
    assert!(fits(&mut disk(&new, 0), &mut disk(&new, 0), &names).unwrap());
    assert!(fits(&mut disk(old, 0), &mut disk(&new, 0), &names).unwrap());
}

#[test]
fn fr14_a_partition_android_never_wrote_to_is_blank() {
    let mut d = disk(&LAYOUT, 0);
    assert!(partition_is_blank(&mut d, "userdata").unwrap());
    let mut written = disk(&LAYOUT, 0x5a);
    assert!(!partition_is_blank(&mut written, "userdata").unwrap());
    assert!(partition_is_blank(&mut d, "nonexistent").is_err());
}

#[test]
fn nfr5_a_corrupted_partition_table_does_not_panic() {
    let mut d = disk(&[("corrupt", 8)], 0);
    // Overwrite the first entry's last sector (bytes 40..48) with a value smaller than first sector (2048).
    d.seek(SeekFrom::Start(2 * SECTOR + 40)).unwrap();
    d.write_all(&1000u64.to_le_bytes()).unwrap();
    let result = partitions(&mut d);
    assert!(result.is_err());

    let mut d = disk(&[("overflow", 8)], 0);
    d.seek(SeekFrom::Start(SECTOR + 72)).unwrap();
    d.write_all(&u64::MAX.to_le_bytes()).unwrap();
    let result = partitions(&mut d);
    assert!(result.is_err());
}

/// CRC-32 as the GPT uses it (IEEE, reflected), written out here so the test does not
/// trust the code it checks.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Like `disk`, but a complete table as real tools write it: header size, both CRCs and
/// the backup header and entries at the end of the disk.
fn full_disk(parts: &[(&str, u64)], fill: u8) -> Cursor<Vec<u8>> {
    let mut d = disk(parts, fill);
    let total = d.get_ref().len() as u64 / SECTOR;
    let last = total - 1;
    let entries = d.get_ref()[(2 * SECTOR) as usize..(34 * SECTOR) as usize].to_vec();
    let mut header = d.get_ref()[SECTOR as usize..(2 * SECTOR) as usize].to_vec();
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&1u64.to_le_bytes());
    header[32..40].copy_from_slice(&last.to_le_bytes());
    header[88..92].copy_from_slice(&crc32(&entries[..128 * 128]).to_le_bytes());
    let mut backup = header.clone();
    backup[24..32].copy_from_slice(&last.to_le_bytes());
    backup[32..40].copy_from_slice(&1u64.to_le_bytes());
    backup[72..80].copy_from_slice(&(last - 32).to_le_bytes());
    for h in [&mut header, &mut backup] {
        h[16..20].fill(0);
        let crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
    }
    let buf = d.get_mut();
    buf[SECTOR as usize..(2 * SECTOR) as usize].copy_from_slice(&header);
    buf[((last - 32) * SECTOR) as usize..(last * SECTOR) as usize].copy_from_slice(&entries);
    buf[(last * SECTOR) as usize..((last + 1) * SECTOR) as usize].copy_from_slice(&backup);
    d
}

/// Checks a header at `lba` and its entries the way the Linux kernel does.
fn assert_table_is_valid(d: &Cursor<Vec<u8>>, lba: u64) {
    let buf = d.get_ref();
    let mut header = buf[(lba * SECTOR) as usize..((lba + 1) * SECTOR) as usize].to_vec();
    assert_eq!(&header[0..8], b"EFI PART", "a header at sector {lba}");
    let stored = u32::from_le_bytes(header[16..20].try_into().unwrap());
    header[16..20].fill(0);
    assert_eq!(crc32(&header[..92]), stored, "header CRC at sector {lba}");
    let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
    let start = (entries_lba * SECTOR) as usize;
    let entries = &buf[start..start + 128 * 128];
    let stored = u32::from_le_bytes(header[88..92].try_into().unwrap());
    assert_eq!(
        crc32(entries),
        stored,
        "entries CRC of the header at sector {lba}"
    );
}

#[test]
fn fr14_partitions_an_image_no_longer_has_are_dropped_and_the_data_stays() {
    let mut d = full_disk(&LAYOUT, 0);
    let before = partitions(&mut d).unwrap();
    for (name, byte) in [("metadata", 0x4d), ("userdata", 0x75)] {
        let p = before.iter().find(|p| p.name == name).unwrap();
        d.seek(SeekFrom::Start(p.start)).unwrap();
        d.write_all(&vec![byte; p.len as usize]).unwrap();
    }

    let dropped = keep_only(&mut d, &["metadata", "userdata"]).unwrap();

    assert_eq!(dropped, ["misc", "vbmeta_a", "super"]);
    let after = partitions(&mut d).unwrap();
    let kept: Vec<_> = before
        .iter()
        .filter(|p| p.name == "metadata" || p.name == "userdata")
        .cloned()
        .collect();
    assert_eq!(after, kept, "the kept partitions stay where they were");
    assert!(content(&mut d, "metadata").iter().all(|&b| b == 0x4d));
    assert!(content(&mut d, "userdata").iter().all(|&b| b == 0x75));
    let last = d.get_ref().len() as u64 / SECTOR - 1;
    assert_table_is_valid(&d, 1);
    assert_table_is_valid(&d, last);
}

#[test]
fn fr14_a_disk_that_already_has_only_the_kept_partitions_is_left_as_it_is() {
    let mut d = full_disk(&[("metadata", 64), ("userdata", 1024)], 0x33);
    let before = d.get_ref().clone();
    assert!(keep_only(&mut d, &["metadata", "userdata"])
        .unwrap()
        .is_empty());
    assert!(d.get_ref() == &before, "nothing is written");
}

#[test]
fn nfr5_dropping_partitions_from_a_disk_without_a_table_is_refused() {
    let mut d = Cursor::new(vec![0u8; 64 * SECTOR as usize]);
    assert!(keep_only(&mut d, &["userdata"]).is_err());
}
