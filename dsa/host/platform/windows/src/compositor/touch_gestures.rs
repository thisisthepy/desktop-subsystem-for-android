//! Windows 11's three- and four-finger touch gestures, turned off while a task window is in
//! front.
//!
//! While those gestures are on (Settings, Bluetooth & devices, Touch), Windows holds every touch
//! contact back until it knows the contact is not one of its gestures, and keeps three or more
//! fingers from every app. No public API turns them off for one app's windows, so, while the
//! user's setting `touch_gestures_off` is on (the default), the window manager turns them off
//! for the user while one of their task windows is the active window, and puts the user's own
//! value back as soon as another window is. Other apps keep the gestures as the user set them.
//!
//! Settings changes them through `SystemParametersInfo` with an action that `winuser.h` does
//! not name: win32k keeps its per-user settings in one table, in the order of their actions,
//! and `TouchGestureSetting` sits where the pair `0x2030` (get) and `0x2031` (set) falls. The
//! Installer test workflow checks that pair on Windows 11 (build 26200 read 1 before and 0
//! after, and ten fingers then reached a task). Because the action is undocumented, a
//! Windows update may change it: a call that fails, or a value that does not read back, stops
//! the window manager from trying again, and the settings app shows why. Windows before build
//! 22000 (Windows 10, Server 2022) has no such gestures, and its actions in that range may mean
//! something else, so it is left alone.
//!
//! While the gestures are off, the user's value is kept in a file in their DSA folder. A
//! window manager that ends without putting it back (a crash, or the process being ended)
//! leaves the file, and the next start puts the value back before anything else.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::touch_gestures::TouchGesturesState;
use dsa_transport::Connector;

use super::log::{note, CompositorLogger};

/// The build the window manager names itself by when it reports the gestures.
const BUILD: &str = "dsa-wm touch gestures";

/// The first Windows build with three- and four-finger touch gestures: Windows 11.
pub const FIRST_BUILD: u32 = 22000;

/// How long the gestures stay off after a task window stops being the active window, for
/// another task window to become it. Moving from one task window to another then never turns
/// them on and off again in between.
pub const SETTLE: Duration = Duration::from_millis(150);

/// How long a call to the service may take.
const ANSWER: Duration = Duration::from_secs(2);

/// How often the window manager tries the service again while it cannot reach it.
const RETRY: Duration = Duration::from_secs(5);

/// How often the thread wakes while nothing changes.
const IDLE_WAKE: Duration = Duration::from_secs(1);

/// Reads and changes the user's touch gesture setting: 0 is off, anything else is the
/// user's value with the gestures on.
pub trait GestureSetting: Send {
    fn read(&mut self) -> Result<u32, String>;
    /// Sets `value`, and checks that it reads back.
    fn write(&mut self, value: u32) -> Result<(), String>;
}

/// Keeps the user's own value while the gestures are off, so a later start can put it back.
pub trait OriginalValue: Send {
    /// The value an earlier run kept and did not put back, if any.
    fn load(&self) -> Option<u32>;
    fn save(&self, value: u32) -> Result<(), String>;
    fn clear(&self) -> Result<(), String>;
}

/// The user's value kept in a file, as a decimal number.
pub struct OriginalFile(pub PathBuf);

impl OriginalValue for OriginalFile {
    fn load(&self) -> Option<u32> {
        std::fs::read_to_string(&self.0).ok()?.trim().parse().ok()
    }
    fn save(&self, value: u32) -> Result<(), String> {
        if let Some(dir) = self.0.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
        }
        std::fs::write(&self.0, value.to_string())
            .map_err(|e| format!("writing {}: {e}", self.0.display()))
    }
    fn clear(&self) -> Result<(), String> {
        match std::fs::remove_file(&self.0) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("removing {}: {e}", self.0.display())),
        }
    }
}

/// What the window manager needs to turn the gestures off for the user.
pub struct TouchGestureSetup {
    /// Whether this Windows has the gestures at all.
    pub supported: bool,
    pub setting: Box<dyn GestureSetting>,
    pub original: Box<dyn OriginalValue>,
}

impl TouchGestureSetup {
    /// The user this process runs as, through `SystemParametersInfo`, with their value kept in
    /// `original` while the gestures are off.
    pub fn system(original: PathBuf) -> TouchGestureSetup {
        let build = crate::android_vm::windows_build().map(|(build, _)| build);
        TouchGestureSetup {
            supported: build.is_some_and(|build| build >= FIRST_BUILD),
            setting: Box::new(SystemGestures::default()),
            original: Box::new(OriginalFile(original)),
        }
    }

    /// [`TouchGestureSetup::system`] with the value kept in the user's DSA folder. `None` when
    /// this process cannot tell where that is.
    pub fn for_this_user() -> Option<TouchGestureSetup> {
        crate::paths::UserRoot::current()
            .map(|root| TouchGestureSetup::system(root.dir().join("touch-gestures-original.txt")))
    }
}

/// The gestures as one user has them, and whether the window manager holds them off.
pub struct TouchGestures {
    setting: Box<dyn GestureSetting>,
    original: Box<dyn OriginalValue>,
    state: TouchGesturesState,
    /// The user's value while the window manager holds the gestures off.
    held: Option<u32>,
}

impl TouchGestures {
    /// Puts back a value an earlier run left off, then finds whether the gestures can be read.
    /// Returns what it did, for the log.
    pub fn start(setup: TouchGestureSetup) -> (TouchGestures, Vec<String>) {
        let TouchGestureSetup {
            supported,
            setting,
            original,
        } = setup;
        let mut gestures = TouchGestures {
            setting,
            original,
            state: TouchGesturesState::Unknown,
            held: None,
        };
        let mut notes = Vec::new();
        let left = gestures.original.load();
        if !supported {
            gestures.state = TouchGesturesState::Unsupported;
            if left.is_some() {
                // This Windows has no such gestures, so the value means nothing here.
                let _ = gestures.original.clear();
            }
            notes.push(
                "touch gestures: this Windows has no three- and four-finger touch gestures"
                    .to_string(),
            );
            return (gestures, notes);
        }
        if let Some(value) = left {
            match gestures.setting.write(value) {
                Ok(()) => {
                    notes.push(format!(
                        "touch gestures: the window manager ended with them off; the user's \
                         value {value} is back"
                    ));
                    if let Err(why) = gestures.original.clear() {
                        notes.push(format!("touch gestures: {why}"));
                    }
                }
                Err(why) => {
                    notes.push(format!(
                        "touch gestures: the window manager ended with them off, and the user's \
                         value {value} could not be put back: {why}"
                    ));
                    gestures.state = TouchGesturesState::Failed(why);
                    return (gestures, notes);
                }
            }
        }
        gestures.state = match gestures.setting.read() {
            Ok(value) => {
                notes.push(format!(
                    "touch gestures: Windows reads {value}; they are turned off while a task \
                     window is in front, when the setting is on"
                ));
                TouchGesturesState::Available
            }
            Err(why) => {
                notes.push(format!("touch gestures: cannot be used: {why}"));
                TouchGesturesState::Failed(why)
            }
        };
        (gestures, notes)
    }

    /// What the window manager can do with the gestures.
    pub fn state(&self) -> &TouchGesturesState {
        &self.state
    }

    /// Whether the window manager holds the gestures off now.
    pub fn holding_off(&self) -> bool {
        self.held.is_some()
    }

    /// A task window is the active window. With `wanted` (the user's setting), the gestures
    /// are turned off unless they already are; without it, the user's value is put back.
    pub fn in_front(&mut self, wanted: bool) -> Vec<String> {
        if !wanted {
            return self.not_in_front();
        }
        if self.state != TouchGesturesState::Available || self.held.is_some() {
            return Vec::new();
        }
        let value = match self.setting.read() {
            Ok(value) => value,
            Err(why) => return self.failed(why),
        };
        if value == 0 {
            // The user has them off already.
            return Vec::new();
        }
        if let Err(why) = self.original.save(value) {
            // Turned off without a record, a crash would leave them off for good.
            return vec![format!(
                "touch gestures: left on, since the user's value cannot be kept: {why}"
            )];
        }
        if let Err(why) = self.setting.write(0) {
            let _ = self.original.clear();
            return self.failed(why);
        }
        self.held = Some(value);
        Vec::new()
    }

    /// No task window is the active window: the user's value is put back.
    pub fn not_in_front(&mut self) -> Vec<String> {
        let Some(value) = self.held.take() else {
            return Vec::new();
        };
        match self.setting.write(value) {
            Ok(()) => match self.original.clear() {
                Ok(()) => Vec::new(),
                Err(why) => vec![format!("touch gestures: {why}")],
            },
            // The record stays, so the next start tries again.
            Err(why) => self.failed(why),
        }
    }

    fn failed(&mut self, why: String) -> Vec<String> {
        let note =
            format!("touch gestures: Windows refused, so they are left alone from now on: {why}");
        self.state = TouchGesturesState::Failed(why);
        vec![note]
    }
}

/// The user's setting and the window manager's report, through the local API.
struct Service {
    connector: Box<dyn Connector>,
    client: Option<HostApiClient>,
    /// The last state the service was told, on this connection.
    told: Option<TouchGesturesState>,
    last_try: Option<Instant>,
    /// The user's setting as last read; on until the service says otherwise.
    wanted: bool,
}

impl Service {
    fn connected(&mut self) -> Option<&mut HostApiClient> {
        if self.client.is_none() {
            if self.last_try.is_some_and(|at| at.elapsed() < RETRY) {
                return None;
            }
            self.last_try = Some(Instant::now());
            self.client = HostApiClient::connect(&mut *self.connector, BUILD, ANSWER).ok();
            self.told = None;
        }
        self.client.as_mut()
    }

    /// Reads the user's setting again; the last value stays while the service cannot say.
    fn refresh(&mut self) -> bool {
        if let Some(client) = self.connected() {
            match client.settings() {
                Ok(report) => self.wanted = report.settings.touch_gestures_off,
                Err(_) => self.client = None,
            }
        }
        self.wanted
    }

    /// Tells the service `state` unless it has it already.
    fn tell(&mut self, state: &TouchGesturesState) {
        if self.client.is_some() && self.told.as_ref() == Some(state) {
            return;
        }
        let Some(client) = self.connected() else {
            return;
        };
        match client.report_touch_gestures(state) {
            Ok(()) => self.told = Some(state.clone()),
            Err(_) => self.client = None,
        }
    }
}

/// Turns the gestures off while a task window is in front, until `stop` is set or the window
/// manager's thread ends. `fronts` says, on each change, whether a task window is the active
/// window. The user's value is back when it returns.
pub fn follow(
    connector: Box<dyn Connector>,
    fronts: Receiver<bool>,
    mut gestures: TouchGestures,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let log = |notes: Vec<String>| {
        for line in notes {
            note(logger.as_deref(), &line);
        }
    };
    let mut service = Service {
        connector,
        client: None,
        told: None,
        last_try: None,
        wanted: true,
    };
    service.refresh();
    service.tell(gestures.state());
    let mut front = false;
    // When the gestures go back on, once no task window has come to the front since.
    let mut back_at: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        // Idle, the thread wakes only now and then, for the stop flag and the service.
        let wait = back_at.map_or(IDLE_WAKE, |at| at.saturating_duration_since(Instant::now()));
        match fronts.recv_timeout(wait) {
            Ok(now) if now != front => {
                front = now;
                if front {
                    back_at = None;
                    if gestures.state() == &TouchGesturesState::Available {
                        let wanted = service.refresh();
                        log(gestures.in_front(wanted));
                    }
                } else if gestures.holding_off() {
                    back_at = Some(Instant::now() + SETTLE);
                }
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if back_at.is_some_and(|at| Instant::now() >= at) {
            back_at = None;
            log(gestures.not_in_front());
        }
        let state = gestures.state().clone();
        service.tell(&state);
    }
    log(gestures.not_in_front());
    let state = gestures.state().clone();
    service.tell(&state);
}

/// The user's gestures through `SystemParametersInfo`.
#[derive(Default)]
pub struct SystemGestures {
    /// Whether the set action takes a pointer to the value rather than the value itself,
    /// once a set has shown which.
    by_pointer: Option<bool>,
}

/// The undocumented actions Settings reads and changes the gestures with.
const GET_TOUCH_GESTURES: u32 = 0x2030;
const SET_TOUCH_GESTURES: u32 = 0x2031;

impl GestureSetting for SystemGestures {
    fn read(&mut self) -> Result<u32, String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;
        let mut value = 0u32;
        // SAFETY: a get action that writes one 32-bit value into a local.
        let read = unsafe {
            SystemParametersInfoW(GET_TOUCH_GESTURES, 0, (&mut value as *mut u32).cast(), 0)
        };
        if read == 0 {
            return Err(format!(
                "Windows would not say whether its three- and four-finger touch gestures are \
                 on (SystemParametersInfo 0x2030): {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(value)
    }

    fn write(&mut self, value: u32) -> Result<(), String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
        };
        // The newer actions take their value in pvParam itself (as SPI_SETCARETTIMEOUT does);
        // the other form gives a pointer to it, for an action that wants that. The form
        // that worked last goes first.
        let forms = match self.by_pointer {
            Some(true) => [true, false],
            _ => [false, true],
        };
        let mut why = String::new();
        for by_pointer in forms {
            let mut local = value;
            let param = if by_pointer {
                (&mut local as *mut u32).cast()
            } else {
                value as usize as *mut core::ffi::c_void
            };
            // SAFETY: the set action reads at most the 32-bit local, which outlives the call;
            // in the value form Windows takes the number itself and reads no memory. Saved to
            // the profile and announced, as Settings does.
            let set = unsafe {
                SystemParametersInfoW(
                    SET_TOUCH_GESTURES,
                    0,
                    param,
                    SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
                )
            };
            if set == 0 {
                why = format!(
                    "Windows would not change its three- and four-finger touch gestures \
                     (SystemParametersInfo 0x2031): {}",
                    std::io::Error::last_os_error()
                );
                continue;
            }
            match self.read() {
                Ok(now) if (now == 0) == (value == 0) => {
                    self.by_pointer = Some(by_pointer);
                    return Ok(());
                }
                Ok(now) => {
                    why = format!(
                        "Windows reads {now} after its three- and four-finger touch gestures \
                         were set to {value} (SystemParametersInfo 0x2031)"
                    )
                }
                Err(read) => why = read,
            }
        }
        Err(why)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::thread;

    use dsa_host_core::host_api::HostApiService;
    use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
    use dsa_host_core::settings::{MemorySettings, Settings, SettingsStore};
    use dsa_transport::local;

    /// What a fake Windows holds.
    #[derive(Default)]
    struct Windows {
        value: u32,
        /// Set to refuse every set, as an update that moved the action would.
        refuse: bool,
        writes: Vec<u32>,
        reads: usize,
    }

    #[derive(Clone, Default)]
    struct FakeSetting(Arc<Mutex<Windows>>);

    impl GestureSetting for FakeSetting {
        fn read(&mut self) -> Result<u32, String> {
            let mut windows = self.0.lock().unwrap();
            windows.reads += 1;
            Ok(windows.value)
        }
        fn write(&mut self, value: u32) -> Result<(), String> {
            let mut windows = self.0.lock().unwrap();
            if windows.refuse {
                return Err("The parameter is incorrect. (os error 87)".into());
            }
            windows.writes.push(value);
            windows.value = value;
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct FakeOriginal(Arc<Mutex<Option<u32>>>);

    impl OriginalValue for FakeOriginal {
        fn load(&self) -> Option<u32> {
            *self.0.lock().unwrap()
        }
        fn save(&self, value: u32) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(value);
            Ok(())
        }
        fn clear(&self) -> Result<(), String> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    fn setup(setting: &FakeSetting, original: &FakeOriginal) -> TouchGestureSetup {
        TouchGestureSetup {
            supported: true,
            setting: Box::new(setting.clone()),
            original: Box::new(original.clone()),
        }
    }

    fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !check() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The window manager's thread over an in-memory service that keeps settings.
    struct Following {
        fronts: mpsc::Sender<bool>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
        store: Arc<MemorySettings>,
        api: local::MemoryLocalConnector,
        _service: RunningService,
    }

    impl Following {
        fn start(setup: TouchGestureSetup) -> Following {
            let (listener, api) = local::pair();
            let store = Arc::new(MemorySettings::default());
            let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
                .with_settings(store.clone(), Default::default());
            let service = RunningService::start_service(Box::new(listener), service);
            let (fronts, rx) = mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let (gestures, _) = TouchGestures::start(setup);
            let connector: Box<dyn Connector> = Box::new(api.clone());
            let flag = Arc::clone(&stop);
            let thread = thread::spawn(move || follow(connector, rx, gestures, flag, None));
            Following {
                fronts,
                stop,
                thread: Some(thread),
                store,
                api,
                _service: service,
            }
        }

        fn reported(&self) -> TouchGesturesState {
            let mut api = self.api.clone();
            HostApiClient::connect(&mut api, "test", ANSWER)
                .unwrap()
                .settings()
                .unwrap()
                .touch_gestures
        }

        fn end(mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    #[test]
    fn fr5_touch_gestures_are_off_only_while_a_task_window_is_in_front() {
        let setting = FakeSetting::default();
        setting.0.lock().unwrap().value = 1;
        let original = FakeOriginal::default();
        let following = Following::start(setup(&setting, &original));
        wait_until(
            "the window manager to say the gestures can be turned off",
            || following.reported() == TouchGesturesState::Available,
        );

        // A task window comes to the front: the gestures go off, with the user's value kept.
        following.fronts.send(true).unwrap();
        wait_until("the gestures to go off", || {
            setting.0.lock().unwrap().value == 0
        });
        assert_eq!(original.load(), Some(1));

        // Another window comes to the front: the user's value is back, and nothing is kept.
        following.fronts.send(false).unwrap();
        wait_until("the user's value to be back", || {
            setting.0.lock().unwrap().value == 1
        });
        assert_eq!(original.load(), None);

        // Moving from one task window to another leaves them off throughout.
        following.fronts.send(true).unwrap();
        wait_until("the gestures to go off again", || {
            setting.0.lock().unwrap().value == 0
        });
        let writes = setting.0.lock().unwrap().writes.len();
        following.fronts.send(false).unwrap();
        following.fronts.send(true).unwrap();
        thread::sleep(SETTLE * 3);
        assert_eq!(setting.0.lock().unwrap().writes.len(), writes);
        assert_eq!(setting.0.lock().unwrap().value, 0);
        following.fronts.send(false).unwrap();
        wait_until("the user's value to be back", || {
            setting.0.lock().unwrap().value == 1
        });

        // With the setting off, a task window in front leaves the gestures as they are.
        following
            .store
            .set(
                "test",
                &Settings {
                    touch_gestures_off: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        let writes = setting.0.lock().unwrap().writes.len();
        following.fronts.send(true).unwrap();
        thread::sleep(SETTLE * 3);
        assert_eq!(setting.0.lock().unwrap().value, 1);
        assert_eq!(setting.0.lock().unwrap().writes.len(), writes);
        following.fronts.send(false).unwrap();

        // Ending with a task window in front puts the user's value back.
        following.store.set("test", &Settings::default()).unwrap();
        following.fronts.send(true).unwrap();
        wait_until("the gestures to go off", || {
            setting.0.lock().unwrap().value == 0
        });
        following.end();
        assert_eq!(setting.0.lock().unwrap().value, 1);
        assert_eq!(original.load(), None);
    }

    #[test]
    fn fr5_a_window_manager_that_ended_with_the_gestures_off_puts_them_back_when_it_starts() {
        // The last run turned them off and then crashed: Windows reads 0, the record says 2.
        let setting = FakeSetting::default();
        setting.0.lock().unwrap().value = 0;
        let original = FakeOriginal::default();
        original.save(2).unwrap();
        let (gestures, notes) = TouchGestures::start(setup(&setting, &original));
        assert_eq!(setting.0.lock().unwrap().value, 2);
        assert_eq!(original.load(), None);
        assert_eq!(gestures.state(), &TouchGesturesState::Available);
        assert!(
            notes.iter().any(|line| line.contains("value 2 is back")),
            "{notes:?}"
        );
    }

    #[test]
    fn fr5_a_refused_call_disables_the_setting_with_the_reason() {
        let setting = FakeSetting::default();
        {
            let mut windows = setting.0.lock().unwrap();
            windows.value = 1;
            windows.refuse = true;
        }
        let original = FakeOriginal::default();
        let following = Following::start(setup(&setting, &original));
        following.fronts.send(true).unwrap();
        wait_until("the window manager to say Windows refused", || {
            matches!(following.reported(), TouchGesturesState::Failed(_))
        });
        let TouchGesturesState::Failed(why) = following.reported() else {
            unreachable!()
        };
        assert!(why.contains("os error 87"), "{why}");
        assert_eq!(
            setting.0.lock().unwrap().value,
            1,
            "left as the user has it"
        );
        assert_eq!(original.load(), None, "nothing to put back");

        // Nothing is tried again while the window manager runs.
        let reads = setting.0.lock().unwrap().reads;
        following.fronts.send(false).unwrap();
        following.fronts.send(true).unwrap();
        thread::sleep(SETTLE * 3);
        assert_eq!(setting.0.lock().unwrap().reads, reads);
        following.end();
    }

    #[test]
    fn fr5_windows_without_the_gestures_is_left_alone_and_the_setting_hidden() {
        let setting = FakeSetting::default();
        setting.0.lock().unwrap().value = 7;
        let original = FakeOriginal::default();
        let following = Following::start(TouchGestureSetup {
            supported: false,
            ..setup(&setting, &original)
        });
        wait_until("the window manager to say there are no gestures", || {
            following.reported() == TouchGesturesState::Unsupported
        });
        following.fronts.send(true).unwrap();
        thread::sleep(SETTLE * 3);
        following.end();
        let windows = setting.0.lock().unwrap();
        assert_eq!((windows.reads, windows.writes.len()), (0, 0));
    }

    #[test]
    fn fr5_gestures_the_user_has_off_already_are_left_alone() {
        let setting = FakeSetting::default();
        let original = FakeOriginal::default();
        let (mut gestures, _) = TouchGestures::start(setup(&setting, &original));
        assert!(gestures.in_front(true).is_empty());
        assert!(!gestures.holding_off());
        assert!(gestures.not_in_front().is_empty());
        assert!(setting.0.lock().unwrap().writes.is_empty());
        assert_eq!(original.load(), None);
    }

    #[test]
    fn fr5_the_users_value_is_kept_in_a_file_until_it_is_back() {
        let dir = std::env::temp_dir().join(format!("dsa-touch-gestures-{}", std::process::id()));
        let file = OriginalFile(dir.join("touch-gestures-original.txt"));
        assert_eq!(file.load(), None);
        file.save(1).unwrap();
        assert_eq!(OriginalFile(file.0.clone()).load(), Some(1));
        file.clear().unwrap();
        assert_eq!(file.load(), None);
        file.clear().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
