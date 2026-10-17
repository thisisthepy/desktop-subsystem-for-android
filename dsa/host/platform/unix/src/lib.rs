//! The local API over Unix sockets, shared by the Linux and macOS hosts.
//!
//! The server binds a socket file whose permissions decide who may connect at all, and
//! reads each client's user ID from the kernel (`SO_PEERCRED` on Linux, `getpeereid` on
//! macOS), so it never trusts a user named in a message.

#[cfg(unix)]
pub mod socket;
