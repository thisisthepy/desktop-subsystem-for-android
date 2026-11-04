//! The platform-neutral part of the host.
//!
//! Nothing here names a host operating system API. The platform crates under
//! `dsa/host/platform/` supply the pieces that do (the guest launcher and the transport)
//! through the traits defined here and in `dsa-transport`.
//!
//! - [`session`]: one guest's life from start to stop.
//! - [`adb_bridge`]: developer mode's loopback port, whose connections go to the guest's adbd.
//! - [`accessibility`]: the accessibility tree of a task window, for host accessibility
//!   clients such as screen readers.
//! - [`bootconfig`]: the kernel bootconfig that carries each boot's secret to the guest.
//! - [`handshake`]: the control channel's handshake as a session uses it.
//! - [`host_api`]: the local API service, which acts on each caller's own Android, and its
//!   client.
//! - [`host_config`]: the configuration file's defaults and rules that every host shares.
//! - [`host_paths`]: where the host keeps its files, a port each platform fills in.
//! - [`tasks`]: the guest's app tasks, which the session keeps and hands on.
//! - [`input`]: pointer input in a task's window, as the guest task receives it.
//! - [`keys`]: key input in a task's window.
//! - [`power`]: the host computer's power, which each guest's battery state follows.
//! - [`power_hints`]: the guest's power hints, and how hard the host runs its processors.
//! - [`links`]: web links apps in the guest open, which the host opens in its browser.
//! - [`apk_info`]: what an APK on the host says about itself (its name, icon, version,
//!   permissions and signer), read from the file before Android sees it.
//! - [`guest_log`]: the guest's log, streamed only while someone reads it, and
//!   [`diagnostics`]: the archive a problem report carries.
//! - [`files`]: host files the user opens or shares in an app in the guest.
//! - [`clipboard`]: the text clipboard the host and the guest share.
//! - [`notifications`]: the notifications of the guest, which the host shows.
//! - [`widgets`]: the widgets of the guest, which the host places on its desktop.
//! - [`video`]: the guest's video, decoded on the host's hardware decoder.
//! - [`ime`]: text input through the host's input method in a task's window.
//! - [`settings`]: the settings a user changes from the settings app, kept by the service.
//! - [`memory`]: how much memory a user's Android takes, in the guest and on the host.
//! - [`window_manager`]: keeping a user's window manager running while their Android runs.
//! - [`compositor`]: platform-neutral graphics stream bookkeeping, renderer interfaces, and
//!   which window each task and each frame goes to.

pub mod accessibility;
pub mod adb_bridge;
pub mod apk;
pub mod apk_info;
pub mod bootconfig;
pub mod clipboard;
pub mod clock_trace;
pub mod compositor;
pub mod diagnostics;
pub mod files;
pub mod guest_display;
pub mod guest_log;
pub mod guest_log_api;
pub mod handshake;
pub mod host_api;
pub mod host_config;
pub mod host_paths;
#[cfg(feature = "conformance")]
pub mod host_api_conformance;
pub mod ime;
pub mod input;
pub mod input_trace;
pub mod keys;
pub mod links;
pub mod memory;
pub mod notifications;
pub mod power;
pub mod power_hints;
pub mod resize_trace;
pub mod session;
pub mod settings;
pub mod tasks;
pub mod update;
pub mod video;
pub mod widgets;
pub mod window_manager;
