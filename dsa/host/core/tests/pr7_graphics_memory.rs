//! What the service makes for the graphics memory it shares with the guest: the regions'
//! sizes, the grants table's header, and the kernel parameter the guest gets.

use dsa_host_core::graphics_memory::{
    ack_bytes, data_bytes, grants_bytes, kernel_parameter, write_grants_header, DATA_NAME,
    DEFAULT_DATA_BYTES, GRANTS_MAGIC, GRANTS_NAME, GRANULE,
};

const MIB: u64 = 1024 * 1024;

#[test]
fn pr7_the_graphics_region_is_256_mib_unless_set_and_0_turns_it_off() {
    assert_eq!(data_bytes(None), Some(DEFAULT_DATA_BYTES));
    assert_eq!(DEFAULT_DATA_BYTES, 512 * MIB);
    assert_eq!(data_bytes(Some(0)), None);
    assert_eq!(data_bytes(Some(512)), Some(512 * MIB));
    // Whole 2 MiB, which HCS maps memory in.
    assert_eq!(data_bytes(Some(33)), Some(34 * MIB));
    // Too small to hold the release slots and one 16 MiB block.
    assert_eq!(data_bytes(Some(8)), None);
    assert_eq!(data_bytes(Some(u64::MAX)), None);
}

#[test]
fn pr7_the_grants_table_has_an_entry_for_each_granule() {
    // 4096 granules of 64 KiB, 32 bytes each, after a 64 byte header: one 2 MiB unit.
    assert_eq!(grants_bytes(256 * MIB), 2 * MIB);
    // 131072 entries are 4 MiB, and the header needs a third unit.
    assert_eq!(grants_bytes(8192 * MIB), 6 * MIB);
    assert_eq!(ack_bytes(256 * MIB), GRANULE);
    assert_eq!(ack_bytes(1024 * MIB), 2 * GRANULE);
}

#[test]
fn pr7_the_grants_header_is_the_one_the_renderer_and_the_kernel_read() {
    let mut table = vec![0xFFu8; 128];
    assert!(write_grants_header(&mut table, 0x1234, 256 * MIB));
    assert_eq!(&table[..8], &GRANTS_MAGIC);
    let field = |at: usize| u64::from_le_bytes(table[at..at + 8].try_into().unwrap());
    assert_eq!(field(8), 0x1234);
    assert_eq!(field(16), 256 * MIB);
    assert_eq!(field(24), GRANULE);
    assert_eq!(field(32), GRANULE);
    // No renderer has taken it over yet, and sequence numbers start at 1.
    assert_eq!(field(40), 0);
    assert_eq!(field(48), 1);
    assert!(!write_grants_header(&mut [0u8; 16], 1, 256 * MIB));
    // The renderer's and the guest kernel's layout.
    let header = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../graphics/include/dsa_graphics_memory.h"),
    )
    .unwrap();
    for line in [
        "inline constexpr uint64_t kGraphicsGranule = 64 * 1024;",
        "inline constexpr uint64_t kGrantsHeaderBytes = 64;",
        "inline constexpr uint64_t kGrantsRegionIdAt = 8;",
        "inline constexpr uint64_t kGrantsDataLengthAt = 16;",
        "inline constexpr uint64_t kGrantsGranuleAt = 24;",
        "inline constexpr uint64_t kGrantsAckBytesAt = 32;",
        "inline constexpr uint64_t kGrantsGenerationAt = 40;",
        "inline constexpr uint64_t kGrantsNextSeqAt = 48;",
    ] {
        assert!(header.contains(line), "{line}");
    }
}

#[test]
fn pr7_the_guest_hears_both_regions_in_one_kernel_parameter() {
    assert_eq!(GRANTS_NAME, "graphics_grants");
    assert_eq!(DATA_NAME, "graphics_memory");
    assert_eq!(
        kernel_parameter((0x1_1000_0000, 2 * MIB), (0x1_1020_0000, 256 * MIB)).as_deref(),
        Some("0x110000000,2097152,0x110200000,268435456")
    );
    assert_eq!(
        kernel_parameter((0, 2 * MIB), (0x1_1020_0000, 256 * MIB)),
        None
    );
    assert_eq!(
        kernel_parameter((0x1_1000_0000, 2 * MIB), (0x1_1020_0000, 0)),
        None
    );
}
