//! What the host machine says about itself, which every host reads its own way.

/// Facts about the computer the host runs on.
pub trait HostFacts: Send + Sync {
    /// The computer's physical memory, in KiB; `None` when it cannot be read.
    fn physical_memory_kb(&self) -> Option<u64>;

    /// The memory the computer is installed with, in KiB, the figure a user knows it by
    /// ("32 GB"); `None` when it cannot be read. A host with no better figure than the
    /// physical memory (the guest rounds it to whole GiB) keeps this default.
    fn installed_memory_kb(&self) -> Option<u64> {
        self.physical_memory_kb()
    }
}
