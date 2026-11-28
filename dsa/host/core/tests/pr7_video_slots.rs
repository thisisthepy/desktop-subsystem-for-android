//! Decoded pictures through memory the host shares with the guest: the slots' ownership, the
//! handoff on the video port, what comes back when a connection ends, and the copy path a
//! stream falls back to. The region is a fake in this process's memory, and the connection
//! the in-memory transport.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::video::{
    serve_connection, serve_connection_logged, Codec, CodecSupport, Packet, Picture, VideoDecoder,
    VideoDecoders, VideoEnd, SLOT_WAIT,
};
use dsa_host_core::video_slots::{
    read_header, write_header, MemoryRegion, PictureSlots, SharedRegion, StreamSlots, MAGIC,
    SLOTS_PER_STREAM, SLOTS_START, SLOT_ALIGNMENT,
};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    Envelope, Role, VideoCodec, VideoDecoderOpen, VideoFrame, VideoPacket, VideoSlotReturned,
};
use dsa_protocol::handshake::local_hello;
use dsa_protocol::{decode, encode};
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);
const REGION_ID: u64 = 0x5eed_1234_abcd_0001;
const REGION_BYTES: usize = 4 * 1024 * 1024 + SLOTS_START;

/// A 4x2 picture (12 bytes of NV12) holding `value` in every sample.
const WIDTH: u32 = 4;
const HEIGHT: u32 = 2;
const PICTURE_BYTES: usize = 12;

fn region() -> Arc<MemoryRegion> {
    Arc::new(MemoryRegion::new(REGION_BYTES, REGION_ID))
}

/// A decoder that turns each packet into a picture of its first byte, at once. One that
/// writes `direct`ly writes it into a slot itself, as the Windows decoder does.
struct SlotDecoder {
    slots: Option<Arc<StreamSlots>>,
    direct: bool,
}

impl VideoDecoder for SlotDecoder {
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String> {
        let value = packet.data[0];
        if let (true, Some(slots)) = (self.direct, &self.slots) {
            if let Some(slot) = slots.take(PICTURE_BYTES) {
                let written = slots.write(&slot, |bytes| {
                    bytes.fill(value);
                    true
                });
                if written {
                    return Ok(vec![Picture {
                        id: packet.id,
                        width: WIDTH,
                        height: HEIGHT,
                        stride: WIDTH,
                        nv12: Vec::new(),
                        slot: Some(slot),
                    }]);
                }
                // A slot that cannot be written goes back, and the picture is read into
                // memory, as the Windows decoder does.
                slots.give_back(slot.number);
            }
        }
        Ok(vec![Picture {
            id: packet.id,
            width: WIDTH,
            height: HEIGHT,
            stride: WIDTH,
            nv12: vec![value; PICTURE_BYTES],
            slot: None,
        }])
    }

    fn drain(&mut self) -> Result<Vec<Picture>, String> {
        Ok(Vec::new())
    }

    fn flush(&mut self) {}

    fn use_slots(&mut self, slots: Arc<StreamSlots>) {
        self.slots = Some(slots);
    }
}

struct SharedDecoders {
    slots: Option<Arc<PictureSlots>>,
    direct: bool,
}

impl VideoDecoders for SharedDecoders {
    fn capabilities(&self) -> Vec<CodecSupport> {
        vec![CodecSupport {
            codec: Codec::Av1,
            max_width: 4096,
            max_height: 2304,
        }]
    }

    fn open(&self, _: Codec, _: u32, _: u32) -> Result<Box<dyn VideoDecoder>, String> {
        Ok(Box::new(SlotDecoder {
            slots: None,
            direct: self.direct,
        }))
    }

    fn picture_slots(&self) -> Result<Arc<PictureSlots>, String> {
        self.slots
            .clone()
            .ok_or_else(|| "no region in this test".to_string())
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

fn packet(frame_index: u64, value: u8) -> Body {
    Body::VideoPacket(VideoPacket {
        frame_index,
        timestamp_us: frame_index as i64 * 16_666,
        data: vec![value],
        codec_config: false,
        end_of_stream: false,
    })
}

/// The guest's end of a stream opened naming the region `guest_id`, whether the host said its
/// pictures may come in shared memory, the host's thread, and what the host logged.
struct Stream {
    guest: Box<dyn Connection>,
    shared: bool,
    host: thread::JoinHandle<VideoEnd>,
    log: Arc<Mutex<Vec<String>>>,
}

fn open(decoders: SharedDecoders, guest_id: u64) -> Stream {
    let (mut listener, mut connector) = memory::pair();
    let log = Arc::new(Mutex::new(Vec::new()));
    let host_log = Arc::clone(&log);
    let host = thread::spawn(move || {
        let connection = listener.accept(WAIT).expect("accepts");
        serve_connection_logged(connection, &decoders, "dsa-host test", &|line: &str| {
            host_log.lock().unwrap().push(line.to_string())
        })
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
            codec: VideoCodec::Av1 as i32,
            width: 3840,
            height: 1920,
            picture_region_id: guest_id,
        }),
    );
    let shared = match next(guest.as_mut()) {
        Body::VideoDecoderOpened(opened) => {
            assert!(opened.ok, "{}", opened.error);
            opened.shared_pictures
        }
        other => panic!("expected the answer, got {other:?}"),
    };
    Stream {
        guest,
        shared,
        host,
        log,
    }
}

#[test]
fn pr7_a_picture_goes_in_a_slot_and_only_its_number_crosses_the_connection() {
    for direct in [false, true] {
        let region = region();
        let slots = PictureSlots::open(region.clone()).expect("a region with a header");
        let mut stream = open(
            SharedDecoders {
                slots: Some(Arc::clone(&slots)),
                direct,
            },
            REGION_ID,
        );
        assert!(stream.shared);
        send(stream.guest.as_mut(), packet(1, 0x41));
        let picture = frame(next(stream.guest.as_mut()));
        assert_eq!(picture.frame_index, 1);
        assert_eq!((picture.width, picture.height, picture.stride), (4, 2, 4));
        assert_ne!(picture.slot, 0, "the picture names a slot");
        assert!(picture.nv12.is_empty(), "no picture bytes in the message");
        let offset = picture.slot_offset as usize;
        assert!(offset >= SLOTS_START && offset % SLOT_ALIGNMENT == 0);
        assert_eq!(
            region.read(offset, PICTURE_BYTES),
            vec![0x41; PICTURE_BYTES]
        );
        stream.guest.close();
        assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
    }
}

#[test]
fn fr26_a_slot_the_decoder_never_wrote_goes_back_and_its_packet_gets_no_picture() {
    // A surface the decoder handed back unwritten holds zeros, solid green in the app. Before,
    // it went to the guest like any picture, in its slot or its message.
    for direct in [false, true] {
        let region = region();
        let slots = PictureSlots::open(region.clone()).unwrap();
        let mut stream = open(
            SharedDecoders {
                slots: Some(Arc::clone(&slots)),
                direct,
            },
            REGION_ID,
        );
        send(stream.guest.as_mut(), packet(1, 0));
        let none = frame(next(stream.guest.as_mut()));
        assert_eq!(none.frame_index, 1, "the packet's work ends at once");
        assert_eq!((none.width, none.height), (0, 0), "with no picture");
        assert_eq!(none.slot, 0);
        assert!(none.nv12.is_empty());
        assert!(!none.end_of_stream);
        assert_eq!(slots.held(), 0, "its slot went back at once");
        // The stream goes on with the next picture, in a slot.
        send(stream.guest.as_mut(), packet(2, 0x42));
        let next_picture = frame(next(stream.guest.as_mut()));
        assert_eq!(next_picture.frame_index, 2);
        assert_ne!(next_picture.slot, 0);
        assert_eq!(
            region.read(next_picture.slot_offset as usize, PICTURE_BYTES),
            vec![0x42; PICTURE_BYTES]
        );
        stream.guest.close();
        assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
    }
}

#[test]
fn pr7_the_host_does_not_write_a_slot_the_guest_still_holds() {
    let region = region();
    let slots = PictureSlots::open(region.clone()).unwrap();
    let mut stream = open(
        SharedDecoders {
            slots: Some(Arc::clone(&slots)),
            direct: true,
        },
        REGION_ID,
    );
    send(stream.guest.as_mut(), packet(1, 1));
    let first = frame(next(stream.guest.as_mut()));
    send(stream.guest.as_mut(), packet(2, 2));
    let second = frame(next(stream.guest.as_mut()));
    // Held by the guest: the second picture went elsewhere, and the first is untouched.
    assert_ne!(first.slot_offset, second.slot_offset);
    assert_eq!(
        region.read(first.slot_offset as usize, PICTURE_BYTES),
        vec![1; PICTURE_BYTES]
    );
    // Given back, the first slot's place is used again.
    send(
        stream.guest.as_mut(),
        Body::VideoSlotReturned(VideoSlotReturned { slot: first.slot }),
    );
    send(stream.guest.as_mut(), packet(3, 3));
    let third = frame(next(stream.guest.as_mut()));
    assert_eq!(third.slot_offset, first.slot_offset);
    assert_ne!(third.slot, first.slot, "a reused place gets a new number");
    assert_eq!(
        region.read(third.slot_offset as usize, PICTURE_BYTES),
        vec![3; PICTURE_BYTES]
    );
    assert_eq!(
        region.read(second.slot_offset as usize, PICTURE_BYTES),
        vec![2; PICTURE_BYTES]
    );
    stream.guest.close();
    assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
}

#[test]
fn pr7_a_guest_that_keeps_its_slots_gets_the_next_pictures_in_messages() {
    let region = region();
    let slots = PictureSlots::open(region.clone()).unwrap();
    let mut stream = open(
        SharedDecoders {
            slots: Some(Arc::clone(&slots)),
            direct: false,
        },
        REGION_ID,
    );
    let started = Instant::now();
    for index in 1..=SLOTS_PER_STREAM as u64 + 1 {
        send(stream.guest.as_mut(), packet(index, index as u8));
    }
    for index in 1..=SLOTS_PER_STREAM as u64 {
        let picture = frame(next(stream.guest.as_mut()));
        assert_eq!(picture.frame_index, index);
        assert_ne!(picture.slot, 0, "picture {index} has a slot");
    }
    // Every slot the stream may hold is the guest's: the next picture waited for one, then
    // came in its message.
    let last = frame(next(stream.guest.as_mut()));
    assert_eq!(last.slot, 0);
    assert_eq!(last.nv12, vec![SLOTS_PER_STREAM as u8 + 1; PICTURE_BYTES]);
    assert!(started.elapsed() >= SLOT_WAIT);
    stream.guest.close();
    assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
}

#[test]
fn pr7_every_slot_comes_back_when_the_connection_ends_and_the_memory_is_given_back() {
    let region = region();
    let slots = PictureSlots::open(region.clone()).unwrap();
    let mut stream = open(
        SharedDecoders {
            slots: Some(Arc::clone(&slots)),
            direct: true,
        },
        REGION_ID,
    );
    send(stream.guest.as_mut(), packet(1, 9));
    send(stream.guest.as_mut(), packet(2, 9));
    frame(next(stream.guest.as_mut()));
    frame(next(stream.guest.as_mut()));
    assert_eq!(slots.held(), 2);
    assert!(
        region.discarded().is_empty(),
        "nothing given back while it plays"
    );
    // The guest goes without giving them back.
    stream.guest.close();
    assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
    assert_eq!(slots.held(), 0);
    let discarded = region.discarded();
    assert_eq!(discarded.len(), 1, "{discarded:?}");
    assert_eq!(discarded[0].0, SLOTS_START);
    assert!(discarded[0].1 >= 2 * SLOT_ALIGNMENT);
}

#[test]
fn nfr1_the_pages_no_slot_holds_go_back_while_a_stream_still_plays() {
    let region = region();
    let slots = PictureSlots::open(region.clone()).unwrap();
    let stream = slots.stream();
    // A large picture, then a small one placed after it while the large one is lent.
    let large = stream.take(3 * SLOT_ALIGNMENT).unwrap();
    assert!(stream.write(&large, |_| true));
    let small = stream.take(PICTURE_BYTES).unwrap();
    assert!(stream.write(&small, |_| true));
    assert_eq!(small.offset as usize, SLOTS_START + 3 * SLOT_ALIGNMENT);
    assert_eq!(slots.touched(), 4 * SLOT_ALIGNMENT);
    // The large one comes back and the stream sends only small ones from now on: its pages
    // go back, the small slot's stay.
    assert!(stream.give_back(large.number));
    assert_eq!(slots.give_back_unused(), 3 * SLOT_ALIGNMENT);
    assert_eq!(region.discarded(), vec![(SLOTS_START, 3 * SLOT_ALIGNMENT)]);
    assert_eq!(
        slots.touched(),
        4 * SLOT_ALIGNMENT,
        "the small slot still reaches that far"
    );
    // Nothing more to give back while the small one is held.
    assert_eq!(slots.give_back_unused(), 0);
    assert!(stream.give_back(small.number));
    assert_eq!(slots.give_back_unused(), SLOT_ALIGNMENT);
    assert_eq!(slots.touched(), 0);
    // A slot taken again where the pages went is written again.
    let again = stream.take(3 * SLOT_ALIGNMENT).unwrap();
    assert_eq!(again.offset as usize, SLOTS_START);
    assert!(stream.write(&again, |bytes| {
        bytes.fill(5);
        true
    }));
}

#[test]
fn pr7_a_guest_that_maps_another_region_gets_its_pictures_in_messages() {
    let slots = PictureSlots::open(region()).unwrap();
    let mut stream = open(
        SharedDecoders {
            slots: Some(slots),
            direct: true,
        },
        REGION_ID ^ 1,
    );
    assert!(!stream.shared);
    send(stream.guest.as_mut(), packet(1, 7));
    let picture = frame(next(stream.guest.as_mut()));
    assert_eq!(picture.slot, 0);
    assert_eq!(picture.nv12, vec![7; PICTURE_BYTES]);
    stream.guest.close();
    assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
    let log = stream.log.lock().unwrap().join("\n");
    assert!(log.contains("another shared picture region"), "{log}");
}

#[test]
fn pr7_without_a_region_every_picture_comes_in_its_message() {
    for guest_id in [0, REGION_ID] {
        let mut stream = open(
            SharedDecoders {
                slots: None,
                direct: true,
            },
            guest_id,
        );
        assert!(!stream.shared);
        send(stream.guest.as_mut(), packet(1, 5));
        let picture = frame(next(stream.guest.as_mut()));
        assert_eq!(picture.slot, 0);
        assert_eq!(picture.nv12, vec![5; PICTURE_BYTES]);
        // A stray slot number from the guest changes nothing.
        send(
            stream.guest.as_mut(),
            Body::VideoSlotReturned(VideoSlotReturned { slot: 3 }),
        );
        send(stream.guest.as_mut(), packet(2, 6));
        assert_eq!(frame(next(stream.guest.as_mut())).frame_index, 2);
        stream.guest.close();
        assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
    }
}

#[test]
fn pr7_the_default_host_shares_nothing_and_says_so() {
    struct Plain;
    impl VideoDecoders for Plain {
        fn capabilities(&self) -> Vec<CodecSupport> {
            Vec::new()
        }
        fn open(&self, _: Codec, _: u32, _: u32) -> Result<Box<dyn VideoDecoder>, String> {
            Err("none".into())
        }
    }
    assert!(Plain.picture_slots().is_err());
    // A connection to it still works the old way.
    let (mut listener, mut connector) = memory::pair();
    let host = thread::spawn(move || {
        let connection = listener.accept(WAIT).expect("accepts");
        serve_connection(connection, &Plain, "dsa-host test")
    });
    let mut guest = connector.connect(WAIT).expect("connects");
    send(
        guest.as_mut(),
        Body::Hello(local_hello(Role::GuestVideo, "dsa-codec2 test")),
    );
    assert!(matches!(next(guest.as_mut()), Body::HelloAccepted(_)));
    guest.close();
    let _ = host.join().unwrap();
}

#[test]
fn pr7_a_slot_is_written_only_by_its_stream_and_only_before_it_is_lent() {
    let slots = PictureSlots::open(region()).unwrap();
    let one = slots.stream();
    let two = slots.stream();
    let slot = one.take(100).expect("room");
    assert_eq!(slot.len, 100);
    assert!(!two.write(&slot, |_| true), "another stream's slot");
    assert!(one.write(&slot, |bytes| {
        bytes.fill(1);
        true
    }));
    assert!(one.lend(&slot));
    assert!(!one.write(&slot, |_| true), "lent to the guest");
    assert!(
        !two.give_back(slot.number),
        "not the other stream's to give back"
    );
    assert!(one.give_back(slot.number));
    assert!(!one.give_back(slot.number), "already back");
    // A stream holds at most its share, and the region no more than it has room for.
    let held: Vec<_> = (0..SLOTS_PER_STREAM).map(|_| one.take(10)).collect();
    assert!(held.iter().all(Option::is_some));
    assert!(one.take(10).is_none());
    assert!(two.take(REGION_BYTES).is_none());
    drop(one);
    assert_eq!(slots.held(), 0);
}

#[test]
fn pr7_a_picture_read_straight_into_a_slot_is_the_picture_read_into_memory() {
    use dsa_host_core::video_layout::{
        picture_from_surface_area, picture_into, picture_stride, Area, SurfaceFormat,
    };
    // A 6x4 surface of NV12 (pitch 8) and of P010 (pitch 16), the picture 3x2 at 2,2.
    let nv12: Vec<u8> = (0..8 * 6).map(|i| i as u8).collect();
    let p010: Vec<u8> = (0..16 * 6).map(|i| (i * 3) as u8).collect();
    let area = Area {
        left: 2,
        top: 2,
        width: 3,
        height: 2,
    };
    for (data, format, pitch) in [
        (&nv12, SurfaceFormat::Nv12, 8),
        (&p010, SurfaceFormat::P010, 16),
    ] {
        let read = picture_from_surface_area(1, data, format, pitch, 4, area).expect("fits");
        assert_eq!(read.stride, picture_stride(3));
        let mut slot = vec![0u8; read.nv12.len()];
        assert!(picture_into(&mut slot, data, format, pitch, 4, area));
        assert_eq!(slot, read.nv12);
        // A slot of another length is refused rather than half written.
        let mut short = vec![0u8; read.nv12.len() - 1];
        assert!(!picture_into(&mut short, data, format, pitch, 4, area));
    }
}

/// A region that only reserves its size, as the Windows section does: it records every commit,
/// refuses those `refuse` says to, and says whether a range was committed before it was written.
struct ReservedRegion {
    memory: MemoryRegion,
    commits: Mutex<Vec<(usize, usize)>>,
    refuse: Box<dyn Fn(usize) -> bool + Send + Sync>,
}

impl ReservedRegion {
    fn new(refuse: impl Fn(usize) -> bool + Send + Sync + 'static) -> Arc<ReservedRegion> {
        Arc::new(ReservedRegion {
            memory: MemoryRegion::new(REGION_BYTES, REGION_ID),
            commits: Mutex::new(Vec::new()),
            refuse: Box::new(refuse),
        })
    }

    fn commits(&self) -> Vec<(usize, usize)> {
        self.commits.lock().unwrap().clone()
    }

    /// Whether every byte of `len` from `offset` lies in a range committed so far.
    fn is_committed(&self, offset: usize, len: usize) -> bool {
        let commits = self.commits();
        (offset..offset + len)
            .step_by(4096)
            .chain([offset + len - 1])
            .all(|byte| commits.iter().any(|&(at, n)| (at..at + n).contains(&byte)))
    }
}

impl SharedRegion for ReservedRegion {
    fn as_ptr(&self) -> *mut u8 {
        self.memory.as_ptr()
    }

    fn len(&self) -> usize {
        self.memory.len()
    }

    fn discard(&self, offset: usize, len: usize) {
        assert!(
            self.is_committed(offset, len),
            "discards only committed pages"
        );
        self.memory.discard(offset, len);
    }

    fn commit(&self, offset: usize, len: usize) -> Result<(), String> {
        if (self.refuse)(offset) {
            return Err("the paging file is too small for this operation to complete".into());
        }
        self.commits.lock().unwrap().push((offset, len));
        Ok(())
    }
}

#[test]
fn pr7_a_slot_is_committed_before_its_first_write_and_only_once() {
    let region = ReservedRegion::new(|_| false);
    let slots = PictureSlots::open(region.clone()).expect("a region with a header");
    // The header is all the host reads before a picture.
    assert_eq!(region.commits(), vec![(0, SLOTS_START)]);
    let stream = slots.stream();
    let slot = stream.take(SLOT_ALIGNMENT + 100).expect("room");
    assert_eq!(region.commits().len(), 1, "taking a slot commits nothing");
    assert!(stream.write(&slot, |bytes| {
        assert!(
            region.is_committed(slot.offset as usize, 2 * SLOT_ALIGNMENT),
            "the slot's pages are committed before it is written"
        );
        bytes.fill(3);
        true
    }));
    assert_eq!(
        region.commits()[1..],
        [(slot.offset as usize, 2 * SLOT_ALIGNMENT)]
    );
    // Written again, or written again after it came back: no second commit.
    assert!(stream.write(&slot, |_| true));
    assert!(stream.give_back(slot.number));
    let again = stream.take(100).expect("room");
    assert_eq!(again.offset, slot.offset);
    assert!(stream.write(&again, |_| true));
    assert_eq!(region.commits().len(), 2, "{:?}", region.commits());
    // A larger slot at the same place commits only the granules it adds.
    assert!(stream.give_back(again.number));
    let larger = stream.take(3 * SLOT_ALIGNMENT).expect("room");
    assert_eq!(larger.offset, slot.offset);
    assert!(stream.write(&larger, |_| true));
    assert_eq!(
        region.commits()[2..],
        [(slot.offset as usize + 2 * SLOT_ALIGNMENT, SLOT_ALIGNMENT)]
    );
    // When the last stream ends, the committed pages are discarded and nothing is committed
    // again for the next one.
    drop(stream);
    assert_eq!(
        region.memory.discarded(),
        vec![(SLOTS_START, 3 * SLOT_ALIGNMENT)]
    );
    let next = slots.stream();
    let slot = next.take(100).expect("room");
    assert!(next.write(&slot, |_| true));
    assert_eq!(region.commits().len(), 3);
    assert_eq!(slots.commit_refused(), None);
}

#[test]
fn pr7_a_slot_whose_commit_is_refused_takes_the_copy_path() {
    // The header was committed when the region was made; nothing after it can be.
    let region = ReservedRegion::new(|offset| offset >= SLOTS_START);
    let slots = PictureSlots::open(region.clone()).expect("the header is committed");
    // A decoder writing a slot itself is told the slot was not written.
    let own = slots.stream();
    let slot = own.take(PICTURE_BYTES).expect("room");
    assert!(!own.write(&slot, |_| panic!("an uncommitted slot is never written")));
    assert!(own.give_back(slot.number));
    let first = slots.commit_refused().expect("the reason is kept");
    assert!(first.contains("paging file"), "{first}");
    drop(own);
    // Through the video port, both kinds of decoder send the picture in its message.
    for direct in [false, true] {
        let mut stream = open(
            SharedDecoders {
                slots: Some(Arc::clone(&slots)),
                direct,
            },
            REGION_ID,
        );
        assert!(stream.shared, "the region is still offered");
        for value in [0x21, 0x22] {
            send(stream.guest.as_mut(), packet(u64::from(value), value));
            let picture = frame(next(stream.guest.as_mut()));
            assert_eq!(picture.slot, 0, "direct {direct}");
            assert_eq!(picture.nv12, vec![value; PICTURE_BYTES]);
        }
        stream.guest.close();
        assert_eq!(stream.host.join().unwrap(), VideoEnd::Closed);
    }
    assert_eq!(slots.held(), 0);
    assert_eq!(region.commits(), vec![(0, SLOTS_START)]);
    assert!(region.memory.discarded().is_empty(), "nothing was written");
    assert_eq!(
        slots.commit_refused(),
        Some(first),
        "the first reason stays"
    );
}

/// A region whose discard takes a while and leaves the pages zeroed, as a discarded page of a
/// Windows section may read: it says when a discard has begun.
struct SlowDiscardRegion {
    memory: MemoryRegion,
    begun: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl SharedRegion for SlowDiscardRegion {
    fn as_ptr(&self) -> *mut u8 {
        self.memory.as_ptr()
    }

    fn len(&self) -> usize {
        self.memory.len()
    }

    fn discard(&self, offset: usize, len: usize) {
        if let Some(begun) = self.begun.lock().unwrap().take() {
            let _ = begun.send(());
        }
        thread::sleep(Duration::from_millis(200));
        // SAFETY: inside the region; the test writes it only through the slots.
        unsafe { std::ptr::write_bytes(self.memory.as_ptr().add(offset), 0, len) };
        self.memory.discard(offset, len);
    }
}

#[test]
fn pr7_a_stream_that_opens_while_the_last_one_is_discarded_keeps_its_first_picture() {
    // The owner's YouTube mini player (dev build 0.2.4-dev.23) stayed a green box, which is
    // what a picture of zeros looks like. A player that moves to a new surface releases its
    // decoder and makes another at once: the new stream must not write its first picture
    // while the slots the old one touched are still being discarded, or the discard wipes it.
    let (begun, discarding) = std::sync::mpsc::channel();
    let region = Arc::new(SlowDiscardRegion {
        memory: MemoryRegion::new(REGION_BYTES, REGION_ID),
        begun: Mutex::new(Some(begun)),
    });
    let slots = PictureSlots::open(region.clone()).unwrap();
    let old = slots.stream();
    let slot = old.take(PICTURE_BYTES).unwrap();
    assert!(old.write(&slot, |bytes| {
        bytes.fill(0x11);
        true
    }));
    let ending = thread::spawn(move || drop(old));
    discarding.recv_timeout(WAIT).expect("the discard begins");
    // The new stream opens while the discard runs.
    let new = slots.stream();
    let first = new.take(PICTURE_BYTES).expect("room");
    assert_eq!(
        first.offset, slot.offset,
        "the first free place is the old stream's"
    );
    assert!(new.write(&first, |bytes| {
        bytes.fill(0x55);
        true
    }));
    ending.join().unwrap();
    assert_eq!(
        region.memory.read(first.offset as usize, PICTURE_BYTES),
        vec![0x55; PICTURE_BYTES],
        "the new stream's picture survives the old stream's discard"
    );
}

#[test]
fn pr7_a_region_whose_header_cannot_be_committed_carries_no_pictures() {
    let region = ReservedRegion::new(|_| true);
    let why = PictureSlots::open(region).err().expect("refused");
    assert!(why.contains("header could not be committed"), "{why}");
}

#[test]
fn pr7_the_region_header_names_the_region_both_sides_map() {
    let mut bytes = vec![0u8; 64];
    assert_eq!(read_header(&bytes), None);
    assert!(write_header(&mut bytes, 0x1122_3344_5566_7788, 1 << 26));
    assert_eq!(&bytes[..8], &MAGIC);
    assert_eq!(
        &bytes[8..16],
        &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
        "little endian"
    );
    assert_eq!(read_header(&bytes), Some((0x1122_3344_5566_7788, 1 << 26)));
    // A region with room for a picture carries them; one with room only for its header does not.
    let whole: Arc<dyn SharedRegion> = Arc::new(MemoryRegion::new(REGION_BYTES, REGION_ID));
    assert_eq!(PictureSlots::open(whole).map(|s| s.id()), Ok(REGION_ID));
    let tiny: Arc<dyn SharedRegion> = Arc::new(MemoryRegion::new(SLOTS_START, REGION_ID));
    assert!(PictureSlots::open(tiny).is_err());
}
