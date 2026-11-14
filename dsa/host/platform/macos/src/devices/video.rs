//! The guest's video, decoded with VideoToolbox on the Mac's media engine.
//!
//! The guest's codec HAL sends an H.264 or HEVC stream as Android's `MediaCodec` takes it:
//! Annex B, each NAL unit after a start code, with the parameter sets in a codec
//! configuration packet or in line before a key frame. VideoToolbox takes a format
//! description made from the parameter sets and each picture's NAL units with a 4-byte
//! length in front of each, so the stream is split here ([`nal_units`]), the parameter sets
//! are kept ([`ParameterSets`]) and the rest is rewritten ([`length_prefixed`]).
//!
//! A format is offered to the guest only when the Mac decodes it in hardware
//! (`VTIsHardwareDecodeSupported`), and each session asks for the hardware decoder: one that
//! would decode on the processor is left to the guest's own software decoder, which costs the
//! same and keeps the Mac's processor free. Pictures come out as bi-planar 4:2:0 video range
//! (`420v`), which is NV12, in presentation order (VideoToolbox's temporal processing, by the
//! time the guest gave each packet).
//!
//! VP9 and AV1 are not offered: VideoToolbox decodes them only through a supplemental
//! decoder with its own configuration records, so the guest's software decoders keep them.

use dsa_host_core::video::Codec;

/// One NAL unit's role, as far as the decoder needs to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalKind {
    /// HEVC's video parameter set.
    Vps,
    Sps,
    Pps,
    /// An access unit delimiter, which VideoToolbox does not need.
    Delimiter,
    /// Anything else: slices, SEI, the end of a sequence.
    Other,
}

/// The kind of the NAL unit `nal` in `codec`'s syntax.
pub fn nal_kind(codec: Codec, nal: &[u8]) -> NalKind {
    let Some(&header) = nal.first() else {
        return NalKind::Other;
    };
    match codec {
        Codec::H264 => match header & 0x1f {
            7 => NalKind::Sps,
            8 => NalKind::Pps,
            9 => NalKind::Delimiter,
            _ => NalKind::Other,
        },
        Codec::Hevc => match (header >> 1) & 0x3f {
            32 => NalKind::Vps,
            33 => NalKind::Sps,
            34 => NalKind::Pps,
            35 => NalKind::Delimiter,
            _ => NalKind::Other,
        },
        Codec::Vp9 | Codec::Av1 => NalKind::Other,
    }
}

/// Where the start code at `at` ends, if one starts there (`00 00 01` or `00 00 00 01`).
fn start_code_end(data: &[u8], at: usize) -> Option<usize> {
    match data.get(at..) {
        Some([0, 0, 1, ..]) => Some(at + 3),
        Some([0, 0, 0, 1, ..]) => Some(at + 4),
        _ => None,
    }
}

/// The NAL units of `data`. Annex B (start codes) is what the guest sends; a packet without a
/// start code in front is read as NAL units with a 4-byte big-endian length each when that
/// fits it exactly, and as one NAL unit otherwise. Empty units are left out.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    if start_code_end(data, 0).is_none() {
        if let Some(units) = length_prefixed_units(data) {
            return units;
        }
        return if data.is_empty() {
            Vec::new()
        } else {
            vec![data]
        };
    }
    let mut units = Vec::new();
    let mut start = None;
    let mut at = 0;
    while at < data.len() {
        if let Some(end) = start_code_end(data, at) {
            if let Some(from) = start {
                units.push(&data[from..at]);
            }
            start = Some(end);
            at = end;
        } else {
            at += 1;
        }
    }
    if let Some(from) = start {
        units.push(&data[from..]);
    }
    units
        .into_iter()
        // The zero bytes before a 4-byte start code belong to neither unit.
        .map(|unit| {
            let mut end = unit.len();
            while end > 0 && unit[end - 1] == 0 {
                end -= 1;
            }
            &unit[..end]
        })
        .filter(|unit| !unit.is_empty())
        .collect()
}

fn length_prefixed_units(data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut units = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let (len, after) = rest.split_first_chunk::<4>()?;
        let len = u32::from_be_bytes(*len) as usize;
        if len == 0 || len > after.len() {
            return None;
        }
        units.push(&after[..len]);
        rest = &after[len..];
    }
    (!units.is_empty()).then_some(units)
}

/// The NAL units that make a picture, each with a 4-byte big-endian length in front, as
/// VideoToolbox reads a sample. Parameter sets and delimiters are left out: the parameter
/// sets are in the format description.
pub fn length_prefixed(codec: Codec, units: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for unit in units {
        if nal_kind(codec, unit) != NalKind::Other {
            continue;
        }
        out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        out.extend_from_slice(unit);
    }
    out
}

/// The parameter sets of a stream, the latest of each kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParameterSets {
    pub vps: Option<Vec<u8>>,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

impl ParameterSets {
    /// Takes the parameter sets among `units`. True when any of them changed.
    pub fn take(&mut self, codec: Codec, units: &[&[u8]]) -> bool {
        let mut changed = false;
        for unit in units {
            let slot = match nal_kind(codec, unit) {
                NalKind::Vps => &mut self.vps,
                NalKind::Sps => &mut self.sps,
                NalKind::Pps => &mut self.pps,
                _ => continue,
            };
            if slot.as_deref() != Some(*unit) {
                *slot = Some(unit.to_vec());
                changed = true;
            }
        }
        changed
    }

    /// The sets a format description is made from, in the order VideoToolbox takes them:
    /// SPS and PPS for H.264, VPS, SPS and PPS for HEVC. `None` until all are known.
    pub fn for_format(&self, codec: Codec) -> Option<Vec<&[u8]>> {
        let (sps, pps) = (self.sps.as_deref()?, self.pps.as_deref()?);
        match codec {
            Codec::H264 => Some(vec![sps, pps]),
            Codec::Hevc => Some(vec![self.vps.as_deref()?, sps, pps]),
            Codec::Vp9 | Codec::Av1 => None,
        }
    }
}

#[cfg(target_os = "macos")]
pub use toolbox::VideoToolboxDecoders;

#[cfg(target_os = "macos")]
mod toolbox {
    use std::ffi::c_void;
    use std::sync::Mutex;

    use dsa_host_core::video::{Codec, CodecSupport, Packet, Picture, VideoDecoder, VideoDecoders};

    use super::{length_prefixed, nal_units, ParameterSets};
    use crate::devices::camera::{pack_nv12, Plane};
    use crate::security::{
        kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks, CFDictionaryCreate,
    };

    type OSStatus = i32;
    type CFTypeRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CMTime {
        value: i64,
        timescale: i32,
        flags: u32,
        epoch: i64,
    }

    /// `kCMTimeFlags_Valid`.
    const TIME_VALID: u32 = 1;

    #[repr(C)]
    struct SampleTiming {
        duration: CMTime,
        presentation: CMTime,
        decode: CMTime,
    }

    type OutputCallback = extern "C" fn(
        refcon: *mut c_void,
        frame: *mut c_void,
        status: OSStatus,
        flags: u32,
        image: *const c_void,
        presentation: CMTime,
        duration: CMTime,
    );

    #[repr(C)]
    struct OutputCallbackRecord {
        callback: OutputCallback,
        refcon: *mut c_void,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
        fn CFRelease(object: CFTypeRef);
    }

    #[link(name = "CoreMedia", kind = "framework")]
    extern "C" {
        fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
            allocator: CFTypeRef,
            count: usize,
            pointers: *const *const u8,
            sizes: *const usize,
            nal_length_size: i32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
            allocator: CFTypeRef,
            count: usize,
            pointers: *const *const u8,
            sizes: *const usize,
            nal_length_size: i32,
            extensions: CFTypeRef,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn CMBlockBufferCreateWithMemoryBlock(
            allocator: CFTypeRef,
            memory: *mut c_void,
            length: usize,
            block_allocator: CFTypeRef,
            source: *const c_void,
            offset: usize,
            data_length: usize,
            flags: u32,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn CMBlockBufferReplaceDataBytes(
            source: *const c_void,
            destination: CFTypeRef,
            offset: usize,
            length: usize,
        ) -> OSStatus;
        fn CMSampleBufferCreateReady(
            allocator: CFTypeRef,
            data: CFTypeRef,
            format: CFTypeRef,
            samples: isize,
            timing_count: isize,
            timing: *const SampleTiming,
            size_count: isize,
            sizes: *const usize,
            out: *mut CFTypeRef,
        ) -> OSStatus;
    }

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
        fn CVPixelBufferLockBaseAddress(buffer: *const c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *const c_void, flags: u64) -> i32;
        fn CVPixelBufferGetWidth(buffer: *const c_void) -> usize;
        fn CVPixelBufferGetHeight(buffer: *const c_void) -> usize;
        fn CVPixelBufferGetPlaneCount(buffer: *const c_void) -> usize;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: *const c_void, plane: usize) -> *const u8;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: *const c_void, plane: usize) -> usize;
        fn CVPixelBufferGetHeightOfPlane(buffer: *const c_void, plane: usize) -> usize;
    }

    #[link(name = "VideoToolbox", kind = "framework")]
    extern "C" {
        static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: CFTypeRef;
        fn VTIsHardwareDecodeSupported(codec: u32) -> u8;
        fn VTDecompressionSessionCreate(
            allocator: CFTypeRef,
            format: CFTypeRef,
            decoder_specification: CFTypeRef,
            image_attributes: CFTypeRef,
            output: *const OutputCallbackRecord,
            out: *mut CFTypeRef,
        ) -> OSStatus;
        fn VTDecompressionSessionDecodeFrame(
            session: CFTypeRef,
            sample: CFTypeRef,
            flags: u32,
            frame: *mut c_void,
            info: *mut u32,
        ) -> OSStatus;
        fn VTDecompressionSessionFinishDelayedFrames(session: CFTypeRef) -> OSStatus;
        fn VTDecompressionSessionWaitForAsynchronousFrames(session: CFTypeRef) -> OSStatus;
        fn VTDecompressionSessionCanAcceptFormatDescription(
            session: CFTypeRef,
            format: CFTypeRef,
        ) -> u8;
        fn VTDecompressionSessionInvalidate(session: CFTypeRef);
    }

    /// `kCMVideoCodecType_H264` and `kCMVideoCodecType_HEVC`.
    const AVC1: u32 = u32::from_be_bytes(*b"avc1");
    const HVC1: u32 = u32::from_be_bytes(*b"hvc1");
    /// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`: NV12.
    const NV12_420V: i32 = i32::from_be_bytes(*b"420v");
    /// `kCFNumberSInt32Type`.
    const SINT32: isize = 3;
    /// `kVTDecodeFrame_EnableTemporalProcessing`: pictures out in presentation order.
    const TEMPORAL: u32 = 1 << 3;
    /// `kCMBlockBufferAssureMemoryNowFlag`.
    const ASSURE_MEMORY: u32 = 1;
    const LOCK_READ_ONLY: u64 = 1;

    /// The largest picture each format is offered for.
    const MAX_SIDE: u32 = 4096;
    const MAX_SIDE_HEVC: u32 = 8192;

    /// What the output callback hands back.
    #[derive(Default)]
    struct Output {
        pictures: Vec<Picture>,
        error: Option<String>,
    }

    unsafe fn copy_picture(id: u64, image: *const c_void) -> Option<Picture> {
        if image.is_null() || CVPixelBufferGetPlaneCount(image) != 2 {
            return None;
        }
        if CVPixelBufferLockBaseAddress(image, LOCK_READ_ONLY) != 0 {
            return None;
        }
        let width = CVPixelBufferGetWidth(image) as u32;
        let height = CVPixelBufferGetHeight(image) as u32;
        let plane = |index: usize| -> Option<Plane<'static>> {
            let base = CVPixelBufferGetBaseAddressOfPlane(image, index);
            if base.is_null() {
                return None;
            }
            let bytes_per_row = CVPixelBufferGetBytesPerRowOfPlane(image, index);
            let rows = CVPixelBufferGetHeightOfPlane(image, index);
            Some(Plane {
                bytes: std::slice::from_raw_parts(base, bytes_per_row * rows),
                bytes_per_row,
            })
        };
        let frame = match (plane(0), plane(1)) {
            (Some(luma), Some(chroma)) => pack_nv12(width, height, luma, chroma, 0),
            _ => None,
        };
        CVPixelBufferUnlockBaseAddress(image, LOCK_READ_ONLY);
        frame.map(|frame| Picture {
            id,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            nv12: frame.nv12,
        })
    }

    extern "C" fn did_decode(
        refcon: *mut c_void,
        frame: *mut c_void,
        status: OSStatus,
        _flags: u32,
        image: *const c_void,
        _presentation: CMTime,
        _duration: CMTime,
    ) {
        // A panic must not unwind into VideoToolbox.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: the refcon is the decoder's boxed output, which outlives its session.
            let output = unsafe { &*(refcon as *const Mutex<Output>) };
            let Ok(mut output) = output.lock() else {
                return;
            };
            if status != 0 {
                output.error = Some(format!(
                    "the Mac's decoder could not decode a picture (status {status})"
                ));
                return;
            }
            // No picture with a good status: a frame the decoder dropped.
            if let Some(picture) = unsafe { copy_picture(frame as u64, image) } {
                output.pictures.push(picture);
            }
        }));
    }

    /// The attributes a session is made with: NV12 out, and only the hardware decoder.
    unsafe fn session_dictionaries() -> (CFTypeRef, CFTypeRef) {
        let keys = [kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder];
        let values = [kCFBooleanTrue];
        let specification = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        let format = CFNumberCreate(
            std::ptr::null(),
            SINT32,
            &NV12_420V as *const i32 as *const c_void,
        );
        let keys = [kCVPixelBufferPixelFormatTypeKey];
        let values = [format];
        let attributes = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if !format.is_null() {
            CFRelease(format);
        }
        (specification, attributes)
    }

    unsafe fn release(object: CFTypeRef) {
        if !object.is_null() {
            CFRelease(object);
        }
    }

    /// One VideoToolbox session, made when the stream's parameter sets are known.
    struct ToolboxDecoder {
        codec: Codec,
        sets: ParameterSets,
        format: CFTypeRef,
        session: CFTypeRef,
        /// Boxed so its address, the callback's refcon, stays put.
        output: Box<Mutex<Output>>,
    }

    // The session and the format description are used from the connection's thread only;
    // VideoToolbox's own threads reach only `output`, through its lock.
    unsafe impl Send for ToolboxDecoder {}

    impl ToolboxDecoder {
        fn take_output(&self) -> Result<Vec<Picture>, String> {
            let mut output = self.output.lock().map_err(|_| "the decoder failed")?;
            let pictures = std::mem::take(&mut output.pictures);
            match output.error.take() {
                Some(why) if pictures.is_empty() => Err(why),
                _ => Ok(pictures),
            }
        }

        unsafe fn make_format(&self) -> Result<CFTypeRef, String> {
            let sets = self
                .sets
                .for_format(self.codec)
                .ok_or("the stream's parameter sets have not come yet")?;
            let pointers: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
            let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
            let mut format: CFTypeRef = std::ptr::null();
            let status = match self.codec {
                Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    std::ptr::null(),
                    sets.len(),
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    4,
                    &mut format,
                ),
                _ => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    std::ptr::null(),
                    sets.len(),
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    4,
                    std::ptr::null(),
                    &mut format,
                ),
            };
            if status != 0 || format.is_null() {
                return Err(format!(
                    "the stream's parameter sets are not ones the Mac can decode (status {status})"
                ));
            }
            Ok(format)
        }

        /// Ends the session, handing back the pictures it still held.
        unsafe fn end_session(&mut self) {
            if !self.session.is_null() {
                VTDecompressionSessionFinishDelayedFrames(self.session);
                VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                self.session = std::ptr::null();
            }
        }

        /// Makes the format description from new parameter sets, and a session for it when
        /// the one running cannot take it.
        unsafe fn reconfigure(&mut self) -> Result<(), String> {
            let format = self.make_format()?;
            if !self.session.is_null()
                && VTDecompressionSessionCanAcceptFormatDescription(self.session, format) == 0
            {
                self.end_session();
            }
            release(self.format);
            self.format = format;
            if self.session.is_null() {
                let (specification, attributes) = session_dictionaries();
                let record = OutputCallbackRecord {
                    callback: did_decode,
                    refcon: &*self.output as *const Mutex<Output> as *mut c_void,
                };
                let mut session: CFTypeRef = std::ptr::null();
                let status = VTDecompressionSessionCreate(
                    std::ptr::null(),
                    self.format,
                    specification,
                    attributes,
                    &record,
                    &mut session,
                );
                release(specification);
                release(attributes);
                if status != 0 || session.is_null() {
                    return Err(format!(
                        "the Mac's hardware decoder could not start for {} (status {status})",
                        self.codec.name()
                    ));
                }
                self.session = session;
            }
            Ok(())
        }

        unsafe fn decode_sample(&mut self, packet: &Packet, data: &[u8]) -> Result<(), String> {
            let mut block: CFTypeRef = std::ptr::null();
            let status = CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null(),
                std::ptr::null_mut(),
                data.len(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                data.len(),
                ASSURE_MEMORY,
                &mut block,
            );
            if status != 0 || block.is_null() {
                return Err(format!("no memory for a packet (status {status})"));
            }
            let status = CMBlockBufferReplaceDataBytes(data.as_ptr().cast(), block, 0, data.len());
            if status != 0 {
                release(block);
                return Err(format!("a packet could not be copied (status {status})"));
            }
            let invalid = CMTime {
                value: 0,
                timescale: 0,
                flags: 0,
                epoch: 0,
            };
            let timing = SampleTiming {
                duration: invalid,
                presentation: CMTime {
                    value: packet.timestamp_us,
                    timescale: 1_000_000,
                    flags: TIME_VALID,
                    epoch: 0,
                },
                decode: invalid,
            };
            let size = data.len();
            let mut sample: CFTypeRef = std::ptr::null();
            let status = CMSampleBufferCreateReady(
                std::ptr::null(),
                block,
                self.format,
                1,
                1,
                &timing,
                1,
                &size,
                &mut sample,
            );
            release(block);
            if status != 0 || sample.is_null() {
                return Err(format!("a packet could not be wrapped (status {status})"));
            }
            let mut info = 0u32;
            let status = VTDecompressionSessionDecodeFrame(
                self.session,
                sample,
                TEMPORAL,
                packet.id as usize as *mut c_void,
                &mut info,
            );
            release(sample);
            if status != 0 {
                return Err(format!(
                    "the Mac's decoder refused a packet (status {status})"
                ));
            }
            Ok(())
        }
    }

    impl VideoDecoder for ToolboxDecoder {
        fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
            let units = nal_units(&packet.data);
            if self.sets.take(self.codec, &units) || (self.format.is_null() && !packet.codec_config)
            {
                // A configuration that is not whole yet waits for the rest.
                match unsafe { self.reconfigure() } {
                    Ok(()) => {}
                    Err(_) if packet.codec_config => return self.take_output(),
                    Err(why) => return Err(why),
                }
            }
            let data = length_prefixed(self.codec, &units);
            if data.is_empty() {
                return self.take_output();
            }
            if self.session.is_null() {
                return Err("the stream's parameter sets have not come yet".into());
            }
            unsafe { self.decode_sample(&packet, &data)? };
            self.take_output()
        }

        fn drain(&mut self) -> Result<Vec<Picture>, String> {
            if !self.session.is_null() {
                unsafe {
                    VTDecompressionSessionFinishDelayedFrames(self.session);
                    VTDecompressionSessionWaitForAsynchronousFrames(self.session);
                }
            }
            self.take_output()
        }

        fn flush(&mut self) {
            let _ = self.drain();
            if let Ok(mut output) = self.output.lock() {
                *output = Output::default();
            }
        }
    }

    impl Drop for ToolboxDecoder {
        fn drop(&mut self) {
            unsafe {
                self.end_session();
                release(self.format);
            }
        }
    }

    /// The Mac's hardware video decoders.
    #[derive(Debug, Default)]
    pub struct VideoToolboxDecoders;

    impl VideoDecoders for VideoToolboxDecoders {
        fn capabilities(&self) -> Vec<CodecSupport> {
            let mut out = Vec::new();
            for (codec, kind, side) in [
                (Codec::H264, AVC1, MAX_SIDE),
                (Codec::Hevc, HVC1, MAX_SIDE_HEVC),
            ] {
                if unsafe { VTIsHardwareDecodeSupported(kind) } != 0 {
                    out.push(CodecSupport {
                        codec,
                        max_width: side,
                        max_height: side,
                    });
                }
            }
            out
        }

        fn open(
            &self,
            codec: Codec,
            _width: u32,
            _height: u32,
        ) -> Result<Box<dyn VideoDecoder>, String> {
            if !matches!(codec, Codec::H264 | Codec::Hevc) {
                return Err(format!(
                    "the Mac does not decode {} for Android",
                    codec.name()
                ));
            }
            Ok(Box::new(ToolboxDecoder {
                codec,
                sets: ParameterSets::default(),
                format: std::ptr::null(),
                session: std::ptr::null(),
                output: Box::default(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264_SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1e, 0x95];
    const H264_PPS: &[u8] = &[0x68, 0xce, 0x38, 0x80];
    const H264_IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x33];

    fn annex_b(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, unit) in units.iter().enumerate() {
            // Both start code lengths, as encoders mix them.
            if i % 2 == 0 {
                out.extend_from_slice(&[0, 0, 0, 1]);
            } else {
                out.extend_from_slice(&[0, 0, 1]);
            }
            out.extend_from_slice(unit);
        }
        out
    }

    #[test]
    fn pr4_a_streams_nal_units_are_found_between_start_codes() {
        let data = annex_b(&[H264_SPS, H264_PPS, H264_IDR]);
        assert_eq!(nal_units(&data), vec![H264_SPS, H264_PPS, H264_IDR]);
        assert!(nal_units(&[]).is_empty());
        assert!(nal_units(&[0, 0, 0, 1]).is_empty());
    }

    #[test]
    fn pr4_a_length_prefixed_packet_is_read_as_it_is() {
        let mut data = Vec::new();
        for unit in [H264_SPS, H264_IDR] {
            data.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            data.extend_from_slice(unit);
        }
        assert_eq!(nal_units(&data), vec![H264_SPS, H264_IDR]);
        // Neither start codes nor lengths that fit: one unit.
        assert_eq!(nal_units(&[0x65, 1, 2]), vec![&[0x65u8, 1, 2][..]]);
    }

    #[test]
    fn pr4_a_picture_reaches_videotoolbox_with_lengths_and_without_its_parameter_sets() {
        let units = [&[0x09u8, 0xf0][..], H264_SPS, H264_PPS, H264_IDR];
        let data = length_prefixed(Codec::H264, &units);
        let mut expected = (H264_IDR.len() as u32).to_be_bytes().to_vec();
        expected.extend_from_slice(H264_IDR);
        assert_eq!(data, expected);
    }

    #[test]
    fn pr4_the_parameter_sets_make_a_format_once_all_have_come() {
        let mut sets = ParameterSets::default();
        assert!(sets.take(Codec::H264, &[H264_SPS]));
        assert!(sets.for_format(Codec::H264).is_none());
        assert!(sets.take(Codec::H264, &[H264_PPS, H264_IDR]));
        assert_eq!(sets.for_format(Codec::H264), Some(vec![H264_SPS, H264_PPS]));
        // The same sets again change nothing; a new one does.
        assert!(!sets.take(Codec::H264, &[H264_SPS, H264_PPS]));
        assert!(sets.take(Codec::H264, &[&[0x68, 0xce, 0x3c, 0x80]]));
    }

    #[test]
    fn pr4_hevc_needs_its_video_parameter_set_too() {
        let vps: &[u8] = &[0x40, 0x01, 0x0c];
        let sps: &[u8] = &[0x42, 0x01, 0x01];
        let pps: &[u8] = &[0x44, 0x01, 0xc1];
        let idr: &[u8] = &[0x26, 0x01, 0xaf];
        assert_eq!(nal_kind(Codec::Hevc, vps), NalKind::Vps);
        assert_eq!(nal_kind(Codec::Hevc, idr), NalKind::Other);
        let mut sets = ParameterSets::default();
        sets.take(Codec::Hevc, &[sps, pps]);
        assert!(sets.for_format(Codec::Hevc).is_none());
        sets.take(Codec::Hevc, &[vps]);
        assert_eq!(sets.for_format(Codec::Hevc), Some(vec![vps, sps, pps]));
    }
}
