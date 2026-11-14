//! The guest's sound on the host: the jitter buffer, the conversion to the device's format,
//! and a stream on the audio port over the in-memory transport, against a fake device; and
//! each app's output under its own volume, which only the host sets.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::app_audio::{self, AppStream, AppVolumes, VolumeChange};
use dsa_host_core::audio::{
    serve_port, serve_stream, AudioDevice, CaptureSink, DeviceFormat, JitterBuffer, Playback,
    Recording, Renderer, SampleType, SharedBuffer, StreamEnd, BYTES_PER_FRAME, MAX_BUFFER_FRAMES,
    MAX_STREAMS, TARGET_BUFFER_FRAMES,
};
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::host_api_conformance::{sample_app_stream, FakeAndroids, RunningService};
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{
    AudioInputOpen, AudioInputOpened, AudioOutputCommand, AudioOutputControl, AudioOutputOpen,
    AudioPcm, AudioSampleFormat, Envelope, Hello, RejectReason, Role,
};
use dsa_protocol::handshake::local_hello;
use dsa_protocol::{decode, encode};
use dsa_transport::local;
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

/// `frames` stereo frames whose left sample is `value` and right is `-value`.
fn pcm(frames: usize, value: i16) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames * BYTES_PER_FRAME);
    for _ in 0..frames {
        out.extend_from_slice(&value.to_le_bytes());
        out.extend_from_slice(&(-value).to_le_bytes());
    }
    out
}

fn pull(buffer: &mut JitterBuffer, frames: usize) -> Vec<i16> {
    let mut out = vec![1234; frames * 2];
    buffer.pull(&mut out);
    out
}

#[test]
fn pr4_nothing_plays_until_the_target_is_queued() {
    let mut buffer = JitterBuffer::new(100, 200);
    buffer.start();
    assert!(buffer.push(0, &pcm(60, 5)));
    assert!(pull(&mut buffer, 10).iter().all(|&s| s == 0));
    assert_eq!(buffer.buffered_frames(), 60);
    assert!(buffer.push(60, &pcm(40, 5)));
    let out = pull(&mut buffer, 10);
    assert_eq!(&out[..2], &[5, -5]);
    assert_eq!(buffer.buffered_frames(), 90);
}

#[test]
fn pr4_an_underrun_plays_silence_and_waits_for_the_target_again() {
    let mut buffer = JitterBuffer::new(10, 40);
    buffer.start();
    buffer.push(0, &pcm(10, 7));
    let out = pull(&mut buffer, 15);
    assert!(out[..20].iter().all(|&s| s.abs() == 7));
    assert!(out[20..].iter().all(|&s| s == 0));
    assert_eq!(buffer.stats().underrun_frames, 5);
    // Less than the target again: still silent.
    buffer.push(10, &pcm(5, 7));
    assert!(pull(&mut buffer, 2).iter().all(|&s| s == 0));
    buffer.push(15, &pcm(5, 7));
    assert_eq!(&pull(&mut buffer, 1)[..], &[7, -7]);
}

#[test]
fn pr4_a_full_queue_drops_the_oldest_sound_back_to_the_target() {
    let mut buffer = JitterBuffer::new(10, 20);
    buffer.start();
    buffer.push(0, &pcm(15, 1));
    buffer.push(15, &pcm(10, 2));
    assert_eq!(buffer.buffered_frames(), 10);
    assert_eq!(buffer.stats().dropped_frames, 15);
    assert!(pull(&mut buffer, 10).iter().all(|&s| s.abs() == 2));
}

#[test]
fn pr4_frames_the_guest_skipped_become_silence() {
    let mut buffer = JitterBuffer::new(1, 100);
    buffer.start();
    buffer.push(0, &pcm(2, 3));
    buffer.push(5, &pcm(2, 3));
    let out = pull(&mut buffer, 7);
    assert_eq!(out, vec![3, -3, 3, -3, 0, 0, 0, 0, 0, 0, 3, -3, 3, -3]);
    assert_eq!(buffer.stats().gap_frames, 3);
}

#[test]
fn pr4_frames_sent_again_are_not_played_twice() {
    let mut buffer = JitterBuffer::new(1, 100);
    buffer.start();
    buffer.push(0, &pcm(4, 3));
    buffer.push(2, &pcm(4, 4));
    assert_eq!(buffer.buffered_frames(), 6);
}

#[test]
fn pr4_pause_holds_and_flush_drops_the_queue() {
    let mut buffer = JitterBuffer::new(1, 100);
    buffer.start();
    buffer.push(0, &pcm(4, 3));
    buffer.pause();
    assert!(pull(&mut buffer, 2).iter().all(|&s| s == 0));
    assert_eq!(buffer.buffered_frames(), 4);
    buffer.start();
    assert_eq!(&pull(&mut buffer, 1)[..], &[3, -3]);
    buffer.flush();
    assert_eq!(buffer.buffered_frames(), 0);
    // After a flush the next push starts a new timeline, with no silence for the gap.
    buffer.push(1000, &pcm(1, 9));
    assert_eq!(buffer.stats().gap_frames, 0);
    assert_eq!(&pull(&mut buffer, 1)[..], &[9, -9]);
}

#[test]
fn pr4_a_piece_that_is_not_whole_frames_is_refused() {
    let mut buffer = JitterBuffer::default();
    assert!(!buffer.push(0, &[1, 2, 3]));
    assert_eq!(buffer.buffered_frames(), 0);
}

#[test]
fn pr4_the_default_queue_is_40_ms_and_at_most_80_ms() {
    assert_eq!(TARGET_BUFFER_FRAMES, 1920);
    assert_eq!(MAX_BUFFER_FRAMES, 3840);
}

fn primed(frames: usize, value: i16) -> SharedBuffer {
    let mut buffer = JitterBuffer::new(1, 100_000);
    buffer.start();
    buffer.push(0, &pcm(frames, value));
    SharedBuffer::new(buffer)
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[test]
fn pr4_stereo_float_at_48_khz_is_the_same_sound() {
    let source = primed(4800, 16384);
    let format = DeviceFormat {
        sample_rate: 48_000,
        channels: 2,
        sample: SampleType::F32,
    };
    let mut renderer = Renderer::new(format, source).unwrap();
    let mut out = vec![0u8; 100 * format.bytes_per_frame()];
    renderer.render(&mut out);
    let samples = f32s(&out);
    // The first frames are the renderer's own start; after them, the sound itself.
    assert_eq!(&samples[20..24], &[0.5, -0.5, 0.5, -0.5]);
}

#[test]
fn pr4_a_44_1_khz_device_gets_the_same_duration() {
    let source = primed(48_000, 1000);
    let format = DeviceFormat {
        sample_rate: 44_100,
        channels: 2,
        sample: SampleType::I16,
    };
    let mut renderer = Renderer::new(format, source.clone()).unwrap();
    let mut out = vec![0u8; 44_100 / 2 * format.bytes_per_frame()];
    renderer.render(&mut out);
    // Half a second at the device's rate took about half a second of the guest's sound.
    let left = 48_000 - source.lock().buffered_frames();
    assert!((23_900..=24_300).contains(&left), "took {left} frames");
    let first = i16::from_le_bytes([out[400], out[401]]);
    assert_eq!(first, 1000);
}

#[test]
fn pr4_a_mono_device_gets_the_mean_and_extra_channels_are_silent() {
    let mono = DeviceFormat {
        sample_rate: 48_000,
        channels: 1,
        sample: SampleType::I16,
    };
    let mut buffer = JitterBuffer::new(1, 1000);
    buffer.start();
    let mut data = Vec::new();
    for _ in 0..100 {
        data.extend_from_slice(&100i16.to_le_bytes());
        data.extend_from_slice(&300i16.to_le_bytes());
    }
    buffer.push(0, &data);
    let mut renderer = Renderer::new(mono, SharedBuffer::new(buffer)).unwrap();
    let mut out = vec![0u8; 50 * 2];
    renderer.render(&mut out);
    assert_eq!(i16::from_le_bytes([out[40], out[41]]), 200);

    let surround = DeviceFormat {
        sample_rate: 48_000,
        channels: 6,
        sample: SampleType::F32,
    };
    let mut renderer = Renderer::new(surround, primed(100, 16384)).unwrap();
    let mut out = vec![0u8; 50 * surround.bytes_per_frame()];
    renderer.render(&mut out);
    let samples = f32s(&out);
    assert_eq!(&samples[60..66], &[0.5, -0.5, 0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn pr4_an_empty_queue_renders_silence_without_waiting() {
    let format = DeviceFormat {
        sample_rate: 48_000,
        channels: 2,
        sample: SampleType::F32,
    };
    let mut renderer = Renderer::new(format, SharedBuffer::default()).unwrap();
    let mut out = vec![0xffu8; 480 * format.bytes_per_frame()];
    renderer.render(&mut out);
    assert!(f32s(&out).iter().all(|&s| s == 0.0));
}

#[test]
fn pr4_a_format_without_channels_is_refused() {
    let format = DeviceFormat {
        sample_rate: 48_000,
        channels: 0,
        sample: SampleType::F32,
    };
    assert!(Renderer::new(format, SharedBuffer::default()).is_err());
}

/// A device that records what it was asked to play.
#[derive(Default, Clone)]
struct FakeDevice {
    playing: Arc<Mutex<Vec<(String, SharedBuffer)>>>,
    stopped: Arc<Mutex<usize>>,
    /// The sink of the capture device while it records.
    mic: Arc<Mutex<Option<CaptureSink>>>,
    /// How many times the capture device opened, and closed.
    mic_opened: Arc<Mutex<usize>>,
    mic_closed: Arc<Mutex<usize>>,
    /// Why the capture device does not open, when it does not.
    mic_blocked: Option<String>,
}

struct FakeRecording {
    mic: Arc<Mutex<Option<CaptureSink>>>,
    closed: Arc<Mutex<usize>>,
}

impl Recording for FakeRecording {
    fn device_frames(&self) -> u32 {
        480
    }
}

impl Drop for FakeRecording {
    fn drop(&mut self) {
        *self.mic.lock().unwrap() = None;
        *self.closed.lock().unwrap() += 1;
    }
}

impl FakeDevice {
    /// Records rames frames on the capture device; false when it is not recording.
    fn speak(&self, frames: usize, value: i16) -> bool {
        match self.mic.lock().unwrap().as_mut() {
            Some(sink) => {
                sink(&pcm(frames, value));
                true
            }
            None => false,
        }
    }

    fn recording(&self) -> bool {
        self.mic.lock().unwrap().is_some()
    }
}

struct FakePlayback(Arc<Mutex<usize>>);

impl Playback for FakePlayback {}

impl Drop for FakePlayback {
    fn drop(&mut self) {
        *self.0.lock().unwrap() += 1;
    }
}

impl AudioDevice for FakeDevice {
    fn play(&self, name: &str, source: SharedBuffer) -> Result<Box<dyn Playback>, String> {
        self.playing
            .lock()
            .unwrap()
            .push((name.to_string(), source));
        Ok(Box::new(FakePlayback(Arc::clone(&self.stopped))))
    }

    fn record(&self, _name: &str, sink: CaptureSink) -> Result<Box<dyn Recording>, String> {
        if let Some(why) = &self.mic_blocked {
            return Err(why.clone());
        }
        *self.mic_opened.lock().unwrap() += 1;
        *self.mic.lock().unwrap() = Some(sink);
        Ok(Box::new(FakeRecording {
            mic: Arc::clone(&self.mic),
            closed: Arc::clone(&self.mic_closed),
        }))
    }
}

fn send(connection: &mut dyn Connection, body: Body) {
    connection
        .send(&encode(&Envelope { body: Some(body) }))
        .unwrap();
}

fn recv(connection: &mut dyn Connection) -> Option<Body> {
    decode(&connection.recv(Some(WAIT)).unwrap()).unwrap().body
}

fn open_message() -> AudioOutputOpen {
    AudioOutputOpen {
        sample_rate: 48_000,
        channel_count: 2,
        format: AudioSampleFormat::S16le as i32,
        frames_per_burst: 960,
        name: "primary".into(),
    }
}

fn control(command: AudioOutputCommand) -> Body {
    Body::AudioOutputControl(AudioOutputControl {
        command: command as i32,
    })
}

/// Serves one stream on the host side of an in-memory connection.
fn serve(
    device: FakeDevice,
) -> (
    thread::JoinHandle<Result<StreamEnd, String>>,
    Box<dyn Connection>,
) {
    let (mut listener, mut connector) = memory::pair();
    let guest = connector.connect(WAIT).unwrap();
    let mut host = listener.accept(WAIT).unwrap();
    let handle =
        thread::spawn(move || serve_stream(host.as_mut(), &device, "dsa-wm test", &|_: &str| {}));
    (handle, guest)
}

fn guest_hello() -> Hello {
    local_hello(Role::GuestAudio, "dsa-audio test")
}

#[test]
fn pr4_the_guests_sound_reaches_the_device() {
    let device = FakeDevice::default();
    let (handle, mut guest) = serve(device.clone());
    send(guest.as_mut(), Body::Hello(guest_hello()));
    assert!(matches!(recv(guest.as_mut()), Some(Body::HelloAccepted(_))));
    send(guest.as_mut(), Body::AudioOutputOpen(open_message()));
    match recv(guest.as_mut()) {
        Some(Body::AudioOutputReady(ready)) => {
            assert_eq!(ready.buffered_frames as usize, TARGET_BUFFER_FRAMES)
        }
        other => panic!("expected AudioOutputReady, got {other:?}"),
    }
    send(guest.as_mut(), control(AudioOutputCommand::Start));
    for burst in 0..3u64 {
        send(
            guest.as_mut(),
            Body::AudioPcm(AudioPcm {
                position: burst * 960,
                data: pcm(960, 11),
            }),
        );
    }
    send(guest.as_mut(), control(AudioOutputCommand::Standby));
    send(guest.as_mut(), control(AudioOutputCommand::Start));
    guest.close();
    let end = handle.join().unwrap().unwrap();
    assert_eq!(end.name, "primary");
    assert_eq!(end.stats.received_frames, 2880);
    let playing = device.playing.lock().unwrap();
    // Played on the first start, stopped for standby, played again on the next start.
    assert_eq!(playing.len(), 2);
    assert_eq!(playing[0].0, "primary");
    assert_eq!(*device.stopped.lock().unwrap(), 2);
}

#[test]
fn pr4_the_control_ports_role_is_refused_on_the_audio_port() {
    let (handle, mut guest) = serve(FakeDevice::default());
    send(
        guest.as_mut(),
        Body::Hello(local_hello(Role::Guest, "dsa-channeld test")),
    );
    match recv(guest.as_mut()) {
        Some(Body::HelloRejected(rejected)) => {
            assert_eq!(rejected.reason, RejectReason::UnexpectedRole as i32)
        }
        other => panic!("expected HelloRejected, got {other:?}"),
    }
    assert!(handle.join().unwrap().is_err());
}

#[test]
fn pr4_a_format_the_host_does_not_play_is_refused() {
    let device = FakeDevice::default();
    let (handle, mut guest) = serve(device.clone());
    send(guest.as_mut(), Body::Hello(guest_hello()));
    recv(guest.as_mut());
    send(
        guest.as_mut(),
        Body::AudioOutputOpen(AudioOutputOpen {
            sample_rate: 44_100,
            ..open_message()
        }),
    );
    let error = handle.join().unwrap().unwrap_err();
    assert!(error.contains("44100"), "{error}");
    assert_eq!(guest.recv(Some(WAIT)), Err(TransportError::Closed));
    assert!(device.playing.lock().unwrap().is_empty());
}

#[test]
fn pr4_a_bad_message_ends_only_its_stream() {
    let (handle, mut guest) = serve(FakeDevice::default());
    send(guest.as_mut(), Body::Hello(guest_hello()));
    recv(guest.as_mut());
    send(guest.as_mut(), Body::AudioOutputOpen(open_message()));
    recv(guest.as_mut());
    guest.send(&[0xff, 0xff, 0xff]).unwrap();
    let end = handle.join().unwrap().unwrap();
    assert!(!end.reason.is_empty());
}

#[test]
fn pr4_a_piece_that_is_not_whole_frames_ends_the_stream() {
    let (handle, mut guest) = serve(FakeDevice::default());
    send(guest.as_mut(), Body::Hello(guest_hello()));
    recv(guest.as_mut());
    send(guest.as_mut(), Body::AudioOutputOpen(open_message()));
    recv(guest.as_mut());
    send(
        guest.as_mut(),
        Body::AudioPcm(AudioPcm {
            position: 0,
            data: vec![1, 2, 3],
        }),
    );
    let end = handle.join().unwrap().unwrap();
    assert!(end.reason.contains("whole frames"), "{}", end.reason);
}

#[test]
fn pr4_streams_play_side_by_side_up_to_the_limit() {
    let device = FakeDevice::default();
    let (mut listener, mut connector) = memory::pair();
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let device: Arc<dyn AudioDevice> = Arc::new(device.clone());
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            serve_port(
                &mut listener,
                device,
                "dsa-wm test",
                &stop,
                Arc::new(|_: &str| {}),
            )
        })
    };
    let mut guests = Vec::new();
    for _ in 0..MAX_STREAMS {
        let mut guest = connector.connect(WAIT).unwrap();
        send(guest.as_mut(), Body::Hello(guest_hello()));
        assert!(matches!(recv(guest.as_mut()), Some(Body::HelloAccepted(_))));
        send(guest.as_mut(), Body::AudioOutputOpen(open_message()));
        assert!(matches!(
            recv(guest.as_mut()),
            Some(Body::AudioOutputReady(_))
        ));
        send(guest.as_mut(), control(AudioOutputCommand::Start));
        guests.push(guest);
    }
    // One more is closed at once; the others play on.
    let mut extra = connector.connect(WAIT).unwrap();
    assert_eq!(extra.recv(Some(WAIT)), Err(TransportError::Closed));
    let deadline = std::time::Instant::now() + WAIT;
    while device.playing.lock().unwrap().len() < MAX_STREAMS {
        assert!(
            std::time::Instant::now() < deadline,
            "not every stream played"
        );
        thread::sleep(Duration::from_millis(5));
    }
    for mut guest in guests {
        guest.close();
    }
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
}

fn input_open_message() -> AudioInputOpen {
    AudioInputOpen {
        sample_rate: 48_000,
        channel_count: 2,
        format: AudioSampleFormat::S16le as i32,
        frames_per_burst: 960,
        name: "primary".into(),
    }
}

/// Says Hello on the audio port and opens an input stream; returns what the host answered.
fn open_input(guest: &mut dyn Connection) -> AudioInputOpened {
    send(guest, Body::Hello(guest_hello()));
    assert!(matches!(recv(guest), Some(Body::HelloAccepted(_))));
    send(guest, Body::AudioInputOpen(input_open_message()));
    match recv(guest) {
        Some(Body::AudioInputOpened(opened)) => opened,
        other => panic!("expected AudioInputOpened, got {other:?}"),
    }
}

/// The next recorded piece the host sends.
fn recv_pcm(guest: &mut dyn Connection) -> AudioPcm {
    match recv(guest) {
        Some(Body::AudioPcm(pcm)) => pcm,
        other => panic!("expected AudioPcm, got {other:?}"),
    }
}

#[test]
fn fr26_the_capture_devices_sound_reaches_the_guests_input_stream() {
    let device = FakeDevice::default();
    let (handle, mut guest) = serve(device.clone());
    let opened = open_input(guest.as_mut());
    assert_eq!(opened.error, "");
    assert_eq!(opened.device_frames, 480);
    assert!(device.speak(480, 21));
    assert!(device.speak(480, 22));
    let first = recv_pcm(guest.as_mut());
    assert_eq!(first.position, 0);
    assert_eq!(first.data, pcm(480, 21));
    let second = recv_pcm(guest.as_mut());
    assert_eq!(second.position, 480);
    assert_eq!(second.data, pcm(480, 22));
    guest.close();
    let end = handle.join().unwrap().unwrap();
    assert_eq!(end.name, "primary");
    assert_eq!(end.stats.received_frames, 960);
    // Nothing went to the output device.
    assert!(device.playing.lock().unwrap().is_empty());
}

#[test]
fn fr26_the_capture_device_is_open_only_while_the_input_stream_is() {
    let device = FakeDevice::default();
    let (handle, mut guest) = serve(device.clone());
    send(guest.as_mut(), Body::Hello(guest_hello()));
    recv(guest.as_mut());
    // The handshake alone opens nothing.
    assert!(!device.recording());
    send(guest.as_mut(), Body::AudioInputOpen(input_open_message()));
    assert!(matches!(
        recv(guest.as_mut()),
        Some(Body::AudioInputOpened(_))
    ));
    assert!(device.recording());
    guest.close();
    handle.join().unwrap().unwrap();
    assert!(!device.recording());
    assert_eq!(*device.mic_opened.lock().unwrap(), 1);
    assert_eq!(*device.mic_closed.lock().unwrap(), 1);
}

#[test]
fn fr26_an_output_stream_never_opens_the_capture_device() {
    let device = FakeDevice::default();
    let (handle, mut guest) = serve(device.clone());
    send(guest.as_mut(), Body::Hello(guest_hello()));
    recv(guest.as_mut());
    send(guest.as_mut(), Body::AudioOutputOpen(open_message()));
    recv(guest.as_mut());
    send(guest.as_mut(), control(AudioOutputCommand::Start));
    guest.close();
    handle.join().unwrap().unwrap();
    assert_eq!(*device.mic_opened.lock().unwrap(), 0);
}

#[test]
fn fr26_a_blocked_microphone_fails_the_open_and_the_host_goes_on() {
    let device = FakeDevice {
        mic_blocked: Some("the user turned the microphone off for desktop apps".into()),
        ..FakeDevice::default()
    };
    let (mut listener, mut connector) = memory::pair();
    let stop = Arc::new(AtomicBool::new(false));
    let server = {
        let device: Arc<dyn AudioDevice> = Arc::new(device.clone());
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            serve_port(
                &mut listener,
                device,
                "dsa-wm test",
                &stop,
                Arc::new(|_: &str| {}),
            )
        })
    };
    let mut guest = connector.connect(WAIT).unwrap();
    let opened = open_input(guest.as_mut());
    assert!(opened.error.contains("microphone off"), "{}", opened.error);
    assert_eq!(guest.recv(Some(WAIT)), Err(TransportError::Closed));
    // The port still serves: sound still plays, and a second try is answered the same way.
    let mut output = connector.connect(WAIT).unwrap();
    send(output.as_mut(), Body::Hello(guest_hello()));
    assert!(matches!(
        recv(output.as_mut()),
        Some(Body::HelloAccepted(_))
    ));
    send(output.as_mut(), Body::AudioOutputOpen(open_message()));
    assert!(matches!(
        recv(output.as_mut()),
        Some(Body::AudioOutputReady(_))
    ));
    let mut again = connector.connect(WAIT).unwrap();
    assert!(!open_input(again.as_mut()).error.is_empty());
    output.close();
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
}

#[test]
fn fr26_a_format_the_host_does_not_record_is_refused() {
    let device = FakeDevice::default();
    let (handle, mut guest) = serve(device.clone());
    send(guest.as_mut(), Body::Hello(guest_hello()));
    recv(guest.as_mut());
    send(
        guest.as_mut(),
        Body::AudioInputOpen(AudioInputOpen {
            channel_count: 1,
            ..input_open_message()
        }),
    );
    let error = handle.join().unwrap().unwrap_err();
    assert!(error.contains("1 channels"), "{error}");
    assert_eq!(guest.recv(Some(WAIT)), Err(TransportError::Closed));
    assert_eq!(*device.mic_opened.lock().unwrap(), 0);
}

#[test]
fn fr26_recorded_sound_the_guest_does_not_take_is_dropped_not_waited_on() {
    let device = FakeDevice::default();
    let (handle, mut guest) = serve(device.clone());
    open_input(guest.as_mut());
    // The capture device's thread never waits, however much it records.
    let started = std::time::Instant::now();
    for _ in 0..1000 {
        assert!(device.speak(480, 1));
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    guest.close();
    let end = handle.join().unwrap().unwrap();
    let accounted = end.stats.received_frames + end.stats.dropped_frames;
    assert!(accounted > 0 && accounted <= 480_000, "{accounted}");
}

/// A device that plays each output as its app, at the app's volume, as a host's per-app
/// mixer does: it reads what the output plays from the [`AppVolumes`] the platform follows.
#[derive(Clone)]
struct MixerDevice {
    volumes: Arc<AppVolumes>,
    /// Each output it was asked to play, as the app and volume it opened its session with.
    sessions: Arc<Mutex<Vec<(AppStream, SharedBuffer)>>>,
}

impl AudioDevice for MixerDevice {
    fn play(&self, name: &str, source: SharedBuffer) -> Result<Box<dyn Playback>, String> {
        let (app, _) = self.volumes.app_of(name);
        self.sessions.lock().unwrap().push((app, source));
        Ok(Box::new(FakePlayback(Arc::new(Mutex::new(0)))))
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn fr26_volume_is_controlled_on_the_host_only() {
    // The service, with the caller's Android, and the platform's player following it.
    let (listener, connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(Box::new(listener), androids.clone());
    let volumes = AppVolumes::new();
    let guest_changes: Arc<Mutex<Vec<AppStream>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let follower = {
        let (volumes, stop, guest_changes) = (
            Arc::clone(&volumes),
            Arc::clone(&stop),
            Arc::clone(&guest_changes),
        );
        let mut connector = connector.clone();
        thread::spawn(move || {
            app_audio::follow(
                &mut connector,
                "dsa-wm test",
                &volumes,
                &stop,
                &|_: &str| {},
                &|changed: &[AppStream]| guest_changes.lock().unwrap().extend_from_slice(changed),
            )
        })
    };
    let mut client = {
        let mut connector = connector.clone();
        HostApiClient::connect(&mut connector, "test client", WAIT).unwrap()
    };
    client.start().unwrap();
    let android = androids.of("test").expect("the caller's Android");
    // The agent says the app plays on "app 0" at Android's volume of 40%.
    android.list_app_audio(vec![
        sample_app_stream(app_audio::PRIMARY_STREAM, "", 1.0),
        sample_app_stream("app 0", "com.example.music", 0.4),
    ]);
    wait_until("the platform heard which app plays on app 0", || {
        volumes.app_of("app 0").0.package_name == "com.example.music"
    });

    // The guest's sound reaches the host as the app made it, at full scale: the host neither
    // applies Android's volume nor any of its own to the samples it queues.
    let device = MixerDevice {
        volumes: Arc::clone(&volumes),
        sessions: Arc::default(),
    };
    let (mut listener, mut audio) = memory::pair();
    let mut guest = audio.connect(WAIT).unwrap();
    let mut host = listener.accept(WAIT).unwrap();
    let handle = {
        let device = device.clone();
        thread::spawn(move || serve_stream(host.as_mut(), &device, "dsa-wm test", &|_: &str| {}))
    };
    send(guest.as_mut(), Body::Hello(guest_hello()));
    assert!(matches!(recv(guest.as_mut()), Some(Body::HelloAccepted(_))));
    send(
        guest.as_mut(),
        Body::AudioOutputOpen(AudioOutputOpen {
            name: "app 0".into(),
            ..open_message()
        }),
    );
    assert!(matches!(
        recv(guest.as_mut()),
        Some(Body::AudioOutputReady(_))
    ));
    send(guest.as_mut(), control(AudioOutputCommand::Start));
    for burst in 0..3u64 {
        send(
            guest.as_mut(),
            Body::AudioPcm(AudioPcm {
                position: burst * 960,
                data: pcm(960, 8000),
            }),
        );
    }
    wait_until("the stream played", || {
        !device.sessions.lock().unwrap().is_empty()
    });
    wait_until("the sound was queued", || {
        device.sessions.lock().unwrap()[0]
            .1
            .lock()
            .buffered_frames()
            == 2880
    });
    {
        let sessions = device.sessions.lock().unwrap();
        let (app, source) = &sessions[0];
        // The output plays as its app, at the volume Android gives the app, in the host's mixer.
        assert_eq!(app.package_name, "com.example.music");
        assert_eq!(app.app_label, "Music");
        assert!((app.volume - 0.4).abs() < 1e-6);
        let mut queued = source.lock();
        let mut out = vec![0i16; 2880 * 2];
        queued.pull(&mut out);
        assert!(
            out.chunks_exact(2).all(|frame| frame == [8000, -8000]),
            "every sample reached the host as the app made it"
        );
        assert_eq!(queued.stats().peak, 8000);
    }

    // The app sets its volume (or a volume key is pressed): its session's volume follows.
    android.list_app_audio(vec![
        sample_app_stream(app_audio::PRIMARY_STREAM, "", 1.0),
        sample_app_stream("app 0", "com.example.music", 0.7),
    ]);
    wait_until("the app's session took Android's new volume", || {
        (volumes.app_of("app 0").0.volume - 0.7).abs() < 1e-6
    });
    {
        let changed = guest_changes.lock().unwrap();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].stream, "app 0");
    }

    // The user sets the app's volume in the host's mixer: it goes back to Android, once.
    assert!(volumes.host_changed("app 0", 0.25, false));
    assert!(!volumes.host_changed("app 0", 0.25, false));
    wait_until("the host's volume reached Android", || {
        !android.app_volumes.lock().unwrap().is_empty()
    });
    assert_eq!(
        *android.app_volumes.lock().unwrap(),
        vec![VolumeChange {
            package_name: "com.example.music".into(),
            volume: 0.25,
            muted: false,
        }]
    );
    // And a mute, too.
    assert!(volumes.host_changed("app 0", 0.25, true));
    wait_until("the host's mute reached Android", || {
        android.app_volumes.lock().unwrap().len() == 2
    });
    assert!(android.app_volumes.lock().unwrap()[1].muted);

    guest.close();
    let end = handle.join().unwrap().unwrap();
    assert_eq!(end.stats.peak, 8000);
    stop.store(true, Ordering::Relaxed);
    follower.join().unwrap();
}

#[test]
fn fr26_the_peak_is_the_largest_sample_the_guest_sent() {
    let mut buffer = JitterBuffer::new(1, 100);
    buffer.push(0, &pcm(2, 100));
    buffer.push(2, &pcm(2, -32767));
    buffer.push(4, &pcm(2, 5));
    assert_eq!(buffer.stats().peak, 32767);
}
