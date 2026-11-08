//! The host's devices, lent to the apps in the guest: sound out and in, the cameras, the
//! location, the sensors, printing, the user's folders, the user's settings, web links and
//! the computer's power.
//!
//! Each device has a platform-neutral half in `dsa-host-core` (the guest's port, the rules,
//! the messages) and a Linux half here that reaches the desktop's own service for it:
//!
//! ```text
//! sound out and in   PulseAudio's simple API (PipeWire serves it through pipewire-pulse)
//! cameras            Video4Linux2, else the camera portal through PipeWire
//! location           GeoClue2, on the system bus
//! sensors            the kernel's IIO devices, and iio-sensor-proxy for the light level
//! printing           the print portal's dialog, else CUPS' lp
//! folders            the XDG user folders, opened with the user's own rights
//! settings           the time zone, the locale, and the portal's appearance settings
//! web links          xdg-open
//! power              UPower, power-profiles-daemon, logind and /proc/meminfo
//! ```
//!
//! The devices that need the user's desktop session (sound, cameras, location, sensors,
//! printing, links and settings) run in a process of the user's: [`start_guest_ports`] serves
//! the guest's device ports, which the service relays from the user's own VM to sockets in
//! the user's runtime folder ([`relay`]), and [`start_followers`] follows the local API. In an
//! X11 session without Wayland [`start_monitor_refresh`] records the monitors' refresh rate,
//! as the window manager does in a Wayland session. The folders and the power belong to the service: [`file_share::UserFolders`] and
//! [`power::follow`].
//!
//! Nothing here takes the host down. A device the computer does not have, or a service that
//! is not running, is logged; the guest sees no such device, and everything else goes on.

pub mod audio;
pub mod camera;
pub mod dbus;
pub mod file_share;
pub mod links;
pub mod location;
pub mod portal_camera;
pub mod power;
pub mod print;
pub mod relay;
pub mod sensors;
pub mod settings;
pub mod v4l2;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use dsa_transport::{Connector, Listener};

/// Where a device writes what it has to say: one line each.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// Makes a new connection to the local API for each follower.
pub type ConnectorFactory = Arc<dyn Fn() -> Box<dyn Connector> + Send + Sync>;

/// The build each device names itself by.
fn build(device: &str) -> String {
    format!("dsa-devices {} {device}", crate::VERSION)
}

/// Listens on vsock `port` for the guest, or logs why it cannot.
fn listen(port: u32, what: &str, log: &Log) -> Option<Box<dyn Listener>> {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
    else {
        log(&format!(
            "{what} is not lent to Android: this session has no XDG_RUNTIME_DIR"
        ));
        return None;
    };
    let path = relay::device_socket(&runtime, port);
    match relay::DeviceSocketListener::bind(&path) {
        Ok(listener) => Some(Box::new(listener)),
        Err(error) => {
            log(&format!(
                "{what} is not lent to Android: {} cannot be opened ({error})",
                path.display()
            ));
            None
        }
    }
}

fn spawn(name: &str, log: &Log, run: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    match thread::Builder::new().name(name.into()).spawn(run) {
        Ok(handle) => Some(handle),
        Err(error) => {
            log(&format!("could not start {name}: {error}"));
            None
        }
    }
}

/// Serves the guest's device ports (sound, cameras, sensors) on vsock until `stop`, each on
/// a thread of its own. A port that cannot be opened is logged and left out.
pub fn start_guest_ports(stop: &Arc<AtomicBool>, log: &Log) -> Vec<JoinHandle<()>> {
    let mut threads = Vec::new();

    if let Some(mut listener) = listen(dsa_host_core::audio::AUDIO_PORT, "sound", log) {
        let (stop, log) = (Arc::clone(stop), Arc::clone(log));
        let device: Arc<dyn dsa_host_core::audio::AudioDevice> = Arc::new(audio::PulseAudio);
        threads.extend(spawn("dsa-audio", &Arc::clone(&log), move || {
            dsa_host_core::audio::serve_port(&mut *listener, device, &build("audio"), &stop, log)
        }));
    }

    if let Some(listener) = listen(dsa_host_core::camera::CAMERA_PORT, "the cameras", log) {
        let (stop, log) = (Arc::clone(stop), Arc::clone(log));
        let cameras = Arc::new(portal_camera::HostCameras::new(Arc::clone(&log)));
        threads.extend(spawn("dsa-camera", &Arc::clone(&log), move || {
            dsa_host_core::camera::serve(listener, cameras, build("camera"), stop, log)
        }));
    }

    if let Some(listener) = listen(dsa_host_core::sensors::SENSOR_PORT, "the sensors", log) {
        let (stop, log) = (Arc::clone(stop), Arc::clone(log));
        let sensors = Arc::new(sensors::LinuxSensors::new());
        threads.extend(spawn("dsa-sensors", &Arc::clone(&log), move || {
            dsa_host_core::sensors::serve(listener, sensors, build("sensors"), stop, log)
        }));
    }

    threads
}

/// Records the monitors' refresh rate from the X display until `stop`, in an X11 session
/// without Wayland, where no window manager hears it from Wayland
/// ([`crate::monitor_refresh::follow_x11`]). In any other session it starts nothing.
pub fn start_monitor_refresh(stop: &Arc<AtomicBool>, log: &Log) -> Option<JoinHandle<()>> {
    if !crate::monitor_refresh::in_x11_only_session() {
        return None;
    }
    let (stop, run_log) = (Arc::clone(stop), Arc::clone(log));
    spawn("dsa-monitor-refresh", log, move || {
        if let Err(why) = crate::monitor_refresh::follow_x11(stop, &*run_log) {
            run_log(&format!(
                "display: the monitors' refresh rate is not followed, so Android keeps the \
                 rate it booted with: {why}"
            ));
        }
    })
}

/// Which followers of the local API to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Followers {
    pub location: bool,
    pub print: bool,
    pub links: bool,
    pub settings: bool,
}

impl Default for Followers {
    fn default() -> Followers {
        Followers {
            location: true,
            print: true,
            links: true,
            settings: true,
        }
    }
}

/// Follows the local API for the devices it carries (the location, print jobs, web links and
/// the user's settings) until `stop`, each on a thread of its own with its own connection.
pub fn start_followers(
    connect: ConnectorFactory,
    which: Followers,
    stop: &Arc<AtomicBool>,
    log: &Log,
) -> Vec<JoinHandle<()>> {
    let mut threads = Vec::new();
    if which.location {
        let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
        threads.extend(spawn("dsa-location", &Arc::clone(&log), move || {
            location::follow(connector, stop, log)
        }));
    }
    if which.print {
        let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
        threads.extend(spawn("dsa-print", &Arc::clone(&log), move || {
            let printer: Arc<dyn print::Printer> = Arc::new(print::DesktopPrinter);
            print::serve(connector, printer, print::default_spool_dir(), stop, log)
        }));
    }
    if which.links {
        let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
        threads.extend(spawn("dsa-links", &Arc::clone(&log), move || {
            links::follow(connector, &mut links::XdgOpener, stop, log)
        }));
    }
    if which.settings {
        let (connector, stop, log) = (connect(), Arc::clone(stop), Arc::clone(log));
        threads.extend(spawn("dsa-settings", &Arc::clone(&log), move || {
            settings::follow(connector, stop, log)
        }));
    }
    threads
}
