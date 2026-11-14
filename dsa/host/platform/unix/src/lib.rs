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
//!
//! [`fd_frames`] is the connection between the window manager and its graphics renderer
//! process, whose frames can carry file descriptors, and [`renderer_launch`] starts that
//! process.
//!
//! [`https_get`] is the HTTPS fetch, through `curl`, that the passkey check reads a website's
//! Digital Asset Links with: certificate checked, no redirect followed. [`backup_folder`]
//! keeps the user's backup folder to its owner alone.
//!
//! [`dns_resolver`] is the system's resolver (`getaddrinfo` and `res_query`), which answers
//! the guest's DNS queries.

pub mod backup_folder;
#[cfg(unix)]
pub mod device_socket;
#[cfg(unix)]
pub mod dns_resolver;
#[cfg(unix)]
pub mod fd_frames;
pub mod https_get;
pub mod paths;
#[cfg(unix)]
pub mod renderer_launch;
#[cfg(unix)]
pub mod socket;
pub mod vhdx;
