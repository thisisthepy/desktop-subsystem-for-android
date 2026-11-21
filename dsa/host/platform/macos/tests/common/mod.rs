//! A small VHDX for the tests that read or convert one.

use dsa_host_macos::vhdx::{
    guid_bytes, BAT_REGION, FILE_PARAMETERS, LOGICAL_SECTOR_SIZE, METADATA_REGION,
    VIRTUAL_DISK_SIZE,
};

pub const MIB: usize = 1 << 20;

pub fn put(file: &mut [u8], at: usize, bytes: &[u8]) {
    file[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A dynamic VHDX of three 1 MiB blocks: the first and last hold data, the middle one was
/// never written.
pub fn small_vhdx(log: bool) -> Vec<u8> {
    let mut file = vec![0u8; 5 * MIB];
    put(&mut file, 0, b"vhdxfile");
    let header = 64 * 1024;
    put(&mut file, header, b"head");
    put(&mut file, header + 8, &1u64.to_le_bytes());
    if log {
        put(&mut file, header + 48, &[1u8; 16]);
    }
    let regions = 192 * 1024;
    put(&mut file, regions, b"regi");
    put(&mut file, regions + 8, &2u32.to_le_bytes());
    for (i, (id, at)) in [(BAT_REGION, MIB), (METADATA_REGION, 2 * MIB)]
        .into_iter()
        .enumerate()
    {
        let entry = regions + 16 + i * 32;
        put(&mut file, entry, &guid_bytes(id));
        put(&mut file, entry + 16, &(at as u64).to_le_bytes());
        put(&mut file, entry + 24, &(MIB as u32).to_le_bytes());
    }
    let meta = 2 * MIB;
    put(&mut file, meta, b"metadata");
    put(&mut file, meta + 10, &3u16.to_le_bytes());
    let items: [(&str, Vec<u8>); 3] = [
        (
            FILE_PARAMETERS,
            [(MIB as u32).to_le_bytes(), 0u32.to_le_bytes()].concat(),
        ),
        (VIRTUAL_DISK_SIZE, (3 * MIB as u64).to_le_bytes().to_vec()),
        (LOGICAL_SECTOR_SIZE, 512u32.to_le_bytes().to_vec()),
    ];
    for (i, (id, value)) in items.iter().enumerate() {
        let entry = meta + 32 + i * 32;
        let offset = 64 * 1024 + i * 64;
        put(&mut file, entry, &guid_bytes(id));
        put(&mut file, entry + 16, &(offset as u32).to_le_bytes());
        put(&mut file, entry + 20, &(value.len() as u32).to_le_bytes());
        put(&mut file, meta + offset, value);
    }
    let bat = MIB;
    put(&mut file, bat, &(6u64 | (3u64 << 20)).to_le_bytes());
    put(&mut file, bat + 16, &(6u64 | (4u64 << 20)).to_le_bytes());
    file[3 * MIB..4 * MIB].fill(0xa5);
    file[4 * MIB + 100] = 0x5a;
    file
}
