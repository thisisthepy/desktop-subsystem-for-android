//! The guest's video, decoded on the host's hardware decoder.
//!
//! The guest's codec HAL opens one connection to the video port for each decoder an app
//! creates (`MediaCodec`). It sends the stream's packets in decode order; the host decodes
//! them on its own hardware and sends each picture back, in presentation order, as 8-bit
//! 4:2:0 (NV12). The HAL copies each picture into the buffer the app (or its surface) gave
//! it, so decoded frames reach the app the way a software decoder's do, while the decoding
//! itself costs the guest nothing.
//!
//! [`serve_connection`] runs one connection: the handshake (a Hello of
//! [`Role::GuestVideo`], which needs no secret), the host's [`VideoCapabilities`], the open,
//! and then packets in and pictures out. Any guest process can reach the port, so every
//! message is checked and nothing the peer sends can take the host down: a bad stream is an
//! error on that connection only.
//!
//! Nothing here names a host operating system API. The platform supplies the decoder through
//! [`VideoDecoders`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    Envelope, Role, VideoCapabilities, VideoCodec, VideoCodecSupport, VideoDecoderOpened,
    VideoError, VideoFlushed, VideoFrame,
};
use dsa_protocol::handshake::{host_accept_open, local_hello};
use dsa_protocol::{decode, encode};
use dsa_transport::{Connection, FrameSender, Listener, TransportError};

/// The vsock port the guest's codec HAL connects to.
pub const VIDEO_PORT: u32 = 5569;

/// How long the host waits for the guest's Hello and its VideoDecoderOpen.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest packet the host takes: 8 MiB, more than any access unit of a 4K stream.
pub const MAX_PACKET_BYTES: usize = 8 * 1024 * 1024;

/// The most decoders that run at once for one guest. A hardware decoder has a few sessions;
/// a guest that asks for more gets an answer saying so, and its app falls back to another
/// decoder.
pub const MAX_DECODERS: usize = 8;

/// The most packets the host keeps the guest's frame index for, waiting for their pictures.
/// A decoder holds at most a few dozen; a stream whose pictures never come cannot make the
/// host keep more.
pub const MAX_PENDING: usize = 256;

/// A coding format the host decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Vp9,
    Av1,
}

impl Codec {
    /// The format a control message names, if it is one.
    pub fn from_control(codec: i32) -> Option<Codec> {
        match VideoCodec::try_from(codec) {
            Ok(VideoCodec::H264) => Some(Codec::H264),
            Ok(VideoCodec::Hevc) => Some(Codec::Hevc),
            Ok(VideoCodec::Vp9) => Some(Codec::Vp9),
            Ok(VideoCodec::Av1) => Some(Codec::Av1),
            _ => None,
        }
    }

    pub fn to_control(self) -> VideoCodec {
        match self {
            Codec::H264 => VideoCodec::H264,
            Codec::Hevc => VideoCodec::Hevc,
            Codec::Vp9 => VideoCodec::Vp9,
            Codec::Av1 => VideoCodec::Av1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
        }
    }
}

/// One format the host's hardware decodes, up to a picture size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecSupport {
    pub codec: Codec,
    pub max_width: u32,
    pub max_height: u32,
}

/// One packet for the platform's decoder. `id` is the host's own, in the order the packets
/// arrived, and comes back on the picture the packet becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub id: u64,
    pub data: Vec<u8>,
    /// Codec configuration (parameter sets) rather than a picture.
    pub codec_config: bool,
    /// When the guest presents the picture, in microseconds, as it gave it. A decoder that
    /// puts its pictures in presentation order by time (VideoToolbox) is given it.
    pub timestamp_us: i64,
}

/// One decoded picture, NV12: `stride * height` bytes of luma, then `stride * ceil(height/2)`
/// of interleaved chroma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// The id of the packet it came from.
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub nv12: Vec<u8>,
}

impl Picture {
    /// The bytes an NV12 picture of this size and stride takes.
    pub fn nv12_len(width: u32, height: u32, stride: u32) -> Option<usize> {
        if stride < width || width == 0 || height == 0 {
            return None;
        }
        let rows = height as usize + (height as usize).div_ceil(2);
        (stride as usize).checked_mul(rows)
    }
}

/// What a decoder did on its own since it was last asked, for the stream's log line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DecoderReport {
    /// Time spent copying pictures out of the decoder's surfaces into memory.
    pub readback: Duration,
    /// How many pictures that was.
    pub readbacks: u32,
    /// How the last picture's surface was laid out (its size, row pitch and where its chroma
    /// was read from), when the decoder can say.
    pub surface: Option<String>,
}

/// One decoder session on the host's hardware.
pub trait VideoDecoder: Send {
    /// Takes one packet and returns the pictures that are ready now, in presentation order.
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String>;
    /// The stream has ended: returns every picture still held.
    fn drain(&mut self) -> Result<Vec<Picture>, String>;
    /// Drops every packet and picture held, as for a seek.
    fn flush(&mut self);
    /// What the decoder did since the last call, and resets it. A decoder with nothing of its
    /// own to say reports nothing.
    fn take_report(&mut self) -> DecoderReport {
        DecoderReport::default()
    }
}

/// The host's video decoders.
pub trait VideoDecoders: Send + Sync {
    /// The formats the host's hardware decodes. Asked once for each connection.
    fn capabilities(&self) -> Vec<CodecSupport>;
    /// Opens a decoder for `codec`, for a stream of about `width` by `height` (0 when not
    /// known).
    fn open(&self, codec: Codec, width: u32, height: u32) -> Result<Box<dyn VideoDecoder>, String>;
    /// Every connection has ended and every decoder is closed. A platform whose decoders
    /// share a device or a media library lets it go here, since nothing uses it until the
    /// guest opens a decoder again: a host that kept them would hold the memory a video
    /// played with for as long as it runs. Called by [`serve`] each time the last running
    /// connection ends; a decoder may open again at any moment after.
    fn all_closed(&self) {}
}

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoEnd {
    /// The guest closed it.
    Closed,
    /// It ended on an error; the detail says what.
    Failed(String),
}

fn send(connection: &mut dyn Connection, body: Body) -> Result<(), TransportError> {
    connection.send(&encode(&Envelope { body: Some(body) }))
}

fn frame_of(picture: Picture, frame_index: u64, timestamp_us: i64) -> VideoFrame {
    VideoFrame {
        frame_index,
        timestamp_us,
        width: picture.width,
        height: picture.height,
        stride: picture.stride,
        nv12: picture.nv12,
        end_of_stream: false,
    }
}

/// The guest's frame index and timestamp of each packet whose picture has not come yet.
#[derive(Default)]
struct Pending {
    next_id: u64,
    packets: HashMap<u64, (u64, i64)>,
}

impl Pending {
    fn add(&mut self, frame_index: u64, timestamp_us: i64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        if self.packets.len() >= MAX_PENDING {
            // The oldest has had its chance; a decoder never holds this many.
            if let Some(oldest) = self.packets.keys().min().copied() {
                self.packets.remove(&oldest);
            }
        }
        self.packets.insert(id, (frame_index, timestamp_us));
        id
    }

    fn take(&mut self, id: u64) -> Option<(u64, i64)> {
        self.packets.remove(&id)
    }

    /// Forgets every packet; ids go on from where they were, so a late picture of an old
    /// packet is never taken for a new one.
    fn clear(&mut self) {
        self.packets.clear();
    }
}

/// Sends `pictures` to the guest, each with its packet's frame index and timestamp. A
/// picture whose packet is unknown (it was flushed) is dropped.
fn send_pictures(
    connection: &mut dyn Connection,
    outgoing: &mut Outgoing,
    pending: &mut Pending,
    pictures: Vec<Picture>,
) -> Result<(), TransportError> {
    for picture in pictures {
        let Some((frame_index, timestamp_us)) = pending.take(picture.id) else {
            outgoing.stats().dropped(Instant::now(), true);
            continue;
        };
        let expected = Picture::nv12_len(picture.width, picture.height, picture.stride);
        if expected != Some(picture.nv12.len()) {
            outgoing.stats().dropped(Instant::now(), false);
            continue;
        }
        let size = (picture.width, picture.height);
        let waited = outgoing.send(
            connection,
            Body::VideoFrame(frame_of(picture, frame_index, timestamp_us)),
        )?;
        outgoing.stats().handed_on(Instant::now(), size, waited);
    }
    Ok(())
}

/// How often a stream's numbers go to the log while it plays.
pub const STATS_INTERVAL: Duration = Duration::from_secs(5);

/// The pictures that may wait, encoded, for the guest to take them while the decoder goes on
/// with the next packets. The guest takes a picture only once its app's surface has a buffer
/// for it; with nothing waiting here, the decoder stood still for as long, and a stream whose
/// buffers came back unevenly played at the pace of its slowest picture. Each waiting picture
/// is one decoded frame in memory (345 KiB at 640x360, 13.5 MiB at 4096x2304), so they are few.
pub const PICTURES_IN_FLIGHT: usize = 3;

/// One stream's numbers over a few seconds: how many packets came in and pictures went out,
/// and where the time went. Written to the log every [`STATS_INTERVAL`], so a stream that
/// plays slowly says which stage holds it up.
#[derive(Debug, Default, Clone)]
pub struct StreamStats {
    started: Option<Instant>,
    packets: u32,
    decoded: u32,
    handed_on: u32,
    sent: u32,
    flushed: u32,
    malformed: u32,
    decode: Duration,
    readback: Duration,
    readbacks: u32,
    waited: Duration,
    send: Duration,
    size: Option<(u32, u32)>,
    surface: Option<String>,
}

impl StreamStats {
    fn begin(&mut self, now: Instant) {
        if self.started.is_none() {
            self.started = Some(now);
        }
    }

    /// A packet was decoded in `took` (the decoder's copies out of its surfaces included),
    /// giving `pictures` pictures.
    pub fn decoded(&mut self, now: Instant, took: Duration, pictures: u32) {
        self.begin(now);
        self.packets += 1;
        self.decoded += pictures;
        self.decode += took;
    }

    /// What the decoder said of its own stages.
    pub fn decoder(&mut self, report: DecoderReport) {
        self.readback += report.readback;
        self.readbacks += report.readbacks;
        if report.surface.is_some() {
            self.surface = report.surface;
        }
    }

    /// A picture of `size` was handed on to go to the guest, after waiting `waited` for room
    /// among the pictures in flight (the guest not taking the ones before).
    pub fn handed_on(&mut self, now: Instant, size: (u32, u32), waited: Duration) {
        self.begin(now);
        self.handed_on += 1;
        self.waited += waited;
        self.size = Some(size);
    }

    /// A message reached the guest's socket in `took`.
    pub fn sent(&mut self, took: Duration) {
        self.sent += 1;
        self.send += took;
    }

    /// A picture was dropped: its packet was flushed, or it is not the size it says.
    pub fn dropped(&mut self, now: Instant, flushed: bool) {
        self.begin(now);
        if flushed {
            self.flushed += 1;
        } else {
            self.malformed += 1;
        }
    }

    /// The log line for the numbers so far, once [`STATS_INTERVAL`] has passed since they
    /// began, and a new count from then. `None` before then, and while nothing has happened.
    pub fn report(&mut self, now: Instant, codec: Codec) -> Option<String> {
        let started = self.started?;
        let span = now.checked_duration_since(started)?;
        if span < STATS_INTERVAL {
            return None;
        }
        let seconds = span.as_secs_f64();
        let average = |total: Duration, count: u32| {
            if count == 0 {
                0.0
            } else {
                total.as_secs_f64() * 1000.0 / f64::from(count)
            }
        };
        let size = self
            .size
            .map(|(w, h)| format!(" {w}x{h}"))
            .unwrap_or_default();
        let surface = self
            .surface
            .as_ref()
            .map(|s| format!("; surface {s}"))
            .unwrap_or_default();
        let line = format!(
            "video: {}{size} over {seconds:.1} s: {} packets, {} pictures decoded ({:.1}/s), {} \
             handed on ({:.1}/s), {} dropped ({} flushed, {} malformed); average ms: decode \
             {:.2} (copy out of the GPU {:.2}), waiting for the guest to take a picture {:.2}, \
             send {:.2}{surface}",
            codec.name(),
            self.packets,
            self.decoded,
            f64::from(self.decoded) / seconds,
            self.handed_on,
            f64::from(self.handed_on) / seconds,
            self.flushed + self.malformed,
            self.flushed,
            self.malformed,
            average(self.decode, self.packets),
            average(self.readback, self.readbacks),
            average(self.waited, self.handed_on),
            average(self.send, self.sent),
        );
        *self = StreamStats {
            started: Some(now),
            size: self.size,
            surface: self.surface.take(),
            ..StreamStats::default()
        };
        Some(line)
    }
}

/// Where the messages to the guest go: straight to the connection, or, when the connection
/// can send from a second handle, to a thread of their own through [`PICTURES_IN_FLIGHT`]
/// places, so the decoder goes on while the guest takes the pictures before. Every message
/// goes the same way, so they reach the guest in the order they were made.
struct Outgoing {
    queue: Option<SyncSender<Vec<u8>>>,
    thread: Option<thread::JoinHandle<()>>,
    failed: Arc<Mutex<Option<TransportError>>>,
    stats: Arc<Mutex<StreamStats>>,
}

impl Outgoing {
    fn start(connection: &dyn Connection, stats: Arc<Mutex<StreamStats>>) -> Outgoing {
        let failed = Arc::new(Mutex::new(None));
        let mut outgoing = Outgoing {
            queue: None,
            thread: None,
            failed: Arc::clone(&failed),
            stats: Arc::clone(&stats),
        };
        let Some(sender) = connection.try_clone_sender() else {
            return outgoing;
        };
        let (queue, waiting) = sync_channel::<Vec<u8>>(PICTURES_IN_FLIGHT);
        let spawned = thread::Builder::new()
            .name("dsa-video-send".into())
            .spawn(move || Outgoing::run(sender, waiting, failed, stats));
        if let Ok(thread) = spawned {
            outgoing.queue = Some(queue);
            outgoing.thread = Some(thread);
        }
        outgoing
    }

    fn run(
        mut sender: Box<dyn FrameSender>,
        waiting: Receiver<Vec<u8>>,
        failed: Arc<Mutex<Option<TransportError>>>,
        stats: Arc<Mutex<StreamStats>>,
    ) {
        for frame in waiting {
            let started = Instant::now();
            if let Err(error) = sender.send(&frame) {
                *failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
                return;
            }
            let took = started.elapsed();
            stats.lock().unwrap_or_else(|e| e.into_inner()).sent(took);
        }
    }

    fn stats(&self) -> std::sync::MutexGuard<'_, StreamStats> {
        self.stats.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The error the sending thread stopped on, if it has.
    fn failure(&self) -> Option<TransportError> {
        self.failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Sends `body`, after the messages before it; a message waits for room among those in
    /// flight. Returns how long it waited for that room.
    fn send(
        &mut self,
        connection: &mut dyn Connection,
        body: Body,
    ) -> Result<Duration, TransportError> {
        let frame = encode(&Envelope { body: Some(body) });
        let Some(queue) = &self.queue else {
            let started = Instant::now();
            connection.send(&frame)?;
            let took = started.elapsed();
            self.stats().sent(took);
            return Ok(Duration::ZERO);
        };
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let started = Instant::now();
        if queue.send(frame).is_err() {
            return Err(self.failure().unwrap_or(TransportError::Closed));
        }
        Ok(started.elapsed())
    }

    /// Ends the sending thread. The stream has ended (the guest closed it, or it failed), so
    /// what still waits has no one to go to: the connection is closed first, which also ends
    /// a send blocked on a guest that stopped reading.
    fn finish(&mut self, connection: &mut dyn Connection) {
        self.queue = None;
        if let Some(thread) = self.thread.take() {
            connection.close();
            let _ = thread.join();
        }
    }
}

/// Runs one connection on the video port to its end. `build` names this host in its Hello.
pub fn serve_connection(
    connection: Box<dyn Connection>,
    decoders: &dyn VideoDecoders,
    build: &str,
) -> VideoEnd {
    serve_connection_logged(connection, decoders, build, &|_: &str| {})
}

/// As [`serve_connection`], with `log` taking the stream's numbers every [`STATS_INTERVAL`]
/// while it plays (see [`StreamStats`]).
pub fn serve_connection_logged(
    mut connection: Box<dyn Connection>,
    decoders: &dyn VideoDecoders,
    build: &str,
    log: &dyn Fn(&str),
) -> VideoEnd {
    let local = local_hello(Role::Host, build);
    let accepted = host_accept_open(connection.as_mut(), &local, Role::GuestVideo, OPEN_TIMEOUT);
    if let Err(error) = accepted {
        return VideoEnd::Failed(format!("the handshake failed: {error}"));
    }
    let capabilities = decoders.capabilities();
    let offered = VideoCapabilities {
        codecs: capabilities
            .iter()
            .map(|support| VideoCodecSupport {
                codec: support.codec.to_control() as i32,
                max_width: support.max_width,
                max_height: support.max_height,
            })
            .collect(),
    };
    if let Err(error) = send(connection.as_mut(), Body::VideoCapabilities(offered)) {
        return VideoEnd::Failed(format!("the capabilities did not go out: {error}"));
    }

    let open = match connection.recv(Some(OPEN_TIMEOUT)) {
        Ok(frame) => match decode(&frame).ok().and_then(|e| e.body) {
            Some(Body::VideoDecoderOpen(open)) => open,
            // The HAL asks for the capabilities only, then closes.
            _ => return VideoEnd::Failed("the guest did not ask for a decoder".into()),
        },
        Err(TransportError::Closed) => return VideoEnd::Closed,
        Err(error) => return VideoEnd::Failed(format!("no decoder was asked for: {error}")),
    };
    let refuse = |connection: &mut dyn Connection, why: String| {
        let _ = send(
            connection,
            Body::VideoDecoderOpened(VideoDecoderOpened {
                ok: false,
                error: why.clone(),
            }),
        );
        connection.close();
        VideoEnd::Failed(why)
    };
    let Some(codec) = Codec::from_control(open.codec) else {
        let why = format!("unknown video format {}", open.codec);
        return refuse(connection.as_mut(), why);
    };
    let Some(support) = capabilities.iter().find(|s| s.codec == codec) else {
        return refuse(
            connection.as_mut(),
            format!("this computer has no hardware decoder for {}", codec.name()),
        );
    };
    if open.width > support.max_width || open.height > support.max_height {
        return refuse(
            connection.as_mut(),
            format!(
                "{} video of {}x{} is larger than this computer's decoder takes ({}x{})",
                codec.name(),
                open.width,
                open.height,
                support.max_width,
                support.max_height
            ),
        );
    }
    let mut decoder = match decoders.open(codec, open.width, open.height) {
        Ok(decoder) => decoder,
        Err(why) => return refuse(connection.as_mut(), why),
    };
    if let Err(error) = send(
        connection.as_mut(),
        Body::VideoDecoderOpened(VideoDecoderOpened {
            ok: true,
            error: String::new(),
        }),
    ) {
        return VideoEnd::Failed(format!("the answer did not go out: {error}"));
    }

    let stats = Arc::new(Mutex::new(StreamStats::default()));
    let mut outgoing = Outgoing::start(connection.as_ref(), Arc::clone(&stats));
    let end = run_stream(
        connection.as_mut(),
        decoder.as_mut(),
        codec,
        &mut outgoing,
        log,
    );
    outgoing.finish(connection.as_mut());
    end
}

/// Packets in and pictures out, until the guest closes the connection or it fails.
fn run_stream(
    connection: &mut dyn Connection,
    decoder: &mut dyn VideoDecoder,
    codec: Codec,
    outgoing: &mut Outgoing,
    log: &dyn Fn(&str),
) -> VideoEnd {
    let mut pending = Pending::default();
    loop {
        let frame = match connection.recv(None) {
            Ok(frame) => frame,
            Err(TransportError::Closed) => return VideoEnd::Closed,
            Err(error) => return VideoEnd::Failed(format!("the connection failed: {error}")),
        };
        if let Some(error) = outgoing.failure() {
            return VideoEnd::Failed(format!("a message to the guest did not go out: {error}"));
        }
        let sent = match decode(&frame).ok().and_then(|e| e.body) {
            Some(Body::VideoPacket(packet)) => {
                if packet.data.len() > MAX_PACKET_BYTES {
                    return VideoEnd::Failed(format!(
                        "a packet of {} bytes is larger than {MAX_PACKET_BYTES}",
                        packet.data.len()
                    ));
                }
                let mut result = Ok(());
                if !packet.data.is_empty() {
                    let id = pending.add(packet.frame_index, packet.timestamp_us);
                    let started = Instant::now();
                    let decoded = decoder.decode(Packet {
                        id,
                        data: packet.data,
                        codec_config: packet.codec_config,
                        timestamp_us: packet.timestamp_us,
                    });
                    let took = started.elapsed();
                    {
                        let mut stats = outgoing.stats();
                        let count = decoded.as_ref().map_or(0, |p| p.len() as u32);
                        stats.decoded(Instant::now(), took, count);
                        stats.decoder(decoder.take_report());
                    }
                    result = match decoded {
                        Ok(pictures) => send_pictures(connection, outgoing, &mut pending, pictures),
                        Err(why) => outgoing
                            .send(connection, Body::VideoError(VideoError { error: why }))
                            .map(drop),
                    };
                }
                if result.is_ok() && packet.end_of_stream {
                    result = match decoder.drain() {
                        Ok(pictures) => send_pictures(connection, outgoing, &mut pending, pictures),
                        Err(why) => outgoing
                            .send(connection, Body::VideoError(VideoError { error: why }))
                            .map(drop),
                    };
                    outgoing.stats().decoder(decoder.take_report());
                }
                if result.is_ok() && packet.end_of_stream {
                    pending.clear();
                    let end = VideoFrame {
                        frame_index: packet.frame_index,
                        timestamp_us: packet.timestamp_us,
                        end_of_stream: true,
                        ..VideoFrame::default()
                    };
                    result = outgoing.send(connection, Body::VideoFrame(end)).map(drop);
                }
                result
            }
            Some(Body::VideoFlush(_)) => {
                decoder.flush();
                pending.clear();
                outgoing
                    .send(connection, Body::VideoFlushed(VideoFlushed {}))
                    .map(drop)
            }
            _ => {
                let why = "the guest sent a message the video port does not take";
                return VideoEnd::Failed(why.into());
            }
        };
        if let Err(error) = sent {
            return VideoEnd::Failed(format!("a message to the guest did not go out: {error}"));
        }
        if let Some(line) = outgoing.stats().report(Instant::now(), codec) {
            log(&line);
        }
    }
}

/// Accepts connections on `listener` until `stop` is set, and runs each on a thread of its
/// own, at most [`MAX_DECODERS`] at once. `log` takes a line for each connection that ends on
/// an error. A listener that fails (the guest's VM went away) is tried again a second later.
/// Each time the last running connection ends, its decoder already closed, `decoders` hears
/// it ([`VideoDecoders::all_closed`]).
pub fn serve(
    mut listener: Box<dyn Listener>,
    decoders: Arc<dyn VideoDecoders>,
    build: String,
    stop: Arc<AtomicBool>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let running = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        let mut connection = match listener.accept(Duration::from_millis(250)) {
            Ok(connection) => connection,
            Err(TransportError::Timeout) => continue,
            Err(error) => {
                log(&format!(
                    "video: the video port took no connection: {error}"
                ));
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        if running.load(Ordering::SeqCst) >= MAX_DECODERS {
            log(&format!(
                "video: refused a decoder; {MAX_DECODERS} are already running"
            ));
            connection.close();
            continue;
        }
        running.fetch_add(1, Ordering::SeqCst);
        let (decoders, build, counter, log) = (
            Arc::clone(&decoders),
            build.clone(),
            Arc::clone(&running),
            Arc::clone(&log),
        );
        let spawned = thread::Builder::new()
            .name("dsa-video".into())
            .spawn(move || {
                let end = serve_connection_logged(
                    connection,
                    decoders.as_ref(),
                    &build,
                    &|line: &str| log(line),
                );
                if let VideoEnd::Failed(why) = end {
                    log(&format!("video: a decoder ended: {why}"));
                }
                // The connection's decoder was dropped when serve_connection returned.
                if counter.fetch_sub(1, Ordering::SeqCst) == 1 {
                    decoders.all_closed();
                }
            });
        if spawned.is_err() {
            running.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
