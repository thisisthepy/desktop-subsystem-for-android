//! The graphics memory the host shares with the guest: what the service makes for it, before
//! the renderer hands it out.
//!
//! Two regions, both attached to the VM when it is made: the data region, which the guest may
//! write, where Vulkan host-visible memory and gralloc buffers' staging copies live, and the
//! grants table, which the guest may only read, where the renderer says which part of the data
//! region each guest process may map. The renderer's allocator and the guest kernel's driver
//! keep the rest of the layout (`dsa/graphics/include/dsa_graphics_memory.h`); the service only
//! sizes the regions and writes the table's header, and a test keeps the constants here in step
//! with that header.
//!
//! The guest learns where the regions are on the control channel (`GuestSharedMemory`, named
//! [`GRANTS_NAME`] and [`DATA_NAME`]), once both are known.

/// The name the grants table goes by in `GuestSharedMemory`.
pub const GRANTS_NAME: &str = "graphics_grants";

/// The name the data region goes by in `GuestSharedMemory`.
pub const DATA_NAME: &str = "graphics_memory";

/// The unit the data region is handed out in.
pub const GRANULE: u64 = 64 * 1024;

/// The first eight bytes of the grants table.
pub const GRANTS_MAGIC: [u8; 8] = *b"DSAGRNT1";

/// The table's header, then one entry of [`GRANT_ENTRY_BYTES`] for each granule.
pub const GRANTS_HEADER_BYTES: u64 = 64;
pub const GRANT_ENTRY_BYTES: u64 = 32;

/// The data region's size when the settings say nothing: room for a game's staging buffers.
/// Its pages take memory only once the guest or the GPU writes them.
pub const DEFAULT_DATA_BYTES: u64 = 512 * 1024 * 1024;

/// The smallest data region worth making: its release slots and 16 MiB, the block a guest
/// driver allocates host-visible memory in.
pub const MIN_DATA_BYTES: u64 = GRANULE + 16 * 1024 * 1024;

/// The granularity HCS maps shared memory in.
pub const HCS_GRANULARITY: u64 = 2 * 1024 * 1024;

/// The release slots at the start of the data region, which are never handed out: one u64 for
/// each granule, in whole granules.
pub fn ack_bytes(data_len: u64) -> u64 {
    (data_len / GRANULE * 8).div_ceil(GRANULE) * GRANULE
}

/// The size of the grants table for a data region of `data_len`, in whole HCS units.
pub fn grants_bytes(data_len: u64) -> u64 {
    (GRANTS_HEADER_BYTES + data_len / GRANULE * GRANT_ENTRY_BYTES).div_ceil(HCS_GRANULARITY)
        * HCS_GRANULARITY
}

/// The data region's size for a setting of `mb` MiB, or `None` when it is off (0) or too small
/// to be worth it. `None` for `mb` gives the default. Whole HCS units.
pub fn data_bytes(mb: Option<u64>) -> Option<u64> {
    let length = match mb {
        Some(0) => return None,
        Some(mb) => mb.checked_mul(1024 * 1024)?,
        None => DEFAULT_DATA_BYTES,
    };
    if length < MIN_DATA_BYTES {
        return None;
    }
    Some(length.div_ceil(HCS_GRANULARITY) * HCS_GRANULARITY)
}

/// Writes the grants table's header into `table` (the start of the section, zero after the
/// header) for a data region of `data_len` with the id `id`. False when `table` is too short.
pub fn write_grants_header(table: &mut [u8], id: u64, data_len: u64) -> bool {
    if (table.len() as u64) < GRANTS_HEADER_BYTES {
        return false;
    }
    table[..8].copy_from_slice(&GRANTS_MAGIC);
    let fields = [id, data_len, GRANULE, ack_bytes(data_len), 0, 1];
    for (i, value) in fields.iter().enumerate() {
        let at = 8 + i * 8;
        table[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    table[56..64].fill(0);
    true
}

/// The value `vendor.dsa.graphics_memory` and the kernel's parameter take once both regions are
/// known: `0x<grants address>,<grants length>,0x<data address>,<data length>`, the addresses as
/// the host reported them.
pub fn kernel_parameter(grants: (u64, u64), data: (u64, u64)) -> Option<String> {
    if grants.0 == 0 || grants.1 == 0 || data.0 == 0 || data.1 == 0 {
        return None;
    }
    Some(format!(
        "{:#x},{},{:#x},{}",
        grants.0, grants.1, data.0, data.1
    ))
}
