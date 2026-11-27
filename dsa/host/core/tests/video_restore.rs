//! A stream whose decoder is released while it gets no packet, and opened again when the next
//! one comes: what is let go, how the new decoder is brought to where the old one stood, and
//! that the guest never gets a picture decoded without its references. The decoder is a fake
//! that keeps a reference picture as an H.264 decoder does, and the connection the in-memory
//! transport.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::video::{
    serve_connection_idle, Codec, CodecSupport, Packet, Picture, VideoDecoder, VideoDecoders,
    VideoEnd,
};
use dsa_host_core::video_keyframe::Access;
use dsa_host_core::video_restore::{Restore, MAX_RESTORE_BYTES, MAX_RESTORE_PACKETS};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    Envelope, Role, VideoCodec, VideoDecoderOpen, VideoFlush, VideoFrame, VideoPacket,
};
use dsa_protocol::handshake::local_hello;
use dsa_protocol::{decode, encode};
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);
const IDLE: Duration = Duration::from_millis(100);

/// What a delta frame decodes to on a decoder that holds no reference picture: blocks of
/// whatever its surface held, which a viewer sees as garbage.
const GARBAGE: u8 = 0xee;

/// A decoder that keeps one reference picture, as an H.264 decoder does. A key frame's
/// picture is its last byte; a delta frame's is the reference plus its last byte, and becomes
/// the reference. A delta frame with no reference decodes into [`GARBAGE`]. It takes no
/// picture before it has been given the stream's parameter sets. One that `hold`s gives each
/// picture out only with the next packet, as a decoder that reorders does.
struct ReferenceDecoder {
    configured: bool,
    reference: Option<u8>,
    hold: bool,
    held: Option<Picture>,
    events: Arc<Mutex<Vec<String>>>,
}

fn picture(id: u64, value: u8) -> Picture {
    Picture {
        id,
        width: 2,
        height: 2,
        stride: 2,
        nv12: vec![value; 6],
        slot: None,
    }
}

impl VideoDecoder for ReferenceDecoder {
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
        if packet.codec_config {
            self.configured = true;
            return Ok(Vec::new());
        }
        if !self.configured {
            return Err("the decoder has no parameter sets".into());
        }
        let value = *packet.data.last().expect("a packet with data");
        let out = if packet.data[4] & 0x1f == 5 {
            self.reference = Some(value);
            value
        } else if let Some(reference) = self.reference {
            let next = reference.wrapping_add(value);
            self.reference = Some(next);
            next
        } else {
            GARBAGE
        };
        let decoded = picture(packet.id, out);
        if self.hold {
            Ok(self.held.replace(decoded).into_iter().collect())
        } else {
            Ok(vec![decoded])
        }
    }

    fn drain(&mut self) -> Result<Vec<Picture>, String> {
        Ok(self.held.take().into_iter().collect())
    }

    fn flush(&mut self) {
        self.held = None;
        self.reference = None;
    }

    fn release(&mut self) -> Option<String> {
        self.events.lock().unwrap().push("released".into());
        Some("its transform with its surfaces".into())
    }
}

impl Drop for ReferenceDecoder {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("dropped".into());
    }
}

struct ReferenceDecoders {
    hold: bool,
    /// How many opens after the first are refused, as a GPU that was reset refuses them.
    refused: Mutex<u32>,
    events: Arc<Mutex<Vec<String>>>,
}

impl VideoDecoders for ReferenceDecoders {
    fn capabilities(&self) -> Vec<CodecSupport> {
        vec![CodecSupport {
            codec: Codec::H264,
            max_width: 1920,
            max_height: 1088,
        }]
    }

    fn open(&self, _: Codec, _: u32, _: u32) -> Result<Box<dyn VideoDecoder>, String> {
        let first = !self.events.lock().unwrap().iter().any(|e| e == "opened");
        let mut refused = self.refused.lock().unwrap();
        if !first && *refused > 0 {
            *refused -= 1;
            self.events.lock().unwrap().push("refused".into());
            return Err("the GPU was reset".into());
        }
        self.events.lock().unwrap().push("opened".into());
        Ok(Box::new(ReferenceDecoder {
            configured: false,
            reference: None,
            hold: self.hold,
            held: None,
            events: Arc::clone(&self.events),
        }))
    }

    fn decoder_closed(&self) {
        self.events.lock().unwrap().push("decoder closed".into());
    }
}

type Stream = (
    Box<dyn Connection>,
    thread::JoinHandle<VideoEnd>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<String>>>,
);

/// A guest with an H.264 decoder open on a host that releases it after [`IDLE`], with the
/// decoders' events and the host's log lines.
fn stream(hold: bool) -> Stream {
    stream_refusing(hold, 0)
}

/// As [`stream`], with the `refused` opens after the first refused.
fn stream_refusing(hold: bool, refused: u32) -> Stream {
    let (mut listener, mut connector) = memory::pair();
    let events = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let decoders = ReferenceDecoders {
        hold,
        refused: Mutex::new(refused),
        events: Arc::clone(&events),
    };
    let host = {
        let lines = Arc::clone(&lines);
        thread::spawn(move || {
            let connection = listener.accept(WAIT).expect("accepts");
            serve_connection_idle(
                connection,
                &decoders,
                "dsa-host test",
                &|line: &str| lines.lock().unwrap().push(line.to_string()),
                IDLE,
            )
        })
    };
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
            codec: VideoCodec::H264 as i32,
            width: 1280,
            height: 720,
            picture_region_id: 0,
        }),
    );
    assert!(matches!(next(guest.as_mut()), Body::VideoDecoderOpened(o) if o.ok));
    (guest, host, events, lines)
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

fn frame(connection: &mut dyn Connection) -> VideoFrame {
    match next(connection) {
        Body::VideoFrame(frame) => frame,
        other => panic!("expected a picture, got {other:?}"),
    }
}

/// The value a frame's picture holds, or `None` for a frame that carries no picture.
fn value(frame: &VideoFrame) -> Option<u8> {
    frame.nv12.first().copied()
}

fn packet(frame_index: u64, data: Vec<u8>, codec_config: bool) -> Body {
    Body::VideoPacket(VideoPacket {
        frame_index,
        timestamp_us: frame_index as i64 * 33_333,
        data,
        codec_config,
        end_of_stream: false,
    })
}

/// The stream's parameter sets (an SPS), as Android sends them before the first picture.
fn config(frame_index: u64) -> Body {
    packet(frame_index, vec![0, 0, 0, 1, 0x67, 0x42], true)
}

/// An IDR picture whose value is `value`.
fn key(frame_index: u64, value: u8) -> Body {
    packet(frame_index, vec![0, 0, 0, 1, 0x65, 0x88, value], false)
}

/// A P picture that adds `value` to its reference.
fn delta(frame_index: u64, value: u8) -> Body {
    packet(frame_index, vec![0, 0, 0, 1, 0x41, 0x9a, value], false)
}

/// Waits until `events` holds `count` entries, or the wait runs out.
fn wait_for(events: &Mutex<Vec<String>>, count: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let now = events.lock().unwrap().clone();
        if now.len() >= count || std::time::Instant::now() > deadline {
            return now;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn nfr1_stalled_decoder_releases_its_transform_and_surfaces() {
    // A feed's preview decodes one picture and is never fed again; its decoder stayed open
    // with its transform and surfaces until the app closed the codec, minutes later.
    let (mut guest, host, events, lines) = stream(false);
    send(guest.as_mut(), config(0));
    send(guest.as_mut(), key(1, 10));
    assert_eq!(value(&frame(guest.as_mut())), Some(10));
    assert_eq!(
        wait_for(&events, 4),
        ["opened", "released", "dropped", "decoder closed"]
    );
    // Once, not again every interval while nothing comes.
    thread::sleep(IDLE * 4);
    assert_eq!(events.lock().unwrap().len(), 4);
    {
        let lines = lines.lock().unwrap();
        assert!(
            lines.iter().any(|l| l.contains("no packet for")
                && l.contains("released its decoder (its transform with its surfaces)")),
            "{lines:?}"
        );
    }
    // The app closes the codec: nothing is left to let go, and nothing is let go twice.
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
    assert_eq!(
        events.lock().unwrap().len(),
        4,
        "{:?}",
        events.lock().unwrap()
    );
    let lines = lines.lock().unwrap();
    let ended: Vec<_> = lines.iter().filter(|l| l.contains("ended")).collect();
    assert_eq!(ended.len(), 1, "{lines:?}");
    assert!(
        ended[0].contains("already gone while the stream was idle"),
        "{}",
        ended[0]
    );
}

#[test]
fn pr4_a_paused_stream_resumes_where_it_left_off_after_its_decoder_was_released() {
    let (mut guest, host, events, lines) = stream(false);
    send(guest.as_mut(), config(0));
    send(guest.as_mut(), key(1, 10));
    send(guest.as_mut(), delta(2, 1));
    send(guest.as_mut(), delta(3, 2));
    let shown: Vec<_> = (0..3).map(|_| value(&frame(guest.as_mut()))).collect();
    assert_eq!(shown, [Some(10), Some(11), Some(13)]);
    // Paused past the idle time: the decoder goes.
    assert_eq!(
        wait_for(&events, 4),
        ["opened", "released", "dropped", "decoder closed"]
    );
    // Resumed: the next delta frame decodes against the picture the stream paused at, and the
    // pictures the guest already has are not sent again.
    send(guest.as_mut(), delta(4, 3));
    let resumed = frame(guest.as_mut());
    assert_eq!(resumed.frame_index, 4);
    assert_eq!(value(&resumed), Some(16));
    send(guest.as_mut(), delta(5, 4));
    let after = frame(guest.as_mut());
    assert_eq!((after.frame_index, value(&after)), (5, Some(20)));
    assert_eq!(events.lock().unwrap()[4], "opened");
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
    let lines = lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("opened its decoder again where it stood")),
        "{lines:?}"
    );
}

#[test]
fn pr4_reopened_decoder_shows_no_garbage_before_the_next_keyframe() {
    // A flush leaves nothing to restore the decoder from: the decoder opened again after the
    // pause starts at the next key frame, and the packets before it carry no picture, so the
    // app keeps its last good one and sees no error.
    let (mut guest, host, events, _lines) = stream(false);
    send(guest.as_mut(), config(0));
    send(guest.as_mut(), key(1, 10));
    assert_eq!(value(&frame(guest.as_mut())), Some(10));
    send(guest.as_mut(), Body::VideoFlush(VideoFlush {}));
    assert!(matches!(next(guest.as_mut()), Body::VideoFlushed(_)));
    assert_eq!(
        wait_for(&events, 4),
        ["opened", "released", "dropped", "decoder closed"]
    );
    // A flush while the decoder is gone opens nothing.
    send(guest.as_mut(), Body::VideoFlush(VideoFlush {}));
    assert!(matches!(next(guest.as_mut()), Body::VideoFlushed(_)));
    send(guest.as_mut(), delta(2, 1));
    send(guest.as_mut(), delta(3, 1));
    for index in [2, 3] {
        let skipped = frame(guest.as_mut());
        assert_eq!(skipped.frame_index, index);
        assert_eq!(
            value(&skipped),
            None,
            "a delta frame with no reference shows nothing"
        );
        assert!(!skipped.end_of_stream);
    }
    assert_eq!(
        events.lock().unwrap().len(),
        4,
        "no decoder opens for a skipped packet"
    );
    // The key frame decodes: the new decoder was given the parameter sets the guest sent once.
    send(guest.as_mut(), key(4, 20));
    send(guest.as_mut(), delta(5, 1));
    let first = frame(guest.as_mut());
    assert_eq!((first.frame_index, value(&first)), (4, Some(20)));
    let second = frame(guest.as_mut());
    assert_eq!((second.frame_index, value(&second)), (5, Some(21)));
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
}

#[test]
fn pr4_a_picture_the_released_decoder_still_held_reaches_the_guest_once() {
    // A decoder that gives each picture out a packet late still held the paused stream's last
    // picture when it was released. The decoder opened again makes it again, and it goes to the
    // guest with its own frame, once; the pictures before it do not go again.
    let (mut guest, host, events, _lines) = stream(true);
    send(guest.as_mut(), config(0));
    send(guest.as_mut(), key(1, 10));
    send(guest.as_mut(), delta(2, 1));
    let first = frame(guest.as_mut());
    assert_eq!((first.frame_index, value(&first)), (1, Some(10)));
    assert_eq!(
        wait_for(&events, 4),
        ["opened", "released", "dropped", "decoder closed"]
    );
    send(guest.as_mut(), delta(3, 2));
    let held = frame(guest.as_mut());
    assert_eq!((held.frame_index, value(&held)), (2, Some(11)));
    send(guest.as_mut(), delta(4, 3));
    let next_one = frame(guest.as_mut());
    assert_eq!((next_one.frame_index, value(&next_one)), (3, Some(13)));
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
}

#[test]
fn pr4_the_end_of_a_paused_stream_still_brings_out_the_picture_its_decoder_held() {
    let (mut guest, host, _events, _lines) = stream(true);
    send(guest.as_mut(), config(0));
    send(guest.as_mut(), key(1, 10));
    send(guest.as_mut(), delta(2, 1));
    assert_eq!(frame(guest.as_mut()).frame_index, 1);
    thread::sleep(IDLE * 4);
    send(
        guest.as_mut(),
        Body::VideoPacket(VideoPacket {
            frame_index: 3,
            timestamp_us: 99_999,
            data: Vec::new(),
            codec_config: false,
            end_of_stream: true,
        }),
    );
    let held = frame(guest.as_mut());
    assert_eq!((held.frame_index, value(&held)), (2, Some(11)));
    let end = frame(guest.as_mut());
    assert!(end.end_of_stream);
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
}

#[test]
fn nfr5_a_decoder_that_does_not_open_again_fails_that_packet_only() {
    // The GPU was reset while the video was paused: the packet that wanted a decoder gets an
    // error, which the guest finishes as a picture lost, and the stream starts again at its
    // next key frame once a decoder opens.
    let (mut guest, host, events, _lines) = stream_refusing(false, 1);
    send(guest.as_mut(), config(0));
    send(guest.as_mut(), key(1, 10));
    assert_eq!(value(&frame(guest.as_mut())), Some(10));
    assert_eq!(
        wait_for(&events, 4),
        ["opened", "released", "dropped", "decoder closed"]
    );
    send(guest.as_mut(), delta(2, 1));
    match next(guest.as_mut()) {
        Body::VideoError(error) => assert!(error.error.contains("the GPU was reset"), "{error:?}"),
        other => panic!("expected an error, got {other:?}"),
    }
    send(guest.as_mut(), delta(3, 1));
    assert_eq!(value(&frame(guest.as_mut())), None);
    send(guest.as_mut(), key(4, 20));
    let first = frame(guest.as_mut());
    assert_eq!((first.frame_index, value(&first)), (4, Some(20)));
    assert_eq!(
        *events.lock().unwrap(),
        [
            "opened",
            "released",
            "dropped",
            "decoder closed",
            "refused",
            "opened"
        ]
    );
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
}

fn kept(id: u64, data: &[u8], codec_config: bool) -> Packet {
    Packet {
        id,
        data: data.to_vec(),
        codec_config,
        timestamp_us: id as i64,
    }
}

fn ids(packets: &[Packet]) -> Vec<u64> {
    packets.iter().map(|p| p.id).collect()
}

#[test]
fn nfr1_restore_keeps_only_the_packets_since_the_last_key_frame() {
    let mut restore = Restore::new();
    assert!(!restore.can_restore(), "nothing before the first key frame");
    restore.configuration(&kept(0, b"sps", true));
    restore.configuration(&kept(1, b"pps", true));
    restore.decoded(&kept(2, b"delta before any key", false), Access::Delta);
    assert!(!restore.can_restore());
    restore.decoded(&kept(3, b"key", false), Access::Key);
    restore.decoded(&kept(4, b"d", false), Access::Delta);
    assert_eq!(ids(&restore.packets()), [0, 1, 3, 4]);
    // The next key frame starts what is kept anew, and what came before it goes.
    restore.decoded(&kept(5, b"key", false), Access::Key);
    restore.decoded(&kept(6, b"d", false), Access::Delta);
    assert_eq!(ids(&restore.packets()), [0, 1, 5, 6]);
    assert_eq!(restore.held_bytes(), 3 + 3 + 3 + 3 + 3 + 1);
}

#[test]
fn pr4_restore_gives_the_configuration_in_effect_at_the_key_frame() {
    let mut restore = Restore::new();
    restore.configuration(&kept(0, b"sps 1", true));
    restore.decoded(&kept(1, b"key", false), Access::Key);
    // New parameter sets between pictures: they take their place among them, and the ones the
    // key frame was decoded with still come first.
    restore.configuration(&kept(2, b"sps 2", true));
    restore.decoded(&kept(3, b"d", false), Access::Delta);
    assert_eq!(ids(&restore.packets()), [0, 1, 2, 3]);
    // The latest run is what a decoder waiting for the next key frame is given.
    assert_eq!(ids(&restore.config_packets()), [2]);
    restore.decoded(&kept(4, b"key", false), Access::Key);
    assert_eq!(ids(&restore.packets()), [2, 4]);
}

#[test]
fn pr4_restore_after_a_flush_or_failure_waits_for_the_next_key_frame() {
    let mut restore = Restore::new();
    restore.configuration(&kept(0, b"sps", true));
    restore.decoded(&kept(1, b"key", false), Access::Key);
    restore.lost();
    assert!(!restore.can_restore());
    assert!(restore.packets().is_empty());
    restore.decoded(&kept(2, b"d", false), Access::Delta);
    assert!(
        !restore.can_restore(),
        "a delta frame after a flush starts nothing"
    );
    // The parameter sets stay: the guest does not send them again.
    assert_eq!(ids(&restore.config_packets()), [0]);
    restore.decoded(&kept(3, b"key", false), Access::Key);
    assert_eq!(ids(&restore.packets()), [0, 3]);
}

#[test]
fn nfr1_restore_gives_up_past_its_limits() {
    let mut restore = Restore::new();
    restore.decoded(&kept(0, b"key", false), Access::Key);
    for id in 1..MAX_RESTORE_PACKETS as u64 {
        restore.decoded(&kept(id, b"d", false), Access::Delta);
    }
    assert!(restore.can_restore());
    assert_eq!(restore.kept_packets(), MAX_RESTORE_PACKETS);
    restore.decoded(&kept(9999, b"d", false), Access::Delta);
    assert!(
        !restore.can_restore(),
        "too many packets since the key frame"
    );
    assert_eq!(restore.held_bytes(), 0);
    // A large stream: past the byte limit, the next key frame starts again.
    restore.decoded(
        &kept(0, &vec![1; MAX_RESTORE_BYTES / 2], false),
        Access::Key,
    );
    restore.decoded(
        &kept(1, &vec![1; MAX_RESTORE_BYTES / 2], false),
        Access::Delta,
    );
    assert!(restore.can_restore());
    restore.decoded(&kept(2, &[1], false), Access::Delta);
    assert!(!restore.can_restore(), "too many bytes since the key frame");
    restore.decoded(&kept(3, b"key", false), Access::Key);
    assert!(restore.can_restore());
}

#[test]
fn pr4_restore_from_an_open_key_frame_leaves_its_leading_pictures_out() {
    // An HEVC CRA's leading pictures that refer to pictures before it cannot be decoded by a
    // decoder that starts at it.
    let mut restore = Restore::new();
    restore.decoded(&kept(0, b"cra", false), Access::OpenKey);
    restore.decoded(&kept(1, b"rasl", false), Access::Leading);
    restore.decoded(&kept(2, b"rasl", false), Access::Leading);
    restore.decoded(&kept(3, b"trail", false), Access::Delta);
    restore.decoded(&kept(4, b"trail", false), Access::Delta);
    assert_eq!(ids(&restore.packets()), [0, 3, 4]);
}
