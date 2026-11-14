//! The key frame gate: after the decoder opens, after a flush and after a size change, no
//! picture decoded from a packet that refers to pictures the decoder does not hold reaches
//! the guest. A fake decoder stands in for the host's hardware decoder and conceals as one
//! does: a picture it cannot predict comes out all zeros, which shows as green.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::video::{
    serve_connection, Codec, CodecSupport, Packet, Picture, StreamStats, VideoDecoder,
    VideoDecoders, VideoEnd, STATS_INTERVAL,
};
use dsa_host_core::video_keyframe::{access, Access, Admit, KeyframeGate, MAX_SKIPPED};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    Envelope, Role, VideoCodec, VideoDecoderOpen, VideoFlush, VideoFrame, VideoPacket,
};
use dsa_protocol::handshake::local_hello;
use dsa_protocol::{decode, encode};
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);

/// A well decoded picture's samples, and a concealed one's (green).
const GOOD: u8 = 0x80;
const GREEN: u8 = 0;

// H.264: an IDR slice, a P slice, and an I slice of a picture that is not an IDR.
const H264_IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x10];
const H264_P: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x02, 0x03];
const H264_I: &[u8] = &[0, 0, 1, 0x41, 0x88, 0x84, 0x10];
// H.264 parameter sets then an IDR slice, as one access unit.
const H264_SPS_PPS_IDR: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1e, 0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80, 0, 0, 0, 1, 0x65, 0x88,
    0x84,
];

// HEVC: an IDR, a CRA, a RASL picture and a trailing picture.
const HEVC_IDR: &[u8] = &[0, 0, 0, 1, 0x26, 0x01, 0xaf];
const HEVC_CRA: &[u8] = &[0, 0, 0, 1, 0x2a, 0x01, 0xaf];
const HEVC_RASL: &[u8] = &[0, 0, 0, 1, 0x10, 0x01, 0xaf];
const HEVC_TRAIL: &[u8] = &[0, 0, 0, 1, 0x02, 0x01, 0xaf];

// AV1: a temporal delimiter, then a frame OBU whose header says a key frame or an inter frame.
const AV1_KEY: &[u8] = &[0x12, 0x00, 0x32, 0x02, 0x10, 0x00];
const AV1_INTER: &[u8] = &[0x12, 0x00, 0x32, 0x02, 0x30, 0x00];

/// Writes bits, most significant first.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    used: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, count: u32) -> &mut Self {
        for i in (0..count).rev() {
            if self.used % 8 == 0 {
                self.bytes.push(0);
            }
            let bit = ((value >> i) & 1) as u8;
            *self.bytes.last_mut().unwrap() |= bit << (7 - self.used % 8);
            self.used += 1;
        }
        self
    }

    fn done(&mut self) -> Vec<u8> {
        // A few bytes of frame data after the header.
        let mut out = std::mem::take(&mut self.bytes);
        out.extend_from_slice(&[0x5a, 0x5a, 0x5a]);
        out
    }
}

/// A VP9 key frame of profile 0 at `width` by `height`.
fn vp9_key(width: u32, height: u32) -> Vec<u8> {
    BitWriter::default()
        .put(2, 2) // frame_marker
        .put(0, 2) // profile
        .put(0, 1) // show_existing_frame
        .put(0, 1) // frame_type: key
        .put(1, 1) // show_frame
        .put(0, 1) // error_resilient_mode
        .put(0x49_83_42, 24)
        .put(0, 3) // color_space
        .put(0, 1) // color_range
        .put(width - 1, 16)
        .put(height - 1, 16)
        .done()
}

/// A VP9 inter frame of profile 0.
fn vp9_inter() -> Vec<u8> {
    BitWriter::default()
        .put(2, 2)
        .put(0, 2)
        .put(0, 1)
        .put(1, 1) // frame_type: not a key frame
        .put(1, 1)
        .put(0, 1)
        .put(0, 6)
        .done()
}

/// A VP9 intra-only frame of profile 0 at a size of its own: not shown, and not where a
/// stream may start, since the reference slots it does not refresh are still needed.
fn vp9_intra_only(width: u32, height: u32) -> Vec<u8> {
    BitWriter::default()
        .put(2, 2)
        .put(0, 2)
        .put(0, 1)
        .put(1, 1) // frame_type: not a key frame
        .put(0, 1) // show_frame
        .put(0, 1) // error_resilient_mode
        .put(1, 1) // intra_only
        .put(0, 2) // reset_frame_context
        .put(0x49_83_42, 24)
        .put(0x01, 8) // refresh_frame_flags
        .put(width - 1, 16)
        .put(height - 1, 16)
        .done()
}

#[test]
fn fr26_each_formats_key_frames_are_found_in_its_bitstream() {
    assert_eq!(access(Codec::H264, H264_IDR), Access::Key);
    assert_eq!(access(Codec::H264, H264_SPS_PPS_IDR), Access::Key);
    assert_eq!(access(Codec::H264, H264_I), Access::Key);
    assert_eq!(access(Codec::H264, H264_P), Access::Delta);
    // Parameter sets alone hold no picture.
    assert_eq!(
        access(Codec::H264, &[0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1e]),
        Access::Unknown
    );
    assert_eq!(access(Codec::Hevc, HEVC_IDR), Access::Key);
    assert_eq!(access(Codec::Hevc, HEVC_CRA), Access::OpenKey);
    assert_eq!(access(Codec::Hevc, HEVC_RASL), Access::Leading);
    assert_eq!(access(Codec::Hevc, HEVC_TRAIL), Access::Delta);
    assert_eq!(access(Codec::Vp9, &vp9_key(640, 360)), Access::Key);
    assert_eq!(access(Codec::Vp9, &vp9_inter()), Access::Delta);
    assert_eq!(access(Codec::Vp9, &vp9_intra_only(640, 360)), Access::Delta);
    assert_eq!(access(Codec::Av1, AV1_KEY), Access::Key);
    assert_eq!(access(Codec::Av1, AV1_INTER), Access::Delta);
    // Bytes that are none of these pass rather than stop a stream.
    assert_eq!(access(Codec::H264, &[10]), Access::Unknown);
    assert_eq!(access(Codec::Vp9, &[]), Access::Unknown);
}

#[test]
fn fr26_a_stream_starts_at_its_first_key_frame() {
    let mut gate = KeyframeGate::new(Codec::Av1);
    assert_eq!(gate.admit(AV1_INTER), Admit::Skip);
    assert_eq!(gate.admit(AV1_INTER), Admit::Skip);
    assert_eq!(gate.admit(AV1_KEY), Admit::Decode);
    assert_eq!(gate.admit(AV1_INTER), Admit::Decode);
    // A flush: the decoder has nothing to predict from until the next key frame.
    gate.rearm();
    assert_eq!(gate.admit(AV1_INTER), Admit::Skip);
    assert_eq!(gate.admit(AV1_KEY), Admit::Decode);
}

#[test]
fn fr26_a_size_change_on_a_frame_that_is_not_a_key_frame_waits_for_one() {
    let mut gate = KeyframeGate::new(Codec::Vp9);
    assert_eq!(gate.admit(&vp9_key(640, 360)), Admit::Decode);
    assert_eq!(gate.admit(&vp9_inter()), Admit::Decode);
    // A key frame at a new size starts the new sequence itself.
    assert_eq!(gate.admit(&vp9_key(854, 480)), Admit::Decode);
    assert_eq!(gate.admit(&vp9_inter()), Admit::Decode);
    // A new size without a key frame: nothing decodes until one comes.
    assert_eq!(gate.admit(&vp9_intra_only(1280, 720)), Admit::Skip);
    assert_eq!(gate.admit(&vp9_inter()), Admit::Skip);
    assert_eq!(gate.admit(&vp9_key(1280, 720)), Admit::Decode);
    assert_eq!(gate.admit(&vp9_inter()), Admit::Decode);
}

#[test]
fn fr26_an_hevc_stream_started_at_a_cra_skips_its_leading_pictures() {
    let mut gate = KeyframeGate::new(Codec::Hevc);
    assert_eq!(gate.admit(HEVC_TRAIL), Admit::Skip);
    assert_eq!(gate.admit(HEVC_CRA), Admit::Decode);
    // They refer to the pictures before the CRA, which this decoder never had.
    assert_eq!(gate.admit(HEVC_RASL), Admit::Skip);
    assert_eq!(gate.admit(HEVC_TRAIL), Admit::Decode);
    // A later CRA's leading pictures have their references.
    assert_eq!(gate.admit(HEVC_CRA), Admit::Decode);
    assert_eq!(gate.admit(HEVC_RASL), Admit::Decode);
    // An IDR has none that refer back.
    gate.rearm();
    assert_eq!(gate.admit(HEVC_IDR), Admit::Decode);
    assert_eq!(gate.admit(HEVC_TRAIL), Admit::Decode);
}

#[test]
fn fr26_a_stream_with_no_key_frame_it_recognizes_still_plays() {
    let mut gate = KeyframeGate::new(Codec::H264);
    for _ in 0..MAX_SKIPPED {
        assert_eq!(gate.admit(H264_P), Admit::Skip);
    }
    assert_eq!(gate.admit(H264_P), Admit::GaveUp);
    assert_eq!(gate.admit(H264_P), Admit::Decode);
}

#[test]
fn fr26_the_streams_numbers_count_the_packets_before_a_key_frame() {
    let start = Instant::now();
    let mut stats = StreamStats::default();
    for i in 0..4u64 {
        stats.before_keyframe(start + Duration::from_millis(i * 33));
    }
    stats.decoded(start, Duration::from_millis(2), 1);
    stats.handed_on(start, (640, 360), Duration::ZERO);
    let line = stats
        .report(start + STATS_INTERVAL, Codec::Av1)
        .expect("a line once the interval has passed");
    assert!(line.contains("5 packets, 1 pictures decoded"), "{line}");
    assert!(
        line.contains("4 dropped (0 flushed, 0 malformed, 4 before a key frame)"),
        "{line}"
    );
}

/// A decoder that conceals as a hardware decoder does: a packet it cannot predict (no key
/// frame since it opened or was flushed) still gives a picture, all zeros. It hands each
/// picture out at once and logs every packet it was given.
struct ConcealingDecoder {
    codec: Codec,
    reference: bool,
    given: Arc<Mutex<Vec<u64>>>,
}

impl VideoDecoder for ConcealingDecoder {
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
        self.given.lock().unwrap().push(packet.id);
        if packet.codec_config {
            return Ok(Vec::new());
        }
        if matches!(
            access(self.codec, &packet.data),
            Access::Key | Access::OpenKey
        ) {
            self.reference = true;
        }
        let value = if self.reference { GOOD } else { GREEN };
        Ok(vec![Picture {
            id: packet.id,
            width: 2,
            height: 2,
            stride: 2,
            nv12: vec![value; 6],
        }])
    }

    fn drain(&mut self) -> Result<Vec<Picture>, String> {
        Ok(Vec::new())
    }

    fn flush(&mut self) {
        self.reference = false;
    }
}

struct ConcealingDecoders {
    given: Arc<Mutex<Vec<u64>>>,
}

impl VideoDecoders for ConcealingDecoders {
    fn capabilities(&self) -> Vec<CodecSupport> {
        [Codec::H264, Codec::Hevc, Codec::Vp9, Codec::Av1]
            .into_iter()
            .map(|codec| CodecSupport {
                codec,
                max_width: 4096,
                max_height: 2304,
            })
            .collect()
    }

    fn open(&self, codec: Codec, _: u32, _: u32) -> Result<Box<dyn VideoDecoder>, String> {
        Ok(Box::new(ConcealingDecoder {
            codec,
            reference: false,
            given: Arc::clone(&self.given),
        }))
    }
}

fn send(connection: &mut dyn Connection, body: Body) {
    connection
        .send(&encode(&Envelope { body: Some(body) }))
        .expect("send");
}

fn next(connection: &mut dyn Connection) -> Body {
    let frame = connection
        .recv(Some(WAIT))
        .expect("a message from the host");
    decode(&frame).expect("decodes").body.expect("has a body")
}

fn frame(body: Body) -> VideoFrame {
    match body {
        Body::VideoFrame(frame) => frame,
        other => panic!("expected a picture, got {other:?}"),
    }
}

/// The guest's end of an opened stream, the host's thread, and the packet ids the decoder was
/// given (one id for each packet that reached it, in order).
type Opened = (
    Box<dyn Connection>,
    thread::JoinHandle<VideoEnd>,
    Arc<Mutex<Vec<u64>>>,
);

/// A stream of `codec`, opened.
fn opened(codec: VideoCodec) -> Opened {
    let (mut listener, mut connector) = memory::pair();
    let given = Arc::new(Mutex::new(Vec::new()));
    let decoders = ConcealingDecoders {
        given: Arc::clone(&given),
    };
    let host = thread::spawn(move || {
        let connection = listener.accept(WAIT).expect("accepts");
        serve_connection(connection, &decoders, "dsa-host test")
    });
    let mut guest = connector.connect(WAIT).expect("connects");
    send(
        guest.as_mut(),
        Body::Hello(local_hello(Role::GuestVideo, "dsa-codec2 test")),
    );
    assert!(matches!(next(guest.as_mut()), Body::HelloAccepted(_)));
    assert!(matches!(next(guest.as_mut()), Body::VideoCapabilities(_)));
    send(
        guest.as_mut(),
        Body::VideoDecoderOpen(VideoDecoderOpen {
            codec: codec as i32,
            width: 640,
            height: 360,
        }),
    );
    assert!(matches!(next(guest.as_mut()), Body::VideoDecoderOpened(o) if o.ok));
    (guest, host, given)
}

fn packet(frame_index: u64, data: &[u8]) -> Body {
    Body::VideoPacket(VideoPacket {
        frame_index,
        timestamp_us: frame_index as i64 * 33_333,
        data: data.to_vec(),
        codec_config: false,
        end_of_stream: false,
    })
}

/// The answer to one packet: its picture's sample value, or `None` when the host says the
/// packet gives no picture.
fn answer(guest: &mut dyn Connection, frame_index: u64) -> Option<u8> {
    let frame = frame(next(guest));
    assert_eq!(frame.frame_index, frame_index);
    assert!(!frame.end_of_stream);
    if frame.width == 0 {
        assert_eq!((frame.height, frame.stride), (0, 0));
        assert!(frame.nv12.is_empty());
        return None;
    }
    Some(frame.nv12[0])
}

#[test]
fn fr26_after_a_flush_no_picture_comes_out_before_a_key_frame() {
    let (mut guest, host, given) = opened(VideoCodec::H264);
    // A stream joined between key frames (a seek the app did not align): nothing to predict
    // from yet.
    send(guest.as_mut(), packet(1, H264_P));
    assert_eq!(answer(guest.as_mut(), 1), None);
    send(guest.as_mut(), packet(2, H264_SPS_PPS_IDR));
    assert_eq!(answer(guest.as_mut(), 2), Some(GOOD));
    send(guest.as_mut(), packet(3, H264_P));
    assert_eq!(answer(guest.as_mut(), 3), Some(GOOD));

    // A seek, a swipe to the next video, a codec reused for another stream: a flush, and the
    // packets after it start between key frames.
    send(guest.as_mut(), Body::VideoFlush(VideoFlush {}));
    assert!(matches!(next(guest.as_mut()), Body::VideoFlushed(_)));
    for index in 4..7 {
        send(guest.as_mut(), packet(index, H264_P));
        assert_eq!(answer(guest.as_mut(), index), None, "packet {index}");
    }
    send(guest.as_mut(), packet(7, H264_IDR));
    assert_eq!(answer(guest.as_mut(), 7), Some(GOOD));
    send(guest.as_mut(), packet(8, H264_P));
    assert_eq!(answer(guest.as_mut(), 8), Some(GOOD));

    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
    // The decoder was never given a packet it could not predict: the host's ids 0 to 3 are
    // the packets of frames 2, 3, 7 and 8.
    assert_eq!(*given.lock().unwrap(), [0, 1, 2, 3]);
}

#[test]
fn fr26_a_size_change_mid_stream_shows_no_picture_before_its_key_frame() {
    let (mut guest, host, given) = opened(VideoCodec::Vp9);
    send(guest.as_mut(), packet(1, &vp9_key(640, 360)));
    assert_eq!(answer(guest.as_mut(), 1), Some(GOOD));
    send(guest.as_mut(), packet(2, &vp9_inter()));
    assert_eq!(answer(guest.as_mut(), 2), Some(GOOD));
    // The quality changes: a frame of the new size that is not a key frame, and the frames
    // after it, until the new size's key frame.
    send(guest.as_mut(), packet(3, &vp9_intra_only(1280, 720)));
    assert_eq!(answer(guest.as_mut(), 3), None);
    send(guest.as_mut(), packet(4, &vp9_inter()));
    assert_eq!(answer(guest.as_mut(), 4), None);
    send(guest.as_mut(), packet(5, &vp9_key(1280, 720)));
    assert_eq!(answer(guest.as_mut(), 5), Some(GOOD));
    send(guest.as_mut(), packet(6, &vp9_inter()));
    assert_eq!(answer(guest.as_mut(), 6), Some(GOOD));
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
    assert_eq!(given.lock().unwrap().len(), 4);
}

#[test]
fn fr26_an_av1_stream_reused_after_a_flush_shows_no_green_picture() {
    let (mut guest, host, _given) = opened(VideoCodec::Av1);
    let mut index = 0;
    for round in 0..3 {
        // Each round: a flush (but the first), then a few inter frames, a key frame and its
        // inter frames, as when the next Short plays in the same codec.
        if round > 0 {
            send(guest.as_mut(), Body::VideoFlush(VideoFlush {}));
            assert!(matches!(next(guest.as_mut()), Body::VideoFlushed(_)));
        }
        for data in [AV1_INTER, AV1_INTER, AV1_KEY, AV1_INTER, AV1_INTER] {
            index += 1;
            send(guest.as_mut(), packet(index, data));
            let shown = answer(guest.as_mut(), index);
            assert_ne!(shown, Some(GREEN), "frame {index} came out green");
        }
    }
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
}
