//! The guest's sound on the user's default output device, through WASAPI. The guest's
//! microphone, the default capture device, is in [`super::microphone`].
//!
//! The window manager runs in the user's session, so the sound plays where the user hears
//! it. Each stream of the audio port gets a shared-mode client of the default output device
//! in the guest's own format (48 kHz, stereo, 16-bit); Windows converts to the device's mix
//! format and mixes it with every other program. A thread per stream fills the device's
//! buffer each time the device asks, from the stream's jitter buffer, and never waits on
//! the guest. When the user picks another default device, or the device goes away, the
//! stream moves to the new default within a second.
//!
//! Windows' volume mixer is the only volume the guest's sound has: the guest sends every
//! output at full scale. Each output's client is in an audio session of the app it plays
//! ([`AppVolumes`]). The session's identity comes from the app's package, so the mixer keeps
//! one line for the app across runs, and the line has the app's name and the icon of the app's
//! own window program. The session plays at the volume Android gives the app, and takes a new
//! one when the app sets its volume or a volume key is pressed in Android. A volume or a mute
//! the user sets on the line is heard through the session's events and goes back to Android.
//! The guest's shared output, which every sound of an app without an output of its own plays
//! on, is DSA's line.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::app_audio::{session_id, AppStream, AppVolumes};
use dsa_host_core::audio::{
    AudioDevice, CaptureSink, DeviceFormat, Playback, Recording, Renderer, SampleType,
    SharedBuffer, CHANNELS, SAMPLE_RATE,
};
use windows::core::{implement, BOOL, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionDisconnectReason, AudioSessionState, IAudioClient,
    IAudioRenderClient, IAudioSessionControl, IAudioSessionEvents, IAudioSessionEvents_Impl,
    IMMDevice, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
    WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// The guest's format, which the client asks Windows to convert from.
pub(super) const FORMAT: DeviceFormat = DeviceFormat {
    sample_rate: SAMPLE_RATE,
    channels: CHANNELS,
    sample: SampleType::I16,
};

/// The device buffer each client asks for: 20 ms, in 100 ns units.
const BUFFER_DURATION: i64 = 20 * 10_000;

/// How often the stream checks which device is the default.
const DEFAULT_CHECK: Duration = Duration::from_secs(1);

/// How long the stream waits before it opens a device again after one failed.
const RETRY: Duration = Duration::from_millis(500);

/// How often the stream looks at which app its output plays and how loud.
const APP_CHECK: Duration = Duration::from_millis(50);

/// The context the window manager's own changes of a session carry, so the session's events
/// tell them apart from the user's.
const OWN_CHANGE: GUID = GUID::from_u128(0x3c1f5a2e_8d47_4b6a_9e0f_d5a17c2b6e94);

/// The user's default output device, and their default capture device for the guest's
/// microphone.
pub struct DefaultOutput {
    log: Arc<dyn Fn(&str) + Send + Sync>,
    volumes: Arc<AppVolumes>,
}

impl DefaultOutput {
    /// Plays on the default output device, each output as the app `volumes` says it plays;
    /// `log` takes lines for the window manager's log.
    pub fn new(log: Arc<dyn Fn(&str) + Send + Sync>, volumes: Arc<AppVolumes>) -> DefaultOutput {
        DefaultOutput { log, volumes }
    }
}

struct WasapiPlayback {
    stop: Arc<AtomicBool>,
    device_frames: Arc<AtomicU32>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Playback for WasapiPlayback {
    fn device_frames(&self) -> u32 {
        self.device_frames.load(Ordering::Relaxed)
    }
}

impl Drop for WasapiPlayback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl AudioDevice for DefaultOutput {
    fn play(&self, name: &str, source: SharedBuffer) -> Result<Box<dyn Playback>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let device_frames = Arc::new(AtomicU32::new(0));
        let (opened_tx, opened_rx) = mpsc::channel();
        let thread = {
            let stop = Arc::clone(&stop);
            let device_frames = Arc::clone(&device_frames);
            let log = Arc::clone(&self.log);
            let volumes = Arc::clone(&self.volumes);
            let name = name.to_string();
            thread::Builder::new()
                .name("dsa-audio-render".into())
                .spawn(move || {
                    let stream = Stream {
                        name: &name,
                        volumes: &volumes,
                        log: &*log,
                    };
                    render(&stream, source, &stop, &device_frames, opened_tx)
                })
                .map_err(|e| format!("no thread to play on: {e}"))?
        };
        let playback = WasapiPlayback {
            stop,
            device_frames,
            thread: Some(thread),
        };
        // The first device either opens or says why not. Later failures are the thread's to
        // recover from.
        match opened_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Box::new(playback)),
            Ok(Err(why)) => Err(why),
            Err(_) => Err("the default output device did not open in time".into()),
        }
    }

    fn record(&self, name: &str, sink: CaptureSink) -> Result<Box<dyn Recording>, String> {
        super::microphone::record(name, sink, Arc::clone(&self.log))
    }
}

/// COM on this thread, for as long as it lives.
pub(super) struct Com;

impl Com {
    pub(super) fn enter() -> Result<Com, String> {
        // SAFETY: initialises COM for this thread; undone on drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| format!("COM did not start: {e}"))?;
        Ok(Com)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx of this thread.
        unsafe { CoUninitialize() };
    }
}

/// An event the device sets each time it wants more sound.
pub(super) struct Event(pub(super) windows_sys::Win32::Foundation::HANDLE);

impl Event {
    pub(super) fn new() -> Result<Event, String> {
        // SAFETY: an unnamed auto-reset event with default security.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if handle.is_null() {
            return Err(format!(
                "no event for the device: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Event(handle))
    }

    /// Whether the event was set within `timeout`.
    pub(super) fn wait(&self, timeout: Duration) -> bool {
        // SAFETY: a valid event handle this struct owns.
        unsafe { WaitForSingleObject(self.0, timeout.as_millis() as u32) == WAIT_OBJECT_0 }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle CreateEventW returned, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The audio session an output plays in, as its app: its name and icon in the volume mixer,
/// its volume, and the events that tell of the user's changes.
struct Session {
    control: IAudioSessionControl,
    volume: ISimpleAudioVolume,
    events: IAudioSessionEvents,
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the events this session registered, on this thread's COM.
        let _ = unsafe {
            self.control
                .UnregisterAudioSessionNotification(&self.events)
        };
    }
}

impl Session {
    /// Sets the session's volume and mute to what Android gives the app.
    fn apply(&self, app: &AppStream) {
        // SAFETY: the session's volume, on this thread's COM, with the window manager's context.
        unsafe {
            let _ = self.volume.SetMasterVolume(app.volume, &OWN_CHANGE);
            let _ = self.volume.SetMute(app.muted, &OWN_CHANGE);
        }
    }
}

/// Hears the user's changes of a session's volume and mute in the volume mixer.
#[implement(IAudioSessionEvents)]
struct SessionEvents {
    stream: String,
    volumes: Arc<AppVolumes>,
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnSimpleVolumeChanged(
        &self,
        volume: f32,
        muted: BOOL,
        context: *const GUID,
    ) -> windows::core::Result<()> {
        // SAFETY: Windows passes the change's context, or null for none.
        let own = !context.is_null() && unsafe { *context } == OWN_CHANGE;
        if !own {
            self.volumes
                .host_changed(&self.stream, volume, muted.as_bool());
        }
        Ok(())
    }

    fn OnChannelVolumeChanged(
        &self,
        _: u32,
        _: *const f32,
        _: u32,
        _: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnStateChanged(&self, _: AudioSessionState) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The session identity of `app`: one for each app, and one for DSA's shared output.
fn session_guid(app: &AppStream) -> GUID {
    GUID::from_u128(u128::from_be_bytes(session_id(&app.package_name)))
}

/// The icon the volume mixer shows for `app`: the icon of the app's own window program, as
/// Windows names a resource of a file (`path,-id`), else this program's own.
fn icon_path(app: &AppStream) -> String {
    let own = std::env::current_exe()
        .map(|path| format!("{},0", path.display()))
        .unwrap_or_default();
    if app.is_shared() {
        return own;
    }
    let program: Option<PathBuf> = crate::app_exe::default_apps_dir()
        .map(|dir| crate::app_exe::program_path(&dir, &app.package_name, &app.app_label));
    match program {
        Some(path) if path.is_file() => {
            format!("{},-{}", path.display(), dsa_exe_resources::ICON_GROUP_ID)
        }
        _ => own,
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Gives `client`'s session the app's name, icon and volume, and hears the user's changes.
fn open_session(
    client: &IAudioClient,
    app: &AppStream,
    stream: &Stream,
) -> Result<Session, String> {
    // SAFETY: an initialised client of this thread's COM.
    let control: IAudioSessionControl =
        unsafe { client.GetService() }.map_err(|e| format!("the session has no controls: {e}"))?;
    // SAFETY: an initialised client of this thread's COM.
    let volume: ISimpleAudioVolume =
        unsafe { client.GetService() }.map_err(|e| format!("the session has no volume: {e}"))?;
    let name = wide(app.display_name());
    let icon = wide(&icon_path(app));
    // SAFETY: NUL-terminated strings that outlive the calls, and the window manager's context.
    unsafe {
        let _ = control.SetDisplayName(PCWSTR(name.as_ptr()), &OWN_CHANGE);
        let _ = control.SetIconPath(PCWSTR(icon.as_ptr()), &OWN_CHANGE);
    }
    let events: IAudioSessionEvents = SessionEvents {
        stream: stream.name.to_string(),
        volumes: Arc::clone(stream.volumes),
    }
    .into();
    // SAFETY: events that live as long as the session, which unregisters them when it goes.
    unsafe { control.RegisterAudioSessionNotification(&events) }
        .map_err(|e| format!("the session tells nothing of its volume: {e}"))?;
    let session = Session {
        control,
        volume,
        events,
    };
    session.apply(app);
    Ok(session)
}

/// One shared-mode client of one device, started.
struct Client {
    id: String,
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
    event: Event,
    /// What the client plays as, and the revision of it the session has.
    app: AppStream,
    revision: u64,
    /// The app's session, when Windows gave its controls.
    session: Option<Session>,
}

impl Drop for Client {
    fn drop(&mut self) {
        // The session lets go of its events before the client stops.
        self.session = None;
        // SAFETY: a started client of this thread's COM.
        let _ = unsafe { self.client.Stop() };
    }
}

pub(super) fn device_id(device: &IMMDevice) -> Result<String, String> {
    // SAFETY: a device of this thread's COM; the string is freed below.
    let id: PWSTR = unsafe { device.GetId() }.map_err(|e| format!("the device has no ID: {e}"))?;
    // SAFETY: GetId returned a valid NUL-terminated string.
    let text = unsafe { id.to_string() }.unwrap_or_default();
    // SAFETY: GetId's string, allocated with the COM allocator.
    unsafe { CoTaskMemFree(Some(id.0 as *const _)) };
    Ok(text)
}

fn default_device(enumerator: &IMMDeviceEnumerator) -> Result<IMMDevice, String> {
    // SAFETY: an enumerator of this thread's COM.
    unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        .map_err(|e| format!("there is no default output device: {e}"))
}

/// What a render thread plays: the output's name, where it learns which app that is, and
/// where it logs.
struct Stream<'a> {
    name: &'a str,
    volumes: &'a Arc<AppVolumes>,
    log: &'a (dyn Fn(&str) + Send + Sync),
}

fn open(enumerator: &IMMDeviceEnumerator, stream: &Stream) -> Result<(Client, u32), String> {
    let (app, revision) = stream.volumes.app_of(stream.name);
    let device = default_device(enumerator)?;
    let id = device_id(&device)?;
    // SAFETY: activating the device's audio client on this thread's COM.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| format!("the default output device has no audio client: {e}"))?;
    let block_align = FORMAT.bytes_per_frame() as u16;
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: FORMAT.channels,
        nSamplesPerSec: FORMAT.sample_rate,
        nAvgBytesPerSec: FORMAT.sample_rate * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    // The app's own session: every output that plays the app shares its line in the mixer.
    let session_guid = session_guid(&app);
    // SAFETY: a valid format; Windows converts it to the device's mix format. The session's
    // identity outlives the call.
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            BUFFER_DURATION,
            0,
            &format,
            Some(&session_guid as *const GUID),
        )
    }
    .map_err(|e| format!("the default output device refused the guest's format: {e}"))?;
    let event = Event::new()?;
    // SAFETY: the event outlives the client, which this struct drops first.
    unsafe { client.SetEventHandle(HANDLE(event.0)) }
        .map_err(|e| format!("the device takes no event: {e}"))?;
    // SAFETY: an initialised client.
    let buffer_frames =
        unsafe { client.GetBufferSize() }.map_err(|e| format!("the device has no buffer: {e}"))?;
    // SAFETY: an initialised client.
    let render: IAudioRenderClient =
        unsafe { client.GetService() }.map_err(|e| format!("the device takes no sound: {e}"))?;
    // The device's own delay, at the guest's rate (the stream's rate before conversion).
    // SAFETY: an initialised client.
    let latency = unsafe { client.GetStreamLatency() }.unwrap_or(0).max(0) as u64;
    let device_frames =
        (latency * u64::from(SAMPLE_RATE) / 10_000_000) as u32 + buffer_frames.min(SAMPLE_RATE);
    // The session is set before the first sound, so nothing plays at another volume.
    let session = match open_session(&client, &app, stream) {
        Ok(session) => Some(session),
        Err(why) => {
            (stream.log)(&format!(
                "audio stream {}: plays without its app's line in the volume mixer: {why}",
                stream.name
            ));
            None
        }
    };
    // SAFETY: an initialised client.
    unsafe { client.Start() }.map_err(|e| format!("the device did not start: {e}"))?;
    Ok((
        Client {
            id,
            client,
            render,
            buffer_frames,
            event,
            app,
            revision,
            session,
        },
        device_frames,
    ))
}

/// Fills what the device has room for. Returns false when the device went away.
fn fill(client: &Client, renderer: &mut Renderer) -> bool {
    // SAFETY: a started client.
    let padding = match unsafe { client.client.GetCurrentPadding() } {
        Ok(padding) => padding,
        Err(_) => return false,
    };
    let frames = client.buffer_frames.saturating_sub(padding);
    if frames == 0 {
        return true;
    }
    // SAFETY: the device's buffer has room for `frames` frames of the client's format.
    let data = match unsafe { client.render.GetBuffer(frames) } {
        Ok(data) => data,
        Err(_) => return false,
    };
    let bytes = frames as usize * FORMAT.bytes_per_frame();
    // SAFETY: GetBuffer returned `bytes` writable bytes, released below.
    let out = unsafe { std::slice::from_raw_parts_mut(data, bytes) };
    renderer.render(out);
    // SAFETY: releases the frames GetBuffer gave.
    unsafe { client.render.ReleaseBuffer(frames, 0) }.is_ok()
}

/// Follows what the output plays: a new volume goes to the session, and another app on the
/// output needs a client in that app's session. False when the client must be opened again.
fn follow_app(client: &mut Client, stream: &Stream) -> bool {
    let (app, revision) = stream.volumes.app_of(stream.name);
    if revision == client.revision {
        return true;
    }
    if !app.same_app(&client.app) {
        (stream.log)(&format!(
            "audio stream {} now plays {}",
            stream.name,
            app.display_name()
        ));
        return false;
    }
    if let Some(session) = &client.session {
        session.apply(&app);
    }
    client.app = app;
    client.revision = revision;
    true
}

fn render(
    stream: &Stream,
    source: SharedBuffer,
    stop: &AtomicBool,
    device_frames: &AtomicU32,
    opened: mpsc::Sender<Result<(), String>>,
) {
    let (name, log) = (stream.name, stream.log);
    let mut opened = Some(opened);
    let report = |opened: &mut Option<mpsc::Sender<Result<(), String>>>, result| {
        if let Some(tx) = opened.take() {
            let _ = tx.send(result);
        }
    };
    let _com = match Com::enter() {
        Ok(com) => com,
        Err(e) => return report(&mut opened, Err(e)),
    };
    // SAFETY: COM is entered on this thread.
    let enumerator: IMMDeviceEnumerator =
        match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
            Ok(enumerator) => enumerator,
            Err(e) => {
                return report(
                    &mut opened,
                    Err(format!("the audio devices cannot be listed: {e}")),
                )
            }
        };
    let mut renderer = match Renderer::new(FORMAT, source) {
        Ok(renderer) => renderer,
        Err(e) => return report(&mut opened, Err(e)),
    };
    while !stop.load(Ordering::Relaxed) {
        let mut client = match open(&enumerator, stream) {
            Ok((client, frames)) => {
                device_frames.store(frames, Ordering::Relaxed);
                report(&mut opened, Ok(()));
                log(&format!(
                    "audio stream {name} plays as {} on {} ({} frames of device buffer)",
                    client.app.display_name(),
                    client.id,
                    client.buffer_frames
                ));
                client
            }
            Err(e) => {
                if opened.is_some() {
                    return report(&mut opened, Err(e));
                }
                log(&format!("audio stream {name}: {e}"));
                thread::sleep(RETRY);
                continue;
            }
        };
        let mut checked = Instant::now();
        let mut app_checked = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            client.event.wait(Duration::from_millis(100));
            if !fill(&client, &mut renderer) {
                log(&format!(
                    "audio stream {name}: the output device went away; moving to the default"
                ));
                break;
            }
            if app_checked.elapsed() >= APP_CHECK {
                app_checked = Instant::now();
                if !follow_app(&mut client, stream) {
                    break;
                }
            }
            if checked.elapsed() >= DEFAULT_CHECK {
                checked = Instant::now();
                let moved = default_device(&enumerator)
                    .and_then(|d| device_id(&d))
                    .map(|id| id != client.id)
                    .unwrap_or(false);
                if moved {
                    log(&format!(
                        "audio stream {name}: the default output device changed"
                    ));
                    break;
                }
            }
        }
        drop(client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr26_each_app_has_a_session_of_its_own_and_dsa_one_for_the_shared_output() {
        let music = AppStream {
            stream: "app 0".into(),
            package_name: "com.example.music".into(),
            app_label: "Music".into(),
            volume: 1.0,
            muted: false,
        };
        let shared = AppStream::unknown("primary");
        assert_eq!(session_guid(&music), session_guid(&music.clone()));
        assert_ne!(session_guid(&music), session_guid(&shared));
        assert_ne!(session_guid(&music), GUID::from_u128(0));
        // The shared output shows this program's own icon.
        assert!(icon_path(&shared).ends_with(",0"));
    }
}
