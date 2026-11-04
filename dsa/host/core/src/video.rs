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
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    Envelope, Role, VideoCapabilities, VideoCodec, VideoCodecSupport, VideoDecoderOpened,
    VideoError, VideoFlushed, VideoFrame,
};
use dsa_protocol::handshake::{host_accept_open, local_hello};
use dsa_protocol::{decode, encode};
use dsa_transport::{Connection, Listener, TransportError};

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

/// One decoder session on the host's hardware.
pub trait VideoDecoder: Send {
    /// Takes one packet and returns the pictures that are ready now, in presentation order.
    fn decode(&mut self, packet: Packet) -> Result<Vec<Picture>, String>;
    /// The stream has ended: returns every picture still held.
    fn drain(&mut self) -> Result<Vec<Picture>, String>;
    /// Drops every packet and picture held, as for a seek.
    fn flush(&mut self);
}

/// The host's video decoders.
pub trait VideoDecoders: Send + Sync {
    /// The formats the host's hardware decodes. Asked once for each connection.
    fn capabilities(&self) -> Vec<CodecSupport>;
    /// Opens a decoder for `codec`, for a stream of about `width` by `height` (0 when not
    /// known).
    fn open(&self, codec: Codec, width: u32, height: u32) -> Result<Box<dyn VideoDecoder>, String>;
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
    pending: &mut Pending,
    pictures: Vec<Picture>,
) -> Result<(), TransportError> {
    for picture in pictures {
        let Some((frame_index, timestamp_us)) = pending.take(picture.id) else {
            continue;
        };
        let expected = Picture::nv12_len(picture.width, picture.height, picture.stride);
        if expected != Some(picture.nv12.len()) {
            continue;
        }
        send(
            connection,
            Body::VideoFrame(frame_of(picture, frame_index, timestamp_us)),
        )?;
    }
    Ok(())
}

/// Runs one connection on the video port to its end. `build` names this host in its Hello.
pub fn serve_connection(
    mut connection: Box<dyn Connection>,
    decoders: &dyn VideoDecoders,
    build: &str,
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

    let mut pending = Pending::default();
    loop {
        let frame = match connection.recv(None) {
            Ok(frame) => frame,
            Err(TransportError::Closed) => return VideoEnd::Closed,
            Err(error) => return VideoEnd::Failed(format!("the connection failed: {error}")),
        };
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
                    let decoded = decoder.decode(Packet {
                        id,
                        data: packet.data,
                        codec_config: packet.codec_config,
                    });
                    result = match decoded {
                        Ok(pictures) => send_pictures(connection.as_mut(), &mut pending, pictures),
                        Err(why) => send(
                            connection.as_mut(),
                            Body::VideoError(VideoError { error: why }),
                        ),
                    };
                }
                if result.is_ok() && packet.end_of_stream {
                    result = match decoder.drain() {
                        Ok(pictures) => send_pictures(connection.as_mut(), &mut pending, pictures),
                        Err(why) => send(
                            connection.as_mut(),
                            Body::VideoError(VideoError { error: why }),
                        ),
                    };
                }
                if result.is_ok() && packet.end_of_stream {
                    pending.clear();
                    let end = VideoFrame {
                        frame_index: packet.frame_index,
                        timestamp_us: packet.timestamp_us,
                        end_of_stream: true,
                        ..VideoFrame::default()
                    };
                    result = send(connection.as_mut(), Body::VideoFrame(end));
                }
                result
            }
            Some(Body::VideoFlush(_)) => {
                decoder.flush();
                pending.clear();
                send(connection.as_mut(), Body::VideoFlushed(VideoFlushed {}))
            }
            _ => {
                let why = "the guest sent a message the video port does not take";
                return VideoEnd::Failed(why.into());
            }
        };
        if let Err(error) = sent {
            return VideoEnd::Failed(format!("a message to the guest did not go out: {error}"));
        }
    }
}

/// Accepts connections on `listener` until `stop` is set, and runs each on a thread of its
/// own, at most [`MAX_DECODERS`] at once. `log` takes a line for each connection that ends on
/// an error. A listener that fails (the guest's VM went away) is tried again a second later.
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
                let end = serve_connection(connection, decoders.as_ref(), &build);
                if let VideoEnd::Failed(why) = end {
                    log(&format!("video: a decoder ended: {why}"));
                }
                counter.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            running.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
