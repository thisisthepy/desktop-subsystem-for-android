//! The guest's video, decoded with Media Foundation on the host's GPU.
//!
//! Each decoder is a Media Foundation transform (the H.264, HEVC, VP9 or AV1 video decoder
//! Windows has) given a Direct3D 11 device, which makes it decode on the GPU's video engine
//! (DXVA) rather than on the processor. Each picture comes out as an NV12 texture, which is
//! read back and handed to the core ([`dsa_host_core::video`]) to send to the guest.
//!
//! A format is offered to the guest only when its decoder takes the Direct3D device: one
//! that would decode on the processor is left to the guest's own software decoder, which
//! costs the same and keeps the host's processor free.
//!
//! [`picture_from_planes`] is the copy of a picture out of a texture's memory, and is tested on
//! every platform; the rest is Windows only.

use dsa_host_core::video::Picture;

/// Copies the visible `width` by `height` of an NV12 picture whose rows are `pitch` bytes
/// apart and whose chroma plane starts `plane_rows` rows after the luma plane (the decoder
/// pads its surfaces to a multiple of 16 or more). Returns `None` when `data` is too short.
pub fn picture_from_planes(
    id: u64,
    data: &[u8],
    pitch: usize,
    plane_rows: usize,
    width: u32,
    height: u32,
) -> Option<Picture> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || pitch < w || plane_rows < h {
        return None;
    }
    let chroma_rows = h.div_ceil(2);
    let needed = pitch.checked_mul(plane_rows + chroma_rows)?;
    if data.len() < needed {
        return None;
    }
    // The stride sent is the visible width rounded up to 2, so each chroma row holds whole
    // pairs.
    let stride = w + (w & 1);
    let mut nv12 = Vec::with_capacity(stride * (h + chroma_rows));
    for row in 0..h {
        let start = row * pitch;
        nv12.extend_from_slice(&data[start..start + stride.min(pitch)]);
        nv12.resize(nv12.len() + stride - stride.min(pitch), 0);
    }
    let chroma = plane_rows * pitch;
    for row in 0..chroma_rows {
        let start = chroma + row * pitch;
        nv12.extend_from_slice(&data[start..start + stride.min(pitch)]);
        nv12.resize(nv12.len() + stride - stride.min(pitch), 0);
    }
    Some(Picture {
        id,
        width,
        height,
        stride: stride as u32,
        nv12,
    })
}

use dsa_host_core::video::Codec;

/// The argument that makes the window manager find the formats this computer decodes on its
/// GPU and print them, one line each (see [`probe_line`]), then end. The window manager runs
/// itself with it, so the decoders, their Direct3D devices and the GPU's video driver are
/// loaded in a process that ends, not in the window manager, until the guest opens a codec.
pub const PROBE_ARGUMENT: &str = "--probe-video";

const ALL_CODECS: [Codec; 4] = [Codec::H264, Codec::Hevc, Codec::Vp9, Codec::Av1];

/// The probe's line for `codec`: `gpu <name>` when it decodes on the GPU, else
/// `no <name>: <why>`.
pub fn probe_line(codec: Codec, result: &Result<(), String>) -> String {
    match result {
        Ok(()) => format!("gpu {}", codec.name()),
        Err(why) => format!("no {}: {}", codec.name(), why.replace(['\r', '\n'], " ")),
    }
}

/// The formats a probe's output says decode on the GPU, in the order printed. Lines it does
/// not know are skipped.
pub fn parse_probe(text: &str) -> Vec<Codec> {
    text.lines()
        .filter_map(|line| {
            let name = line.trim().strip_prefix("gpu ")?;
            ALL_CODECS.into_iter().find(|codec| codec.name() == name)
        })
        .collect()
}

#[cfg(windows)]
pub use system::{probe_here, MediaFoundationDecoders};

#[cfg(windows)]
mod system {
    use std::sync::OnceLock;

    use dsa_host_core::video::{Codec, CodecSupport, Packet, Picture, VideoDecoder, VideoDecoders};
    use windows::core::{Interface, GUID};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11Multithread, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer, IMFActivate, IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFTransform,
        MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
        MFMediaType_Video, MFStartup, MFTEnumEx, MFVideoArea, MFVideoFormat_AV1,
        MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_VP90,
        MFSTARTUP_NOSOCKET, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH,
        MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
        MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
        MFT_REGISTER_TYPE_INFO, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
        MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
        MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_VERSION,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

    use super::picture_from_planes;

    /// The largest picture offered for each format: 4096 by 2304, what H.264's highest
    /// common level and every hardware decoder of the others take.
    const MAX_WIDTH: u32 = 4096;
    const MAX_HEIGHT: u32 = 2304;

    fn subtype(codec: Codec) -> GUID {
        match codec {
            Codec::H264 => MFVideoFormat_H264,
            Codec::Hevc => MFVideoFormat_HEVC,
            Codec::Vp9 => MFVideoFormat_VP90,
            Codec::Av1 => MFVideoFormat_AV1,
        }
    }

    fn started() -> Result<(), String> {
        static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
        STARTED
            .get_or_init(|| {
                // SAFETY: Media Foundation's startup, once for the process.
                unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
                    .map_err(|error| format!("Media Foundation did not start: {error}"))
            })
            .clone()
    }

    /// A Direct3D 11 device on the default GPU with video support, and the device manager a
    /// decoder takes it through.
    struct Gpu {
        _device: ID3D11Device,
        manager: IMFDXGIDeviceManager,
    }

    fn gpu() -> Result<Gpu, String> {
        let mut device = None;
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        // SAFETY: the out pointer lives for the call.
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .map_err(|e| format!("no Direct3D 11 device with video support: {e}"))?;
        let device = device.ok_or("Direct3D made no device")?;
        // The decoder uses the device from its own threads.
        if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
            // SAFETY: a valid interface.
            unsafe {
                let _ = multithread.SetMultithreadProtected(true);
            }
        }
        let mut token = 0u32;
        let mut manager = None;
        // SAFETY: out pointers that live for the call.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }
            .map_err(|e| format!("no DXGI device manager: {e}"))?;
        let manager = manager.ok_or("Media Foundation made no device manager")?;
        // SAFETY: the device and the token the manager gave.
        unsafe { manager.ResetDevice(&device, token) }
            .map_err(|e| format!("the device manager did not take the device: {e}"))?;
        Ok(Gpu {
            _device: device,
            manager,
        })
    }

    /// The first decoder transform for `codec`, in the order Windows ranks them.
    fn transform(codec: Codec) -> Result<IMFTransform, String> {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: subtype(codec),
        };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: the type info and the out pointers live for the call; the array is freed
        // below.
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input as *const MFT_REGISTER_TYPE_INFO),
                None,
                &mut activates,
                &mut count,
            )
        }
        .map_err(|e| format!("cannot list the {} decoders: {e}", codec.name()))?;
        let mut found = None;
        for i in 0..count as usize {
            // SAFETY: `count` entries, each taken out once; the array is freed below.
            let activate = unsafe { std::ptr::replace(activates.add(i), None) };
            if found.is_none() {
                if let Some(activate) = activate {
                    // SAFETY: a valid activation object.
                    found = unsafe { activate.ActivateObject::<IMFTransform>() }.ok();
                }
            }
        }
        if !activates.is_null() {
            // SAFETY: the array MFTEnumEx allocated, emptied above.
            unsafe { CoTaskMemFree(Some(activates as *const std::ffi::c_void)) };
        }
        found.ok_or_else(|| format!("this computer has no {} decoder", codec.name()))
    }

    fn video_type(subtype: &GUID, width: u32, height: u32) -> Result<IMFMediaType, String> {
        // SAFETY: plain Media Foundation calls on a new type.
        unsafe {
            let media_type = MFCreateMediaType().map_err(|e| e.to_string())?;
            media_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            media_type
                .SetGUID(&MF_MT_SUBTYPE, subtype)
                .map_err(|e| e.to_string())?;
            if width > 0 && height > 0 {
                let size = (u64::from(width) << 32) | u64::from(height);
                media_type
                    .SetUINT64(&MF_MT_FRAME_SIZE, size)
                    .map_err(|e| e.to_string())?;
            }
            Ok(media_type)
        }
    }

    /// A width and a height.
    type Size = (u32, u32);

    /// Chooses NV12 among the decoder's output types. Returns the coded size and the visible
    /// size.
    fn choose_nv12(decoder: &IMFTransform) -> Result<(Size, Size), String> {
        for index in 0.. {
            // SAFETY: plain Media Foundation calls.
            let available = match unsafe { decoder.GetOutputAvailableType(0, index) } {
                Ok(available) => available,
                Err(_) => break,
            };
            // SAFETY: as above.
            let subtype = unsafe { available.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or_default();
            if subtype != MFVideoFormat_NV12 {
                continue;
            }
            // SAFETY: as above.
            unsafe { decoder.SetOutputType(0, &available, 0) }
                .map_err(|e| format!("the decoder did not take NV12 output: {e}"))?;
            // SAFETY: as above.
            let size = unsafe { available.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
            let coded = ((size >> 32) as u32, size as u32);
            let mut area = MFVideoArea::default();
            let mut len = 0u32;
            // SAFETY: the blob is an MFVideoArea, written into one.
            let visible = unsafe {
                available.GetBlob(
                    &MF_MT_MINIMUM_DISPLAY_APERTURE,
                    std::slice::from_raw_parts_mut(
                        (&mut area as *mut MFVideoArea).cast::<u8>(),
                        std::mem::size_of::<MFVideoArea>(),
                    ),
                    Some(&mut len),
                )
            };
            let shown = if visible.is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
                (area.Area.cx as u32, area.Area.cy as u32)
            } else {
                coded
            };
            return Ok((coded, shown));
        }
        Err("the decoder offers no NV12 output".into())
    }

    /// One decoder on the GPU.
    struct MediaFoundationDecoder {
        codec: Codec,
        decoder: IMFTransform,
        _gpu: Gpu,
        coded: (u32, u32),
        shown: (u32, u32),
        provides_samples: bool,
        sample_size: u32,
    }

    // SAFETY: the transform and the device are free threaded (the device is made
    // multithread protected), and each decoder is used by one thread at a time.
    unsafe impl Send for MediaFoundationDecoder {}

    impl MediaFoundationDecoder {
        fn open(codec: Codec, width: u32, height: u32) -> Result<MediaFoundationDecoder, String> {
            // SAFETY: this thread's COM, which it keeps for its life.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            started()?;
            let decoder = transform(codec)?;
            // SAFETY: plain Media Foundation calls.
            let aware = unsafe { decoder.GetAttributes() }
                .and_then(|attributes| unsafe { attributes.GetUINT32(&MF_SA_D3D11_AWARE) })
                .unwrap_or(0);
            if aware == 0 {
                return Err(format!(
                    "this computer's {} decoder does not decode on the GPU",
                    codec.name()
                ));
            }
            let gpu = gpu()?;
            // SAFETY: the manager outlives the decoder (both are kept together).
            unsafe {
                decoder.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)
            }
            .map_err(|e| format!("the {} decoder did not take the GPU: {e}", codec.name()))?;
            let input = video_type(&subtype(codec), width, height)?;
            // SAFETY: plain Media Foundation calls.
            unsafe { decoder.SetInputType(0, &input, 0) }.map_err(|e| {
                format!("the {} decoder did not take the stream: {e}", codec.name())
            })?;
            let (coded, shown) = choose_nv12(&decoder)?;
            // SAFETY: as above.
            let info = unsafe { decoder.GetOutputStreamInfo(0) }
                .map_err(|e| format!("the decoder's output is unknown: {e}"))?;
            // SAFETY: as above.
            unsafe {
                let _ = decoder.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
                let _ = decoder.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
            }
            Ok(MediaFoundationDecoder {
                codec,
                decoder,
                _gpu: gpu,
                coded,
                shown,
                provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                sample_size: info.cbSize,
            })
        }

        fn input_sample(&self, packet: &Packet) -> Result<IMFSample, String> {
            // SAFETY: a new sample and buffer, written within their length.
            unsafe {
                let sample = MFCreateSample().map_err(|e| e.to_string())?;
                let buffer =
                    MFCreateMemoryBuffer(packet.data.len() as u32).map_err(|e| e.to_string())?;
                let mut data = std::ptr::null_mut();
                buffer
                    .Lock(&mut data, None, None)
                    .map_err(|e| e.to_string())?;
                std::ptr::copy_nonoverlapping(packet.data.as_ptr(), data, packet.data.len());
                let _ = buffer.Unlock();
                buffer
                    .SetCurrentLength(packet.data.len() as u32)
                    .map_err(|e| e.to_string())?;
                sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
                // The packet's id rides through the decoder as the sample's time, which comes
                // out on the picture it becomes, in presentation order.
                sample
                    .SetSampleTime(packet.id as i64)
                    .map_err(|e| e.to_string())?;
                Ok(sample)
            }
        }

        /// Takes every picture the decoder has ready.
        fn pictures(&mut self) -> Result<Vec<Picture>, String> {
            let mut out = Vec::new();
            loop {
                let sample = if self.provides_samples {
                    None
                } else {
                    // SAFETY: a new sample with a buffer of the size the decoder asked for.
                    unsafe {
                        let sample = MFCreateSample().map_err(|e| e.to_string())?;
                        let buffer = MFCreateMemoryBuffer(self.sample_size.max(1))
                            .map_err(|e| e.to_string())?;
                        sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
                        Some(sample)
                    }
                };
                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                // SAFETY: one output buffer for the decoder's one stream.
                let result = unsafe { self.decoder.ProcessOutput(0, &mut buffers, &mut status) };
                // SAFETY: each taken out once, so the references are released.
                let sample = unsafe { std::mem::ManuallyDrop::take(&mut buffers[0].pSample) };
                // SAFETY: as above.
                drop(unsafe { std::mem::ManuallyDrop::take(&mut buffers[0].pEvents) });
                match result {
                    Ok(()) => {}
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(out),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let (coded, shown) = choose_nv12(&self.decoder)?;
                        self.coded = coded;
                        self.shown = shown;
                        continue;
                    }
                    Err(e) => return Err(format!("the {} decoder failed: {e}", self.codec.name())),
                }
                let Some(sample) = sample else {
                    continue;
                };
                if let Some(picture) = self.read(&sample)? {
                    out.push(picture);
                }
            }
        }

        fn read(&self, sample: &IMFSample) -> Result<Option<Picture>, String> {
            // SAFETY: plain Media Foundation calls; the buffer is unlocked before it goes.
            unsafe {
                let id = sample.GetSampleTime().map_err(|e| e.to_string())? as u64;
                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .map_err(|e| e.to_string())?;
                let (width, height) = self.shown;
                let rows = self.coded.1.max(height) as usize;
                if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                    let mut data = std::ptr::null_mut();
                    let mut pitch = 0i32;
                    two_d
                        .Lock2D(&mut data, &mut pitch)
                        .map_err(|e| format!("cannot read the picture: {e}"))?;
                    let pitch = pitch.unsigned_abs() as usize;
                    let len = pitch * (rows + rows.div_ceil(2));
                    let bytes = std::slice::from_raw_parts(data, len);
                    let picture = picture_from_planes(id, bytes, pitch, rows, width, height);
                    let _ = two_d.Unlock2D();
                    return Ok(picture);
                }
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                buffer
                    .Lock(&mut data, None, Some(&mut len))
                    .map_err(|e| format!("cannot read the picture: {e}"))?;
                let pitch = self.coded.0.max(width) as usize;
                let bytes = std::slice::from_raw_parts(data, len as usize);
                let picture = picture_from_planes(id, bytes, pitch, rows, width, height);
                let _ = buffer.Unlock();
                Ok(picture)
            }
        }
    }

    impl VideoDecoder for MediaFoundationDecoder {
        fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
            // VP9 and AV1 carry their configuration in the stream; the configuration Android
            // passes separately (AV1's codec configuration record) is not a frame.
            if packet.codec_config && matches!(self.codec, Codec::Vp9 | Codec::Av1) {
                return Ok(Vec::new());
            }
            let sample = self.input_sample(&packet)?;
            let mut out = Vec::new();
            loop {
                // SAFETY: a sample for the decoder's one input stream.
                match unsafe { self.decoder.ProcessInput(0, &sample, 0) } {
                    Ok(()) => break,
                    Err(e) if e.code() == MF_E_NOTACCEPTING => {
                        // Full: take what is ready, then give the packet again.
                        let ready = self.pictures()?;
                        if ready.is_empty() {
                            return Err(format!(
                                "the {} decoder takes no input and gives no output",
                                self.codec.name()
                            ));
                        }
                        out.extend(ready);
                    }
                    Err(e) => {
                        return Err(format!(
                            "the {} decoder did not take a packet: {e}",
                            self.codec.name()
                        ))
                    }
                }
            }
            out.extend(self.pictures()?);
            Ok(out)
        }

        fn drain(&mut self) -> Result<Vec<Picture>, String> {
            // SAFETY: a plain Media Foundation call.
            unsafe { self.decoder.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
                .map_err(|e| format!("the decoder did not drain: {e}"))?;
            let pictures = self.pictures();
            // SAFETY: as above; the stream goes on after a drain.
            unsafe {
                let _ = self
                    .decoder
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
            }
            pictures
        }

        fn flush(&mut self) {
            // SAFETY: a plain Media Foundation call.
            unsafe {
                let _ = self.decoder.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            }
        }
    }

    /// The host's decoders: the formats whose Media Foundation decoder takes the GPU.
    ///
    /// Nothing is loaded until the guest first asks what it can decode; then a child process
    /// finds out (see [`PROBE_ARGUMENT`](super::PROBE_ARGUMENT)) and the answer is kept. A
    /// decoder is made only when the guest opens one, and freed when it closes.
    pub struct MediaFoundationDecoders {
        supported: OnceLock<Vec<CodecSupport>>,
        log: Box<dyn Fn(&str) + Send + Sync>,
    }

    /// How long the probe may take before it is given up, and no format is offered.
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Opens a decoder of each format in this process and prints a line for each. What
    /// `dsa-wm --probe-video` runs.
    pub fn probe_here() -> String {
        let mut out = String::new();
        for codec in super::ALL_CODECS {
            let result = MediaFoundationDecoder::open(codec, 1280, 720).map(drop);
            out.push_str(&super::probe_line(codec, &result));
            out.push('\n');
        }
        out
    }

    /// Runs this program with the probe argument and returns what it printed.
    fn probe_in_child() -> Result<String, String> {
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
        let mut child = Command::new(exe)
            .arg(super::PROBE_ARGUMENT)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("the probe did not start: {e}"))?;
        let mut stdout = child.stdout.take().ok_or("the probe has no output")?;
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        });
        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < PROBE_TIMEOUT => {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "the probe did not finish in {} s",
                        PROBE_TIMEOUT.as_secs()
                    ));
                }
            }
        }
        reader
            .join()
            .map_err(|_| "the probe's output was lost".to_string())
    }

    impl MediaFoundationDecoders {
        /// Decoders that find their formats when first asked. `log` takes a line for each
        /// format then.
        pub fn lazy(log: Box<dyn Fn(&str) + Send + Sync>) -> MediaFoundationDecoders {
            MediaFoundationDecoders {
                supported: OnceLock::new(),
                log,
            }
        }

        fn find(&self) -> Vec<CodecSupport> {
            let text = match probe_in_child() {
                Ok(text) => text,
                Err(why) => {
                    (self.log)(&format!("video: no format is offered: {why}"));
                    return Vec::new();
                }
            };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                (self.log)(&format!("video: {line}"));
            }
            super::parse_probe(&text)
                .into_iter()
                .map(|codec| CodecSupport {
                    codec,
                    max_width: MAX_WIDTH,
                    max_height: MAX_HEIGHT,
                })
                .collect()
        }
    }

    impl VideoDecoders for MediaFoundationDecoders {
        fn capabilities(&self) -> Vec<CodecSupport> {
            self.supported.get_or_init(|| self.find()).clone()
        }

        fn open(
            &self,
            codec: Codec,
            width: u32,
            height: u32,
        ) -> Result<Box<dyn VideoDecoder>, String> {
            Ok(Box::new(MediaFoundationDecoder::open(
                codec, width, height,
            )?))
        }
    }
}
