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
//! Where each picture lies in the decoder's surface (the chroma plane's row, the visible size)
//! is decided by [`dsa_host_core::video_layout`], which every host shares and which is tested
//! on every platform; the rest is Windows only.

use dsa_host_core::video::Picture;
use dsa_host_core::video_layout::{picture_from_surface, SurfaceFormat};

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
    picture_from_surface(
        id,
        data,
        SurfaceFormat::Nv12,
        pitch,
        plane_rows,
        width,
        height,
    )
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock, Weak};
    use std::time::Instant;

    use dsa_host_core::video::{
        Codec, CodecSupport, DecoderReport, Packet, Picture, VideoDecoder, VideoDecoders,
    };
    use dsa_host_core::video_layout::{
        chroma_start_row, mapped_chroma_row, picture_from_surface_area, rows_from_length,
        staging_rows, stream_frame_size, visible_area, Area, Size, StreamSizes, SurfaceFormat,
    };
    use windows::core::{Interface, GUID};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11Multithread, ID3D11Texture2D, D3D11_BOX,
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice3};
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer, IMFActivate, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaType, IMFSample,
        IMFShutdown, IMFTransform, MFCreateDXGIDeviceManager, MFCreateMediaType,
        MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFShutdown, MFStartup, MFTEnumEx,
        MFVideoArea, MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
        MFVideoFormat_P010, MFVideoFormat_VP90, MFSTARTUP_NOSOCKET, MFT_CATEGORY_VIDEO_DECODER,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN,
        MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_E_NOTACCEPTING,
        MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE,
        MF_MT_GEOMETRIC_APERTURE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
        MF_MT_PAN_SCAN_APERTURE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_VERSION,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

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

    /// A Direct3D 11 device on the default GPU with video support, and the device manager a
    /// decoder takes it through.
    struct Gpu {
        device: ID3D11Device,
        manager: IMFDXGIDeviceManager,
    }

    /// What every open decoder shares: Media Foundation, started, and one GPU device. It lives
    /// as long as a decoder holds it, and no longer: once the last decoder closes, the device
    /// goes, with every surface, sample pool and staging texture the decoders made on it, and
    /// Media Foundation is shut down, so a video played and closed leaves nothing behind. A
    /// device per decoder, as before, made the driver keep its pools once for each decoder
    /// that ran at once.
    struct Platform {
        gpu: Option<Gpu>,
    }

    // SAFETY: the device is multithread protected and the device manager is free threaded;
    // both are used through their own locks.
    unsafe impl Send for Platform {}
    // SAFETY: as above.
    unsafe impl Sync for Platform {}

    impl Platform {
        fn new() -> Result<Platform, String> {
            // SAFETY: Media Foundation's startup, which counts; each Platform shuts it down
            // once when it goes.
            unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
                .map_err(|error| format!("Media Foundation did not start: {error}"))?;
            match gpu() {
                Ok(gpu) => Ok(Platform { gpu: Some(gpu) }),
                Err(why) => {
                    // SAFETY: pairs the startup above.
                    let _ = unsafe { MFShutdown() };
                    Err(why)
                }
            }
        }

        /// The device the decoders decode on, which pictures are read back through.
        fn device(&self) -> Option<&ID3D11Device> {
            self.gpu.as_ref().map(|gpu| &gpu.device)
        }

        fn manager(&self) -> &IMFDXGIDeviceManager {
            &self
                .gpu
                .as_ref()
                .expect("a live platform has its device")
                .manager
        }

        /// Whether a GPU reset removed the device, so no new decoder can use it.
        fn removed(&self) -> bool {
            self.gpu
                .as_ref()
                // SAFETY: a plain query of a live device.
                .is_none_or(|gpu| unsafe { gpu.device.GetDeviceRemovedReason() }.is_err())
        }

        /// The platform the open decoders share, or a new one when none is open (or the one
        /// they share lost its device to a GPU reset).
        fn shared(slot: &Mutex<Weak<Platform>>) -> Result<Arc<Platform>, String> {
            let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(platform) = slot.upgrade().filter(|p| !p.removed()) {
                return Ok(platform);
            }
            let platform = Arc::new(Platform::new()?);
            *slot = Arc::downgrade(&platform);
            Ok(platform)
        }
    }

    impl Drop for Platform {
        fn drop(&mut self) {
            if let Some(gpu) = self.gpu.take() {
                // SAFETY: plain calls on a device nothing else uses now. Direct3D destroys
                // released resources when the context next flushes, and the driver keeps
                // pools beyond live resources until trimmed; both go before the device does.
                unsafe {
                    if let Ok(context) = gpu.device.GetImmediateContext() {
                        context.ClearState();
                        context.Flush();
                    }
                    if let Ok(dxgi) = gpu.device.cast::<IDXGIDevice3>() {
                        dxgi.Trim();
                    }
                }
                // The manager holds the device, so it goes first.
                let Gpu { device, manager } = gpu;
                drop(manager);
                drop(device);
            }
            // SAFETY: pairs the startup in `new`, after everything made with it is released.
            let _ = unsafe { MFShutdown() };
        }
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
        Ok(Gpu { device, manager })
    }

    /// A decoder transform and the activation object it came from. Going, it is taken out of
    /// streaming, made to let go of the GPU's device manager, and shut down, as Media
    /// Foundation asks of an activated object: a transform only released can keep its
    /// surfaces, its sample pool and its hold on the device until the process ends.
    struct Transform {
        transform: IMFTransform,
        activate: IMFActivate,
    }

    impl std::ops::Deref for Transform {
        type Target = IMFTransform;
        fn deref(&self) -> &IMFTransform {
            &self.transform
        }
    }

    impl Drop for Transform {
        fn drop(&mut self) {
            // SAFETY: plain Media Foundation calls on a transform no other thread uses; each
            // may fail on a transform that never streamed, which changes nothing.
            unsafe {
                let t = &self.transform;
                let _ = t.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
                let _ = t.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = t.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
                let _ = t.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
                if let Ok(shutdown) = t.cast::<IMFShutdown>() {
                    let _ = shutdown.Shutdown();
                }
                let _ = self.activate.ShutdownObject();
            }
        }
    }

    /// The first decoder transform for `codec`, in the order Windows ranks them.
    fn transform(codec: Codec) -> Result<Transform, String> {
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
                    found = unsafe { activate.ActivateObject::<IMFTransform>() }
                        .ok()
                        .map(|transform| Transform {
                            transform,
                            activate,
                        });
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

    /// The display area an output type gives, when it gives one: the minimum display
    /// aperture, else the geometric or the pan and scan aperture.
    fn aperture(media_type: &IMFMediaType) -> Option<Area> {
        for key in [
            &MF_MT_MINIMUM_DISPLAY_APERTURE,
            &MF_MT_GEOMETRIC_APERTURE,
            &MF_MT_PAN_SCAN_APERTURE,
        ] {
            let mut area = MFVideoArea::default();
            let mut len = 0u32;
            // SAFETY: the blob is an MFVideoArea, written into one.
            let read = unsafe {
                media_type.GetBlob(
                    key,
                    std::slice::from_raw_parts_mut(
                        (&mut area as *mut MFVideoArea).cast::<u8>(),
                        std::mem::size_of::<MFVideoArea>(),
                    ),
                    Some(&mut len),
                )
            };
            // The area's offset is where the picture starts (a stream cropped at its top or
            // left); its whole part, as cropping is in whole samples.
            if read.is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
                return Some(Area {
                    left: area.OffsetX.value.max(0) as u32,
                    top: area.OffsetY.value.max(0) as u32,
                    width: area.Area.cx as u32,
                    height: area.Area.cy as u32,
                });
            }
        }
        None
    }

    /// The output the decoder gives: its surfaces' format, coded size and display area.
    #[derive(Clone, Copy)]
    struct Output {
        format: SurfaceFormat,
        coded: Size,
        aperture: Option<Area>,
    }

    /// Chooses NV12 among the decoder's output types, else P010 (a 10-bit stream, whose
    /// decoder offers no 8-bit output), which is sent on as 8-bit.
    fn choose_output(decoder: &IMFTransform) -> Result<Output, String> {
        let mut p010 = None;
        for index in 0.. {
            // SAFETY: plain Media Foundation calls.
            let available = match unsafe { decoder.GetOutputAvailableType(0, index) } {
                Ok(available) => available,
                Err(_) => break,
            };
            // SAFETY: as above.
            let subtype = unsafe { available.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or_default();
            if subtype == MFVideoFormat_NV12 {
                return set_output(decoder, &available, SurfaceFormat::Nv12);
            }
            if subtype == MFVideoFormat_P010 && p010.is_none() {
                p010 = Some(available);
            }
        }
        match p010 {
            Some(available) => set_output(decoder, &available, SurfaceFormat::P010),
            None => Err("the decoder offers neither NV12 nor P010 output".into()),
        }
    }

    fn set_output(
        decoder: &IMFTransform,
        available: &IMFMediaType,
        format: SurfaceFormat,
    ) -> Result<Output, String> {
        // SAFETY: plain Media Foundation calls.
        unsafe { decoder.SetOutputType(0, available, 0) }
            .map_err(|e| format!("the decoder did not take {format:?} output: {e}"))?;
        // SAFETY: as above.
        let size = unsafe { available.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
        Ok(Output {
            format,
            coded: ((size >> 32) as u32, size as u32),
            aperture: aperture(available),
        })
    }

    /// The texture a decoded sample's first buffer is, and which of its subresources holds the
    /// picture (a decoder's surfaces are often one texture array), when it is one.
    fn sample_texture(sample: &IMFSample) -> Option<(ID3D11Texture2D, u32)> {
        // SAFETY: plain Media Foundation calls on interfaces the sample holds.
        unsafe {
            let buffer = sample.GetBufferByIndex(0).ok()?;
            let dxgi = buffer.cast::<IMFDXGIBuffer>().ok()?;
            let mut texture: Option<ID3D11Texture2D> = None;
            dxgi.GetResource(
                &ID3D11Texture2D::IID,
                (&mut texture as *mut Option<ID3D11Texture2D>).cast(),
            )
            .ok()?;
            let index = dxgi.GetSubresourceIndex().ok()?;
            Some((texture?, index))
        }
    }

    /// The texture a picture is copied into to be read: made once for a surface size and
    /// format, and used for every picture of that size, so no picture makes one (Media
    /// Foundation's own lock of a surface did).
    struct Staging {
        texture: ID3D11Texture2D,
        width: u32,
        rows: u32,
        format: DXGI_FORMAT,
    }

    /// The height of the texture a decoded sample's first buffer is, when it is one.
    fn texture_rows(sample: &IMFSample) -> Option<usize> {
        // SAFETY: plain Media Foundation and Direct3D calls on interfaces the sample holds.
        unsafe {
            let buffer = sample.GetBufferByIndex(0).ok()?;
            let dxgi = buffer.cast::<IMFDXGIBuffer>().ok()?;
            let mut texture: Option<ID3D11Texture2D> = None;
            dxgi.GetResource(
                &ID3D11Texture2D::IID,
                (&mut texture as *mut Option<ID3D11Texture2D>).cast(),
            )
            .ok()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture?.GetDesc(&mut desc);
            Some(desc.Height as usize)
        }
    }

    /// One decoder on the GPU.
    struct MediaFoundationDecoder {
        codec: Codec,
        /// Declared before the platform, so it goes first: the transform holds the device.
        decoder: Transform,
        platform: Arc<Platform>,
        output: Output,
        /// The size the guest opened the stream with, and the sizes the stream's own headers
        /// gave (VP9, AV1), by packet, for decoders that report no display area.
        opened: Size,
        stream: StreamSizes,
        provides_samples: bool,
        sample_size: u32,
        /// The texture pictures are read back through, once one was needed.
        staging: Option<Staging>,
        /// What the reads back took since the stream's numbers were last logged.
        report: DecoderReport,
    }

    // SAFETY: the transform and the device are free threaded (the device is made
    // multithread protected), and each decoder is used by one thread at a time.
    unsafe impl Send for MediaFoundationDecoder {}

    impl MediaFoundationDecoder {
        fn open(
            codec: Codec,
            width: u32,
            height: u32,
            shared: &Mutex<Weak<Platform>>,
        ) -> Result<MediaFoundationDecoder, String> {
            // SAFETY: this thread's COM, which it keeps for its life.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let platform = Platform::shared(shared)?;
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
            // SAFETY: the manager outlives the decoder (the decoder holds the platform).
            unsafe {
                decoder.ProcessMessage(
                    MFT_MESSAGE_SET_D3D_MANAGER,
                    platform.manager().as_raw() as usize,
                )
            }
            .map_err(|e| format!("the {} decoder did not take the GPU: {e}", codec.name()))?;
            let input = video_type(&subtype(codec), width, height)?;
            // SAFETY: plain Media Foundation calls.
            unsafe { decoder.SetInputType(0, &input, 0) }.map_err(|e| {
                format!("the {} decoder did not take the stream: {e}", codec.name())
            })?;
            let output = choose_output(&decoder)?;
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
                platform,
                output,
                opened: (width, height),
                stream: StreamSizes::default(),
                provides_samples: info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                sample_size: info.cbSize,
                staging: None,
                report: DecoderReport::default(),
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
                        self.output = choose_output(&self.decoder)?;
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

        /// The visible area of the picture of packet `id`: the decoder's display area, else
        /// the size the stream's own headers gave for that packet, else the size the guest
        /// opened the stream with when the coded size is only that rounded up to the codec's
        /// blocks, else the coded size.
        fn shown(&self, id: u64) -> Area {
            let coded = self.output.coded;
            let opened = Some(self.opened).filter(|&(w, h)| {
                w <= coded.0 && h <= coded.1 && coded.0 - w < 64 && coded.1 - h < 64
            });
            visible_area(
                coded,
                self.output.aperture,
                self.stream.for_picture(id).or(opened),
            )
        }

        /// The staging texture for a surface of `width` by `rows` rows (already aligned, see
        /// [`staging_rows`]) in `format`: the one made before when it fits, else a new one.
        fn staging_for(
            &mut self,
            width: u32,
            rows: u32,
            format: DXGI_FORMAT,
        ) -> Option<ID3D11Texture2D> {
            if let Some(staging) = &self.staging {
                if staging.width == width && staging.rows == rows && staging.format == format {
                    return Some(staging.texture.clone());
                }
            }
            // A new size lets the old texture go first.
            self.staging = None;
            let device = self.platform.device()?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: rows,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut texture = None;
            // SAFETY: a plain Direct3D call; the out pointer is a local.
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.ok()?;
            let texture = texture?;
            self.staging = Some(Staging {
                texture: texture.clone(),
                width,
                rows,
                format,
            });
            Some(texture)
        }

        /// Reads the picture in `area` of the sample's texture through the staging texture:
        /// the area is copied to the staging texture's corner, whose height is whole aligned
        /// rows, so its chroma plane starts where its height says on every driver (see
        /// [`staging_rows`]). `None` when the sample is not a texture this can read, and the
        /// caller reads it through Media Foundation's lock instead.
        fn read_staged(&mut self, sample: &IMFSample, id: u64, area: Area) -> Option<Picture> {
            let (texture, index) = sample_texture(sample)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: a plain query of a live texture.
            unsafe { texture.GetDesc(&mut desc) };
            let format = match desc.Format {
                DXGI_FORMAT_NV12 => SurfaceFormat::Nv12,
                DXGI_FORMAT_P010 => SurfaceFormat::P010,
                _ => return None,
            };
            let (right, bottom) = area.reach();
            if right > desc.Width || bottom > desc.Height {
                return None;
            }
            let rows = staging_rows(desc.Height as usize) as u32;
            let staging = self.staging_for(desc.Width, rows, desc.Format)?;
            let device = self.platform.device()?;
            // SAFETY: plain Direct3D calls on live textures of the decoder's device, which is
            // multithread protected; the mapping is read within its length and unmapped
            // before it goes.
            unsafe {
                let context = device.GetImmediateContext().ok()?;
                let reach = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right,
                    bottom,
                    back: 1,
                };
                context.CopySubresourceRegion(
                    &staging,
                    0,
                    0,
                    0,
                    0,
                    &texture,
                    index,
                    Some(&reach as *const _),
                );
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .ok()?;
                let pitch = mapped.RowPitch as usize;
                let chroma = mapped_chroma_row(rows as usize, pitch, mapped.DepthPitch as usize);
                let len = pitch * (chroma + chroma.div_ceil(2));
                let picture = if mapped.pData.is_null() || pitch == 0 {
                    None
                } else {
                    let bytes = std::slice::from_raw_parts(mapped.pData as *const u8, len);
                    picture_from_surface_area(id, bytes, format, pitch, chroma, area)
                };
                context.Unmap(&staging, 0);
                self.report.surface = Some(format!(
                    "{}x{} {format:?} (array slice {index}), read through {}x{rows}, pitch \
                     {pitch}, mapped size {}, chroma at row {chroma}; picture {}x{} at {},{}",
                    desc.Width,
                    desc.Height,
                    desc.Width,
                    mapped.DepthPitch,
                    area.width,
                    area.height,
                    area.left,
                    area.top,
                ));
                picture
            }
        }

        fn read(&mut self, sample: &IMFSample) -> Result<Option<Picture>, String> {
            let started = Instant::now();
            let picture = self.read_unclocked(sample);
            self.report.readback += started.elapsed();
            self.report.readbacks += 1;
            picture
        }

        fn read_unclocked(&mut self, sample: &IMFSample) -> Result<Option<Picture>, String> {
            let format = self.output.format;
            // SAFETY: a plain Media Foundation call.
            let id = unsafe { sample.GetSampleTime() }.map_err(|e| e.to_string())? as u64;
            let area = self.shown(id);
            if let Some(picture) = self.read_staged(sample, id, area) {
                return Ok(Some(picture));
            }
            // The rows the picture reaches down to, its offset included.
            let bottom = (area.top + area.height) as usize;
            let coded_rows = self.output.coded.1 as usize;
            // The chroma plane follows the texture's own rows, which a driver may make taller
            // than the coded size (for the largest picture it expects). Taking it after the
            // coded rows instead read the zero padding: green pictures, or a green band.
            let surface_rows = texture_rows(sample);
            // SAFETY: plain Media Foundation calls; the buffer is unlocked before it goes.
            unsafe {
                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .map_err(|e| e.to_string())?;
                if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                    let mut data = std::ptr::null_mut();
                    let mut pitch = 0i32;
                    two_d
                        .Lock2D(&mut data, &mut pitch)
                        .map_err(|e| format!("cannot read the picture: {e}"))?;
                    let pitch = pitch.unsigned_abs() as usize;
                    let rows = chroma_start_row(surface_rows, coded_rows, bottom);
                    let len = pitch * (rows + rows.div_ceil(2));
                    let bytes = std::slice::from_raw_parts(data, len);
                    let picture = picture_from_surface_area(id, bytes, format, pitch, rows, area);
                    let _ = two_d.Unlock2D();
                    self.report.surface = Some(format!(
                        "read through Media Foundation's lock, pitch {pitch}, chroma at row \
                         {rows}; picture {}x{} at {},{}",
                        area.width, area.height, area.left, area.top,
                    ));
                    return Ok(picture);
                }
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                buffer
                    .Lock(&mut data, None, Some(&mut len))
                    .map_err(|e| format!("cannot read the picture: {e}"))?;
                let sample_bytes = match format {
                    SurfaceFormat::Nv12 => 1,
                    SurfaceFormat::P010 => 2,
                };
                let pitch = self.output.coded.0.max(area.left + area.width) as usize * sample_bytes;
                // A buffer in system memory is packed: its length says how many rows it has.
                let rows = chroma_start_row(
                    surface_rows.or_else(|| rows_from_length(len as usize, pitch)),
                    coded_rows,
                    bottom,
                );
                let bytes = std::slice::from_raw_parts(data, len as usize);
                let picture = picture_from_surface_area(id, bytes, format, pitch, rows, area);
                let _ = buffer.Unlock();
                Ok(picture)
            }
        }
    }

    impl VideoDecoder for MediaFoundationDecoder {
        fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
            // A VP9 key frame or an AV1 sequence header gives the picture's size, which their
            // decoders may not report.
            if let Some(size) = stream_frame_size(self.codec, &packet.data) {
                self.stream.record(packet.id, size);
            }
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

        fn take_report(&mut self) -> DecoderReport {
            std::mem::take(&mut self.report)
        }
    }

    /// The host's decoders: the formats whose Media Foundation decoder takes the GPU.
    ///
    /// Nothing is loaded until the guest first asks what it can decode; then a child process
    /// finds out (see [`PROBE_ARGUMENT`](super::PROBE_ARGUMENT)) and the answer is kept. A
    /// decoder is made only when the guest opens one, and freed when it closes; the GPU device
    /// and Media Foundation that the open decoders share go with the last of them.
    pub struct MediaFoundationDecoders {
        supported: OnceLock<Vec<CodecSupport>>,
        log: Box<dyn Fn(&str) + Send + Sync>,
        /// What the open decoders share, while any is open.
        platform: Mutex<Weak<Platform>>,
        /// A decoder was opened since the last time every one closed.
        opened: AtomicBool,
        /// Told once every decoder has closed and what they shared is gone.
        let_go: Box<dyn Fn() + Send + Sync>,
    }

    /// How long the probe may take before it is given up, and no format is offered.
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Opens a decoder of each format in this process and prints a line for each. What
    /// `dsa-wm --probe-video` runs.
    pub fn probe_here() -> String {
        let mut out = String::new();
        let shared = Mutex::new(Weak::new());
        for codec in super::ALL_CODECS {
            let result = MediaFoundationDecoder::open(codec, 1280, 720, &shared).map(drop);
            out.push_str(&super::probe_line(codec, &result));
            out.push('\n');
        }
        out
    }

    /// Runs this program with the probe argument and returns what it printed.
    fn probe_in_child() -> Result<String, String> {
        crate::child_probe::run_self(&[super::PROBE_ARGUMENT.to_string()], PROBE_TIMEOUT)
            .map(|output| output.stdout)
    }

    impl MediaFoundationDecoders {
        /// Decoders that find their formats when first asked. `log` takes a line for each
        /// format then. `let_go` is called each time the last open decoder has closed and
        /// the device and Media Foundation they shared are released, so the caller can give
        /// back the memory that freed.
        pub fn lazy(
            log: Box<dyn Fn(&str) + Send + Sync>,
            let_go: Box<dyn Fn() + Send + Sync>,
        ) -> MediaFoundationDecoders {
            MediaFoundationDecoders {
                supported: OnceLock::new(),
                log,
                platform: Mutex::new(Weak::new()),
                opened: AtomicBool::new(false),
                let_go,
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
            self.opened.store(true, Ordering::SeqCst);
            Ok(Box::new(MediaFoundationDecoder::open(
                codec,
                width,
                height,
                &self.platform,
            )?))
        }

        fn all_closed(&self) {
            // A connection that only asked what the host decodes opened nothing to let go.
            if !self.opened.swap(false, Ordering::SeqCst) {
                return;
            }
            // A decoder opened since the last connection ended keeps what it shares.
            let alive = self
                .platform
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .strong_count()
                > 0;
            if alive {
                self.opened.store(true, Ordering::SeqCst);
                return;
            }
            (self.log)(
                "video: the last decoder closed; its GPU device and Media Foundation were let go",
            );
            (self.let_go)();
        }
    }
}
