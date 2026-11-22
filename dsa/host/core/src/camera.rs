//! The host computer's cameras, used by the apps in the guest.
//!
//! The guest's camera provider HAL shows each camera of the host to Android as a camera of
//! its own. It opens one connection to the camera port to list the cameras, and one for each
//! camera an app opens. On that connection the host starts the camera at the size the app's
//! streams need and sends each picture as 8-bit 4:2:0 (NV12); the HAL fills the app's buffers
//! from it. A still picture (JPEG) is encoded on the host from the newest picture.
//!
//! [`serve_connection`] runs one connection: the handshake (a Hello of
//! [`Role::GuestCamera`], which needs no secret), the host's [`CameraList`], and then opens,
//! pictures and stills. Any guest process can reach the port, so every message is checked and
//! nothing the peer sends can take the host down: a bad request is an error on that
//! connection only. A camera the user has blocked in the computer's privacy settings is not
//! listed and does not open.
//!
//! The camera is held open only between the guest's `CameraOpen` and its `CameraStop` (or
//! the end of the connection), so the computer's camera light and its in-use indicator show
//! only while an app uses the camera.
//!
//! Nothing here names a host operating system API. The platform supplies the cameras through
//! [`Cameras`].

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    CameraDevice, CameraError, CameraFacing, CameraFrame, CameraList, CameraMode, CameraOpened,
    CameraStillTaken, Envelope, Role,
};
use dsa_protocol::handshake::{host_accept_open, local_hello};
use dsa_protocol::{decode, encode};
use dsa_transport::{Connection, Listener, TransportError};

/// The vsock port the guest's camera provider HAL connects to.
pub const CAMERA_PORT: u32 = 5572;

/// How long the host waits for the guest's Hello.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// The most connections that run at once for one guest: one to list, and one for each camera
/// open, with room for a HAL that restarts.
pub const MAX_CONNECTIONS: usize = 8;

/// The largest picture side the host offers or makes. Larger modes of a camera are left out.
pub const MAX_SIDE: u32 = 4096;

/// The most pixels a mode offered to the guest has: 4096 by 2304, whose NV12 picture fits in
/// one frame of the transport.
pub const MAX_PIXELS: u64 = 4096 * 2304;

/// How long the host waits for the guest's next message while a camera runs, before it
/// takes the camera's next picture.
const POLL: Duration = Duration::from_millis(1);

/// Where a camera faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    Front,
    Back,
    External,
}

impl Facing {
    pub fn to_control(self) -> CameraFacing {
        match self {
            Facing::Front => CameraFacing::Front,
            Facing::Back => CameraFacing::Back,
            Facing::External => CameraFacing::External,
        }
    }
}

/// Where the computer's enclosure places a camera, as the platform reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Front,
    Back,
    Top,
    Bottom,
    Left,
    Right,
    Unknown,
}

/// Where a camera on `panel` faces: front and back as placed, and every other camera (a USB
/// camera, one on an edge, or one the enclosure does not place) external.
pub fn facing_of(panel: Option<Panel>) -> Facing {
    match panel {
        Some(Panel::Front) => Facing::Front,
        Some(Panel::Back) => Facing::Back,
        _ => Facing::External,
    }
}

/// The sensor orientation Android takes (0, 90, 180 or 270) for a camera mounted turned
/// `degrees` clockwise in its enclosure, rounded to the nearest quarter turn.
pub fn sensor_orientation(degrees: i64) -> u32 {
    let quarter = ((degrees.rem_euclid(360) + 45) / 90) % 4;
    (quarter * 90) as u32
}

/// One picture size a camera delivers, and the rates it delivers it at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub min_fps: u32,
    pub max_fps: u32,
}

/// One camera of the host, as the platform finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camera {
    /// The platform's name for it, unchanged while the camera stays plugged in.
    pub id: String,
    pub name: String,
    pub facing: Facing,
    /// Android's sensor orientation; see [`sensor_orientation`].
    pub sensor_orientation: u32,
    /// Every mode it delivers, as the platform lists them.
    pub modes: Vec<Mode>,
    /// The user has blocked the camera in the computer's privacy settings, or nothing may
    /// open it now. Such a camera is not listed to the guest.
    pub blocked: bool,
}

/// The cameras as one line each, for a platform that lists them in a child process so the
/// camera stack never loads into the long-running one: the id, the name, the facing, the
/// sensor orientation, whether it is blocked, and the modes (`WxH@MIN-MAX`, comma
/// separated), split by tabs. Tabs and line breaks in an id or name become spaces.
pub fn list_text(cameras: &[Camera]) -> String {
    let clean = |text: &str| text.replace(['\t', '\r', '\n'], " ");
    let mut out = String::new();
    for camera in cameras {
        let facing = match camera.facing {
            Facing::Front => "front",
            Facing::Back => "back",
            Facing::External => "external",
        };
        let modes: Vec<String> = camera
            .modes
            .iter()
            .map(|m| format!("{}x{}@{}-{}", m.width, m.height, m.min_fps, m.max_fps))
            .collect();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            clean(&camera.id),
            clean(&camera.name),
            facing,
            camera.sensor_orientation,
            u8::from(camera.blocked),
            modes.join(",")
        ));
    }
    out
}

/// The cameras [`list_text`] wrote. A line that does not read as a camera is left out, and so
/// is a mode that does not read as one.
pub fn parse_list_text(text: &str) -> Vec<Camera> {
    let mode = |text: &str| -> Option<Mode> {
        let (size, rates) = text.split_once('@')?;
        let (width, height) = size.split_once('x')?;
        let (min_fps, max_fps) = rates.split_once('-')?;
        Some(Mode {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            min_fps: min_fps.parse().ok()?,
            max_fps: max_fps.parse().ok()?,
        })
    };
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [id, name, facing, orientation, blocked, modes] = fields[..] else {
                return None;
            };
            if id.is_empty() {
                return None;
            }
            let facing = match facing {
                "front" => Facing::Front,
                "back" => Facing::Back,
                "external" => Facing::External,
                _ => return None,
            };
            Some(Camera {
                id: id.to_string(),
                name: name.to_string(),
                facing,
                sensor_orientation: orientation.parse().ok()?,
                modes: modes.split(',').filter_map(mode).collect(),
                blocked: blocked == "1",
            })
        })
        .collect()
}

/// One picture: NV12, `stride * height` bytes of luma, then `stride * ceil(height / 2)` of
/// interleaved chroma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub nv12: Vec<u8>,
    /// When the camera took it, in nanoseconds on a clock that only goes forward while the
    /// camera runs.
    pub timestamp_ns: u64,
}

impl Frame {
    /// The bytes an NV12 picture of this size and stride takes.
    pub fn nv12_len(width: u32, height: u32, stride: u32) -> Option<usize> {
        if width == 0 || height == 0 || stride < width + (width & 1) {
            return None;
        }
        let rows = height as usize + (height as usize).div_ceil(2);
        (stride as usize).checked_mul(rows)
    }

    fn is_whole(&self) -> bool {
        Frame::nv12_len(self.width, self.height, self.stride) == Some(self.nv12.len())
    }
}

/// A running camera. Dropping it stops the camera and lets it go.
pub trait CameraStream: Send {
    /// Waits for the camera's next picture.
    fn next(&mut self) -> Result<Frame, String>;
}

/// The host's cameras.
pub trait Cameras: Send + Sync {
    /// The cameras now present, blocked ones included and marked. Called for each connection.
    fn list(&self) -> Vec<Camera>;
    /// Starts camera `id` in `mode` at `fps` frames a second.
    fn open(&self, id: &str, mode: Mode, fps: u32) -> Result<Box<dyn CameraStream>, String>;
    /// Encodes `frame` as JPEG at `quality` (1 to 100).
    fn encode_jpeg(&self, frame: &Frame, quality: u8) -> Result<Vec<u8>, String>;
}

/// The modes offered to the guest: each size once, with the widest range of rates any of its
/// modes has, largest first. A size that is odd, zero, or larger than [`MAX_SIDE`] is left
/// out, as is one of more than [`MAX_PIXELS`] and a mode with no rate.
pub fn offered_modes(modes: &[Mode]) -> Vec<Mode> {
    let mut out: Vec<Mode> = Vec::new();
    for mode in modes {
        let usable = mode.width >= 2
            && mode.height >= 2
            && mode.width % 2 == 0
            && mode.height % 2 == 0
            && mode.width <= MAX_SIDE
            && mode.height <= MAX_SIDE
            && u64::from(mode.width) * u64::from(mode.height) <= MAX_PIXELS
            && mode.max_fps > 0;
        if !usable {
            continue;
        }
        let min_fps = mode.min_fps.clamp(1, mode.max_fps);
        match out
            .iter_mut()
            .find(|m| m.width == mode.width && m.height == mode.height)
        {
            Some(known) => {
                known.min_fps = known.min_fps.min(min_fps);
                known.max_fps = known.max_fps.max(mode.max_fps);
            }
            None => out.push(Mode { min_fps, ..*mode }),
        }
    }
    out.sort_by(|a, b| {
        let area = |m: &Mode| u64::from(m.width) * u64::from(m.height);
        area(b).cmp(&area(a)).then(b.width.cmp(&a.width))
    });
    out
}

/// The mode and rate to run for a guest asking for `width` by `height` at `fps`: that size
/// when the camera has it, else the smallest size that covers it, else the largest there is;
/// and the rate nearest `fps` the mode delivers (its fastest when `fps` is 0).
pub fn choose_mode(modes: &[Mode], width: u32, height: u32, fps: u32) -> Option<(Mode, u32)> {
    let offered = offered_modes(modes);
    let exact = offered
        .iter()
        .find(|m| m.width == width && m.height == height);
    let covering = offered
        .iter()
        .rev()
        .find(|m| m.width >= width && m.height >= height);
    let mode = *exact.or(covering).or(offered.first())?;
    let rate = if fps == 0 {
        mode.max_fps
    } else {
        fps.clamp(mode.min_fps, mode.max_fps)
    };
    Some((mode, rate))
}

/// A region of a camera's picture: its left and top edges, width and height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The region of a `picture_width` by `picture_height` picture that a `width` by `height`
/// picture is made from: the largest one of that shape, at the centre, at even offsets so it
/// starts on a chroma sample. A picture of another shape is cropped, never stretched, so a
/// 4:3 photo of a 16:9 camera is not distorted. The guest's camera HAL crops its streams by
/// the same rule.
pub fn center_crop(picture_width: u32, picture_height: u32, width: u32, height: u32) -> Crop {
    let mut crop = Crop {
        x: 0,
        y: 0,
        width: picture_width,
        height: picture_height,
    };
    if picture_width == 0 || picture_height == 0 || width == 0 || height == 0 {
        return crop;
    }
    let wide = u64::from(picture_width) * u64::from(height);
    let tall = u64::from(picture_height) * u64::from(width);
    if wide > tall {
        crop.width = ((tall / u64::from(height)) as u32).max(1);
    } else if tall > wide {
        crop.height = ((wide / u64::from(width)) as u32).max(1);
    }
    crop.x = ((picture_width - crop.width) / 2) & !1;
    crop.y = ((picture_height - crop.height) / 2) & !1;
    crop
}

/// `frame` scaled to `width` by `height` (each even and at least 2) by taking the nearest
/// pixel, which is what a still or a smaller stream needs from a camera's picture. A size of
/// another shape is cut from the centre first ([`center_crop`]).
pub fn scale_nv12(frame: &Frame, width: u32, height: u32) -> Option<Frame> {
    if !frame.is_whole() || width < 2 || height < 2 || width % 2 != 0 || height % 2 != 0 {
        return None;
    }
    if width == frame.width && height == frame.height {
        return Some(frame.clone());
    }
    let crop = center_crop(frame.width, frame.height, width, height);
    let (sw, sh, ss) = (
        frame.width as usize,
        frame.height as usize,
        frame.stride as usize,
    );
    let (cx, cy, cw, ch) = (
        crop.x as usize,
        crop.y as usize,
        crop.width as usize,
        crop.height as usize,
    );
    let (dw, dh) = (width as usize, height as usize);
    let mut nv12 = vec![0u8; dw * (dh + dh / 2)];
    for y in 0..dh {
        let sy = cy + y * ch / dh;
        let src = &frame.nv12[sy * ss..sy * ss + sw];
        let dst = &mut nv12[y * dw..(y + 1) * dw];
        for (x, out) in dst.iter_mut().enumerate() {
            *out = src[cx + x * cw / dw];
        }
    }
    let chroma = ss * sh;
    let (scw, sch) = (sw.div_ceil(2), sh.div_ceil(2));
    let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
    for y in 0..dh / 2 {
        let sy = (ccy + y * cch / (dh / 2)).min(sch - 1);
        let row = chroma + sy * ss;
        let out = dw * dh + y * dw;
        for x in 0..dw / 2 {
            let sx = (ccx + x * ccw / (dw / 2)).min(scw - 1);
            nv12[out + 2 * x] = frame.nv12[row + 2 * sx];
            nv12[out + 2 * x + 1] = frame.nv12[row + 2 * sx + 1];
        }
    }
    Some(Frame {
        width,
        height,
        stride: width,
        nv12,
        timestamp_ns: frame.timestamp_ns,
    })
}

/// `frame` as 24-bit pixels, blue, green and red in each, rows `3 * width` bytes long, with
/// the limited range BT.601 colours a camera's YUV carries. What a JPEG encoder takes.
pub fn nv12_to_bgr(frame: &Frame) -> Option<Vec<u8>> {
    if !frame.is_whole() {
        return None;
    }
    let (w, h, s) = (
        frame.width as usize,
        frame.height as usize,
        frame.stride as usize,
    );
    let chroma = s * h;
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let luma = i32::from(frame.nv12[y * s + x]);
            let at = chroma + (y / 2) * s + (x / 2) * 2;
            let u = i32::from(frame.nv12[at]) - 128;
            let v = i32::from(frame.nv12[at + 1]) - 128;
            let c = 298 * (luma - 16);
            let clamp = |n: i32| ((n + 128) >> 8).clamp(0, 255) as u8;
            out.push(clamp(c + 516 * u));
            out.push(clamp(c - 100 * u - 208 * v));
            out.push(clamp(c + 409 * v));
        }
    }
    Some(out)
}

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraEnd {
    /// The guest closed it.
    Closed,
    /// It ended on an error; the detail says what.
    Failed(String),
}

fn send(connection: &mut dyn Connection, body: Body) -> Result<(), TransportError> {
    connection.send(&encode(&Envelope { body: Some(body) }))
}

/// How a connection ends when sending `what` failed: closed when the guest went away first.
fn send_failed(what: &str, error: TransportError) -> CameraEnd {
    match error {
        TransportError::Closed => CameraEnd::Closed,
        error => CameraEnd::Failed(format!("{what} did not go out: {error}")),
    }
}

/// The cameras the guest may use now: every one not blocked that has a mode to offer.
fn listed(cameras: &dyn Cameras) -> Vec<Camera> {
    cameras
        .list()
        .into_iter()
        .filter(|camera| !camera.blocked && !offered_modes(&camera.modes).is_empty())
        .collect()
}

fn device_of(camera: &Camera) -> CameraDevice {
    CameraDevice {
        id: camera.id.clone(),
        name: camera.name.clone(),
        facing: camera.facing.to_control() as i32,
        sensor_orientation: camera.sensor_orientation,
        modes: offered_modes(&camera.modes)
            .into_iter()
            .map(|mode| CameraMode {
                width: mode.width,
                height: mode.height,
                max_fps: mode.max_fps,
                min_fps: mode.min_fps,
            })
            .collect(),
    }
}

fn refused(why: String) -> Body {
    Body::CameraOpened(CameraOpened {
        ok: false,
        error: why,
        ..CameraOpened::default()
    })
}

/// The running camera of a connection.
struct Running {
    stream: Box<dyn CameraStream>,
    /// The newest picture, from which a still is made.
    last: Option<Frame>,
}

/// Sends `frame` to the guest and keeps it as the newest picture.
fn send_frame(
    connection: &mut dyn Connection,
    running: &mut Running,
    mut frame: Frame,
    sequence: u64,
) -> Result<(), TransportError> {
    let envelope = Envelope {
        body: Some(Body::CameraFrame(CameraFrame {
            sequence,
            timestamp_ns: frame.timestamp_ns,
            width: frame.width,
            height: frame.height,
            stride: frame.stride,
            nv12: std::mem::take(&mut frame.nv12),
        })),
    };
    let sent = connection.send(&encode(&envelope));
    if let Some(Body::CameraFrame(message)) = envelope.body {
        frame.nv12 = message.nv12;
    }
    running.last = Some(frame);
    sent
}

/// Runs one connection on the camera port to its end. `build` names this host in its Hello.
pub fn serve_connection(
    mut connection: Box<dyn Connection>,
    cameras: &dyn Cameras,
    build: &str,
) -> CameraEnd {
    let local = local_hello(Role::Host, build);
    let accepted = host_accept_open(connection.as_mut(), &local, Role::GuestCamera, OPEN_TIMEOUT);
    if let Err(error) = accepted {
        return CameraEnd::Failed(format!("the handshake failed: {error}"));
    }
    let list = CameraList {
        cameras: listed(cameras).iter().map(device_of).collect(),
    };
    if let Err(error) = send(connection.as_mut(), Body::CameraList(list)) {
        return send_failed("the camera list", error);
    }

    let mut running: Option<Running> = None;
    let mut sequence = 0u64;
    loop {
        let timeout = running.as_ref().map(|_| POLL);
        let message = match connection.recv(timeout) {
            Ok(frame) => Some(frame),
            Err(TransportError::Timeout) => None,
            Err(TransportError::Closed) => return CameraEnd::Closed,
            Err(error) => return CameraEnd::Failed(format!("the connection failed: {error}")),
        };
        if let Some(frame) = message {
            let sent = match decode(&frame).ok().and_then(|e| e.body) {
                Some(Body::CameraOpen(open)) => {
                    // The camera running before is let go first, so a new size never needs
                    // the camera twice.
                    running = None;
                    // Listed again, so a camera blocked since the list does not open.
                    let present = listed(cameras);
                    let answer = match present.iter().find(|c| c.id == open.id) {
                        None => refused(format!(
                            "the camera {:?} is not there or the user has blocked it in the \
                             computer's privacy settings",
                            open.id
                        )),
                        Some(camera) => {
                            match choose_mode(&camera.modes, open.width, open.height, open.fps) {
                                None => refused(format!("{} has no mode to run", camera.name)),
                                Some((mode, fps)) => match cameras.open(&camera.id, mode, fps) {
                                    Ok(stream) => {
                                        running = Some(Running { stream, last: None });
                                        Body::CameraOpened(CameraOpened {
                                            ok: true,
                                            error: String::new(),
                                            width: mode.width,
                                            height: mode.height,
                                            fps,
                                        })
                                    }
                                    Err(why) => refused(why),
                                },
                            }
                        }
                    };
                    send(connection.as_mut(), answer)
                }
                Some(Body::CameraStop(_)) => {
                    running = None;
                    Ok(())
                }
                Some(Body::CameraStill(still)) => {
                    let answer = still_of(cameras, running.as_ref(), &still);
                    send(connection.as_mut(), Body::CameraStillTaken(answer))
                }
                _ => {
                    let why = "the guest sent a message the camera port does not take";
                    return CameraEnd::Failed(why.into());
                }
            };
            if let Err(error) = sent {
                return send_failed("a message to the guest", error);
            }
        }

        let Some(run) = running.as_mut() else {
            continue;
        };
        match run.stream.next() {
            Ok(frame) if frame.is_whole() => {
                if let Err(error) = send_frame(connection.as_mut(), run, frame, sequence) {
                    return send_failed("a picture", error);
                }
                sequence += 1;
            }
            // A picture whose size does not match its bytes is dropped.
            Ok(_) => {}
            Err(why) => {
                running = None;
                let failed = Body::CameraError(CameraError { error: why });
                if let Err(error) = send(connection.as_mut(), failed) {
                    return send_failed("the error", error);
                }
            }
        }
    }
}

fn still_of(
    cameras: &dyn Cameras,
    running: Option<&Running>,
    still: &dsa_protocol::control::CameraStill,
) -> CameraStillTaken {
    let fail = |why: String| CameraStillTaken {
        request_id: still.request_id,
        jpeg: Vec::new(),
        error: why,
    };
    let Some(last) = running.and_then(|r| r.last.as_ref()) else {
        return fail("the camera has no picture yet".into());
    };
    let (width, height) = (still.width, still.height);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return fail(format!(
            "a still of {width}x{height} is not one the host makes"
        ));
    }
    // An odd size is made one pixel smaller, as 4:2:0 needs even sides.
    let Some(picture) = scale_nv12(last, width & !1, height & !1) else {
        return fail(format!(
            "a still of {width}x{height} is not one the host makes"
        ));
    };
    let quality = still.quality.clamp(1, 100) as u8;
    match cameras.encode_jpeg(&picture, quality) {
        Ok(jpeg) => CameraStillTaken {
            request_id: still.request_id,
            jpeg,
            error: String::new(),
        },
        Err(why) => fail(why),
    }
}

/// Accepts connections on `listener` until `stop` is set, and runs each on a thread of its
/// own, at most [`MAX_CONNECTIONS`] at once. `log` takes a line for each connection that ends
/// on an error. A listener that fails (the guest's VM went away) is tried again a second
/// later.
pub fn serve(
    mut listener: Box<dyn Listener>,
    cameras: Arc<dyn Cameras>,
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
                    "camera: the camera port took no connection: {error}"
                ));
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        if running.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            log(&format!(
                "camera: refused a connection; {MAX_CONNECTIONS} are already open"
            ));
            connection.close();
            continue;
        }
        running.fetch_add(1, Ordering::SeqCst);
        let (cameras, build, counter, log) = (
            Arc::clone(&cameras),
            build.clone(),
            Arc::clone(&running),
            Arc::clone(&log),
        );
        let spawned = thread::Builder::new()
            .name("dsa-camera".into())
            .spawn(move || {
                let end = serve_connection(connection, cameras.as_ref(), &build);
                if let CameraEnd::Failed(why) = end {
                    log(&format!("camera: a connection ended: {why}"));
                }
                counter.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            running.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
