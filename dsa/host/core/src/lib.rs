//! The platform-neutral part of the host.
//!
//! Nothing here names a host operating system API. The platform crates under
//! `dsa/host/platform/` supply the pieces that do (the guest launcher and the transport)
//! through the traits defined here and in `dsa-transport`.
//!
//! - [`session`]: one guest's life from start to stop.
//! - [`bootconfig`]: the kernel bootconfig that carries each boot's secret to the guest.
//! - [`handshake`]: the control channel's handshake as a session uses it.
//! - [`host_api`]: the local API service, which acts on each caller's own Android, and its
//!   client.
//! - [`tasks`]: the guest's app tasks, which the session keeps and hands on.
//! - [`input`]: pointer input in a task's window, as the guest task receives it.
//! - [`keys`]: key input in a task's window.
//! - [`power`]: the host computer's power, which each guest's battery state follows.
//! - [`links`]: web links apps in the guest open, which the host opens in its browser.
//! - [`files`]: host files the user opens or shares in an app in the guest.
//! - [`clipboard`]: the text clipboard the host and the guest share.
//! - [`notifications`]: the notifications of the guest, which the host shows.
//! - [`ime`]: text input through the host's input method in a task's window.
//! - [`settings`]: the settings a user changes from the settings app, kept by the service.
//! - [`memory`]: how much memory a user's Android takes, in the guest and on the host.
//! - [`window_manager`]: keeping a user's window manager running while their Android runs.
//! - [`compositor`]: platform-neutral graphics stream bookkeeping, renderer interfaces, and
//!   which window each task and each frame goes to.

pub mod bootconfig;
pub mod clipboard;
pub mod clock_trace;
pub mod compositor;
pub mod files;
pub mod guest_display;
pub mod handshake;
pub mod host_api;
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
pub mod resize_trace;
pub mod session;
pub mod settings;
pub mod tasks;
pub mod update;
pub mod window_manager;
