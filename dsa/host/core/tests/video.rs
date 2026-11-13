//! The video port: the handshake, the open, and packets in and pictures out, with a fake
//! decoder standing in for the platform's hardware decoder.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::video::{
    serve_connection, Codec, CodecSupport, DecoderReport, Packet, Picture, StreamStats,
    VideoDecoder, VideoDecoders, VideoEnd, PICTURES_IN_FLIGHT, STATS_INTERVAL,
};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    Envelope, Hello, Role, VideoCodec, VideoDecoderOpen, VideoFlush, VideoFrame, VideoPacket,
};
use dsa_protocol::handshake::local_hello;
use dsa_protocol::{decode, encode};
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);

/// A decoder that turns each packet into a 2x2 picture holding the packet's first byte, and
/// holds back one picture to show that output can lag input, as a real decoder's does.
#[derive(Default)]
struct FakeDecoder {
    held: Option<Picture>,
    log: Arc<Mutex<Vec<String>>>,
}

fn picture(id: u64, value: u8) -> Picture {
    Picture {
        id,
        width: 2,
        height: 2,
        stride: 2,
        nv12: vec![value; 6],
    }
}

impl VideoDecoder for FakeDecoder {
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("decode {}", packet.id));
        if packet.data == b"bad" {
            return Err("the stream is corrupt".into());
        }
        if packet.codec_config {
            return Ok(Vec::new());
        }
        let next = picture(packet.id, packet.data[0]);
        Ok(self.held.replace(next).into_iter().collect())
    }

    fn drain(&mut self) -> Result<Vec<Picture>, String> {
        self.log.lock().unwrap().push("drain".into());
        Ok(self.held.take().into_iter().collect())
    }

    fn flush(&mut self) {
        self.log.lock().unwrap().push("flush".into());
        self.held = None;
    }
}

struct FakeDecoders {
    log: Arc<Mutex<Vec<String>>>,
}

impl VideoDecoders for FakeDecoders {
    fn capabilities(&self) -> Vec<CodecSupport> {
        vec![CodecSupport {
            codec: Codec::H264,
            max_width: 1920,
            max_height: 1088,
        }]
    }

    fn open(&self, codec: Codec, width: u32, height: u32) -> Result<Box<dyn VideoDecoder>, String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("open {} {width}x{height}", codec.name()));
        Ok(Box::new(FakeDecoder {
            held: None,
            log: Arc::clone(&self.log),
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

/// Starts the host's end of one connection and returns the guest's end, past the
/// handshake and the capabilities, with the log of what the decoder was asked.
/// The guest's end, the host's thread, and the log of what the decoder was asked.
type Connected = (
    Box<dyn Connection>,
    thread::JoinHandle<VideoEnd>,
    Arc<Mutex<Vec<String>>>,
);

fn connect(role: Role) -> Connected {
    let (mut listener, mut connector) = memory::pair();
    let log = Arc::new(Mutex::new(Vec::new()));
    let decoders = FakeDecoders {
        log: Arc::clone(&log),
    };
    let host = thread::spawn(move || {
        let connection = listener.accept(WAIT).expect("accepts");
        serve_connection(connection, &decoders, "dsa-host test")
    });
    let mut guest = connector.connect(WAIT).expect("connects");
    let hello: Hello = local_hello(role, "dsa-codec2 test");
    send(guest.as_mut(), Body::Hello(hello));
    (guest, host, log)
}

fn open(guest: &mut dyn Connection, codec: VideoCodec, width: u32, height: u32) {
    assert!(matches!(next(guest), Body::HelloAccepted(_)));
    match next(guest) {
        Body::VideoCapabilities(caps) => {
            assert_eq!(caps.codecs.len(), 1);
            assert_eq!(caps.codecs[0].codec, VideoCodec::H264 as i32);
            assert_eq!(caps.codecs[0].max_width, 1920);
        }
        other => panic!("expected the capabilities, got {other:?}"),
    }
    send(
        guest,
        Body::VideoDecoderOpen(VideoDecoderOpen {
            codec: codec as i32,
            width,
            height,
        }),
    );
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

fn frame(body: Body) -> VideoFrame {
    match body {
        Body::VideoFrame(frame) => frame,
        other => panic!("expected a picture, got {other:?}"),
    }
}

#[test]
fn pr4_a_guest_h264_stream_decodes_on_the_host_in_order() {
    let (mut guest, host, log) = connect(Role::GuestVideo);
    open(guest.as_mut(), VideoCodec::H264, 1280, 720);
    match next(guest.as_mut()) {
        Body::VideoDecoderOpened(opened) => assert!(opened.ok, "{}", opened.error),
        other => panic!("expected the answer, got {other:?}"),
    }
    send(
        guest.as_mut(),
        Body::VideoPacket(VideoPacket {
            frame_index: 0,
            timestamp_us: 0,
            data: vec![0, 0, 0, 1, 0x67],
            codec_config: true,
            end_of_stream: false,
        }),
    );
    send(guest.as_mut(), packet(1, &[10]));
    send(guest.as_mut(), packet(2, &[20]));
    let first = frame(next(guest.as_mut()));
    assert_eq!(first.frame_index, 1);
    assert_eq!(first.timestamp_us, 33_333);
    assert_eq!((first.width, first.height, first.stride), (2, 2, 2));
    assert_eq!(first.nv12, vec![10; 6]);

    // The end of the stream brings out the picture held, then the end.
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
    let second = frame(next(guest.as_mut()));
    assert_eq!(second.frame_index, 2);
    assert_eq!(second.nv12, vec![20; 6]);
    let end = frame(next(guest.as_mut()));
    assert!(end.end_of_stream);
    assert!(end.nv12.is_empty());

    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
    assert_eq!(
        *log.lock().unwrap(),
        [
            "open H.264 1280x720",
            "decode 0",
            "decode 1",
            "decode 2",
            "drain"
        ]
    );
}

#[test]
fn pr4_a_flush_drops_what_the_decoder_held() {
    let (mut guest, host, log) = connect(Role::GuestVideo);
    open(guest.as_mut(), VideoCodec::H264, 0, 0);
    assert!(matches!(next(guest.as_mut()), Body::VideoDecoderOpened(o) if o.ok));
    send(guest.as_mut(), packet(1, &[10]));
    send(guest.as_mut(), Body::VideoFlush(VideoFlush {}));
    assert!(matches!(next(guest.as_mut()), Body::VideoFlushed(_)));
    send(guest.as_mut(), packet(7, &[70]));
    send(guest.as_mut(), packet(8, &[80]));
    // Nothing from before the flush comes out.
    assert_eq!(frame(next(guest.as_mut())).frame_index, 7);
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
    assert!(log.lock().unwrap().contains(&"flush".to_string()));
}

#[test]
fn pr4_a_corrupt_packet_is_an_error_on_that_stream_only() {
    let (mut guest, host, _log) = connect(Role::GuestVideo);
    open(guest.as_mut(), VideoCodec::H264, 0, 0);
    assert!(matches!(next(guest.as_mut()), Body::VideoDecoderOpened(o) if o.ok));
    send(guest.as_mut(), packet(1, b"bad"));
    match next(guest.as_mut()) {
        Body::VideoError(error) => assert_eq!(error.error, "the stream is corrupt"),
        other => panic!("expected an error, got {other:?}"),
    }
    // The stream goes on.
    send(guest.as_mut(), packet(2, &[20]));
    send(guest.as_mut(), packet(3, &[30]));
    assert_eq!(frame(next(guest.as_mut())).frame_index, 2);
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
}

#[test]
fn pr4_a_format_the_host_cannot_decode_is_refused_with_the_reason() {
    let (mut guest, host, log) = connect(Role::GuestVideo);
    open(guest.as_mut(), VideoCodec::Av1, 1920, 1080);
    match next(guest.as_mut()) {
        Body::VideoDecoderOpened(opened) => {
            assert!(!opened.ok);
            assert!(opened.error.contains("AV1"), "{}", opened.error);
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    assert!(matches!(host.join().unwrap(), VideoEnd::Failed(_)));
    assert!(log.lock().unwrap().is_empty(), "no decoder was opened");
}

#[test]
fn pr4_a_stream_larger_than_the_decoder_takes_is_refused() {
    let (mut guest, host, _log) = connect(Role::GuestVideo);
    open(guest.as_mut(), VideoCodec::H264, 3840, 2160);
    match next(guest.as_mut()) {
        Body::VideoDecoderOpened(opened) => assert!(!opened.ok),
        other => panic!("expected the refusal, got {other:?}"),
    }
    assert!(matches!(host.join().unwrap(), VideoEnd::Failed(_)));
}

#[test]
fn pr4_the_video_port_refuses_any_other_role() {
    let (mut guest, host, _log) = connect(Role::Guest);
    assert!(matches!(next(guest.as_mut()), Body::HelloRejected(_)));
    assert!(matches!(host.join().unwrap(), VideoEnd::Failed(_)));
}

#[test]
fn pr4_an_nv12_picture_is_a_luma_plane_and_half_as_many_chroma_rows() {
    assert_eq!(Picture::nv12_len(1920, 1080, 1920), Some(1920 * 1620));
    assert_eq!(Picture::nv12_len(5, 5, 8), Some(8 * 8));
    assert_eq!(Picture::nv12_len(8, 4, 4), None);
    assert_eq!(Picture::nv12_len(0, 4, 4), None);
}

/// Records the presentation time each packet reaches the decoder with.
struct TimedDecoders {
    times: Arc<Mutex<Vec<(u64, i64)>>>,
}

struct TimedDecoder {
    times: Arc<Mutex<Vec<(u64, i64)>>>,
}

impl VideoDecoder for TimedDecoder {
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
        self.times
            .lock()
            .unwrap()
            .push((packet.id, packet.timestamp_us));
        Ok(Vec::new())
    }
    fn drain(&mut self) -> Result<Vec<Picture>, String> {
        Ok(Vec::new())
    }
    fn flush(&mut self) {}
}

impl VideoDecoders for TimedDecoders {
    fn capabilities(&self) -> Vec<CodecSupport> {
        FakeDecoders {
            log: Arc::default(),
        }
        .capabilities()
    }
    fn open(&self, _: Codec, _: u32, _: u32) -> Result<Box<dyn VideoDecoder>, String> {
        Ok(Box::new(TimedDecoder {
            times: Arc::clone(&self.times),
        }))
    }
}

#[test]
fn pr4_each_packet_reaches_the_decoder_with_its_presentation_time() {
    let (mut listener, mut connector) = memory::pair();
    let times = Arc::new(Mutex::new(Vec::new()));
    let decoders = TimedDecoders {
        times: Arc::clone(&times),
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
    open(guest.as_mut(), VideoCodec::H264, 1280, 720);
    assert!(matches!(next(guest.as_mut()), Body::VideoDecoderOpened(o) if o.ok));
    // Decode order is not presentation order: a B frame comes after the frame it precedes.
    send(guest.as_mut(), packet(1, &[10]));
    send(guest.as_mut(), packet(3, &[30]));
    send(guest.as_mut(), packet(2, &[20]));
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
    let times: Vec<i64> = times.lock().unwrap().iter().map(|(_, t)| *t).collect();
    assert_eq!(times, [33_333, 99_999, 66_666]);
}

/// Decoders that record when each decoder is dropped and when the host says every one is
/// closed.
struct ReleasedDecoders {
    events: Arc<Mutex<Vec<String>>>,
}

struct ReleasedDecoder {
    events: Arc<Mutex<Vec<String>>>,
}

impl VideoDecoder for ReleasedDecoder {
    fn decode(&mut self, _: Packet) -> Result<Vec<Picture>, String> {
        Ok(Vec::new())
    }
    fn drain(&mut self) -> Result<Vec<Picture>, String> {
        Ok(Vec::new())
    }
    fn flush(&mut self) {}
}

impl Drop for ReleasedDecoder {
    fn drop(&mut self) {
        self.events.lock().unwrap().push("dropped".into());
    }
}

impl VideoDecoders for ReleasedDecoders {
    fn capabilities(&self) -> Vec<CodecSupport> {
        FakeDecoders {
            log: Arc::default(),
        }
        .capabilities()
    }
    fn open(&self, _: Codec, _: u32, _: u32) -> Result<Box<dyn VideoDecoder>, String> {
        self.events.lock().unwrap().push("opened".into());
        Ok(Box::new(ReleasedDecoder {
            events: Arc::clone(&self.events),
        }))
    }
    fn all_closed(&self) {
        self.events.lock().unwrap().push("all closed".into());
    }
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
fn nfr2_the_platform_hears_once_the_last_decoder_closed_so_it_lets_its_device_go() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (listener, connector) = memory::pair();
    let events = Arc::new(Mutex::new(Vec::new()));
    let decoders: Arc<dyn VideoDecoders> = Arc::new(ReleasedDecoders {
        events: Arc::clone(&events),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let host = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            dsa_host_core::video::serve(
                Box::new(listener),
                decoders,
                "dsa-host test".into(),
                stop,
                Arc::new(|_: &str| {}),
            )
        })
    };
    let mut guests = Vec::new();
    for _ in 0..2 {
        let mut guest = connector.clone().connect(WAIT).expect("connects");
        send(
            guest.as_mut(),
            Body::Hello(local_hello(Role::GuestVideo, "dsa-codec2 test")),
        );
        open(guest.as_mut(), VideoCodec::H264, 1280, 720);
        assert!(matches!(next(guest.as_mut()), Body::VideoDecoderOpened(o) if o.ok));
        guests.push(guest);
    }
    assert_eq!(wait_for(&events, 2), ["opened", "opened"]);

    // One video closes while the other still plays: the platform keeps what it shares.
    guests[0].close();
    assert_eq!(wait_for(&events, 3), ["opened", "opened", "dropped"]);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        events.lock().unwrap().len(),
        3,
        "{:?}",
        events.lock().unwrap()
    );

    // The last one closes: its decoder is gone first, then the platform hears it.
    guests[1].close();
    assert_eq!(
        wait_for(&events, 5),
        ["opened", "opened", "dropped", "dropped", "all closed"]
    );
    stop.store(true, Ordering::Relaxed);
    host.join().unwrap();
}

#[test]
fn fr26_a_streams_numbers_go_to_the_log_every_few_seconds() {
    let start = Instant::now();
    let mut stats = StreamStats::default();
    // Nothing has happened: nothing to say.
    assert_eq!(stats.report(start + STATS_INTERVAL * 2, Codec::Av1), None);
    for i in 0..150u32 {
        let at = start + Duration::from_millis(u64::from(i) * 33);
        stats.decoded(at, Duration::from_millis(4), 1);
        stats.decoder(DecoderReport {
            readback: Duration::from_millis(1),
            readbacks: 1,
            surface: Some("640x384 NV12, pitch 640, chroma at row 384".into()),
        });
        if i < 2 {
            stats.dropped(at, true);
            continue;
        }
        stats.handed_on(at, (640, 360), Duration::from_millis(10));
        stats.sent(Duration::from_millis(1));
    }
    // Not before the interval.
    assert_eq!(
        stats.report(start + Duration::from_secs(4), Codec::Av1),
        None
    );
    let line = stats
        .report(start + STATS_INTERVAL, Codec::Av1)
        .expect("a line once the interval has passed");
    assert!(
        line.starts_with("video: AV1 640x360 over 5.0 s: 150 packets"),
        "{line}"
    );
    assert!(line.contains("150 pictures decoded (30.0/s)"), "{line}");
    assert!(line.contains("148 handed on (29.6/s)"), "{line}");
    assert!(
        line.contains("2 dropped (2 flushed, 0 malformed)"),
        "{line}"
    );
    assert!(
        line.contains("decode 4.00 (copy out of the GPU 1.00)"),
        "{line}"
    );
    assert!(
        line.contains("waiting for the guest to take a picture 10.00"),
        "{line}"
    );
    assert!(line.contains("send 1.00"), "{line}");
    assert!(line.contains("chroma at row 384"), "{line}");
    // A new count starts: the next line comes an interval later, not at once.
    assert_eq!(
        stats.report(start + STATS_INTERVAL + Duration::from_secs(1), Codec::Av1),
        None
    );
}

#[test]
fn fr26_messages_keep_their_order_behind_the_pictures_in_flight() {
    // More pictures than may wait for the guest, then a flush: every picture comes out in
    // its order and the flush's answer after them, as the guest reads them.
    let (mut guest, host, _log) = connect(Role::GuestVideo);
    open(guest.as_mut(), VideoCodec::H264, 0, 0);
    assert!(matches!(next(guest.as_mut()), Body::VideoDecoderOpened(o) if o.ok));
    let count = PICTURES_IN_FLIGHT as u64 * 3;
    for index in 1..=count + 1 {
        send(guest.as_mut(), packet(index, &[index as u8]));
    }
    for index in 1..=count {
        let picture = frame(next(guest.as_mut()));
        assert_eq!(picture.frame_index, index);
        assert_eq!(picture.nv12, vec![index as u8; 6]);
    }
    send(guest.as_mut(), Body::VideoFlush(VideoFlush {}));
    assert!(matches!(next(guest.as_mut()), Body::VideoFlushed(_)));
    guest.close();
    assert_eq!(host.join().unwrap(), VideoEnd::Closed);
}
