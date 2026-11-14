//! The guest's output streams through PulseAudio's asynchronous API, which the simple API
//! cannot do: each stream plays as its app, with the app's name and icon in the desktop's
//! sound mixer (`application.name`, `application.icon_name`), at the volume Android gives the
//! app, and hears the user's own changes of its volume and mute there.
//!
//! The guest sends every output at full scale, so the stream's volume in the mixer is the
//! only volume its sound has. A new volume from Android goes to the stream
//! (`pa_context_set_sink_input_volume`); a change the user makes in the mixer arrives as a
//! change of the stream's sink input, whose volume and mute go back to Android through
//! [`AppVolumes::host_changed`]. The stream's own change comes back the same way, as the value
//! Android already has, and goes no further.
//!
//! Each stream has its own threaded main loop, which runs the callbacks, and the stream's own
//! thread renders and writes the sound under the loop's lock. The library is loaded when the
//! first stream opens (`libpulse.so.0`), so a computer without it still runs DSA, without
//! sound.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_host_core::app_audio::{AppStream, AppVolumes};
use dsa_host_core::audio::{Playback, Renderer};

use super::audio::{BufferAttr, SampleSpec, CHANNELS, CHUNK_FRAMES, FRAME_BYTES, RATE};

/// `PA_VOLUME_NORM`: a stream's full volume, 100% in the mixer.
const VOLUME_NORM: u32 = 0x10000;
/// `PA_CHANNELS_MAX`.
const CHANNELS_MAX: usize = 32;
/// `PA_CONTEXT_READY`, `PA_CONTEXT_FAILED`, `PA_CONTEXT_TERMINATED`.
const CONTEXT_READY: c_int = 4;
const CONTEXT_FAILED: c_int = 5;
const CONTEXT_TERMINATED: c_int = 6;
/// `PA_STREAM_READY`, `PA_STREAM_FAILED`, `PA_STREAM_TERMINATED`.
const STREAM_READY: c_int = 2;
const STREAM_FAILED: c_int = 3;
const STREAM_TERMINATED: c_int = 4;
/// `PA_STREAM_INTERPOLATE_TIMING | PA_STREAM_AUTO_TIMING_UPDATE | PA_STREAM_ADJUST_LATENCY`.
const STREAM_FLAGS: c_int = 0x0002 | 0x0008 | 0x2000;
/// `PA_SUBSCRIPTION_MASK_SINK_INPUT`.
const MASK_SINK_INPUT: c_int = 0x0004;
/// `PA_SUBSCRIPTION_EVENT_FACILITY_MASK`, `PA_SUBSCRIPTION_EVENT_SINK_INPUT`.
const FACILITY_MASK: u32 = 0x000f;
const FACILITY_SINK_INPUT: u32 = 0x0002;
/// `PA_SUBSCRIPTION_EVENT_TYPE_MASK`, `PA_SUBSCRIPTION_EVENT_CHANGE`.
const TYPE_MASK: u32 = 0x0030;
const TYPE_CHANGE: u32 = 0x0010;
/// `PA_SEEK_RELATIVE`.
const SEEK_RELATIVE: c_int = 0;
/// `PA_INVALID_INDEX`.
const NO_INDEX: u32 = u32::MAX;

/// How long a stream waits for the sound server to take it.
const CONNECT_WAIT: Duration = Duration::from_secs(5);
/// How long the stream's thread sleeps while the server has no room.
const ROOM_WAIT: Duration = Duration::from_millis(5);
/// How often the stream looks at which app its output plays and how loud.
const APP_CHECK: Duration = Duration::from_millis(50);
/// How long the stream waits before it opens again after the server dropped it.
const RETRY: Duration = Duration::from_secs(1);

/// `pa_cvolume`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct CVolume {
    channels: u8,
    values: [u32; CHANNELS_MAX],
}

/// `pa_channel_map`. Only its size matters here: it places the fields after it.
#[repr(C)]
#[allow(dead_code)]
struct ChannelMap {
    channels: u8,
    map: [c_int; CHANNELS_MAX],
}

/// `pa_sink_input_info`, up to the fields read here and those that place them. The server
/// fills it; only the volume and the mute are read.
#[repr(C)]
#[allow(dead_code)]
struct SinkInputInfo {
    index: u32,
    name: *const c_char,
    owner_module: u32,
    client: u32,
    sink: u32,
    sample_spec: SampleSpec,
    channel_map: ChannelMap,
    volume: CVolume,
    buffer_usec: u64,
    sink_usec: u64,
    resample_method: *const c_char,
    driver: *const c_char,
    mute: c_int,
}

type SuccessFn = extern "C" fn(*mut c_void, c_int, *mut c_void);
type SubscribeFn = extern "C" fn(*mut c_void, u32, u32, *mut c_void);
type SinkInputInfoFn = extern "C" fn(*mut c_void, *const SinkInputInfo, c_int, *mut c_void);

/// The functions of the asynchronous API this uses.
struct Async {
    mainloop_new: unsafe extern "C" fn() -> *mut c_void,
    mainloop_start: unsafe extern "C" fn(*mut c_void) -> c_int,
    mainloop_stop: unsafe extern "C" fn(*mut c_void),
    mainloop_free: unsafe extern "C" fn(*mut c_void),
    mainloop_lock: unsafe extern "C" fn(*mut c_void),
    mainloop_unlock: unsafe extern "C" fn(*mut c_void),
    mainloop_api: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    proplist_new: unsafe extern "C" fn() -> *mut c_void,
    proplist_sets: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    proplist_free: unsafe extern "C" fn(*mut c_void),
    context_new: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void,
    context_connect:
        unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_void) -> c_int,
    context_state: unsafe extern "C" fn(*mut c_void) -> c_int,
    context_errno: unsafe extern "C" fn(*mut c_void) -> c_int,
    context_disconnect: unsafe extern "C" fn(*mut c_void),
    context_unref: unsafe extern "C" fn(*mut c_void),
    set_subscribe_callback: unsafe extern "C" fn(*mut c_void, Option<SubscribeFn>, *mut c_void),
    subscribe:
        unsafe extern "C" fn(*mut c_void, c_int, Option<SuccessFn>, *mut c_void) -> *mut c_void,
    sink_input_info:
        unsafe extern "C" fn(*mut c_void, u32, Option<SinkInputInfoFn>, *mut c_void) -> *mut c_void,
    set_sink_input_volume: unsafe extern "C" fn(
        *mut c_void,
        u32,
        *const CVolume,
        Option<SuccessFn>,
        *mut c_void,
    ) -> *mut c_void,
    set_sink_input_mute: unsafe extern "C" fn(
        *mut c_void,
        u32,
        c_int,
        Option<SuccessFn>,
        *mut c_void,
    ) -> *mut c_void,
    operation_unref: unsafe extern "C" fn(*mut c_void),
    stream_new: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *const SampleSpec,
        *const c_void,
        *mut c_void,
    ) -> *mut c_void,
    stream_connect_playback: unsafe extern "C" fn(
        *mut c_void,
        *const c_char,
        *const BufferAttr,
        c_int,
        *const CVolume,
        *mut c_void,
    ) -> c_int,
    stream_state: unsafe extern "C" fn(*mut c_void) -> c_int,
    stream_writable_size: unsafe extern "C" fn(*mut c_void) -> usize,
    stream_write: unsafe extern "C" fn(
        *mut c_void,
        *const c_void,
        usize,
        Option<extern "C" fn(*mut c_void)>,
        i64,
        c_int,
    ) -> c_int,
    stream_index: unsafe extern "C" fn(*mut c_void) -> u32,
    stream_latency: unsafe extern "C" fn(*mut c_void, *mut u64, *mut c_int) -> c_int,
    stream_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
    stream_unref: unsafe extern "C" fn(*mut c_void),
    strerror: unsafe extern "C" fn(c_int) -> *const c_char,
}

/// Looks up `name` in the library `handle`.
///
/// # Safety
///
/// `T` must be the function pointer type of the symbol.
unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &CStr) -> Result<T, String> {
    // SAFETY: a library handle dlopen returned and a NUL-terminated name.
    let address = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if address.is_null() {
        return Err(format!(
            "libpulse.so.0 lacks {}, which DSA uses",
            name.to_string_lossy()
        ));
    }
    // SAFETY: the caller names the symbol's type; a pointer is the size of a function pointer.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&address) })
}

/// The asynchronous API, loaded once.
fn api() -> Result<&'static Async, String> {
    static API: OnceLock<Result<Async, String>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: a NUL-terminated library name. The library stays loaded for the process.
        let lib =
            unsafe { libc::dlopen(c"libpulse.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if lib.is_null() {
            return Err(
                "this computer has no PulseAudio client library (libpulse.so.0); install \
                 PipeWire's PulseAudio server or PulseAudio"
                    .into(),
            );
        }
        // SAFETY: each symbol is looked up with the type the API declares for it.
        unsafe {
            Ok(Async {
                mainloop_new: symbol(lib, c"pa_threaded_mainloop_new")?,
                mainloop_start: symbol(lib, c"pa_threaded_mainloop_start")?,
                mainloop_stop: symbol(lib, c"pa_threaded_mainloop_stop")?,
                mainloop_free: symbol(lib, c"pa_threaded_mainloop_free")?,
                mainloop_lock: symbol(lib, c"pa_threaded_mainloop_lock")?,
                mainloop_unlock: symbol(lib, c"pa_threaded_mainloop_unlock")?,
                mainloop_api: symbol(lib, c"pa_threaded_mainloop_get_api")?,
                proplist_new: symbol(lib, c"pa_proplist_new")?,
                proplist_sets: symbol(lib, c"pa_proplist_sets")?,
                proplist_free: symbol(lib, c"pa_proplist_free")?,
                context_new: symbol(lib, c"pa_context_new_with_proplist")?,
                context_connect: symbol(lib, c"pa_context_connect")?,
                context_state: symbol(lib, c"pa_context_get_state")?,
                context_errno: symbol(lib, c"pa_context_errno")?,
                context_disconnect: symbol(lib, c"pa_context_disconnect")?,
                context_unref: symbol(lib, c"pa_context_unref")?,
                set_subscribe_callback: symbol(lib, c"pa_context_set_subscribe_callback")?,
                subscribe: symbol(lib, c"pa_context_subscribe")?,
                sink_input_info: symbol(lib, c"pa_context_get_sink_input_info")?,
                set_sink_input_volume: symbol(lib, c"pa_context_set_sink_input_volume")?,
                set_sink_input_mute: symbol(lib, c"pa_context_set_sink_input_mute")?,
                operation_unref: symbol(lib, c"pa_operation_unref")?,
                stream_new: symbol(lib, c"pa_stream_new_with_proplist")?,
                stream_connect_playback: symbol(lib, c"pa_stream_connect_playback")?,
                stream_state: symbol(lib, c"pa_stream_get_state")?,
                stream_writable_size: symbol(lib, c"pa_stream_writable_size")?,
                stream_write: symbol(lib, c"pa_stream_write")?,
                stream_index: symbol(lib, c"pa_stream_get_index")?,
                stream_latency: symbol(lib, c"pa_stream_get_latency")?,
                stream_disconnect: symbol(lib, c"pa_stream_disconnect")?,
                stream_unref: symbol(lib, c"pa_stream_unref")?,
                strerror: symbol(lib, c"pa_strerror")?,
            })
        }
    })
    .as_ref()
    .map_err(Clone::clone)
}

impl Async {
    fn describe(&self, error: c_int) -> String {
        // SAFETY: pa_strerror returns a static string or null.
        let text = unsafe { (self.strerror)(error) };
        if text.is_null() {
            return format!("PulseAudio error {error}");
        }
        // SAFETY: a NUL-terminated static string.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }

    /// Drops an operation the caller does not wait for.
    fn forget(&self, operation: *mut c_void) {
        if !operation.is_null() {
            // SAFETY: an operation the API returned, released once.
            unsafe { (self.operation_unref)(operation) };
        }
    }
}

/// `volume` (0 to 1) on both channels, as the mixer's 0% to 100%.
pub(super) fn cvolume(volume: f32) -> CVolume {
    let level = (volume.clamp(0.0, 1.0) * VOLUME_NORM as f32).round() as u32;
    let mut values = [0u32; CHANNELS_MAX];
    values[0] = level;
    values[1] = level;
    CVolume {
        channels: CHANNELS,
        values,
    }
}

/// The mean of the channels' volumes, where the mixer's 100% is 1.
pub(super) fn fraction(volume: &CVolume) -> f32 {
    let channels = usize::from(volume.channels).min(CHANNELS_MAX);
    if channels == 0 {
        return 1.0;
    }
    let sum: u64 = volume.values[..channels]
        .iter()
        .map(|&v| u64::from(v))
        .sum();
    (sum as f64 / channels as f64 / f64::from(VOLUME_NORM)) as f32
}

/// The properties the mixer shows a stream of `app` by.
pub(super) fn properties(
    app: &AppStream,
    icon_theme: Option<&std::path::Path>,
) -> Vec<(&'static str, String)> {
    let (id, icon) = if app.is_shared() {
        (
            crate::root_approval::ICON_NAME.to_string(),
            crate::root_approval::ICON_NAME.to_string(),
        )
    } else {
        (
            crate::shell::desktop_entry::icon_name(&app.package_name),
            crate::root_approval::icon_name_for(&app.package_name, icon_theme),
        )
    };
    vec![
        ("application.name", app.display_name().to_string()),
        ("application.id", id),
        ("application.icon_name", icon),
        ("media.name", app.display_name().to_string()),
    ]
}

fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', " ")).unwrap_or_default()
}

/// What the main loop's callbacks share with the stream's thread. It is boxed so its address
/// stays put, and dropped only after the main loop has stopped.
struct Callbacks {
    api: &'static Async,
    /// The stream's sink input, once the server made it.
    index: AtomicU32,
    stream: String,
    volumes: Arc<AppVolumes>,
}

extern "C" fn subscribed(context: *mut c_void, event: u32, index: u32, user: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the callbacks this context was given, which outlive its main loop.
        let callbacks = unsafe { &*(user as *const Callbacks) };
        if event & FACILITY_MASK != FACILITY_SINK_INPUT
            || event & TYPE_MASK != TYPE_CHANGE
            || index != callbacks.index.load(Ordering::Relaxed)
        {
            return;
        }
        // SAFETY: the context this callback runs for, under its main loop's lock.
        let operation =
            unsafe { (callbacks.api.sink_input_info)(context, index, Some(changed), user) };
        callbacks.api.forget(operation);
    });
}

extern "C" fn changed(_: *mut c_void, info: *const SinkInputInfo, eol: c_int, user: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        if eol != 0 || info.is_null() {
            return;
        }
        // SAFETY: the callbacks this context was given, which outlive its main loop.
        let callbacks = unsafe { &*(user as *const Callbacks) };
        // SAFETY: the server's description of the sink input, valid during the callback.
        let info = unsafe { &*info };
        callbacks
            .volumes
            .host_changed(&callbacks.stream, fraction(&info.volume), info.mute != 0);
    });
}

/// One stream on the server, as one app.
struct Connection {
    api: &'static Async,
    mainloop: *mut c_void,
    context: *mut c_void,
    stream: *mut c_void,
    callbacks: Box<Callbacks>,
}

// SAFETY: the stream's pointers are used only by the stream's own thread, under the main
// loop's lock, and by the main loop's thread through the callbacks.
unsafe impl Send for Connection {}

impl Drop for Connection {
    fn drop(&mut self) {
        let api = self.api;
        if self.mainloop.is_null() {
            return;
        }
        // SAFETY: what open made, released once under the main loop's lock, then the loop.
        unsafe {
            (api.mainloop_lock)(self.mainloop);
            if !self.stream.is_null() {
                (api.stream_disconnect)(self.stream);
                (api.stream_unref)(self.stream);
            }
            if !self.context.is_null() {
                (api.context_disconnect)(self.context);
                (api.context_unref)(self.context);
            }
            (api.mainloop_unlock)(self.mainloop);
            (api.mainloop_stop)(self.mainloop);
            (api.mainloop_free)(self.mainloop);
        }
    }
}

/// Holds the main loop's lock while it lives.
struct Locked<'a>(&'a Connection);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: the lock this guard took.
        unsafe { (self.0.api.mainloop_unlock)(self.0.mainloop) };
    }
}

impl Connection {
    fn lock(&self) -> Locked<'_> {
        // SAFETY: the connection's main loop.
        unsafe { (self.api.mainloop_lock)(self.mainloop) };
        Locked(self)
    }

    /// Opens a stream of `app` on the user's default device at the app's volume.
    fn open(
        name: &str,
        app: &AppStream,
        volumes: &Arc<AppVolumes>,
        icon_theme: Option<&std::path::Path>,
    ) -> Result<Connection, String> {
        let api = api()?;
        // SAFETY: a new main loop, freed by the connection.
        let mainloop = unsafe { (api.mainloop_new)() };
        if mainloop.is_null() {
            return Err("the sound server's main loop could not be made".into());
        }
        let mut connection = Connection {
            api,
            mainloop,
            context: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            callbacks: Box::new(Callbacks {
                api,
                index: AtomicU32::new(NO_INDEX),
                stream: name.to_string(),
                volumes: Arc::clone(volumes),
            }),
        };
        let user = &*connection.callbacks as *const Callbacks as *mut c_void;
        // SAFETY: a new property list, freed below once the context and the stream have it.
        let proplist = unsafe { (api.proplist_new)() };
        if proplist.is_null() {
            return Err("the stream's properties could not be made".into());
        }
        let properties = properties(app, icon_theme);
        for (key, value) in &properties {
            let (key, value) = (c_string(key), c_string(value));
            // SAFETY: a property list and NUL-terminated strings, which it copies.
            unsafe { (api.proplist_sets)(proplist, key.as_ptr(), value.as_ptr()) };
        }
        let opened = connection.connect(app, proplist, user);
        // SAFETY: the property list made above; the context and the stream keep copies.
        unsafe { (api.proplist_free)(proplist) };
        opened.map(|()| connection)
    }

    fn connect(
        &mut self,
        app: &AppStream,
        proplist: *mut c_void,
        user: *mut c_void,
    ) -> Result<(), String> {
        let api = self.api;
        let display = c_string(app.display_name());
        // SAFETY: the main loop's API, a NUL-terminated name and the property list.
        self.context = unsafe {
            (api.context_new)(
                (api.mainloop_api)(self.mainloop),
                display.as_ptr(),
                proplist,
            )
        };
        if self.context.is_null() {
            return Err("the sound server could not be reached".into());
        }
        // SAFETY: the context, and callbacks that outlive its main loop.
        unsafe { (api.set_subscribe_callback)(self.context, Some(subscribed), user) };
        // SAFETY: the default server, with no flags and no spawn API.
        if unsafe { (api.context_connect)(self.context, std::ptr::null(), 0, std::ptr::null()) } < 0
        {
            // SAFETY: the context that failed.
            return Err(api.describe(unsafe { (api.context_errno)(self.context) }));
        }
        // SAFETY: the main loop, which runs until the connection drops.
        if unsafe { (api.mainloop_start)(self.mainloop) } < 0 {
            return Err("the sound server's main loop did not start".into());
        }
        self.wait_for(|connection| {
            // SAFETY: the context, read under the lock.
            match unsafe { (api.context_state)(connection.context) } {
                CONTEXT_READY => Some(Ok(())),
                CONTEXT_FAILED | CONTEXT_TERMINATED => Some(Err(api.describe(unsafe {
                    // SAFETY: the context that failed.
                    (api.context_errno)(connection.context)
                }))),
                _ => None,
            }
        })?;
        let spec = SampleSpec {
            format: super::audio::PA_SAMPLE_S16LE,
            rate: RATE,
            channels: CHANNELS,
        };
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: super::audio::TARGET_BYTES,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        let volume = cvolume(app.volume);
        // SAFETY: the main loop's lock, given back below on every path.
        unsafe { (api.mainloop_lock)(self.mainloop) };
        // SAFETY: the ready context, the guest's format, the default channel map and the
        // property list, under the lock.
        self.stream = unsafe {
            (api.stream_new)(
                self.context,
                display.as_ptr(),
                &spec,
                std::ptr::null(),
                proplist,
            )
        };
        let made = if self.stream.is_null() {
            Err(())
        } else {
            // SAFETY: the new stream on the default device, at the app's volume.
            let connected = unsafe {
                (api.stream_connect_playback)(
                    self.stream,
                    std::ptr::null(),
                    &attr,
                    STREAM_FLAGS,
                    &volume,
                    std::ptr::null_mut(),
                )
            };
            if connected < 0 {
                Err(())
            } else {
                Ok(())
            }
        };
        // SAFETY: the context, read under the lock.
        let error = unsafe { (api.context_errno)(self.context) };
        // SAFETY: the lock taken above.
        unsafe { (api.mainloop_unlock)(self.mainloop) };
        made.map_err(|()| api.describe(error))?;
        self.wait_for(|connection| {
            // SAFETY: the stream, read under the lock.
            match unsafe { (api.stream_state)(connection.stream) } {
                STREAM_READY => Some(Ok(())),
                STREAM_FAILED | STREAM_TERMINATED => Some(Err(api.describe(unsafe {
                    // SAFETY: the context whose stream failed.
                    (api.context_errno)(connection.context)
                }))),
                _ => None,
            }
        })?;
        let _locked = self.lock();
        // SAFETY: the ready stream.
        let index = unsafe { (api.stream_index)(self.stream) };
        self.callbacks.index.store(index, Ordering::Relaxed);
        if app.muted {
            // SAFETY: the stream's sink input, with no callback.
            api.forget(unsafe {
                (api.set_sink_input_mute)(self.context, index, 1, None, std::ptr::null_mut())
            });
        }
        // SAFETY: the context; changes of sink inputs reach the subscribe callback.
        api.forget(unsafe {
            (api.subscribe)(self.context, MASK_SINK_INPUT, None, std::ptr::null_mut())
        });
        Ok(())
    }

    /// Waits, at most [`CONNECT_WAIT`], for `state` under the lock to say how it went.
    fn wait_for(
        &self,
        state: impl Fn(&Connection) -> Option<Result<(), String>>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + CONNECT_WAIT;
        loop {
            let done = {
                let _locked = self.lock();
                state(self)
            };
            if let Some(done) = done {
                return done;
            }
            if Instant::now() >= deadline {
                return Err("the sound server did not answer in time".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Gives the stream the volume and mute Android gives its app.
    fn set_volume(&self, app: &AppStream) {
        let index = self.callbacks.index.load(Ordering::Relaxed);
        if index == NO_INDEX {
            return;
        }
        let volume = cvolume(app.volume);
        let _locked = self.lock();
        // SAFETY: the stream's sink input, with no callbacks.
        unsafe {
            self.api.forget((self.api.set_sink_input_volume)(
                self.context,
                index,
                &volume,
                None,
                std::ptr::null_mut(),
            ));
            self.api.forget((self.api.set_sink_input_mute)(
                self.context,
                index,
                c_int::from(app.muted),
                None,
                std::ptr::null_mut(),
            ));
        }
    }

    /// The bytes the server has room for, or why the stream ended.
    fn writable(&self) -> Result<usize, String> {
        let _locked = self.lock();
        // SAFETY: the stream, under the lock.
        if unsafe { (self.api.stream_state)(self.stream) } != STREAM_READY {
            // SAFETY: the context whose stream ended.
            return Err(self
                .api
                .describe(unsafe { (self.api.context_errno)(self.context) }));
        }
        // SAFETY: the ready stream.
        let room = unsafe { (self.api.stream_writable_size)(self.stream) };
        if room == usize::MAX {
            return Err("the stream has no room it can say".into());
        }
        Ok(room)
    }

    fn write(&self, data: &[u8]) -> Result<(), String> {
        let _locked = self.lock();
        // SAFETY: the ready stream and bytes it copies before the call returns.
        let written = unsafe {
            (self.api.stream_write)(
                self.stream,
                data.as_ptr().cast(),
                data.len(),
                None,
                0,
                SEEK_RELATIVE,
            )
        };
        if written < 0 {
            // SAFETY: the context whose stream refused the sound.
            return Err(self
                .api
                .describe(unsafe { (self.api.context_errno)(self.context) }));
        }
        Ok(())
    }

    /// The sound still to be heard, in frames at the guest's rate.
    fn latency_frames(&self) -> u32 {
        let _locked = self.lock();
        let (mut micros, mut negative) = (0u64, 0 as c_int);
        // SAFETY: the stream and two places for the answer.
        if unsafe { (self.api.stream_latency)(self.stream, &mut micros, &mut negative) } < 0
            || negative != 0
        {
            return 0;
        }
        u32::try_from(micros * u64::from(RATE) / 1_000_000).unwrap_or(u32::MAX)
    }
}

/// A playing output, stopped and waited for when it is dropped.
pub(super) struct PulsePlayback {
    stop: Arc<AtomicBool>,
    delay: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Playback for PulsePlayback {
    fn device_frames(&self) -> u32 {
        self.delay.load(Ordering::Relaxed)
    }
}

impl Drop for PulsePlayback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Plays `renderer`'s sound as the app the output `name` plays, from `volumes`.
pub(super) fn play(
    name: &str,
    mut renderer: Renderer,
    volumes: Arc<AppVolumes>,
    icon_theme: Option<PathBuf>,
) -> Result<PulsePlayback, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let delay = Arc::new(AtomicU32::new(0));
    let (opened_tx, opened_rx) = mpsc::channel();
    let thread = {
        let (stop, delay, name) = (Arc::clone(&stop), Arc::clone(&delay), name.to_string());
        thread::Builder::new()
            .name(format!("dsa-audio-{name}"))
            .spawn(move || {
                let mut opened = Some(opened_tx);
                let mut chunk = vec![0u8; CHUNK_FRAMES * FRAME_BYTES * 4];
                while !stop.load(Ordering::Relaxed) {
                    let (app, mut revision) = volumes.app_of(&name);
                    let connection =
                        match Connection::open(&name, &app, &volumes, icon_theme.as_deref()) {
                            Ok(connection) => connection,
                            Err(why) => {
                                if let Some(tx) = opened.take() {
                                    let _ = tx.send(Err(why));
                                    return;
                                }
                                eprintln!("sound: stream {name}: {why}");
                                thread::sleep(RETRY);
                                continue;
                            }
                        };
                    if let Some(tx) = opened.take() {
                        let _ = tx.send(Ok(()));
                    }
                    eprintln!("sound: stream {name} plays as {}", app.display_name());
                    let mut app_checked = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        if app_checked.elapsed() >= APP_CHECK {
                            app_checked = Instant::now();
                            let (now, now_revision) = volumes.app_of(&name);
                            if now_revision != revision {
                                if !now.same_app(&app) {
                                    eprintln!(
                                        "sound: stream {name} now plays {}",
                                        now.display_name()
                                    );
                                    break;
                                }
                                connection.set_volume(&now);
                                revision = now_revision;
                            }
                        }
                        let room = match connection.writable() {
                            Ok(room) => room,
                            Err(why) => {
                                eprintln!("sound: stream {name} ended: {why}");
                                thread::sleep(RETRY);
                                break;
                            }
                        };
                        let bytes = room.min(chunk.len()) / FRAME_BYTES * FRAME_BYTES;
                        if bytes < CHUNK_FRAMES * FRAME_BYTES {
                            thread::sleep(ROOM_WAIT);
                            continue;
                        }
                        renderer.render(&mut chunk[..bytes]);
                        if let Err(why) = connection.write(&chunk[..bytes]) {
                            eprintln!("sound: stream {name} ended: {why}");
                            thread::sleep(RETRY);
                            break;
                        }
                        delay.store(connection.latency_frames(), Ordering::Relaxed);
                    }
                    drop(connection);
                }
            })
            .map_err(|e| format!("cannot start the stream's thread: {e}"))?
    };
    let playback = PulsePlayback {
        stop,
        delay,
        thread: Some(thread),
    };
    match opened_rx.recv_timeout(CONNECT_WAIT * 3) {
        Ok(Ok(())) => Ok(playback),
        Ok(Err(why)) => Err(why),
        Err(_) => Err("the sound server did not take the stream in time".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr26_the_asynchronous_api_structures_have_the_c_layout() {
        assert_eq!(std::mem::size_of::<CVolume>(), 132);
        assert_eq!(std::mem::size_of::<ChannelMap>(), 132);
        // index, name, owner_module, client, sink, sample_spec, channel_map, volume: the
        // offsets pulse/introspect.h gives them on a 64-bit computer.
        assert_eq!(std::mem::offset_of!(SinkInputInfo, sample_spec), 28);
        assert_eq!(std::mem::offset_of!(SinkInputInfo, volume), 172);
        assert_eq!(std::mem::offset_of!(SinkInputInfo, buffer_usec), 304);
        assert_eq!(std::mem::offset_of!(SinkInputInfo, mute), 336);
    }

    #[test]
    fn fr26_the_apps_volume_is_the_mixers_percentage_and_back() {
        let volume = cvolume(0.5);
        assert_eq!(volume.channels, 2);
        assert_eq!(volume.values[0], VOLUME_NORM / 2);
        assert!((fraction(&volume) - 0.5).abs() < 1e-4);
        assert_eq!(cvolume(3.0).values[1], VOLUME_NORM);
        let mut uneven = cvolume(1.0);
        uneven.values[1] = 0;
        assert!((fraction(&uneven) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn fr26_a_stream_shows_its_apps_name_and_dsa_shows_the_shared_one() {
        let music = AppStream {
            stream: "app 0".into(),
            package_name: "com.example.music".into(),
            app_label: "Music".into(),
            volume: 1.0,
            muted: false,
        };
        let properties = properties(&music, None);
        assert!(properties.contains(&("application.name", "Music".to_string())));
        let shared = properties_of_shared();
        assert!(shared.contains(&(
            "application.name",
            dsa_host_core::app_audio::SHARED_LABEL.to_string()
        )));
    }

    fn properties_of_shared() -> Vec<(&'static str, String)> {
        properties(&AppStream::unknown("primary"), None)
    }
}
