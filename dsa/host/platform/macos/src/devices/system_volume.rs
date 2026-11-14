//! The Mac's one volume as the volume of the guest's apps.
//!
//! macOS has no volume of its own for each program, so the guest's sound has the volume of
//! the Mac's default output device and nothing else: the guest sends every output at full
//! scale. When an app in Android sets its volume, or a volume key is pressed there, the Mac's
//! volume takes it ([`OneVolume::guest_changed`]); only such a change does, so starting
//! Android or opening an app never moves the Mac's volume. When the user changes the Mac's
//! volume or mute, every app in Android takes it as its own volume
//! ([`OneVolume::seen`]), which is what apps there read.
//!
//! The device's volume is read every half second ([`follow`]): a change DSA made itself is
//! known, and goes no further.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_core::app_audio::{same_volume, AppStream, AppVolumes};

/// How often the Mac's volume is read.
pub const POLL: Duration = Duration::from_millis(500);

/// The Mac's volume and mute, as DSA last saw or set them.
#[derive(Debug, Default)]
pub struct OneVolume {
    last: Option<(f32, bool)>,
}

fn same(a: (f32, bool), b: (f32, bool)) -> bool {
    same_volume(a.0, b.0) && a.1 == b.1
}

impl OneVolume {
    /// Android changed the volume of the apps in `changed`: the volume and mute the Mac takes,
    /// the last of them, unless it has them already.
    pub fn guest_changed(&mut self, changed: &[AppStream]) -> Option<(f32, bool)> {
        let app = changed.last()?;
        let wanted = (app.volume, app.muted);
        if self.last.is_some_and(|last| same(last, wanted)) {
            return None;
        }
        self.last = Some(wanted);
        Some(wanted)
    }

    /// The Mac's volume and mute read now. True when the user changed them since DSA last saw
    /// or set them; the first reading is where things stand, not a change.
    pub fn seen(&mut self, now: (f32, bool)) -> bool {
        match self.last {
            Some(last) if same(last, now) => false,
            Some(_) => {
                self.last = Some(now);
                true
            }
            None => {
                self.last = Some(now);
                false
            }
        }
    }
}

/// Reads the Mac's volume until `stop`, and gives every app in Android the user's change of
/// it. `one` is shared with what sets the Mac's volume when Android changes it.
pub fn follow(volumes: &AppVolumes, one: &Arc<Mutex<OneVolume>>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        if let Some(now) = device::read() {
            let changed = one.lock().unwrap_or_else(|e| e.into_inner()).seen(now);
            if changed {
                for app in volumes.streams() {
                    volumes.host_changed(&app.stream, now.0, now.1);
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

/// Gives the Mac's default output device the volume and mute Android gave an app.
pub fn set(volume: f32, muted: bool) {
    device::write(volume, muted);
}

#[cfg(target_os = "macos")]
mod device {
    use std::ffi::c_void;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectHasProperty(object: u32, address: *const PropertyAddress) -> u8;
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
            data: *mut c_void,
        ) -> i32;
        fn AudioObjectSetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: u32,
            data: *const c_void,
        ) -> i32;
    }

    const fn code(name: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*name)
    }

    /// `kAudioObjectSystemObject`.
    const SYSTEM: u32 = 1;
    /// `kAudioHardwarePropertyDefaultOutputDevice`.
    const DEFAULT_OUTPUT: u32 = code(b"dOut");
    /// `kAudioObjectPropertyScopeGlobal`, `kAudioDevicePropertyScopeOutput`.
    const GLOBAL: u32 = code(b"glob");
    const OUTPUT: u32 = code(b"outp");
    /// `kAudioDevicePropertyVolumeScalar`, `kAudioDevicePropertyMute`.
    const VOLUME: u32 = code(b"volm");
    const MUTE: u32 = code(b"mute");
    /// `kAudioObjectPropertyElementMain`, and the first two channels for a device that has no
    /// volume of its own over them.
    const MAIN: u32 = 0;
    const CHANNELS: [u32; 2] = [1, 2];

    fn address(selector: u32, scope: u32, element: u32) -> PropertyAddress {
        PropertyAddress {
            selector,
            scope,
            element,
        }
    }

    fn has(object: u32, address: &PropertyAddress) -> bool {
        // SAFETY: a property address that outlives the call.
        unsafe { AudioObjectHasProperty(object, address) != 0 }
    }

    fn get<T: Copy + Default>(object: u32, address: &PropertyAddress) -> Option<T> {
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: a value of the property's size for Core Audio to fill.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                address,
                0,
                std::ptr::null(),
                &mut size,
                &mut value as *mut T as *mut c_void,
            )
        };
        (status == 0).then_some(value)
    }

    fn put<T: Copy>(object: u32, address: &PropertyAddress, value: T) -> bool {
        // SAFETY: a value of the property's size for Core Audio to read.
        unsafe {
            AudioObjectSetPropertyData(
                object,
                address,
                0,
                std::ptr::null(),
                std::mem::size_of::<T>() as u32,
                &value as *const T as *const c_void,
            ) == 0
        }
    }

    fn default_output() -> Option<u32> {
        get::<u32>(SYSTEM, &address(DEFAULT_OUTPUT, GLOBAL, MAIN)).filter(|&device| device != 0)
    }

    /// The default output device's volume (0 to 1) and mute.
    pub(super) fn read() -> Option<(f32, bool)> {
        let device = default_output()?;
        let main = address(VOLUME, OUTPUT, MAIN);
        let volume = if has(device, &main) {
            get::<f32>(device, &main)?
        } else {
            let levels: Vec<f32> = CHANNELS
                .iter()
                .filter_map(|&channel| get::<f32>(device, &address(VOLUME, OUTPUT, channel)))
                .collect();
            if levels.is_empty() {
                return None;
            }
            levels.iter().sum::<f32>() / levels.len() as f32
        };
        let mute = address(MUTE, OUTPUT, MAIN);
        let muted = has(device, &mute) && get::<u32>(device, &mute).unwrap_or(0) != 0;
        Some((volume, muted))
    }

    /// Sets the default output device's volume and mute.
    pub(super) fn write(volume: f32, muted: bool) {
        let Some(device) = default_output() else {
            return;
        };
        let volume = volume.clamp(0.0, 1.0);
        let main = address(VOLUME, OUTPUT, MAIN);
        if has(device, &main) {
            put(device, &main, volume);
        } else {
            for channel in CHANNELS {
                let each = address(VOLUME, OUTPUT, channel);
                if has(device, &each) {
                    put(device, &each, volume);
                }
            }
        }
        let mute = address(MUTE, OUTPUT, MAIN);
        if has(device, &mute) {
            put(device, &mute, u32::from(muted));
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod device {
    pub(super) fn read() -> Option<(f32, bool)> {
        None
    }

    pub(super) fn write(_: f32, _: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(volume: f32, muted: bool) -> AppStream {
        AppStream {
            stream: "app 0".into(),
            package_name: "com.example.music".into(),
            app_label: "Music".into(),
            volume,
            muted,
        }
    }

    #[test]
    fn fr26_the_macs_volume_moves_only_when_android_changes_an_apps_volume() {
        let mut one = OneVolume::default();
        // Nothing Android did yet: the first reading is where the Mac stands.
        assert!(!one.seen((0.3, false)));
        assert_eq!(one.guest_changed(&[]), None);
        // An app set its volume: the Mac takes it, once.
        assert_eq!(one.guest_changed(&[app(0.6, false)]), Some((0.6, false)));
        assert_eq!(one.guest_changed(&[app(0.6, false)]), None);
        // DSA's own change, read back, is no change of the user's.
        assert!(!one.seen((0.6, false)));
        // The user turned the Mac down: every app in Android takes it.
        assert!(one.seen((0.2, false)));
        assert!(!one.seen((0.2, false)));
        assert!(one.seen((0.2, true)));
    }
}
