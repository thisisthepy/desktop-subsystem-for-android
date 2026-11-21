//! Ending a process on a Unix host.

/// Ends this process at once, without the libraries' teardown: a driver that hangs in it must
/// not keep a process whose peer is gone.
pub fn end_now(code: i32) -> ! {
    // SAFETY: ends this process.
    unsafe { libc::_exit(code) }
}
