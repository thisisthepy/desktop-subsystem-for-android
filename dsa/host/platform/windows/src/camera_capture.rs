//! The computer's cameras, through Media Foundation, for the apps in the guest.
//!
//! Each camera Media Foundation lists (`MFEnumDeviceSources`, video capture) is offered to
//! the guest with the modes it delivers, where its enclosure places it (front, back, or
//! neither, which Android calls external) and how it is turned, from the device's enclosure
//! location. A camera opens only when the guest starts it, through a source reader that
//! turns whatever the camera sends (YUY2, MJPEG, NV12) into NV12, and closes when the guest
//! stops it. Stills are encoded as JPEG with the Windows Imaging Component.
//!
//! The user's privacy settings (Settings, Privacy and security, Camera) are read before the
//! cameras are listed: when camera access, or access for desktop apps, is off, no camera is
//! offered. A camera Windows refuses to open (access denied) is marked blocked as well.
//!
//! The pure parts ([`consent_blocks`], [`panel_from_windows`], [`device_key`],
//! [`frame_rate`]) are tested on every platform; the rest is Windows only.

use dsa_host_core::camera::Panel;

/// Whether the camera consent values Windows keeps (`Value` under the `webcam` consent
/// store of the user and of the machine, and the user's `NonPackaged` one for desktop apps)
/// block DSA from the cameras: any of them `Deny`. A value that is not there allows.
pub fn consent_blocks(values: &[Option<String>]) -> bool {
    values
        .iter()
        .flatten()
        .any(|value| value.trim().eq_ignore_ascii_case("deny"))
}

/// The panel of `Windows.Devices.Enumeration.Panel`'s value.
pub fn panel_from_windows(value: i32) -> Panel {
    match value {
        1 => Panel::Front,
        2 => Panel::Back,
        3 => Panel::Top,
        4 => Panel::Bottom,
        5 => Panel::Left,
        6 => Panel::Right,
        _ => Panel::Unknown,
    }
}

/// The part of a device interface path that names the device itself, lower case: the same
/// for the camera interface Media Foundation lists and the video capture interface the
/// enclosure location comes with, which differ only in their class after the last `#`.
pub fn device_key(path: &str) -> String {
    let lower = path.trim().to_ascii_lowercase();
    let lower = lower.strip_prefix(r"\\?\").unwrap_or(&lower);
    match lower.rfind('#') {
        Some(end) => lower[..end].to_string(),
        None => lower.to_string(),
    }
}

/// A frame rate given as a fraction, rounded to whole frames a second; 0 when it is not one.
pub fn frame_rate(numerator: u32, denominator: u32) -> u32 {
    if denominator == 0 {
        return 0;
    }
    ((u64::from(numerator) + u64::from(denominator) / 2) / u64::from(denominator)) as u32
}

/// The argument that makes the window manager list the computer's cameras and print them
/// (`dsa_host_core::camera::list_text`), then end. The window manager runs itself with it, so
/// Media Foundation, the camera drivers and the camera frame server's client load in a process
/// that ends, and into the window manager only once an app opens a camera. The links that
/// follow it are cameras whose modes the window manager already knows, which the child does
/// not open again.
pub const LIST_ARGUMENT: &str = "--list-cameras";

/// The links given after [`LIST_ARGUMENT`] in `args`; empty when it is not there.
pub fn known_links(args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|arg| arg.as_str() != LIST_ARGUMENT)
        .skip(1)
        .cloned()
        .collect()
}

#[cfg(windows)]
pub use system::{list_here, MediaFoundationCameras};

#[cfg(windows)]
mod system {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use dsa_host_core::camera::{
        facing_of, list_text, nv12_to_bgr, parse_list_text, sensor_orientation, Camera,
        CameraStream, Cameras, Frame, Mode, Panel,
    };
    use windows::core::{GUID, PWSTR};
    use windows::Devices::Enumeration::{DeviceClass, DeviceInformation};
    use windows::Win32::Foundation::E_ACCESSDENIED;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
        IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
    };
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample,
        IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
        MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video, MFStartup,
        MFVideoFormat_NV12, MFSTARTUP_NOSOCKET, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_MT_FRAME_RATE,
        MF_MT_FRAME_RATE_RANGE_MIN, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
        MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
        MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        MF_VERSION,
    };
    use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED, STREAM_SEEK_CUR,
    };
    use windows::Win32::System::Variant::{VARIANT, VT_R4};
    use windows_future::AsyncStatus;

    use super::{consent_blocks, device_key, frame_rate, panel_from_windows};

    use crate::video_decoder::picture_from_planes;

    fn started() -> Result<(), String> {
        static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
        // SAFETY: this thread's COM, which it keeps for its life.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        STARTED
            .get_or_init(|| {
                // SAFETY: Media Foundation's startup, once for the process.
                unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
                    .map_err(|error| format!("Media Foundation did not start: {error}"))
            })
            .clone()
    }

    /// Reads a text value under the user's or the machine's keys; `None` when it is not
    /// there.
    fn registry_text(machine: bool, key: &str, name: &str) -> Option<String> {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
        };
        let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (key, name) = (wide(key), wide(name));
        let mut value = [0u16; 64];
        let mut size = std::mem::size_of_val(&value) as u32;
        let root = if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        };
        // SAFETY: the buffer and its size in bytes describe the same memory.
        let code = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                value.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if code != ERROR_SUCCESS {
            return None;
        }
        let len = value.iter().position(|&c| c == 0).unwrap_or(value.len());
        Some(String::from_utf16_lossy(&value[..len]))
    }

    /// Whether the user's privacy settings keep DSA from the cameras.
    fn consent_blocked() -> bool {
        let manager = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager";
        let webcam = format!(r"{manager}\ConsentStore\webcam");
        let non_packaged = format!(r"{webcam}\NonPackaged");
        consent_blocks(&[
            registry_text(false, &webcam, "Value"),
            registry_text(true, &webcam, "Value"),
            registry_text(false, &non_packaged, "Value"),
        ])
    }

    /// One camera Media Foundation lists.
    struct Listed {
        link: String,
        name: String,
        activate: IMFActivate,
    }

    fn string_of(activate: &IMFActivate, key: &GUID) -> Option<String> {
        let mut text = PWSTR::null();
        let mut len = 0u32;
        // SAFETY: the string is allocated by the call and freed below.
        unsafe { activate.GetAllocatedString(key, &mut text, &mut len) }.ok()?;
        // SAFETY: the string the call returned, `len` characters long.
        let value = unsafe { text.to_string() }.ok();
        // SAFETY: allocated with CoTaskMemAlloc by the call.
        unsafe { CoTaskMemFree(Some(text.0 as *const std::ffi::c_void)) };
        value
    }

    fn list_devices() -> Result<Vec<Listed>, String> {
        started()?;
        let mut attributes: Option<IMFAttributes> = None;
        // SAFETY: the out pointer lives for the call.
        unsafe { MFCreateAttributes(&mut attributes, 1) }
            .map_err(|e| format!("no attributes: {e}"))?;
        let attributes = attributes.ok_or("Media Foundation made no attributes")?;
        // SAFETY: plain attribute calls.
        unsafe {
            attributes.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
        }
        .map_err(|e| e.to_string())?;
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: the out pointers live for the call; the array is freed below.
        unsafe { MFEnumDeviceSources(&attributes, &mut activates, &mut count) }
            .map_err(|e| format!("cannot list the cameras: {e}"))?;
        let mut out = Vec::new();
        for i in 0..count as usize {
            // SAFETY: `count` entries, each taken out once; the array is freed below.
            let Some(activate) = (unsafe { std::ptr::replace(activates.add(i), None) }) else {
                continue;
            };
            let key = &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK;
            let Some(link) = string_of(&activate, key) else {
                continue;
            };
            let name = string_of(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
                .unwrap_or_else(|| "Camera".into());
            out.push(Listed {
                link,
                name,
                activate,
            });
        }
        if !activates.is_null() {
            // SAFETY: the array MFEnumDeviceSources allocated, emptied above.
            unsafe { CoTaskMemFree(Some(activates as *const std::ffi::c_void)) };
        }
        Ok(out)
    }

    /// Where each video capture device's enclosure places it and how it is turned, by
    /// [`device_key`]. Empty when Windows does not say.
    fn enclosures() -> HashMap<String, (Panel, u32)> {
        let mut out = HashMap::new();
        let class = DeviceClass::VideoCapture;
        let Ok(operation) = DeviceInformation::FindAllAsyncDeviceClass(class) else {
            return out;
        };
        // Waited for here, briefly: the list comes from the device database at once.
        let deadline = Instant::now() + Duration::from_secs(5);
        while operation.Status().ok() == Some(AsyncStatus::Started) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let Ok(devices) = operation.GetResults() else {
            return out;
        };
        let count = devices.Size().unwrap_or(0);
        for i in 0..count {
            let Ok(device) = devices.GetAt(i) else {
                continue;
            };
            let Ok(id) = device.Id() else {
                continue;
            };
            let Ok(location) = device.EnclosureLocation() else {
                continue;
            };
            let panel = location
                .Panel()
                .map(|panel| panel_from_windows(panel.0))
                .unwrap_or(Panel::Unknown);
            let rotation = location.RotationAngleInDegreesClockwise().unwrap_or(0);
            out.insert(device_key(&id.to_string()), (panel, rotation));
        }
        out
    }

    /// A line for the window manager's log.
    type Log = Arc<dyn Fn(&str) + Send + Sync>;

    /// A camera's activation object, its source and its reader: everything that keeps the
    /// camera in use for Windows (its light and its in-use indicator).
    struct Parts {
        activate: IMFActivate,
        source: IMFMediaSource,
        reader: IMFSourceReader,
    }

    /// A camera opened through Media Foundation. [`Opened::release`] lets it go, once, from
    /// any thread: it shuts the source down (a `ReadSample` waiting on another thread then
    /// returns) and drops every reference this process holds to the camera. Dropping the last
    /// [`Opened`] does the same.
    struct Opened {
        parts: Mutex<Option<Parts>>,
        /// The camera's name, and the log its release is written to; `None` while the modes
        /// are only being read.
        announce: Option<(String, Log)>,
    }

    // SAFETY: Media Foundation's capture objects are free threaded, and `parts` is reached
    // through its lock only.
    unsafe impl Send for Opened {}
    // SAFETY: as above.
    unsafe impl Sync for Opened {}

    impl Opened {
        /// The reader, with a reference of its own, so a release meanwhile does not take it
        /// from under the caller; `None` once the camera is let go.
        fn reader(&self) -> Option<IMFSourceReader> {
            let parts = self.parts.lock().unwrap_or_else(|e| e.into_inner());
            parts.as_ref().map(|parts| parts.reader.clone())
        }

        fn release(&self) {
            let taken = self.parts.lock().unwrap_or_else(|e| e.into_inner()).take();
            let Some(parts) = taken else {
                return;
            };
            // SAFETY: plain Media Foundation calls on objects this owns.
            unsafe {
                let _ = parts.source.Shutdown();
                let _ = parts.activate.ShutdownObject();
            }
            let Parts {
                activate,
                source,
                reader,
            } = parts;
            drop(reader);
            drop(source);
            drop(activate);
            if let Some((name, log)) = &self.announce {
                log(&format!("camera: {name} is let go"));
            }
        }
    }

    impl Drop for Opened {
        fn drop(&mut self) {
            self.release();
        }
    }

    const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    fn open_reader(
        activate: IMFActivate,
        announce: Option<(String, Log)>,
    ) -> Result<Opened, windows::core::Error> {
        // SAFETY: plain Media Foundation calls; the objects are kept together and shut down
        // when dropped.
        unsafe {
            let source: IMFMediaSource = activate.ActivateObject()?;
            let mut attributes: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attributes, 1)?;
            let reader = match attributes {
                Some(attributes) => {
                    // The reader converts the camera's own format (and size, and rate) to
                    // the NV12 the guest takes.
                    attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
                    MFCreateSourceReaderFromMediaSource(&source, &attributes)
                }
                None => MFCreateSourceReaderFromMediaSource(&source, None::<&IMFAttributes>),
            };
            let reader = match reader {
                Ok(reader) => reader,
                Err(error) => {
                    let _ = source.Shutdown();
                    let _ = activate.ShutdownObject();
                    return Err(error);
                }
            };
            Ok(Opened {
                parts: Mutex::new(Some(Parts {
                    activate,
                    source,
                    reader,
                })),
                announce,
            })
        }
    }

    /// A native type's size and rate, and its slowest rate when it has a range.
    fn mode_of(media_type: &IMFMediaType) -> Option<Mode> {
        // SAFETY: plain attribute reads.
        unsafe {
            let size = media_type.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
            let rate = media_type
                .GetUINT64(&MF_MT_FRAME_RATE)
                .unwrap_or((30u64 << 32) | 1);
            let max_fps = frame_rate((rate >> 32) as u32, rate as u32);
            let min_fps = media_type
                .GetUINT64(&MF_MT_FRAME_RATE_RANGE_MIN)
                .map(|min| frame_rate((min >> 32) as u32, min as u32))
                .unwrap_or(max_fps);
            Some(Mode {
                width: (size >> 32) as u32,
                height: size as u32,
                min_fps: min_fps.min(max_fps).max(1),
                max_fps,
            })
        }
    }

    fn native_types(reader: &IMFSourceReader) -> Vec<(IMFMediaType, Mode)> {
        let mut out = Vec::new();
        for index in 0..512 {
            // SAFETY: a plain Media Foundation call; it fails past the last type.
            let Ok(media_type) = (unsafe { reader.GetNativeMediaType(STREAM, index) }) else {
                break;
            };
            if let Some(mode) = mode_of(&media_type) {
                out.push((media_type, mode));
            }
        }
        out
    }

    /// The modes a camera delivers, found by opening it once; `Err(true)` when Windows denies
    /// the camera.
    fn modes_of(activate: IMFActivate) -> Result<Vec<Mode>, bool> {
        let denied = |e: windows::core::Error| e.code() == E_ACCESSDENIED;
        let opened = open_reader(activate, None).map_err(denied)?;
        let modes = opened
            .reader()
            .map(|reader| native_types(&reader))
            .unwrap_or_default()
            .into_iter()
            .map(|(_, mode)| mode)
            .collect();
        Ok(modes)
    }

    /// A running camera. Dropping it, or calling its releaser from another thread, lets the
    /// camera go.
    struct MediaFoundationStream {
        opened: Arc<Opened>,
        width: u32,
        height: u32,
    }

    impl Drop for MediaFoundationStream {
        fn drop(&mut self) {
            self.opened.release();
        }
    }

    impl CameraStream for MediaFoundationStream {
        fn releaser(&self) -> Option<Box<dyn Fn() + Send>> {
            let opened = Arc::clone(&self.opened);
            Some(Box::new(move || opened.release()))
        }

        fn next(&mut self) -> Result<Frame, String> {
            // A capture source may give a gap (no sample) now and then; a few in a row mean
            // the camera stopped.
            for _ in 0..100 {
                // A reference of its own for the call, so a release from another thread
                // shuts the source down (and this call returns) without freeing the reader
                // under it.
                let reader = self.opened.reader().ok_or("the camera was let go")?;
                let mut flags = 0u32;
                let mut timestamp = 0i64;
                let mut sample: Option<IMFSample> = None;
                // SAFETY: the out pointers live for the call.
                unsafe {
                    reader.ReadSample(
                        STREAM,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                }
                .map_err(|e| format!("the camera stopped: {e}"))?;
                if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                    return Err("the camera failed".into());
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    return Err("the camera went away".into());
                }
                let Some(sample) = sample else {
                    continue;
                };
                let mut frame = self.read(&sample)?;
                // Media Foundation counts in 100 ns.
                frame.timestamp_ns = timestamp.max(0) as u64 * 100;
                return Ok(frame);
            }
            Err("the camera sends no pictures".into())
        }
    }

    impl MediaFoundationStream {
        fn read(&self, sample: &IMFSample) -> Result<Frame, String> {
            use windows::core::Interface;
            let (width, height) = (self.width, self.height);
            let rows = height as usize;
            // SAFETY: plain Media Foundation calls; the buffer is unlocked before it goes.
            unsafe {
                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .map_err(|e| e.to_string())?;
                let picture = if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                    let mut data = std::ptr::null_mut();
                    let mut pitch = 0i32;
                    two_d
                        .Lock2D(&mut data, &mut pitch)
                        .map_err(|e| format!("cannot read the picture: {e}"))?;
                    let pitch = pitch.unsigned_abs() as usize;
                    let bytes = std::slice::from_raw_parts(data, pitch * (rows + rows.div_ceil(2)));
                    let picture = picture_from_planes(0, bytes, pitch, rows, width, height);
                    let _ = two_d.Unlock2D();
                    picture
                } else {
                    let mut data = std::ptr::null_mut();
                    let mut len = 0u32;
                    buffer
                        .Lock(&mut data, None, Some(&mut len))
                        .map_err(|e| format!("cannot read the picture: {e}"))?;
                    let bytes = std::slice::from_raw_parts(data, len as usize);
                    let picture =
                        picture_from_planes(0, bytes, width as usize, rows, width, height);
                    let _ = buffer.Unlock();
                    picture
                };
                let picture = picture.ok_or("the camera's picture is not its size")?;
                Ok(Frame {
                    width: picture.width,
                    height: picture.height,
                    stride: picture.stride,
                    nv12: picture.nv12,
                    timestamp_ns: 0,
                })
            }
        }
    }

    fn output_type(width: u32, height: u32, fps: u32) -> Result<IMFMediaType, String> {
        // SAFETY: plain Media Foundation calls on a new type.
        unsafe {
            let media_type = MFCreateMediaType().map_err(|e| e.to_string())?;
            media_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            media_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .map_err(|e| e.to_string())?;
            media_type
                .SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    (u64::from(width) << 32) | u64::from(height),
                )
                .map_err(|e| e.to_string())?;
            media_type
                .SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps.max(1)) << 32) | 1)
                .map_err(|e| e.to_string())?;
            Ok(media_type)
        }
    }

    /// How long listing the cameras in a child process may take before no camera is offered.
    const LIST_TIMEOUT: Duration = Duration::from_secs(30);

    /// Lists the cameras in this process and returns them as text, one line each: what
    /// `dsa-wm --list-cameras` runs. The cameras of `known` are not opened, and are listed
    /// with no modes; the caller has them. Lines on why a camera is not offered go to the
    /// standard error.
    pub fn list_here(known: &[String]) -> String {
        let cameras = MediaFoundationCameras::new(Box::new(|line: &str| eprintln!("{line}")));
        {
            let mut modes = cameras.modes.lock().unwrap_or_else(|e| e.into_inner());
            for link in known {
                modes.insert(link.clone(), Vec::new());
            }
        }
        list_text(&cameras.list_in_process())
    }

    /// The computer's cameras, through Media Foundation.
    ///
    /// They are listed in a child process ([`LIST_ARGUMENT`](super::LIST_ARGUMENT)), so the
    /// camera stack loads into this one only when an app opens a camera. The modes of each
    /// camera are found the first time it is listed, by opening it once, and kept while it
    /// stays plugged in; listing again opens no camera.
    pub struct MediaFoundationCameras {
        modes: Mutex<HashMap<String, Vec<Mode>>>,
        log: Log,
    }

    impl MediaFoundationCameras {
        /// `log` takes a line for each camera that cannot be offered, and one when a camera
        /// opens for an app and when it is let go.
        pub fn new(log: Box<dyn Fn(&str) + Send + Sync>) -> MediaFoundationCameras {
            MediaFoundationCameras {
                modes: Mutex::new(HashMap::new()),
                log: Arc::from(log),
            }
        }
    }

    impl MediaFoundationCameras {
        /// The cameras, listed in this process.
        fn list_in_process(&self) -> Vec<Camera> {
            let devices = match list_devices() {
                Ok(devices) => devices,
                Err(why) => {
                    (self.log)(&format!("camera: no camera is offered: {why}"));
                    return Vec::new();
                }
            };
            let blocked = consent_blocked();
            let places = enclosures();
            let mut known = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            known.retain(|link, _| devices.iter().any(|d| &d.link == link));
            let mut out = Vec::new();
            for device in devices {
                let (panel, rotation) = places
                    .get(&device_key(&device.link))
                    .copied()
                    .map_or((None, 0), |(panel, rotation)| (Some(panel), rotation));
                let mut camera = Camera {
                    id: device.link.clone(),
                    name: device.name.clone(),
                    facing: facing_of(panel),
                    sensor_orientation: sensor_orientation(i64::from(rotation)),
                    modes: Vec::new(),
                    blocked,
                };
                if !blocked {
                    match known.get(&device.link).cloned() {
                        Some(modes) => camera.modes = modes,
                        None => match modes_of(device.activate) {
                            Ok(modes) => {
                                known.insert(device.link.clone(), modes.clone());
                                camera.modes = modes;
                            }
                            Err(denied) => {
                                camera.blocked = denied;
                                let why = if denied {
                                    "Windows denies access to it"
                                } else {
                                    "it did not open"
                                };
                                let name = &device.name;
                                (self.log)(&format!("camera: {name} is not offered: {why}"));
                            }
                        },
                    }
                }
                out.push(camera);
            }
            out
        }
    }

    impl Cameras for MediaFoundationCameras {
        fn list(&self) -> Vec<Camera> {
            let mut args = vec![super::LIST_ARGUMENT.to_string()];
            args.extend(
                self.modes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .keys()
                    .cloned(),
            );
            let output = match crate::child_probe::run_self(&args, LIST_TIMEOUT) {
                Ok(output) => output,
                Err(why) => {
                    (self.log)(&format!("camera: no camera is offered: {why}"));
                    return Vec::new();
                }
            };
            for line in output.stderr.lines().filter(|l| !l.trim().is_empty()) {
                (self.log)(line);
            }
            let mut cameras = parse_list_text(&output.stdout);
            let mut known = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            known.retain(|link, _| cameras.iter().any(|c| &c.id == link));
            for camera in &mut cameras {
                if camera.blocked {
                    continue;
                }
                match known.get(&camera.id) {
                    Some(modes) => camera.modes = modes.clone(),
                    None if !camera.modes.is_empty() => {
                        known.insert(camera.id.clone(), camera.modes.clone());
                    }
                    None => {}
                }
            }
            cameras
        }

        fn open(&self, id: &str, mode: Mode, fps: u32) -> Result<Box<dyn CameraStream>, String> {
            if consent_blocked() {
                return Err("camera access is off in the computer's privacy settings".into());
            }
            let device = list_devices()?
                .into_iter()
                .find(|d| d.link == id)
                .ok_or("the camera is not plugged in any more")?;
            let name = device.name.clone();
            let announce = Some((name.clone(), Arc::clone(&self.log)));
            let opened = open_reader(device.activate, announce).map_err(|e| {
                if e.code() == E_ACCESSDENIED {
                    "Windows denies access to the camera (check the privacy settings)".to_string()
                } else {
                    format!("{name} did not open; another program may be using it: {e}")
                }
            })?;
            (self.log)(&format!(
                "camera: {name} opened for an app, {}x{} at {fps} frames a second",
                mode.width, mode.height
            ));
            // Every failure below drops `opened`, which lets the camera go and says so.
            let reader = opened.reader().ok_or("the camera was let go")?;
            // The camera's own mode of that size, at the rate nearest the one asked for.
            let native = native_types(&reader)
                .into_iter()
                .filter(|(_, m)| m.width == mode.width && m.height == mode.height)
                .min_by_key(|(_, m)| {
                    let below = u32::from(m.max_fps < fps);
                    (below, m.max_fps.abs_diff(fps))
                });
            if let Some((native, _)) = native {
                // SAFETY: a plain Media Foundation call.
                unsafe { reader.SetCurrentMediaType(STREAM, None, &native) }
                    .map_err(|e| format!("{name} did not take the mode: {e}"))?;
            }
            let output = output_type(mode.width, mode.height, fps)?;
            // SAFETY: as above.
            unsafe { reader.SetCurrentMediaType(STREAM, None, &output) }
                .map_err(|e| format!("{name} gives no NV12 pictures: {e}"))?;
            drop(reader);
            Ok(Box::new(MediaFoundationStream {
                opened: Arc::new(opened),
                width: mode.width,
                height: mode.height,
            }))
        }

        fn encode_jpeg(&self, frame: &Frame, quality: u8) -> Result<Vec<u8>, String> {
            started()?;
            let bgr = nv12_to_bgr(frame).ok_or("the picture is not whole")?;
            let stride = frame.width * 3;
            let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
            // Room for the largest JPEG of the picture: never more than its pixels and the
            // headers.
            let mut out = vec![0u8; bgr.len() + 64 * 1024];
            // SAFETY: plain Windows Imaging Component calls; `out` outlives the stream.
            unsafe {
                let factory: IWICImagingFactory =
                    CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                        .map_err(|e| fail("the imaging component is not there", e))?;
                let stream = factory
                    .CreateStream()
                    .map_err(|e| fail("no stream for the still", e))?;
                stream
                    .InitializeFromMemory(&out)
                    .map_err(|e| fail("no stream for the still", e))?;
                let encoder = factory
                    .CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())
                    .map_err(|e| fail("no JPEG encoder", e))?;
                encoder
                    .Initialize(&stream, WICBitmapEncoderNoCache)
                    .map_err(|e| fail("the JPEG encoder did not start", e))?;
                let mut frame_encode: Option<IWICBitmapFrameEncode> = None;
                let mut options: Option<IPropertyBag2> = None;
                encoder
                    .CreateNewFrame(&mut frame_encode, &mut options)
                    .map_err(|e| fail("the JPEG encoder made no frame", e))?;
                let frame_encode = frame_encode.ok_or("the JPEG encoder made no frame")?;
                if let Some(options) = &options {
                    let mut name: Vec<u16> = "ImageQuality".encode_utf16().chain([0]).collect();
                    let option = PROPBAG2 {
                        pstrName: PWSTR(name.as_mut_ptr()),
                        ..Default::default()
                    };
                    let mut value = VARIANT::default();
                    let inner = &mut *value.Anonymous.Anonymous;
                    inner.vt = VT_R4;
                    inner.Anonymous.fltVal = f32::from(quality) / 100.0;
                    // The encoder's own quality stays when it does not take this.
                    let _ = options.Write(1, &option, &value);
                }
                frame_encode
                    .Initialize(options.as_ref())
                    .map_err(|e| fail("the JPEG frame did not start", e))?;
                frame_encode
                    .SetSize(frame.width, frame.height)
                    .map_err(|e| fail("the JPEG frame took no size", e))?;
                let mut format = GUID_WICPixelFormat24bppBGR;
                frame_encode
                    .SetPixelFormat(&mut format)
                    .map_err(|e| fail("the JPEG frame took no format", e))?;
                frame_encode
                    .WritePixels(frame.height, stride, &bgr)
                    .map_err(|e| fail("the still was not encoded", e))?;
                frame_encode
                    .Commit()
                    .map_err(|e| fail("the still was not encoded", e))?;
                encoder
                    .Commit()
                    .map_err(|e| fail("the still was not encoded", e))?;
                let mut written = 0u64;
                stream
                    .Seek(0, STREAM_SEEK_CUR, Some(&mut written))
                    .map_err(|e| fail("the still's length is unknown", e))?;
                drop(stream);
                out.truncate(written as usize);
            }
            Ok(out)
        }
    }
}
