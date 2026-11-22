//! The camera port: the handshake, the list, opens, pictures and stills, with fake cameras
//! standing in for the platform's.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::camera::{
    center_crop, choose_mode, facing_of, list_text, nv12_to_bgr, offered_modes, parse_list_text,
    scale_nv12, sensor_orientation, serve_connection, Camera, CameraEnd, CameraStream, Cameras,
    Facing, Frame, Mode, Panel,
};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    CameraFacing, CameraList, CameraOpen, CameraOpened, CameraStill, CameraStop, Envelope, Role,
};
use dsa_protocol::handshake::local_hello;
use dsa_protocol::{decode, encode};
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener};

const WAIT: Duration = Duration::from_secs(5);

fn mode(width: u32, height: u32, min_fps: u32, max_fps: u32) -> Mode {
    Mode {
        width,
        height,
        min_fps,
        max_fps,
    }
}

/// A picture of `width` by `height` whose every luma byte is `value`.
fn picture(width: u32, height: u32, value: u8, timestamp_ns: u64) -> Frame {
    let luma = (width * height) as usize;
    let mut nv12 = vec![value; luma];
    nv12.resize(luma + luma / 2, 0x80);
    Frame {
        width,
        height,
        stride: width,
        nv12,
        timestamp_ns,
    }
}

/// What the fake cameras were asked, and how many are open now.
#[derive(Default)]
struct Record {
    log: Mutex<Vec<String>>,
    open: AtomicUsize,
}

/// A camera that gives pictures 1, 2, 3 and so on (in the luma), then fails after `fail_after`
/// pictures when that is set.
struct FakeStream {
    width: u32,
    height: u32,
    count: u8,
    fail_after: Option<u8>,
    record: Arc<Record>,
}

impl CameraStream for FakeStream {
    fn next(&mut self) -> Result<Frame, String> {
        if Some(self.count) == self.fail_after {
            return Err("the camera was unplugged".into());
        }
        self.count += 1;
        thread::sleep(Duration::from_millis(10));
        Ok(picture(
            self.width,
            self.height,
            self.count,
            u64::from(self.count) * 33_333_333,
        ))
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.record.open.fetch_sub(1, Ordering::SeqCst);
        self.record.log.lock().unwrap().push("close".into());
    }
}

struct FakeCameras {
    cameras: Vec<Camera>,
    fail_after: Option<u8>,
    record: Arc<Record>,
}

fn camera(id: &str, facing: Facing, blocked: bool) -> Camera {
    Camera {
        id: id.into(),
        name: format!("{id} camera"),
        facing,
        sensor_orientation: 0,
        modes: vec![
            mode(640, 480, 15, 30),
            mode(1280, 720, 5, 30),
            mode(1280, 720, 30, 60),
        ],
        blocked,
    }
}

impl FakeCameras {
    fn new(fail_after: Option<u8>) -> FakeCameras {
        FakeCameras {
            cameras: vec![
                camera("front", Facing::Front, false),
                camera("back", Facing::Back, false),
                camera("usb", Facing::External, false),
                camera("blocked", Facing::Front, true),
            ],
            fail_after,
            record: Arc::default(),
        }
    }
}

impl Cameras for FakeCameras {
    fn list(&self) -> Vec<Camera> {
        self.cameras.clone()
    }

    fn open(&self, id: &str, mode: Mode, fps: u32) -> Result<Box<dyn CameraStream>, String> {
        let line = format!("open {id} {}x{} {fps}", mode.width, mode.height);
        self.record.log.lock().unwrap().push(line);
        if self.cameras.iter().any(|c| c.id == id && c.blocked) {
            return Err("blocked".into());
        }
        self.record.open.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeStream {
            width: mode.width,
            height: mode.height,
            count: 0,
            fail_after: self.fail_after,
            record: Arc::clone(&self.record),
        }))
    }

    fn encode_jpeg(&self, frame: &Frame, quality: u8) -> Result<Vec<u8>, String> {
        let (width, height, luma) = (frame.width, frame.height, frame.nv12[0]);
        let text = format!("JPEG {width}x{height} q{quality} y{luma}");
        Ok(text.into_bytes())
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

/// The guest's end past the handshake, the host's thread, the list it sent, and the record of
/// what the cameras were asked.
type Connected = (
    Box<dyn Connection>,
    thread::JoinHandle<CameraEnd>,
    CameraList,
    Arc<Record>,
);

fn connect(fail_after: Option<u8>) -> Connected {
    let (mut listener, mut connector) = memory::pair();
    let cameras = FakeCameras::new(fail_after);
    let record = Arc::clone(&cameras.record);
    let host = thread::spawn(move || {
        let connection = listener.accept(WAIT).expect("accepts");
        serve_connection(connection, &cameras, "dsa-host test")
    });
    let mut guest = connector.connect(WAIT).expect("connects");
    send(
        guest.as_mut(),
        Body::Hello(local_hello(Role::GuestCamera, "dsa-camera test")),
    );
    assert!(matches!(next(guest.as_mut()), Body::HelloAccepted(_)));
    let list = match next(guest.as_mut()) {
        Body::CameraList(list) => list,
        other => panic!("expected the camera list, got {other:?}"),
    };
    (guest, host, list, record)
}

fn open(guest: &mut dyn Connection, id: &str, width: u32, height: u32, fps: u32) -> CameraOpened {
    send(
        guest,
        Body::CameraOpen(CameraOpen {
            id: id.into(),
            width,
            height,
            fps,
        }),
    );
    // Pictures of a camera running before may still be on their way.
    loop {
        match next(guest) {
            Body::CameraOpened(opened) => return opened,
            Body::CameraFrame(_) => continue,
            other => panic!("expected the answer, got {other:?}"),
        }
    }
}

#[test]
fn fr26_the_cameras_are_listed_with_their_facing_and_modes() {
    let (guest, host, list, _) = connect(None);
    let ids: Vec<&str> = list.cameras.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["front", "back", "usb"]);
    let facings: Vec<i32> = list.cameras.iter().map(|c| c.facing).collect();
    assert_eq!(
        facings,
        [
            CameraFacing::Front as i32,
            CameraFacing::Back as i32,
            CameraFacing::External as i32,
        ]
    );
    // Each size once, with every rate it is delivered at, largest first.
    let first = &list.cameras[0];
    let modes: Vec<(u32, u32, u32, u32)> = first
        .modes
        .iter()
        .map(|m| (m.width, m.height, m.min_fps, m.max_fps))
        .collect();
    assert_eq!(modes, [(1280, 720, 5, 60), (640, 480, 15, 30)]);
    drop(guest);
    assert_eq!(host.join().unwrap(), CameraEnd::Closed);
}

#[test]
fn fr26_a_blocked_camera_is_left_out_and_does_not_open() {
    let (mut guest, host, list, record) = connect(None);
    assert!(list.cameras.iter().all(|c| c.id != "blocked"));
    let opened = open(guest.as_mut(), "blocked", 1280, 720, 30);
    assert!(!opened.ok);
    assert!(opened.error.contains("privacy"), "{}", opened.error);
    // The platform was never asked, and the connection goes on.
    assert!(record.log.lock().unwrap().is_empty());
    let opened = open(guest.as_mut(), "front", 640, 480, 30);
    assert!(opened.ok, "{}", opened.error);
    drop(guest);
    assert_eq!(host.join().unwrap(), CameraEnd::Closed);
}

#[test]
fn fr26_pictures_of_the_requested_mode_come_out_in_order() {
    let (mut guest, host, _, record) = connect(None);
    let opened = open(guest.as_mut(), "back", 1280, 720, 30);
    assert!(opened.ok, "{}", opened.error);
    assert_eq!((opened.width, opened.height, opened.fps), (1280, 720, 30));
    for expected in 0..3u64 {
        match next(guest.as_mut()) {
            Body::CameraFrame(frame) => {
                assert_eq!(frame.sequence, expected);
                assert_eq!((frame.width, frame.height, frame.stride), (1280, 720, 1280));
                assert_eq!(frame.nv12.len(), 1280 * 720 * 3 / 2);
                assert_eq!(frame.nv12[0], expected as u8 + 1);
                assert_eq!(frame.timestamp_ns, (expected + 1) * 33_333_333);
            }
            other => panic!("expected a picture, got {other:?}"),
        }
    }
    assert_eq!(
        record.log.lock().unwrap().first().map(String::as_str),
        Some("open back 1280x720 30")
    );
    drop(guest);
    assert_eq!(host.join().unwrap(), CameraEnd::Closed);
}

#[test]
fn fr26_the_camera_is_open_only_while_in_use() {
    let (mut guest, host, _, record) = connect(None);
    assert_eq!(record.open.load(Ordering::SeqCst), 0);
    assert!(open(guest.as_mut(), "front", 640, 480, 15).ok);
    assert_eq!(record.open.load(Ordering::SeqCst), 1);
    // A new size lets the camera go before it runs again, so it is never held twice.
    assert!(open(guest.as_mut(), "front", 1280, 720, 30).ok);
    assert_eq!(record.open.load(Ordering::SeqCst), 1);
    send(guest.as_mut(), Body::CameraStop(CameraStop {}));
    // A still afterwards has no picture, which shows the stop was taken.
    send(
        guest.as_mut(),
        Body::CameraStill(CameraStill {
            request_id: 1,
            width: 640,
            height: 480,
            quality: 90,
        }),
    );
    loop {
        match next(guest.as_mut()) {
            Body::CameraStillTaken(taken) => {
                assert!(taken.jpeg.is_empty());
                break;
            }
            Body::CameraFrame(_) => continue,
            other => panic!("expected the still's answer, got {other:?}"),
        }
    }
    assert_eq!(record.open.load(Ordering::SeqCst), 0);
    // Closing the connection lets a running camera go too.
    assert!(open(guest.as_mut(), "usb", 640, 480, 30).ok);
    assert_eq!(record.open.load(Ordering::SeqCst), 1);
    drop(guest);
    assert_eq!(host.join().unwrap(), CameraEnd::Closed);
    assert_eq!(record.open.load(Ordering::SeqCst), 0);
}

#[test]
fn fr26_a_still_is_encoded_from_the_newest_picture() {
    let (mut guest, host, _, _) = connect(None);
    assert!(open(guest.as_mut(), "front", 1280, 720, 30).ok);
    let newest = match next(guest.as_mut()) {
        Body::CameraFrame(frame) => frame.nv12[0],
        other => panic!("expected a picture, got {other:?}"),
    };
    send(
        guest.as_mut(),
        Body::CameraStill(CameraStill {
            request_id: 9,
            width: 640,
            height: 360,
            quality: 0,
        }),
    );
    loop {
        match next(guest.as_mut()) {
            Body::CameraStillTaken(taken) => {
                assert_eq!(taken.request_id, 9);
                assert!(taken.error.is_empty(), "{}", taken.error);
                let text = String::from_utf8(taken.jpeg).unwrap();
                // Scaled to the still's size; quality 0 becomes the least, 1.
                assert!(text.starts_with("JPEG 640x360 q1 y"), "{text}");
                let luma: u8 = text.rsplit('y').next().unwrap().parse().unwrap();
                assert!(luma >= newest);
                break;
            }
            Body::CameraFrame(_) => continue,
            other => panic!("expected the still, got {other:?}"),
        }
    }
    drop(guest);
    assert_eq!(host.join().unwrap(), CameraEnd::Closed);
}

#[test]
fn fr26_a_camera_that_fails_reports_an_error_and_the_connection_goes_on() {
    let (mut guest, host, _, record) = connect(Some(1));
    assert!(open(guest.as_mut(), "usb", 640, 480, 30).ok);
    assert!(matches!(next(guest.as_mut()), Body::CameraFrame(_)));
    match next(guest.as_mut()) {
        Body::CameraError(error) => assert!(error.error.contains("unplugged")),
        other => panic!("expected the error, got {other:?}"),
    }
    assert_eq!(record.open.load(Ordering::SeqCst), 0);
    // It may be opened again.
    assert!(open(guest.as_mut(), "usb", 640, 480, 30).ok);
    drop(guest);
    assert_eq!(host.join().unwrap(), CameraEnd::Closed);
}

#[test]
fn fr26_a_bad_message_ends_only_its_connection() {
    let (mut guest, host, _, _) = connect(None);
    guest.send(&[0xff, 0x00, 0x13]).expect("send");
    assert!(matches!(host.join().unwrap(), CameraEnd::Failed(_)));
}

#[test]
fn fr26_the_control_role_is_refused_on_the_camera_port() {
    let (mut listener, mut connector) = memory::pair();
    let cameras = FakeCameras::new(None);
    let host = thread::spawn(move || {
        let connection = listener.accept(WAIT).expect("accepts");
        serve_connection(connection, &cameras, "dsa-host test")
    });
    let mut guest = connector.connect(WAIT).expect("connects");
    send(
        guest.as_mut(),
        Body::Hello(local_hello(Role::Guest, "dsa-channeld test")),
    );
    assert!(matches!(next(guest.as_mut()), Body::HelloRejected(_)));
    assert!(matches!(host.join().unwrap(), CameraEnd::Failed(_)));
}

#[test]
fn fr26_the_enclosure_panel_sets_the_facing() {
    assert_eq!(facing_of(Some(Panel::Front)), Facing::Front);
    assert_eq!(facing_of(Some(Panel::Back)), Facing::Back);
    assert_eq!(facing_of(Some(Panel::Unknown)), Facing::External);
    assert_eq!(facing_of(Some(Panel::Top)), Facing::External);
    assert_eq!(facing_of(None), Facing::External);
}

#[test]
fn fr26_the_sensor_orientation_is_a_quarter_turn() {
    assert_eq!(sensor_orientation(0), 0);
    assert_eq!(sensor_orientation(90), 90);
    assert_eq!(sensor_orientation(268), 270);
    assert_eq!(sensor_orientation(-90), 270);
    assert_eq!(sensor_orientation(359), 0);
}

#[test]
fn fr26_modes_are_chosen_from_what_the_camera_delivers() {
    let modes = [
        mode(640, 480, 15, 30),
        mode(1920, 1080, 5, 30),
        mode(1280, 720, 5, 30),
        mode(8192, 4320, 1, 15),
        mode(641, 481, 1, 30),
    ];
    // Too large and odd sizes are not offered.
    let offered: Vec<(u32, u32)> = offered_modes(&modes)
        .iter()
        .map(|m| (m.width, m.height))
        .collect();
    assert_eq!(offered, [(1920, 1080), (1280, 720), (640, 480)]);
    // The size asked for, at the rate asked for.
    assert_eq!(
        choose_mode(&modes, 1280, 720, 24),
        Some((mode(1280, 720, 5, 30), 24))
    );
    // The smallest size that covers it, at a rate the mode delivers.
    assert_eq!(
        choose_mode(&modes, 800, 600, 60),
        Some((mode(1280, 720, 5, 30), 30))
    );
    // Larger than any: the largest.
    assert_eq!(
        choose_mode(&modes, 3840, 2160, 0),
        Some((mode(1920, 1080, 5, 30), 30))
    );
    assert_eq!(choose_mode(&[], 640, 480, 30), None);
}

#[test]
fn fr26_a_picture_scales_and_converts_for_a_still() {
    // 4x2: luma 10, 20, 30, 40 in each row; chroma pairs (100, 200) and (110, 210).
    let frame = Frame {
        width: 4,
        height: 2,
        stride: 4,
        nv12: vec![10, 20, 30, 40, 10, 20, 30, 40, 100, 200, 110, 210],
        timestamp_ns: 5,
    };
    // 2x2 is another shape: the square at the left, from an even offset, not a squeeze.
    let half = scale_nv12(&frame, 2, 2).expect("scales");
    assert_eq!(half.nv12, vec![10, 20, 10, 20, 100, 200]);
    assert_eq!(half.timestamp_ns, 5);
    assert_eq!(scale_nv12(&frame, 3, 2), None);

    let grey = picture(2, 2, 126, 0);
    let bgr = nv12_to_bgr(&grey).expect("converts");
    assert_eq!(bgr.len(), 12);
    // Mid grey stays grey.
    assert!(bgr.iter().all(|&b| (128..=131).contains(&b)), "{bgr:?}");
}

#[test]
fn nfr1_the_camera_list_comes_back_whole_from_the_process_that_listed_it() {
    let mut front = camera(r"\\?\usb#vid_1&pid_2#3#{e5323777}", Facing::Front, false);
    front.name = "Integrated\tCamera\n".into();
    front.sensor_orientation = 90;
    front.modes = vec![mode(1280, 720, 15, 30), mode(640, 480, 1, 60)];
    let mut blocked = camera("usb#other", Facing::External, true);
    blocked.modes = Vec::new();
    let back = camera("usb#back", Facing::Back, false);
    let text = list_text(&[front.clone(), blocked.clone(), back.clone()]);
    assert_eq!(text.lines().count(), 3);
    let parsed = parse_list_text(&text);
    front.name = "Integrated Camera ".into();
    assert_eq!(parsed, vec![front, blocked, back]);
}

#[test]
fn nfr1_a_camera_line_that_does_not_read_is_left_out() {
    let parsed = parse_list_text("garbage\n\tname\tfront\t0\t0\t\nid\tname\tsideways\t0\t0\t\n");
    assert!(parsed.is_empty());
    let parsed = parse_list_text("id\tname\tback\t0\t0\t640x480@1-30,bad,1x2\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].modes, vec![mode(640, 480, 1, 30)]);
}

#[test]
fn nfr8_a_4_3_still_of_a_16_9_picture_is_cropped_at_the_centre_not_stretched() {
    use dsa_host_core::camera::Crop;
    let crop = |x, y, width, height| Crop {
        x,
        y,
        width,
        height,
    };
    assert_eq!(
        center_crop(1920, 1080, 1440, 1080),
        crop(240, 0, 1440, 1080)
    );
    assert_eq!(center_crop(1280, 720, 640, 480), crop(160, 0, 960, 720));
    assert_eq!(center_crop(640, 480, 1280, 720), crop(0, 60, 640, 360));
    assert_eq!(center_crop(1280, 720, 320, 180), crop(0, 0, 1280, 720));
    // A 16x4 picture whose luma is its column number, into an 8x6 (4:3) still: only columns
    // 4 to 8 of the middle, in order, so nothing is squeezed.
    let mut nv12 = Vec::new();
    for _ in 0..4 {
        nv12.extend(0u8..16);
    }
    for _ in 0..2 {
        for x in 0..8u8 {
            nv12.extend([100 + x, 200 + x]);
        }
    }
    let frame = Frame {
        width: 16,
        height: 4,
        stride: 16,
        nv12,
        timestamp_ns: 0,
    };
    assert_eq!(center_crop(16, 4, 8, 6), crop(4, 0, 5, 4));
    let still = scale_nv12(&frame, 8, 6).expect("scales");
    let row = &still.nv12[..8];
    assert!(row.iter().all(|&v| (4..9).contains(&v)), "{row:?}");
    assert!(row.windows(2).all(|w| w[0] <= w[1]), "{row:?}");
    assert_eq!(still.nv12[8 * 6], 102);
}
