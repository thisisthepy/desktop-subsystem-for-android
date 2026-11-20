//! A guest whose kernel crashed: how a host finds out, what it reports, and what it tells the
//! user.
//!
//! The guest's kernel runs with `panic=-1`, so a kernel panic resets the VM at once. Some
//! hypervisors (Apple's Virtualization framework) reset it in place: the VM never ends, its
//! control channel closes, and a fresh boot starts with no host to talk to. Others (crosvm)
//! end the VM. Either way the console holds what happened, and every host reads it the same
//! way through [`ConsoleWatch`] and [`summary`].
//!
//! The session reports such a stop as [`crate::session::StopReason::GuestCrashed`], whose
//! English account starts with [`CRASH_PREFIX`], so every surface that reads the status
//! (the command line, the settings apps, the window managers) can find it and say it in the
//! user's language with [`crash_text`] and [`notification`].

use std::collections::VecDeque;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dsa_transport::Connector;

use crate::host_api::HostApiClient;
use crate::notifications::Notification;
use crate::session::SessionState;
use crate::ui_language::UiLanguage;

/// What the English account of a crashed guest starts with, in the status's last reason. The
/// reason itself follows it in parentheses.
pub const CRASH_PREFIX: &str = "Android crashed (";

/// The key of the host's notice that Android stopped unexpectedly. It is under the prefix of
/// the host's notices about recoveries, so every platform shows it as DSA's own, with nothing
/// for Android to open or dismiss.
pub const NOTICE_KEY: &str = "dsa:recovery:android-crashed";

/// How many console lines before a crash are kept for its report: enough for an oops, its
/// registers and its call trace.
const LINES_BEFORE: usize = 80;

/// How many console lines after a kernel panic are kept: the end of its report, before the
/// reset starts a new boot.
const LINES_AFTER: usize = 12;

/// The English account of a guest that crashed for `why`.
pub fn crashed(why: &str) -> String {
    format!("{CRASH_PREFIX}{why})")
}

/// The reason of a crash, when `reason` (a status's last reason, or its first line) is one.
pub fn crash_reason(reason: &str) -> Option<&str> {
    let first = reason.lines().next().unwrap_or_default();
    let rest = first.strip_prefix(CRASH_PREFIX)?;
    Some(rest.strip_suffix(')').unwrap_or(rest))
}

/// What happened on the console that a host acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleEvent {
    /// The kernel panicked.
    KernelPanic,
    /// A new boot started on the same console: the VM was reset in place.
    Reset,
}

impl ConsoleEvent {
    /// The launcher's account of it, as [`crate::session::GuestLauncher::exited`] says it.
    pub fn sentence(&self) -> &'static str {
        match self {
            ConsoleEvent::KernelPanic => {
                "Android's kernel panicked and its VM restarted by itself, so DSA stopped it"
            }
            ConsoleEvent::Reset => {
                "Android's VM restarted by itself while it ran, so DSA stopped it"
            }
        }
    }
}

/// Reads a running guest's console as it grows, and says when the kernel panicked or the VM
/// was reset in place. One watch serves one boot: a new boot needs a new watch.
#[derive(Debug, Default)]
pub struct ConsoleWatch {
    /// The end of a line that has not been finished yet.
    partial: String,
    /// The last lines, for the report of a crash.
    recent: VecDeque<String>,
    /// The kernel's timestamp of the last line that had one, in seconds.
    last_time: Option<f64>,
    /// Lines seen.
    lines: u64,
    /// What was found, once something was.
    event: Option<ConsoleEvent>,
    /// The console around the crash, once one was found.
    report: Vec<String>,
    /// How many more lines go into the report after a panic.
    after: usize,
}

impl ConsoleWatch {
    pub fn new() -> ConsoleWatch {
        ConsoleWatch::default()
    }

    /// Reads `text`, the console's next output. Returns what it showed, the first time it
    /// shows something; later calls return `None` and only finish the report.
    pub fn feed(&mut self, text: &str) -> Option<ConsoleEvent> {
        self.partial.push_str(text);
        let mut found = None;
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            let line = line.trim_end_matches(['\n', '\r']).to_string();
            if let Some(event) = self.line(line) {
                found.get_or_insert(event);
            }
        }
        found
    }

    /// What was found so far.
    pub fn event(&self) -> Option<&ConsoleEvent> {
        self.event.as_ref()
    }

    /// The console lines around the crash, once one was found.
    pub fn report(&self) -> Option<String> {
        if self.event.is_none() || self.report.is_empty() {
            return None;
        }
        Some(self.report.join("\n"))
    }

    fn line(&mut self, line: String) -> Option<ConsoleEvent> {
        self.lines += 1;
        if self.event.is_some() {
            // The report of a panic ends with a few lines after it, up to a new boot.
            if self.after > 0 && !self.starts_new_boot(&line) {
                self.after -= 1;
                self.report.push(line);
            } else {
                self.after = 0;
            }
            return None;
        }
        if self.starts_new_boot(&line) {
            self.report = self.recent.drain(..).collect();
            self.event = Some(ConsoleEvent::Reset);
            return self.event.clone();
        }
        if let Some(time) = kernel_time(&line) {
            self.last_time = Some(time);
        }
        let panic = is_panic(&line);
        self.recent.push_back(line);
        if self.recent.len() > LINES_BEFORE {
            self.recent.pop_front();
        }
        if panic {
            self.report = self.recent.drain(..).collect();
            self.after = LINES_AFTER;
            self.event = Some(ConsoleEvent::KernelPanic);
            return self.event.clone();
        }
        None
    }

    /// Whether `line` begins another boot than the one the watch has read: the kernel's clock
    /// starts again from zero, or its first message comes again.
    fn starts_new_boot(&self, line: &str) -> bool {
        if self.lines <= 1 {
            return false;
        }
        let text = message(line);
        if text.starts_with("Booting Linux on physical CPU") || text.starts_with("Linux version ") {
            return self.last_time.is_some_and(|last| last > 1.0);
        }
        match (self.last_time, kernel_time(line)) {
            (Some(last), Some(now)) => last > 1.0 && now + 1.0 < last,
            _ => false,
        }
    }
}

/// The kernel's timestamp at the start of `line`, `[  229.334583]`, in seconds.
fn kernel_time(line: &str) -> Option<f64> {
    let rest = line.trim_start().strip_prefix('[')?;
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

/// `line` without the kernel's timestamp.
fn message(line: &str) -> &str {
    let trimmed = line.trim_start();
    if kernel_time(trimmed).is_some() {
        if let Some(end) = trimmed.find(']') {
            return trimmed[end + 1..].trim_start();
        }
    }
    trimmed
}

fn is_panic(line: &str) -> bool {
    message(line).starts_with("Kernel panic")
}

fn is_oops(line: &str) -> bool {
    let text = message(line);
    text.starts_with("Internal error: Oops")
        || text.starts_with("Oops")
        || text.starts_with("Unable to handle kernel")
}

fn is_bug(line: &str) -> bool {
    message(line).starts_with("BUG:")
}

/// A frame of a call trace, `  binder_transaction+0x11a8/0x2b70`, as the function's name,
/// unless it is the frame the fault happened in (marked `(P)`), which is usually a library
/// routine such as `rb_insert_color` rather than the code that called it with bad data.
fn caller_frame(line: &str) -> Option<&str> {
    let text = message(line);
    if text.ends_with("(P)") {
        return None;
    }
    let name = text.split('+').next()?.trim();
    let plain = !name.is_empty()
        && text.contains("+0x")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    plain.then_some(name)
}

/// What a guest's console (or the part of it a launcher kept) says about a kernel crash, as
/// a short phrase: `kernel panic in binder_transaction`, with the first sign of trouble the
/// kernel reported before it when there was one. `None` when it shows no crash.
pub fn summary(console: &str) -> Option<String> {
    let lines: Vec<&str> = console.lines().collect();
    let panic = lines.iter().position(|line| is_panic(line));
    let oops = lines.iter().position(|line| is_oops(line));
    let bug = lines.iter().position(|line| is_bug(line));
    let kind = match (panic, oops) {
        (Some(_), _) => "kernel panic",
        (None, Some(_)) => "kernel oops",
        (None, None) => "kernel bug",
    };
    let start = oops.or(panic).or(bug)?;
    let end = panic.map(|at| at + 1).unwrap_or(lines.len());
    let trace = lines[start..end]
        .iter()
        .position(|line| message(line).starts_with("Call trace:"))
        .map(|at| start + at);
    let function = trace.and_then(|at| {
        lines[at + 1..end]
            .iter()
            .find_map(|line| caller_frame(line))
    });
    let mut phrase = match function {
        Some(function) => format!("{kind} in {function}"),
        None => format!("{kind}: {}", short(message(lines[start]))),
    };
    if let Some(bug) = bug.filter(|bug| *bug != start) {
        phrase.push_str(&format!("; before it: {}", short(message(lines[bug]))));
    }
    Some(phrase)
}

/// `text` cut to a length a status line can hold.
fn short(text: &str) -> String {
    const MOST: usize = 80;
    let text = text.trim();
    if text.chars().count() <= MOST {
        return text.to_string();
    }
    let cut: String = text.chars().take(MOST).collect();
    format!("{}...", cut.trim_end())
}

/// The title and the sentence before the reason of the notice that Android stopped
/// unexpectedly, in `lang`.
fn words(lang: UiLanguage) -> (&'static str, &'static str) {
    match lang {
        UiLanguage::English => (
            "Android stopped unexpectedly",
            "Open an app to start Android again. Reason: {why}",
        ),
        UiLanguage::Korean => (
            "Android가 예기치 않게 중지됨",
            "앱을 열면 Android가 다시 시작됩니다. 이유: {why}",
        ),
        UiLanguage::Japanese => (
            "Android が予期せず停止しました",
            "アプリを開くと Android が再び起動します。理由: {why}",
        ),
        UiLanguage::ChineseSimplified => (
            "Android 意外停止",
            "打开一个应用即可再次启动 Android。原因：{why}",
        ),
        UiLanguage::ChineseTraditional => (
            "Android 意外停止",
            "開啟一個應用程式即可再次啟動 Android。原因：{why}",
        ),
        UiLanguage::Spanish => (
            "Android se detuvo inesperadamente",
            "Abre una aplicación para volver a iniciar Android. Motivo: {why}",
        ),
        UiLanguage::French => (
            "Android s'est arrêté de manière inattendue",
            "Ouvrez une application pour redémarrer Android. Raison : {why}",
        ),
        UiLanguage::German => (
            "Android wurde unerwartet beendet",
            "Öffnen Sie eine App, um Android neu zu starten. Grund: {why}",
        ),
        UiLanguage::PortugueseBrazil => (
            "O Android parou inesperadamente",
            "Abra um app para iniciar o Android novamente. Motivo: {why}",
        ),
        UiLanguage::Russian => (
            "Android неожиданно остановился",
            "Откройте приложение, чтобы снова запустить Android. Причина: {why}",
        ),
        UiLanguage::Italian => (
            "Android si è interrotto inaspettatamente",
            "Apri un'app per avviare di nuovo Android. Motivo: {why}",
        ),
        UiLanguage::Vietnamese => (
            "Android đã dừng đột ngột",
            "Mở một ứng dụng để khởi động lại Android. Lý do: {why}",
        ),
        UiLanguage::Indonesian => (
            "Android berhenti secara tak terduga",
            "Buka aplikasi untuk memulai Android lagi. Alasan: {why}",
        ),
        UiLanguage::Turkish => (
            "Android beklenmedik şekilde durdu",
            "Android'i yeniden başlatmak için bir uygulama açın. Neden: {why}",
        ),
    }
}

/// The title of the notice that Android stopped unexpectedly, in `lang`.
pub fn crash_title(lang: UiLanguage) -> &'static str {
    words(lang).0
}

/// What a surface says, in `lang`, about Android having crashed for `why`.
pub fn crash_text(lang: UiLanguage, why: &str) -> String {
    let (title, text) = words(lang);
    format!("{title}. {}", text.replace("{why}", why))
}

/// The text a surface shows for a status's last reason, in `lang`, when it is a crash.
pub fn localized_crash(lang: UiLanguage, reason: &str) -> Option<String> {
    crash_reason(reason).map(|why| crash_text(lang, why))
}

/// The host's notice that Android stopped unexpectedly for `why`, in `lang`.
pub fn notification(why: &str, lang: UiLanguage) -> Notification {
    let (title, text) = words(lang);
    Notification {
        key: NOTICE_KEY.into(),
        package_name: String::new(),
        app_label: crate::update::alerts::SHOWN_AS.into(),
        title: title.into(),
        text: text.replace("{why}", why),
        icon_png: Vec::new(),
        importance: 4,
        high_priority: false,
        clearable: true,
        when_millis: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis() as i64)
            .unwrap_or(0),
        buttons: Vec::new(),
    }
}

/// Follows the status a window manager reads and says when Android has just stopped because
/// it crashed, once per stop.
#[derive(Debug, Default)]
pub struct CrashNotices {
    /// The state last seen, and its reason.
    last: Option<(SessionState, Option<String>)>,
}

impl CrashNotices {
    pub fn new() -> CrashNotices {
        CrashNotices::default()
    }

    /// Reads the status `state` with `last_reason`. Returns the crash's reason when the
    /// session has just stopped for one. A stop already there at the first look is not news:
    /// a window manager that starts after the crash does not tell it again.
    pub fn observe(&mut self, state: SessionState, last_reason: Option<&str>) -> Option<String> {
        let now = (state, last_reason.map(str::to_string));
        let before = self.last.replace(now.clone());
        let before = before?;
        if state != SessionState::Stopped || before == now {
            return None;
        }
        crash_reason(last_reason?).map(str::to_string)
    }
}

/// How often [`watch_in_background`] reads the status.
pub const WATCH_EVERY: Duration = Duration::from_secs(2);

/// Reads the status through `connector` every [`WATCH_EVERY`], from a thread of its own, and
/// hands `tell` the notice for each crash, in the language `language` gives at the time. It
/// reconnects whenever the service goes away, and runs for as long as the process does.
pub fn watch_in_background(
    mut connector: Box<dyn Connector>,
    build: String,
    language: Arc<dyn Fn() -> UiLanguage + Send + Sync>,
    tell: Arc<dyn Fn(Notification) + Send + Sync>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("dsa-crash-watch".into())
        .spawn(move || {
            let mut notices = CrashNotices::new();
            let mut said = false;
            loop {
                let mut client =
                    match HostApiClient::connect(&mut *connector, &build, Duration::from_secs(5)) {
                        Ok(client) => {
                            said = false;
                            client
                        }
                        Err(why) => {
                            if !said {
                                log(&format!(
                                    "a crash of Android is not told until the service answers: \
                                     {why}"
                                ));
                                said = true;
                            }
                            thread::sleep(WATCH_EVERY);
                            continue;
                        }
                    };
                while let Ok(status) = client.status() {
                    if let Some(why) = notices.observe(status.state, status.last_reason.as_deref())
                    {
                        log(&format!("Android crashed: {why}; the user is told"));
                        tell(notification(&why, language()));
                    }
                    thread::sleep(WATCH_EVERY);
                }
                thread::sleep(WATCH_EVERY);
            }
        })
}
