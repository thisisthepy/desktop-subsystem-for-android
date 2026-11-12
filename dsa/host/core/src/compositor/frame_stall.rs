//! Noticing a guest compositor that never comes back after a GPU reset, and restarting
//! Android when it does not.
//!
//! A GPU reset loses every graphics device on the GPU, the guest's included. The renderer
//! makes its own device again, and the guest's compositor (SurfaceFlinger) is meant to build
//! its GPU context again once the renderer answers. When it does not, nothing in the guest
//! ever draws again: every task window stays empty, and so does the window of every app
//! opened after it, until Android is restarted. On one PC that lasted twelve hours, with the
//! compositor saying every few seconds that it had no EGL context yet after the loss.
//!
//! The host cannot read the guest compositor's log, but it sees what that failure does: after
//! the reset no task window gets a frame. [`FrameStall`] follows the GPU resets the window
//! manager learns of (its own device's, and the renderer's `DeviceState`), the task windows
//! that are shown, and a count of the frames that reached any task window. A frame in any
//! window after the reset says the compositor draws again, and the watch ends. When instead
//! a window that is shown (one open through the reset, or one opened since, as an app started
//! by the user) has waited [`STALL_AFTER`] without a single frame in any window, the guest's
//! compositor is taken not to recover: [`FrameStall::tick`] says so once, and the window
//! manager restarts Android ([`restart_android`]), opens the apps that had windows again,
//! and tells the user ([`Notice`]). The time counts from the latest GPU event, so a renderer
//! that takes long to make its device again (24 s has been seen) is not mistaken for a guest
//! that does not recover.
//!
//! Restarting Android is the whole guest: its compositor, the apps and the graphics streams
//! all start fresh, which is what brought the frames back on that PC. A guest that stalls
//! again and again would be restarted for ever, so Android is restarted at most
//! [`RESTARTS`] times in [`RESTART_WINDOW`]; past that the user is told to restart it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dsa_transport::Connector;

use crate::host_api::HostApiClient;
use crate::notifications::Notification;
use crate::session::SessionState;

/// How long a shown task window may go without a frame in any window, after a GPU reset,
/// before the guest's compositor is taken not to recover.
pub const STALL_AFTER: Duration = Duration::from_secs(30);

/// How many times Android is restarted for a stalled compositor in [`RESTART_WINDOW`].
pub const RESTARTS: usize = 2;

/// The time the restarts are counted over.
pub const RESTART_WINDOW: Duration = Duration::from_secs(60 * 60);

/// What the keys of the host's own notices about recoveries start with. It is under the
/// host's own prefix, which no guest notification can have.
pub const NOTICE_PREFIX: &str = "dsa:recovery:";

/// Whether `key` is one of the notices about recoveries: the host's alone, so a click on one
/// goes nowhere, and Android has nothing to open or dismiss.
pub fn is_recovery_key(key: &str) -> bool {
    key.starts_with(NOTICE_PREFIX)
}

/// The guest's compositor did not recover from a GPU reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    /// How long ago the last GPU reset was.
    pub since_reset: Duration,
    /// The shown task windows that waited for a frame, in task ID order.
    pub windows: Vec<i32>,
    /// How long the longest of them waited.
    pub waited: Duration,
    /// Whether Android is restarted for it: false once it has been restarted [`RESTARTS`]
    /// times in [`RESTART_WINDOW`].
    pub restart: bool,
}

impl Stall {
    /// Why, in words for the log.
    pub fn reason(&self) -> String {
        let windows = self
            .windows
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let what = if self.restart {
            "Android is restarted so its apps draw again".to_string()
        } else {
            format!(
                "Android was restarted for this {RESTARTS} times in the last {} minutes \
                 already, so it is left as it is",
                RESTART_WINDOW.as_secs() / 60
            )
        };
        format!(
            "the guest's compositor did not recover from the GPU reset {} s ago: no task window \
             has had a frame since, and shown task window(s) {windows} waited {} s for one; {what}",
            self.since_reset.as_secs(),
            self.waited.as_secs()
        )
    }

    /// What the user is told of it.
    pub fn notice(&self) -> Notice {
        if self.restart {
            Notice::Restarting
        } else {
            Notice::GaveUp
        }
    }
}

/// The GPU resets whose frames have not come back yet, and the windows that wait for them.
#[derive(Debug, Default)]
pub struct FrameStall {
    /// The last GPU event with no frame in any task window since, and the count of frames
    /// then.
    reset: Option<(Instant, u64)>,
    /// Since when each shown task window has been shown, while frames are waited for.
    shown_since: BTreeMap<i32, Instant>,
    /// Whether the windows were looked at since the last GPU event: a window shown at the
    /// first look after it was shown at the event.
    looked: bool,
    /// When Android was restarted for a stall lately.
    restarts: Vec<Instant>,
}

impl FrameStall {
    pub fn new() -> FrameStall {
        FrameStall::default()
    }

    /// The GPU was reset at `now`, or the renderer found its device lost or made it again,
    /// with `frames` frames shown in task windows so far. Each event starts the wait again:
    /// the guest cannot draw before the renderer has its device back.
    pub fn gpu_reset(&mut self, now: Instant, frames: u64) {
        self.reset = Some((now, frames));
        self.looked = false;
    }

    /// Whether frames are waited for after a GPU reset.
    pub fn waiting(&self) -> bool {
        self.reset.is_some()
    }

    /// Looks at the windows at `now`: `shown` are the task windows shown now (open and not
    /// minimized), `frames` the count of frames shown in task windows so far. Says once when
    /// the guest's compositor did not recover; a frame in any window ends the watch.
    pub fn tick(&mut self, now: Instant, shown: &[i32], frames: u64) -> Option<Stall> {
        let Some((reset_at, seen)) = self.reset else {
            self.shown_since.clear();
            return None;
        };
        if frames != seen {
            // The guest draws again.
            self.reset = None;
            self.shown_since.clear();
            return None;
        }
        self.shown_since
            .retain(|task_id, _| shown.contains(task_id));
        // A window not seen before was shown at the event, at the first look after it, or
        // since the last look otherwise.
        let since = if self.looked { now } else { reset_at };
        self.looked = true;
        for task_id in shown {
            self.shown_since.entry(*task_id).or_insert(since);
        }
        let waited = |since: &Instant| now.saturating_duration_since((*since).max(reset_at));
        let longest = self.shown_since.values().map(waited).max()?;
        if longest < STALL_AFTER {
            return None;
        }
        let windows = self.shown_since.keys().copied().collect();
        self.reset = None;
        self.shown_since.clear();
        self.restarts
            .retain(|at| now.saturating_duration_since(*at) < RESTART_WINDOW);
        let restart = self.restarts.len() < RESTARTS;
        if restart {
            self.restarts.push(now);
        }
        Some(Stall {
            since_reset: now.saturating_duration_since(reset_at),
            windows,
            waited: longest,
            restart,
        })
    }
}

/// What the user is told of a stalled guest compositor, as one of the host's own
/// notifications: under DSA's own name, in the user's language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// Android is restarted, and the apps that had windows open again.
    Restarting,
    /// Android was restarted for this too often lately; the user is asked to restart it.
    GaveUp,
}

impl Notice {
    fn word(self) -> &'static str {
        match self {
            Notice::Restarting => "restarting",
            Notice::GaveUp => "gave-up",
        }
    }

    /// The key of this notice.
    pub fn key(self) -> String {
        format!("{NOTICE_PREFIX}graphics:{}", self.word())
    }

    /// The title and text, in Korean when `korean`, else in English.
    pub fn text(self, korean: bool) -> (String, String) {
        let seconds = STALL_AFTER.as_secs();
        match (self, korean) {
            (Notice::Restarting, false) => (
                "Restarting Android".into(),
                format!(
                    "After a GPU reset, no app window showed anything for {seconds} seconds: \
                     Android's graphics did not recover. The apps whose windows were open open \
                     again once it has started."
                ),
            ),
            (Notice::Restarting, true) => (
                "Android를 다시 시작합니다".into(),
                format!(
                    "GPU가 재설정된 뒤 {seconds}초 동안 어떤 앱 창에도 화면이 나오지 않아 \
                     Android의 그래픽이 돌아오지 않았습니다. 창이 열려 있던 앱은 Android가 \
                     시작되면 다시 열립니다."
                ),
            ),
            (Notice::GaveUp, false) => (
                "Android's graphics did not recover".into(),
                "After a GPU reset, app windows show nothing. DSA restarted Android for this \
                 more than once in the last hour; restart it from DSA Settings to see the \
                 apps again."
                    .into(),
            ),
            (Notice::GaveUp, true) => (
                "Android의 그래픽이 돌아오지 않았습니다".into(),
                "GPU가 재설정된 뒤 앱 창에 아무것도 나오지 않습니다. 지난 한 시간 동안 DSA가 \
                 이 때문에 Android를 여러 번 다시 시작했습니다. 앱을 다시 보려면 DSA 설정에서 \
                 Android를 다시 시작하십시오."
                    .into(),
            ),
        }
    }

    /// The notification of this notice, in Korean when `korean`.
    pub fn notification(self, korean: bool) -> Notification {
        let (title, text) = self.text(korean);
        Notification {
            key: self.key(),
            package_name: String::new(),
            app_label: crate::update::alerts::SHOWN_AS.into(),
            title,
            text,
            icon_png: Vec::new(),
            importance: 4,
            high_priority: false,
            clearable: true,
            when_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_millis() as i64)
                .unwrap_or(0),
        }
    }
}

/// What restarting Android needs of the service.
pub trait AndroidControl {
    /// Where the caller's Android is in its life.
    fn state(&mut self) -> Result<SessionState, String>;
    /// Asks it to stop; returns at once.
    fn stop(&mut self) -> Result<(), String>;
    /// Asks it to start; returns at once.
    fn start(&mut self) -> Result<(), String>;
    /// Opens `package` in a task of its own.
    fn launch(&mut self, package: &str) -> Result<(), String>;
}

impl AndroidControl for HostApiClient {
    fn state(&mut self) -> Result<SessionState, String> {
        self.status()
            .map(|status| status.state)
            .map_err(|e| e.to_string())
    }

    fn stop(&mut self) -> Result<(), String> {
        HostApiClient::stop(self)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn start(&mut self) -> Result<(), String> {
        HostApiClient::start(self)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn launch(&mut self, package: &str) -> Result<(), String> {
        self.launch_app(package).map_err(|e| e.to_string())
    }
}

/// How long a restart waits for each step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartTimes {
    /// How often the state is asked for.
    pub poll: Duration,
    /// How long Android may take to stop.
    pub stop: Duration,
    /// How long it may take to start and be ready.
    pub start: Duration,
}

impl Default for RestartTimes {
    fn default() -> Self {
        RestartTimes {
            poll: Duration::from_millis(500),
            // The service gives the guest 30 s to end its processes.
            stop: Duration::from_secs(90),
            // A boot after an update compiles apps; the service reports a slow boot at 300 s.
            start: Duration::from_secs(300),
        }
    }
}

/// Restarts the caller's Android: stops it, waits until it has stopped, starts it, waits until
/// it is ready, and opens each of `packages` again. Returns the packages that opened; `log`
/// is told of each that did not, and an error says which step failed.
pub fn restart_android(
    control: &mut dyn AndroidControl,
    packages: &[String],
    times: RestartTimes,
    log: &dyn Fn(&str),
) -> Result<Vec<String>, String> {
    control
        .stop()
        .map_err(|why| format!("Android was not asked to stop: {why}"))?;
    let deadline = Instant::now() + times.stop;
    loop {
        let state = control.state()?;
        if state == SessionState::Stopped {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Android did not stop: it is still {state} after {} s",
                times.stop.as_secs()
            ));
        }
        thread::sleep(times.poll);
    }
    control
        .start()
        .map_err(|why| format!("Android was not asked to start again: {why}"))?;
    let deadline = Instant::now() + times.start;
    // The start may not have gone in yet at the first look, which still finds it stopped.
    let mut starting = false;
    loop {
        let state = control.state()?;
        match state {
            SessionState::Active | SessionState::Dozing | SessionState::Frozen => break,
            SessionState::Stalled => {
                return Err("Android did not start again: it runs but is not ready".into())
            }
            SessionState::Stopped if starting => {
                return Err("Android did not start again: it stopped while starting".into())
            }
            SessionState::Starting => starting = true,
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Android did not start again: it is still {state} after {} s",
                times.start.as_secs()
            ));
        }
        thread::sleep(times.poll);
    }
    let mut opened = Vec::new();
    for package in packages {
        match control.launch(package) {
            Ok(()) => opened.push(package.clone()),
            Err(why) => log(&format!("{package} did not open again: {why}")),
        }
    }
    Ok(opened)
}

/// A connector that the restarts share, one connection at a time: a window manager makes its
/// connector to the service once, and each restart connects through it.
pub struct SharedConnector(pub Arc<Mutex<Box<dyn Connector>>>);

impl Connector for SharedConnector {
    fn connect(
        &mut self,
        timeout: Duration,
    ) -> Result<Box<dyn dsa_transport::Connection>, dsa_transport::TransportError> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .connect(timeout)
    }
}

/// Restarts the caller's Android on a thread of its own, through a connection to the service
/// `connector` makes, and opens `packages` again: [`restart_android`]. `log` is told how it
/// went.
pub fn restart_in_background(
    mut connector: Box<dyn Connector>,
    build: String,
    packages: Vec<String>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("dsa-restart-android".into())
        .spawn(move || {
            let mut client =
                match HostApiClient::connect(&mut *connector, &build, Duration::from_secs(5)) {
                    Ok(client) => client,
                    Err(why) => {
                        log(&format!(
                            "Android was not restarted: the service cannot be reached: {why}"
                        ));
                        return;
                    }
                };
            let said = Arc::clone(&log);
            let tell = move |line: &str| said(line);
            match restart_android(&mut client, &packages, RestartTimes::default(), &tell) {
                Ok(opened) => log(&format!(
                    "Android restarted after its compositor did not recover from the GPU \
                     reset; opened again: {}",
                    if opened.is_empty() {
                        "no app".to_string()
                    } else {
                        opened.join(", ")
                    }
                )),
                Err(why) => log(&format!("restarting Android went wrong: {why}")),
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn nfr5_a_window_with_no_frame_for_30_s_after_a_reset_is_a_stalled_compositor() {
        let start = Instant::now();
        let mut watch = FrameStall::new();
        watch.gpu_reset(start, 120);
        assert!(watch.tick(after(start, 1), &[4], 120).is_none());
        assert!(watch.tick(after(start, 29), &[4], 120).is_none());
        let stall = watch
            .tick(after(start, 31), &[4], 120)
            .expect("the compositor did not recover");
        assert_eq!(stall.windows, vec![4]);
        assert!(stall.restart);
        assert_eq!(stall.notice(), Notice::Restarting);
        assert!(stall.waited >= STALL_AFTER);
        assert!(
            stall.reason().contains("no task window has had a frame"),
            "{}",
            stall.reason()
        );
        // Said once: the restart that follows does not start another.
        assert!(!watch.waiting());
        assert!(watch.tick(after(start, 90), &[4], 120).is_none());
    }

    #[test]
    fn nfr5_a_frame_in_any_window_after_the_reset_is_a_recovered_compositor() {
        let start = Instant::now();
        let mut watch = FrameStall::new();
        watch.gpu_reset(start, 120);
        assert!(watch.tick(after(start, 20), &[4, 7], 120).is_none());
        // Window 7's app draws again; window 4's app has nothing new to show.
        assert!(watch.tick(after(start, 25), &[4, 7], 121).is_none());
        assert!(!watch.waiting());
        assert!(watch.tick(after(start, 600), &[4, 7], 121).is_none());
    }

    #[test]
    fn nfr5_the_wait_counts_from_the_renderers_new_device_not_from_the_loss() {
        // The renderer took 24 s to make its device again on a PC; the guest cannot draw
        // before that.
        let start = Instant::now();
        let mut watch = FrameStall::new();
        watch.gpu_reset(start, 10);
        assert!(watch.tick(after(start, 20), &[3], 10).is_none());
        watch.gpu_reset(after(start, 24), 10);
        assert!(watch.tick(after(start, 40), &[3], 10).is_none());
        assert!(watch.tick(after(start, 53), &[3], 10).is_none());
        assert!(watch.tick(after(start, 55), &[3], 10).is_some());
    }

    #[test]
    fn nfr5_an_app_opened_long_after_the_reset_that_gets_no_frame_is_a_stall_too() {
        // On the PC, every app opened in the twelve hours after the reset showed nothing.
        let start = Instant::now();
        let mut watch = FrameStall::new();
        watch.gpu_reset(start, 50);
        // No window is shown for an hour: nothing is waited for.
        for minute in 1..=60 {
            assert!(watch.tick(after(start, minute * 60), &[], 50).is_none());
        }
        assert!(watch.waiting());
        // The user opens an app at 3600 s; its window waits from then on.
        assert!(watch.tick(after(start, 3601), &[12], 50).is_none());
        assert!(watch.tick(after(start, 3620), &[12], 50).is_none());
        let stall = watch.tick(after(start, 3632), &[12], 50).unwrap();
        assert_eq!(stall.windows, vec![12]);
        assert!(stall.since_reset >= Duration::from_secs(3600));
    }

    #[test]
    fn nfr5_a_minimized_window_does_not_wait_for_frames() {
        let start = Instant::now();
        let mut watch = FrameStall::new();
        watch.gpu_reset(start, 5);
        assert!(watch.tick(after(start, 10), &[2], 5).is_none());
        // Window 2 is minimized at 20 s and shown again at 40 s: it waits from 40 s.
        assert!(watch.tick(after(start, 20), &[], 5).is_none());
        assert!(watch.tick(after(start, 40), &[2], 5).is_none());
        assert!(watch.tick(after(start, 65), &[2], 5).is_none());
        assert!(watch.tick(after(start, 71), &[2], 5).is_some());
    }

    #[test]
    fn nfr5_a_window_shown_at_the_reset_waits_from_the_reset() {
        let start = Instant::now();
        let mut watch = FrameStall::new();
        watch.gpu_reset(start, 0);
        // The first look after the reset finds the window shown: it was shown at the reset.
        let stall = watch.tick(after(start, 31), &[4], 0).unwrap();
        assert!(stall.waited >= STALL_AFTER);
    }

    #[test]
    fn nfr5_without_a_reset_nothing_is_watched() {
        let start = Instant::now();
        let mut watch = FrameStall::new();
        assert!(watch.tick(after(start, 120), &[1, 2], 0).is_none());
        assert!(!watch.waiting());
    }

    #[test]
    fn nfr5_android_is_restarted_for_a_stall_at_most_twice_an_hour() {
        let start = Instant::now();
        let mut watch = FrameStall::new();
        let mut at = 0;
        let mut stall = || {
            watch.gpu_reset(after(start, at), 0);
            at += 40;
            let stall = watch.tick(after(start, at), &[1], 0).unwrap();
            at += 1;
            stall
        };
        assert!(stall().restart);
        assert!(stall().restart);
        let third = stall();
        assert!(
            !third.restart,
            "a third stall within the hour is left alone"
        );
        assert_eq!(third.notice(), Notice::GaveUp);
        assert!(
            third.reason().contains("left as it is"),
            "{}",
            third.reason()
        );
        drop(stall);
        // An hour after the first restarts, Android is restarted again.
        watch.gpu_reset(after(start, 3700), 0);
        assert!(watch.tick(after(start, 3740), &[1], 0).unwrap().restart);
    }

    #[test]
    fn nfr5_the_notices_are_the_hosts_own_and_in_the_users_language() {
        for notice in [Notice::Restarting, Notice::GaveUp] {
            let english = notice.notification(false);
            let korean = notice.notification(true);
            assert!(is_recovery_key(&english.key));
            assert!(english
                .key
                .starts_with(crate::notifications::HOST_KEY_PREFIX));
            assert_eq!(english.key, korean.key);
            assert_eq!(english.app_label, "DSA");
            assert_ne!(english.title, korean.title);
            // The long dash the house style forbids, named by its code point.
            let dash = char::from_u32(0x2014).unwrap();
            assert!(!english.text.contains(dash) && !korean.text.contains(dash));
        }
        assert!(Notice::Restarting
            .notification(false)
            .text
            .contains("30 seconds"));
        assert!(!is_recovery_key("0|com.example.chat|1|null|10100"));
        assert!(!is_recovery_key("dsa:update:android"));
    }

    /// A service that plays Android's life: each request moves its state after a few looks.
    struct FakeAndroid {
        state: SessionState,
        /// The states to pass through, one per look, before the next is reached.
        ahead: Vec<SessionState>,
        asked: Vec<String>,
        refuse_launch: Option<String>,
        never_starts: bool,
    }

    impl FakeAndroid {
        fn active() -> FakeAndroid {
            FakeAndroid {
                state: SessionState::Active,
                ahead: Vec::new(),
                asked: Vec::new(),
                refuse_launch: None,
                never_starts: false,
            }
        }
    }

    impl AndroidControl for FakeAndroid {
        fn state(&mut self) -> Result<SessionState, String> {
            if !self.ahead.is_empty() {
                self.state = self.ahead.remove(0);
            }
            Ok(self.state)
        }

        fn stop(&mut self) -> Result<(), String> {
            self.asked.push("stop".into());
            self.ahead = vec![SessionState::Stopping, SessionState::Stopped];
            Ok(())
        }

        fn start(&mut self) -> Result<(), String> {
            self.asked.push("start".into());
            self.ahead = if self.never_starts {
                vec![SessionState::Starting]
            } else {
                vec![
                    SessionState::Starting,
                    SessionState::Starting,
                    SessionState::Active,
                ]
            };
            Ok(())
        }

        fn launch(&mut self, package: &str) -> Result<(), String> {
            assert_eq!(
                self.state,
                SessionState::Active,
                "apps open once it is ready"
            );
            self.asked.push(format!("launch {package}"));
            match &self.refuse_launch {
                Some(refused) if refused == package => Err("no such app".into()),
                _ => Ok(()),
            }
        }
    }

    fn quick() -> RestartTimes {
        RestartTimes {
            poll: Duration::from_millis(1),
            stop: Duration::from_millis(200),
            start: Duration::from_millis(200),
        }
    }

    #[test]
    fn nfr5_a_stalled_android_is_stopped_started_and_its_apps_opened_again() {
        let mut android = FakeAndroid::active();
        android.refuse_launch = Some("com.example.gone".into());
        let packages = vec![
            "com.example.flo".to_string(),
            "com.example.gone".to_string(),
            "com.google.android.youtube".to_string(),
        ];
        let told = std::cell::RefCell::new(Vec::new());
        let log = |line: &str| told.borrow_mut().push(line.to_string());
        let opened = restart_android(&mut android, &packages, quick(), &log).unwrap();
        assert_eq!(
            told.into_inner(),
            vec!["com.example.gone did not open again: no such app".to_string()]
        );
        assert_eq!(
            opened,
            vec![
                "com.example.flo".to_string(),
                "com.google.android.youtube".to_string()
            ]
        );
        assert_eq!(
            android.asked,
            vec![
                "stop",
                "start",
                "launch com.example.flo",
                "launch com.example.gone",
                "launch com.google.android.youtube"
            ]
        );
    }

    #[test]
    fn nfr5_an_android_that_does_not_start_again_says_so_and_opens_nothing() {
        let mut android = FakeAndroid::active();
        android.never_starts = true;
        let why = restart_android(&mut android, &["com.example.flo".into()], quick(), &|_| {})
            .unwrap_err();
        assert!(why.contains("did not start again"), "{why}");
        assert!(why.contains("Starting"), "{why}");
        assert_eq!(android.asked, vec!["stop", "start"]);
    }
}
