//! The Windows account the host runs as.

/// The SID of the user this process runs as, as an `S-1-...` string.
pub fn current_user_sid() -> Result<String, String> {
    crate::pipe::current_user_sid()
        .map_err(|error| format!("reading this process's user failed: {error}"))
}
