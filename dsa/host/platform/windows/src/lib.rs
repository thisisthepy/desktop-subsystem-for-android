//! The Windows host.
//!
//! - [`vm`]: the Android VM's configuration, as the HCS document that creates it.
//! - [`hcs`] and [`android_vm`]: creating, starting and stopping that VM (Windows only).
//! - [`network`] and `hcn`: the VM's network, NAT or on the host's LAN, through the Host
//!   Compute Network (`hcn` is Windows only).
//! - [`console_log`]: the VM's serial console, kept in a log per user, and the end of it
//!   that a failed start reports.
//! - [`console_pipe`]: the named pipe that console arrives on (Windows only).
//! - [`hvsocket`]: the control channel's transport, Hyper-V sockets (Windows only).
//! - [`https`]: downloads for image updates and the documents a passkey check reads, through
//!   WinHTTP (Windows only).
//! - [`credential_store`]: each user's backup key and KeyMint root key, sealed by DPAPI and the
//!   computer's TPM (the sealing part on Windows only).
//! - [`pipe`]: the local API over a named pipe (Windows only).
//! - [`gpt`]: finding and copying the partitions of a user's Android disk.
//! - [`guid`]: the VM and service IDs those sockets are addressed by.
//! - [`vm_listener`]: a control channel listener that follows the VM's ID across restarts.
//! - [`host_certificates`]: the root certificates a user or an administrator added to Windows,
//!   which the user's Android trusts beside its own.
//! - [`host_config`]: the configuration file `dsa-hostd` reads.
//! - [`paths`]: where every file DSA makes at run time goes, in the machine's and each user's
//!   DSA folder, and moving files from where earlier releases kept them.
//! - [`monitors`]: the record of the user's monitors the guest display is sized from.
//! - [`power`]: the computer's power, which each guest's battery state follows.
//! - [`guest_performance`]: how hard Windows runs the guest's processors, as the guest's power
//!   hints ask (the applying part on Windows only).
//! - [`host_memory`]: the host's memory for each user's Android: its VM and its window
//!   processes (the measuring part on Windows only).
//! - [`service`]: the local API and one VM per user behind it, as `dsa-hostd` serves them
//!   (Windows only).
//! - [`setup`]: running a downloaded setup file apart from the service, to update DSA
//!   (the process part on Windows only).
//! - [`profile`]: where a user's local application data is, from their SID (Windows only).
//! - [`scm`]: running under the Service Control Manager (Windows only).
//! - [`uninstall`]: what uninstalling needs from `dsa-hostd`.
//! - [`user_disk`]: each user's Android disk (Windows only).
//! - [`user_registry`]: removing the keys DSA wrote in each user's classes (Windows only).
//! - [`account`]: the Windows account the host runs as (Windows only).
//! - [`render_gpu`]: which GPU the host renderer runs on, and the setting that chooses it.
//! - [`renderer`]: the graphics renderer in a process of its own, `dsa-gfx.exe`, and the
//!   window manager's side of it (the processes are Windows only).
//! - [`window_manager`]: starting a user's window manager in their session (Windows only).
//! - [`compositor`]: the DSA Window Manager, which owns no window and hands each task's
//!   frames to its app's window host (Windows only).
//! - [`app_window`]: the app window host, the program that owns an app's task windows
//!   (Windows only), and its UI Automation tree of each task ([`ui_automation`] says what
//!   each node is in it).
//! - [`app_exe`]: each app's own copy of the window host, named and drawn as the app
//!   (Windows only).
//! - [`camera_capture`]: the computer's cameras, through Media Foundation, for the apps in
//!   the guest.
//! - [`video_decoder`]: the guest's video, decoded with Media Foundation on the GPU (the
//!   decoding part on Windows only).
//! - [`sensors`]: the computer's motion and light sensors, read for the guest's sensors HAL
//!   (the reading part on Windows only).
//! - [`start_menu`]: a Start menu shortcut for each app installed in the user's Android, which
//!   Windows search finds (Windows only).

/// The DSA release this program belongs to. The installer build sets `DSA_VERSION` to the
/// release's version; a plain `cargo build` falls back to the crate's version.
pub const VERSION: &str = match option_env!("DSA_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[cfg(windows)]
pub mod account;
#[cfg(windows)]
pub mod adb_discovery;
pub mod android_files;
#[cfg(windows)]
pub mod android_vm;
#[cfg(windows)]
pub mod app_exe;
#[cfg(windows)]
pub mod app_window;
pub mod camera_capture;
pub mod child_probe;
#[cfg(windows)]
pub mod compositor;
pub mod computer_identity;
pub mod console_log;
#[cfg(windows)]
pub mod console_pipe;
pub mod credential_store;
#[cfg(windows)]
pub mod data_reset;
#[cfg(windows)]
pub mod desktop_shortcuts;
#[cfg(windows)]
pub mod dns_resolver;
#[cfg(windows)]
pub mod file_share;
pub mod gpt;
#[cfg(windows)]
pub mod guest_communication_services;
pub mod guest_performance;
pub mod guid;
#[cfg(windows)]
pub mod hcn;
#[cfg(windows)]
pub mod hcs;
#[cfg(windows)]
pub mod host_certificates;
pub mod host_config;
pub mod host_memory;
#[cfg(windows)]
pub mod https;
#[cfg(windows)]
pub mod hvsocket;
#[cfg(windows)]
pub mod installed_apps;
pub mod installer_actions;
#[cfg(windows)]
pub mod jump_list;
#[cfg(windows)]
pub mod machine_user_folder;
pub mod monitors;
pub mod network;
pub mod paths;
#[cfg(windows)]
pub mod pipe;
pub mod pmem_disk;
pub mod power;
#[cfg(windows)]
pub mod profile;
pub mod pstore_disk;
pub mod render_gpu;
pub mod renderer;
pub mod root_approval;
#[cfg(windows)]
pub mod safe_fs;
pub mod safe_paths;
#[cfg(windows)]
pub mod scheduling;
#[cfg(windows)]
pub mod scm;
pub mod sensors;
#[cfg(windows)]
pub mod service;
pub mod setup;
#[cfg(windows)]
pub mod start_menu;
pub mod ui_automation;
pub mod uninstall;
#[cfg(windows)]
pub mod user_disk;
#[cfg(windows)]
pub mod user_registry;
pub mod video_decoder;
pub mod vm;
pub mod vm_listener;
#[cfg(windows)]
pub mod window_manager;
