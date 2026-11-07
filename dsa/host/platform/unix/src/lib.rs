//! The local API over Unix sockets, shared by the Linux and macOS hosts.
//!
//! The server binds a socket file whose permissions decide who may connect at all, and
//! reads each client's user ID from the kernel (`SO_PEERCRED` on Linux, `getpeereid` on
//! macOS), so it never trusts a user named in a message.
//!
//! [`vhdx`] reads the image's VHDX disks into raw disk images, for the VMMs that attach
//! only raw disks.
//!
//! [`paths`] says where a Unix host keeps its files, and [`device_socket`] is where the service
//! hands the guest's device connections to the user's device lender.

#[cfg(unix)]
pub mod device_socket;
pub mod paths;
#[cfg(unix)]
pub mod socket;
pub mod vhdx;
