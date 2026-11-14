//! The platform-neutral part of the host.
//!
//! Nothing here names a host operating system API. The platform crates under
//! `dsa/host/platform/` supply the pieces that do (the guest launcher and the transport)
//! through the traits defined here and in `dsa-transport`.
//!
//! - [`session`]: one guest's life from start to stop.
//! - [`adb_bridge`]: developer mode's loopback port, whose connections go to the guest's
//!   adbd, and [`adb_discovery`]: how adb on this computer finds that port by itself
//!   (auto-connect to the user's adb server, and the port's mDNS advertisement).
//! - [`accessibility`]: the accessibility tree of a task window, for host accessibility
//!   clients such as screen readers.
//! - [`audio`]: the guest's sound, played on the host's default output device, and
//!   [`app_audio`], each app's sound under its own name and volume in the host's mixer.
//! - [`bootconfig`]: the kernel bootconfig that carries each boot's secret to the guest.
//! - [`handshake`]: the control channel's handshake as a session uses it.
//! - [`host_api`]: the local API service, which acts on each caller's own Android, and its
//!   client.
//! - [`host_certificates`]: the root certificates a user or an administrator added to the
//!   host, which Android trusts beside its own.
//! - [`host_config`]: the configuration file's defaults and rules that every host shares.
//! - [`host_paths`]: where the host keeps its files, a port each platform fills in.
//! - [`tasks`]: the guest's app tasks, which the session keeps and hands on.
//! - [`input`]: pointer input in a task's window, as the guest task receives it.
//! - [`keys`]: key input in a task's window.
//! - [`power`]: the host computer's power, which each guest's battery state follows.
//! - [`power_hints`]: the guest's power hints, and how hard the host runs its processors.
//! - [`location`]: the host's location, which the guest's GNSS HAL and its network location
//!   provider give its apps.
//! - [`geocode`]: address lookups of the guest's apps, answered by the host from the
//!   administrative boundaries it keeps ([`boundaries`]).
//! - [`share`]: what the guest's apps share with the host's share UI ("Share on PC").
//! - [`links`]: web links apps in the guest open, which the host opens in its browser.
//! - [`apk_info`]: what an APK on the host says about itself (its name, icon, version,
//!   permissions and signer), read from the file before Android sees it, and
//!   [`apk_bundle`]: split-APK bundles (`.xapk`, `.apks`, `.apkm`) and the splits a device
//!   takes from them.
//! - [`guest_log`]: the guest's log, streamed only while someone reads it, and
//!   [`diagnostics`]: the archive a problem report carries.
//! - [`files`]: host files the user opens or shares in an app in the guest.
//! - [`clipboard`]: the text clipboard the host and the guest share.
//! - [`notifications`]: the notifications of the guest, which the host shows.
//! - [`background`]: the apps that keep running with no window, and their media sessions.
//! - [`widgets`]: the widgets of the guest, which the host places on its desktop.
//! - [`print`]: the documents apps in the guest print, which the host prints.
//! - [`video`]: the guest's video, decoded on the host's hardware decoder.
//! - [`sensors`]: the host's motion and light sensors, read by the guest's sensors HAL.
//! - [`camera`]: the host's cameras, used by the apps in the guest.
//! - [`ime`]: text input through the host's input method in a task's window.
//! - [`file_share`]: the user's host folders in the guest's shared storage, and the guest's
//!   shared storage on the host.
//! - [`erofs`]: erofs images, the read-only file systems the host hands the guest as one file.
//! - [`vhdx`]: the disks inside the image's dynamic VHDX files.
//! - [`settings`]: the settings a user changes from the settings app, kept by the service.
//! - [`system_settings`]: the host's time zone, languages, regional format, theme and accent
//!   color, which each user's Android follows.
//! - [`gamepads`]: the host's game controllers, shown to the guest as gamepads.
//! - [`app_windows`]: a user's window settings for each app.
//! - [`gapps`]: Google Play for a user who asks for it, downloaded and laid over the guest's
//!   system as an erofs image.
//! - [`memory`]: how much memory a user's Android takes, in the guest and on the host.
//! - [`host_memory`]: the computer's physical memory, which Google Play sees as the device's.
//! - [`window_manager`]: keeping a user's window manager running while their Android runs.
//! - [`compositor`]: platform-neutral graphics stream bookkeeping, renderer interfaces, and
//!   which window each task and each frame goes to.
//! - [`renderer`]: the graphics renderer in a process of its own (`dsa-gfx`), which the
//!   window manager starts, supervises and starts again when it ends.
//! - [`host_keys`]: the secrets the host's credential store keeps for each user, and
//!   [`backup`], [`passkeys`] and [`keymint_root`], the system services that use them: app data
//!   backups in an encrypted folder of the user's, passkeys through the host's authenticator,
//!   and the root key of the guest's KeyMint.

pub mod accessibility;
pub mod adb_bridge;
pub mod adb_discovery;
pub mod addon_disk;
pub mod apk;
pub mod apk_bundle;
pub mod apk_info;
pub mod app_audio;
pub mod app_windows;
pub mod apps;
pub mod arm_translation;
pub mod audio;
pub mod background;
pub mod backup;
pub mod bootconfig;
pub mod boundaries;
pub mod camera;
pub mod clipboard;
pub mod clock_trace;
pub mod compositor;
pub mod computer_identity;
pub mod data_reset;
pub mod device_identity;
pub mod diagnostics;
pub mod direct_mapped;
pub mod direct_mapped_disk;
pub mod dns;
pub mod drag;
pub mod erofs;
pub mod file_share;
pub mod files;
pub mod files_door;
pub mod gamepads;
pub mod gapps;
pub mod geocode;
pub mod guest_display;
pub mod guest_log;
pub mod guest_log_api;
pub mod guest_refresh;
pub mod handshake;
pub mod host_api;
#[cfg(feature = "conformance")]
pub mod host_api_conformance;
pub mod host_certificates;
pub mod host_config;
pub mod host_keys;
pub mod host_memory;
pub mod host_paths;
pub mod ime;
pub mod input;
pub mod input_trace;
pub mod keymint_root;
pub mod keys;
pub mod language;
pub mod links;
pub mod location;
pub mod memory;
pub mod native_abi;
pub mod notifications;
pub mod passkeys;
pub mod png;
pub mod power;
pub mod power_hints;
pub mod print;
pub mod renderer;
pub mod resize_trace;
pub mod root_approval;
pub mod root_elevation;
pub mod sensors;
pub mod session;
pub mod settings;
pub mod share;
pub mod system_settings;
pub mod tasks;
pub mod touch_gestures;
pub mod update;
pub mod vhdx;
pub mod video;
pub mod video_layout;
pub mod widevine;
pub mod widget_windows;
pub mod widgets;
pub mod window_manager;
