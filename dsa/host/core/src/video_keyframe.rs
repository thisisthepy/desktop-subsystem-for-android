//! Where a stream may start decoding: its key frames.
//!
//! A decoder that has just opened, been flushed (a seek, a swipe to the next video, a codec
//! the app reuses for another stream) or failed on a packet holds no reference pictures. A
//! packet that predicts from pictures the decoder does not hold still decodes, into whatever
//! its surface held: zeros, which show as a green picture, or blocks of an unrelated picture.
//! The host's hardware decoders do not drop such pictures themselves, so the host gives a
//! decoder nothing from such a point until a key frame ([`KeyframeGate`]). The packets before
//! it give no picture, and the guest is told so for each.
//!
//! A key frame is found in the packet's own bitstream: an H.264 IDR picture (or an I slice,
//! or a recovery point), an HEVC IRAP picture, a VP9 key frame, an AV1 key frame. HEVC's
//! leading pictures that refer to pictures before their CRA (RASL) are skipped too when the
//! stream starts at that CRA, as the standard says a decoder does.
//!
//! Nothing here names a host operating system API.

use crate::video::Codec;
use crate::video_layout::{stream_frame_size, Size};

/// What a packet is to a decoder that starts at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Decodes on its own, and nothing after it refers to anything before it.
    Key,
    /// Decodes on its own, but its leading pictures may refer to pictures before it (an HEVC
    /// CRA or BLA picture).
    OpenKey,
    /// A leading picture of an open key frame that refers to pictures before it (HEVC RASL):
    /// it cannot be decoded when the stream starts at that key frame.
    Leading,
    /// Refers to pictures before it.
    Delta,
    /// Not known: a packet with no picture in it, or one that does not parse.
    Unknown,
}

/// What [`KeyframeGate::admit`] decided for one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    /// The decoder takes it.
    Decode,
    /// It refers to pictures the decoder does not have: it is not decoded, and gives no
    /// picture.
    Skip,
    /// No key frame came in [`MAX_SKIPPED`] packets: the gate gives up and the decoder takes
    /// this one and every one after, as it did before there was a gate. A stream whose key
    /// frames this module does not recognize still plays.
    GaveUp,
}

/// How many packets in a row are skipped while waiting for a key frame before the gate gives
/// up: ten seconds at 30 frames a second, longer than any streaming service's key frame
/// interval.
pub const MAX_SKIPPED: u32 = 300;

/// Lets a decoder take packets only from a key frame on, after it opens, after every flush,
/// after a packet it failed on, and after the stream's size changed on a packet that is not a
/// key frame.
#[derive(Debug, Clone)]
pub struct KeyframeGate {
    codec: Codec,
    waiting: bool,
    skip_leading: bool,
    skipped: u32,
    av1_reduced: bool,
    size: Option<Size>,
}

impl KeyframeGate {
    /// A gate for a decoder that has just opened: it waits for a key frame.
    pub fn new(codec: Codec) -> KeyframeGate {
        KeyframeGate {
            codec,
            waiting: true,
            skip_leading: false,
            skipped: 0,
            av1_reduced: false,
            size: None,
        }
    }

    /// The decoder lost its references (a flush, a packet it failed on, the end of the
    /// stream): it waits for a key frame again.
    pub fn rearm(&mut self) {
        self.waiting = true;
        self.skip_leading = false;
        self.skipped = 0;
    }

    /// Whether the gate is waiting for a key frame.
    pub fn waiting(&self) -> bool {
        self.waiting
    }

    /// Codec configuration the decoder is given: an AV1 configuration record's sequence
    /// header says whether the stream's frames have headers of their own.
    pub fn configuration(&mut self, data: &[u8]) {
        if self.codec == Codec::Av1 {
            let obus = if data.first() == Some(&0x81) && data.len() > 4 {
                &data[4..]
            } else {
                data
            };
            av1_access(obus, &mut self.av1_reduced);
        }
    }

    /// Whether the decoder takes the stream's next packet, `data`.
    pub fn admit(&mut self, data: &[u8]) -> Admit {
        let access = match self.codec {
            Codec::H264 => h264_access(data),
            Codec::Hevc => hevc_access(data),
            Codec::Vp9 => vp9_access(data),
            Codec::Av1 => av1_access(data, &mut self.av1_reduced),
        };
        // A new picture size is a new sequence: only a key frame starts it. VP9's intra-only
        // frames and an AV1 sequence header out of place carry one too.
        if let Some(size) = stream_frame_size(self.codec, data) {
            let changed = self.size.is_some_and(|before| before != size);
            self.size = Some(size);
            if changed && !matches!(access, Access::Key | Access::OpenKey) {
                self.rearm();
            }
        }
        let decode = match access {
            Access::Key => {
                self.waiting = false;
                self.skip_leading = false;
                true
            }
            Access::OpenKey => {
                // Its leading pictures are skipped only when the stream starts here.
                self.skip_leading = self.waiting;
                self.waiting = false;
                true
            }
            Access::Leading => !self.waiting && !self.skip_leading,
            Access::Delta => {
                if !self.waiting {
                    self.skip_leading = false;
                }
                !self.waiting
            }
            Access::Unknown => true,
        };
        if decode {
            if !self.waiting {
                self.skipped = 0;
            }
            return Admit::Decode;
        }
        self.skipped += 1;
        if self.skipped > MAX_SKIPPED {
            self.waiting = false;
            self.skip_leading = false;
            self.skipped = 0;
            return Admit::GaveUp;
        }
        Admit::Skip
    }
}

/// What `data` is to a decoder that starts at it, for `codec`. AV1 frames of a reduced still
/// picture header stream need the sequence header's flag; this reads it from `data` only.
pub fn access(codec: Codec, data: &[u8]) -> Access {
    match codec {
        Codec::H264 => h264_access(data),
        Codec::Hevc => hevc_access(data),
        Codec::Vp9 => vp9_access(data),
        Codec::Av1 => av1_access(data, &mut false),
    }
}

/// The NAL units of an Annex B byte stream: what follows each start code (00 00 01, or
/// 00 00 00 01) up to the next.
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |next| next - 3);
        // The zero of a four-byte start code, and trailing zeros, belong to no unit.
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            units.push(&data[start..end]);
        }
    }
    units
}

/// The first bytes of a NAL unit's payload without its emulation prevention bytes (the 03 of
/// each 00 00 03), enough for the few fields read here.
fn unescaped(payload: &[u8], limit: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(limit);
    let mut zeros = 0;
    for &byte in payload {
        if out.len() >= limit {
            break;
        }
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// Reads bits from the front, most significant first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, at: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.at / 8)?;
        let bit = (byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Some(u32::from(bit))
    }

    fn bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Some(value)
    }

    /// An unsigned Exp-Golomb code, ue(v).
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(self.bits(zeros)? + ((1u32 << zeros) - 1))
    }
}

/// An H.264 access unit: a key frame when it holds an IDR slice, an I slice or a recovery
/// point; a delta frame when it holds other slices.
fn h264_access(data: &[u8]) -> Access {
    let mut slices = false;
    for unit in nal_units(data) {
        match unit[0] & 0x1f {
            // An IDR slice.
            5 => return Access::Key,
            // A slice of a non-IDR picture: an I or SI slice decodes on its own.
            1 => {
                slices = true;
                let header = unescaped(&unit[1..], 8);
                let mut bits = Bits::new(&header);
                let slice_type = bits.ue().and_then(|_| bits.ue());
                if matches!(slice_type, Some(t) if t % 5 == 2 || t % 5 == 4) {
                    return Access::Key;
                }
            }
            // Data partitions are slices too.
            2..=4 => slices = true,
            // SEI: a recovery point (payload type 6) marks where decoding may start.
            6 if unit.get(1) == Some(&6) => return Access::Key,
            _ => {}
        }
    }
    if slices {
        Access::Delta
    } else {
        Access::Unknown
    }
}

/// An HEVC access unit, by the type of its first picture's NAL units.
fn hevc_access(data: &[u8]) -> Access {
    for unit in nal_units(data) {
        let kind = (unit[0] >> 1) & 0x3f;
        match kind {
            // IDR_W_RADL, IDR_N_LP, and the reserved IRAP types.
            19 | 20 | 22 | 23 => return Access::Key,
            // BLA_W_LP, BLA_W_RADL, BLA_N_LP, CRA_NUT.
            16..=18 | 21 => return Access::OpenKey,
            // RASL_N, RASL_R.
            8 | 9 => return Access::Leading,
            0..=15 => return Access::Delta,
            _ => {}
        }
    }
    Access::Unknown
}

/// A VP9 packet, by its first frame (a key frame is always the first of its superframe).
fn vp9_access(data: &[u8]) -> Access {
    let frame = vp9_first_frame(data);
    let mut bits = Bits::new(frame);
    let Some(2) = bits.bits(2) else {
        return Access::Unknown;
    };
    let (Some(low), Some(high)) = (bits.bit(), bits.bit()) else {
        return Access::Unknown;
    };
    if (high << 1) | low == 3 && bits.bit().is_none() {
        return Access::Unknown;
    }
    match (bits.bit(), bits.bit()) {
        // show_existing_frame: shows a frame the decoder already holds.
        (Some(1), _) => Access::Delta,
        (Some(0), Some(0)) => Access::Key,
        (Some(0), Some(1)) => Access::Delta,
        _ => Access::Unknown,
    }
}

/// The first frame of a VP9 packet, which may be a superframe (several frames with an index
/// at the end).
fn vp9_first_frame(data: &[u8]) -> &[u8] {
    let Some(&last) = data.last() else {
        return data;
    };
    if last & 0xe0 != 0xc0 {
        return data;
    }
    let frames = usize::from(last & 0x7) + 1;
    let size_bytes = usize::from((last >> 3) & 0x3) + 1;
    let index = 2 + size_bytes * frames;
    if data.len() < index || data[data.len() - index] != last {
        return data;
    }
    let start = data.len() - index + 1;
    let mut first = 0usize;
    for i in 0..size_bytes {
        first |= usize::from(data[start + i]) << (8 * i);
    }
    if first == 0 || first > data.len() - index {
        return data;
    }
    &data[..first]
}

/// Reads AV1's leb128() from `data` at `at`, returning the value and the bytes it took.
fn leb128(data: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for i in 0..8 {
        let byte = *data.get(at + i)?;
        value |= usize::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// An AV1 temporal unit, by its first frame header. `reduced` is whether the stream's
/// sequence header has a reduced still picture header (every frame then is a key frame); a
/// sequence header in `data` updates it.
fn av1_access(data: &[u8], reduced: &mut bool) -> Access {
    let mut at = 0;
    while at < data.len() {
        let header = data[at];
        let kind = (header >> 3) & 0xf;
        let extension = header & 0x4 != 0;
        let has_size = header & 0x2 != 0;
        let mut start = at + 1 + usize::from(extension);
        let size = if has_size {
            let Some((size, taken)) = leb128(data, start) else {
                return Access::Unknown;
            };
            start += taken;
            size
        } else {
            match data.len().checked_sub(start) {
                Some(size) => size,
                None => return Access::Unknown,
            }
        };
        let Some(end) = start.checked_add(size).filter(|&end| end <= data.len()) else {
            return Access::Unknown;
        };
        let payload = &data[start..end];
        match kind {
            // OBU_SEQUENCE_HEADER: seq_profile (3 bits), still_picture, then the flag.
            1 => {
                if let Some(&first) = payload.first() {
                    *reduced = first & 0x08 != 0;
                }
            }
            // OBU_FRAME_HEADER, OBU_FRAME.
            3 | 6 => {
                if *reduced {
                    return Access::Key;
                }
                let mut bits = Bits::new(payload);
                return match (bits.bit(), bits.bits(2)) {
                    // show_existing_frame: shows a frame the decoder already holds.
                    (Some(1), _) => Access::Delta,
                    // KEY_FRAME.
                    (Some(0), Some(0)) => Access::Key,
                    (Some(0), Some(_)) => Access::Delta,
                    _ => Access::Unknown,
                };
            }
            _ => {}
        }
        at = end;
    }
    Access::Unknown
}
