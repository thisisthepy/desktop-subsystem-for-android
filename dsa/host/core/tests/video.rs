//! The video port: the handshake, the open, and packets in and pictures out, with a fake
//! decoder standing in for the platform's hardware decoder.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::video::{
    serve_connection, Codec, CodecSupport, Packet, Picture, VideoDecoder, VideoDecoders, VideoEnd,
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
fn connect(
    role: Role,
) -> (
    Box<dyn Connection>,
    thread::JoinHandle<VideoEnd>,
    Arc<Mutex<Vec<String>>>,
) {
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
