//! The text clipboard the host and the guest share.
//!
//! Text copied on one side becomes the other side's clipboard text, as long as the user has
//! not turned sharing off. Images and other formats stay where they were copied.

/// The longest text that crosses, in bytes of UTF-8.
///
/// Android hands a new clip to the clipboard service in a binder transaction, and a process's
/// binder buffer is 1 MiB, shared by every transaction in flight. The text travels as UTF-16
/// there, so 256 KiB of UTF-8 takes at most 512 KiB, which leaves room for the rest. A longer
/// clip is dropped, never cut: a paste is either the whole text or nothing.
pub const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

/// Whether `text` is short enough to cross.
pub fn fits(text: &str) -> bool {
    text.len() <= MAX_CLIPBOARD_BYTES
}

/// Says that a clip was too long to cross, for a log line.
pub fn too_long(text: &str, from: &str) -> String {
    format!(
        "clipboard: dropped text copied on the {from}: {} bytes, longer than the {} that cross",
        text.len(),
        MAX_CLIPBOARD_BYTES
    )
}
