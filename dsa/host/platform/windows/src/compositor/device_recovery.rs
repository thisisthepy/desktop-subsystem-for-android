//! Replacing graphics devices a GPU reset removed.
//!
//! A GPU reset (Windows' timeout detection and recovery, a driver update or restart, a GPU
//! taken away) removes every Direct3D device on the GPU. From then on each call on such a
//! device fails with `DXGI_ERROR_DEVICE_REMOVED` or `DXGI_ERROR_DEVICE_RESET`, and so does
//! every resource made on it. Nothing comes back by itself: the devices have to be made again,
//! and everything made on them with them.
//!
//! [`Recovering`] holds the current devices and a way to make new ones. Whoever finds them
//! removed (a failed call, or [`Recovering::recover`] asking the device) has them made again,
//! once however many find it, and each set of devices carries a generation, so whatever was
//! made on an older set knows it has to be made again on the new one.

use std::sync::{Arc, Mutex};

/// `DXGI_ERROR_DEVICE_REMOVED`: the GPU was reset or taken away.
pub const DEVICE_REMOVED: i32 = 0x887A_0005_u32 as i32;
/// `DXGI_ERROR_DEVICE_HUNG`: the device's own commands hung the GPU.
pub const DEVICE_HUNG: i32 = 0x887A_0006_u32 as i32;
/// `DXGI_ERROR_DEVICE_RESET`: the device's own commands made the GPU reset.
pub const DEVICE_RESET: i32 = 0x887A_0007_u32 as i32;
/// `DXGI_ERROR_DRIVER_INTERNAL_ERROR`: the driver failed; the device is lost with it.
pub const DRIVER_INTERNAL_ERROR: i32 = 0x887A_0020_u32 as i32;

/// Whether a call that failed with `code` failed because its device is gone.
pub fn is_device_lost(code: i32) -> bool {
    matches!(
        code,
        DEVICE_REMOVED | DEVICE_HUNG | DEVICE_RESET | DRIVER_INTERNAL_ERROR
    )
}

/// Devices that can say they were removed.
pub trait Removable: Send + Sync {
    /// Why the devices were removed (an `HRESULT`), or `None` while they work.
    fn removed(&self) -> Option<i32>;
}

/// An error that carries an `HRESULT`.
pub trait Code {
    fn code(&self) -> i32;
}

impl Code for windows::core::Error {
    fn code(&self) -> i32 {
        windows::core::Error::code(self).0
    }
}

/// What makes a new set of devices.
pub type MakeDevices<D> = Box<dyn Fn() -> Result<D, String> + Send + Sync>;

/// The current devices, made again whenever they are found removed.
pub struct Recovering<D: Removable> {
    current: Mutex<(Arc<D>, u64)>,
    make: MakeDevices<D>,
}

impl<D: Removable> Recovering<D> {
    /// Makes the first devices.
    pub fn new(make: MakeDevices<D>) -> Result<Recovering<D>, String> {
        let first = make()?;
        Ok(Recovering {
            current: Mutex::new((Arc::new(first), 1)),
            make,
        })
    }

    /// The current devices and their generation.
    pub fn current(&self) -> (Arc<D>, u64) {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        (Arc::clone(&current.0), current.1)
    }

    /// The current devices' generation; it grows by one each time they are made again.
    pub fn generation(&self) -> u64 {
        self.current.lock().unwrap_or_else(|e| e.into_inner()).1
    }

    /// Asks the current devices whether they were removed, and makes new ones if they were.
    /// Returns the reason and the new generation when it made them, and an error when they
    /// were removed but no new ones could be made yet (a GPU still resetting); the next call
    /// tries again.
    pub fn recover(&self) -> Result<Option<(i32, u64)>, String> {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let Some(reason) = current.0.removed() else {
            return Ok(None);
        };
        let made = (self.make)()?;
        current.1 += 1;
        current.0 = Arc::new(made);
        Ok(Some((reason, current.1)))
    }

    /// The devices of `generation` failed as removed devices do: new ones are made, unless
    /// another caller made them since. Returns the generation now current.
    pub fn replace(&self, generation: u64) -> Result<u64, String> {
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if current.1 != generation {
            return Ok(current.1);
        }
        let made = (self.make)()?;
        current.1 += 1;
        current.0 = Arc::new(made);
        Ok(current.1)
    }

    /// Runs `op` on the current devices. When it fails because they were removed, they are
    /// made again and `op` runs once more, on the new ones. Returns what `op` made and the
    /// generation it was made on.
    pub fn with<R, E: Code>(&self, op: impl Fn(&D) -> Result<R, E>) -> Result<(R, u64), E> {
        let (devices, generation) = self.current();
        match op(&devices) {
            Ok(made) => Ok((made, generation)),
            Err(error) if is_device_lost(error.code()) => {
                if self.replace(generation).is_err() {
                    return Err(error);
                }
                let (devices, generation) = self.current();
                op(&devices).map(|made| (made, generation))
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// A device that is removed when its test says so.
    struct FakeDevice {
        number: u32,
        removed: Arc<AtomicBool>,
    }

    impl Removable for FakeDevice {
        fn removed(&self) -> Option<i32> {
            self.removed
                .load(Ordering::SeqCst)
                .then_some(DEVICE_REMOVED)
        }
    }

    #[derive(Debug, PartialEq)]
    struct FakeError(i32);

    impl Code for FakeError {
        fn code(&self) -> i32 {
            self.0
        }
    }

    /// Devices numbered from 1 as they are made; the flag of the newest one in `removed`.
    fn devices() -> (
        Recovering<FakeDevice>,
        Arc<Mutex<Arc<AtomicBool>>>,
        Arc<AtomicU32>,
    ) {
        let made = Arc::new(AtomicU32::new(0));
        let removed = Arc::new(Mutex::new(Arc::new(AtomicBool::new(false))));
        let (count, flag) = (Arc::clone(&made), Arc::clone(&removed));
        let recovering = Recovering::new(Box::new(move || {
            let number = count.fetch_add(1, Ordering::SeqCst) + 1;
            let fresh = Arc::new(AtomicBool::new(false));
            *flag.lock().unwrap() = Arc::clone(&fresh);
            Ok(FakeDevice {
                number,
                removed: fresh,
            })
        }))
        .unwrap();
        (recovering, removed, made)
    }

    /// A shared texture made on a device: the number of the device it was made on.
    fn shared_texture(device: &FakeDevice) -> Result<u32, FakeError> {
        if device.removed.load(Ordering::SeqCst) {
            Err(FakeError(DEVICE_REMOVED))
        } else {
            Ok(device.number)
        }
    }

    #[test]
    fn nfr5_a_removed_device_is_replaced_and_the_next_shared_texture_is_made_on_the_new_one() {
        let (gpu, removed, made) = devices();
        assert_eq!(gpu.with(shared_texture), Ok((1, 1)));
        // The GPU is reset: creating a texture fails with DXGI_ERROR_DEVICE_REMOVED.
        removed.lock().unwrap().store(true, Ordering::SeqCst);
        assert_eq!(gpu.with(shared_texture), Ok((2, 2)));
        assert_eq!(made.load(Ordering::SeqCst), 2);
        // Later textures are made on the new device, which is not made again.
        assert_eq!(gpu.with(shared_texture), Ok((2, 2)));
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn nfr5_a_device_removed_during_a_swapchain_resize_is_replaced_and_later_frames_show() {
        let (gpu, removed, made) = devices();
        // A frame's texture is opened on the devices current then.
        let (opened_on, first) = (gpu.generation(), gpu.current().0.number);
        assert_eq!((opened_on, first), (1, 1));
        // The GPU is reset while the window resizes its swapchain for a new size: the resize
        // fails as the removed device's calls do, the devices are made again, and the resize
        // runs once more on the new ones, so the window stays and takes its size.
        removed.lock().unwrap().store(true, Ordering::SeqCst);
        let resize =
            |device: &FakeDevice| shared_texture(device).map(|number| (number, (1048, 1278)));
        assert_eq!(gpu.with(resize), Ok(((2, (1048, 1278)), 2)));
        // The texture opened before the reset is of the old devices: its frame is not shown,
        // and the next one, in a texture opened on the new devices, is.
        assert_ne!(opened_on, gpu.generation());
        for _ in 0..3 {
            assert_eq!(gpu.with(shared_texture), Ok((2, 2)));
        }
        // Frames after the reset make nothing again.
        assert_eq!(made.load(Ordering::SeqCst), 2);
        assert_eq!(gpu.recover(), Ok(None));
    }

    #[test]
    fn nfr5_a_second_reset_after_frames_on_the_new_devices_is_survived_too() {
        let (gpu, removed, made) = devices();
        for expected in 2..=3u32 {
            removed.lock().unwrap().store(true, Ordering::SeqCst);
            assert_eq!(gpu.recover(), Ok(Some((DEVICE_REMOVED, expected as u64))));
            assert_eq!(gpu.with(shared_texture), Ok((expected, expected as u64)));
        }
        assert_eq!(made.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn nfr5_asking_a_removed_device_replaces_it_before_anything_fails() {
        let (gpu, removed, _) = devices();
        assert_eq!(gpu.recover(), Ok(None));
        removed.lock().unwrap().store(true, Ordering::SeqCst);
        assert_eq!(gpu.recover(), Ok(Some((DEVICE_REMOVED, 2))));
        assert_eq!(gpu.current().0.number, 2);
        assert_eq!(gpu.recover(), Ok(None));
    }

    #[test]
    fn nfr5_many_callers_that_find_one_removed_device_make_one_new_device() {
        let (gpu, removed, made) = devices();
        let (_, generation) = gpu.current();
        removed.lock().unwrap().store(true, Ordering::SeqCst);
        // Every window that saw the old device fail asks; only the first one remakes it.
        assert_eq!(gpu.replace(generation), Ok(2));
        assert_eq!(gpu.replace(generation), Ok(2));
        assert_eq!(gpu.replace(generation), Ok(2));
        assert_eq!(made.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn nfr5_a_failure_that_is_not_a_lost_device_makes_nothing_again() {
        let (gpu, _, made) = devices();
        let invalid = 0x8007_0057_u32 as i32;
        assert_eq!(
            gpu.with(|_| Err::<u32, _>(FakeError(invalid))),
            Err(FakeError(invalid))
        );
        assert_eq!(made.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn nfr5_the_codes_of_a_lost_device_are_known() {
        for code in [
            DEVICE_REMOVED,
            DEVICE_HUNG,
            DEVICE_RESET,
            DRIVER_INTERNAL_ERROR,
        ] {
            assert!(is_device_lost(code), "0x{:08x}", code as u32);
        }
        assert!(!is_device_lost(0x887A_000A_u32 as i32));
        assert!(!is_device_lost(0));
    }
}
