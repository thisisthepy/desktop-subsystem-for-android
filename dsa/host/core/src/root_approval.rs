//! Answering an Android app's su call on the host, and asking the host's administrator.
//!
//! The guest's su relay sends a [`control::SuRequest`] for each su call and waits up to
//! [`ANSWER_TIMEOUT`](crate::root_elevation::ANSWER_TIMEOUT) for the [`control::SuDecision`].
//! [`RootApproval`] makes that decision for one user's Android: it asks the [`Policy`] first,
//! and only when the policy says the host's
//! administrator has to approve does it show the platform's administrator prompt
//! ([`ApprovalPrompt`]: UAC on Windows, Authorization Services on macOS, polkit on Linux). An
//! approval starts the app's grace, so the app's next su call within it is not asked again.
//!
//! The prompt has [`PROMPT_TIMEOUT`] to answer, shorter than su's own limit, so the host's
//! "timed out" reaches su. It runs on a thread of its own: past its time the request has timed
//! out, the prompt is withdrawn where the platform can withdraw it, and its late answer is
//! ignored. A prompt that panics is a refusal, not a crash. Prompts go one at a time, so a
//! second call from an app waits for the first to be answered and is then approved within the
//! grace that answer started.
//!
//! The same prompt approves moving an app to the level that keeps root without asking
//! ([`RootApproval::approve_keep_root`]), so a user who is not an administrator can still
//! have an administrator approve it in their own session.
//!
//! Every decision goes to the host log with the app, the time, the command and the outcome,
//! so the computer's owner can read back what ran as root.
//!
//! The policy is rebuilt from the settings at each start of Android ([`RootApproval::restart`]),
//! which clears every grace. Between starts the per-app levels are read again at each request,
//! so a level changed in the settings applies to the next su call and clears that app's grace;
//! turning the whole feature off stops every approval at once, while turning it on applies only
//! from the next start, as the guest's su is set up at boot.
//!
//! The prompt is the only part that names a host OS API, so everything here runs and is
//! tested on every platform with a fake prompt.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use dsa_protocol::control;

use crate::resize_trace::utc_stamp;
use crate::root_elevation::{AppLevel, Decision, DecisionDenied, Level, Policy};
use crate::settings::SettingsStore;
use crate::tasks::AppInfo;

/// The file in a [`FolderApprover`]'s folder where each asked request is appended as one JSON
/// line.
pub const REQUESTS_FILE: &str = "requests.jsonl";

/// The file in a [`FolderApprover`]'s folder that holds the administrator's next answer (`allow`,
/// `deny`, `cancel` or `timeout`).
pub const ANSWER_FILE: &str = "answer";

const FOLDER_POLL: Duration = Duration::from_millis(25);

/// How long the host's administrator prompt may stay open. Shorter than the guest's
/// [`ANSWER_TIMEOUT`](crate::root_elevation::ANSWER_TIMEOUT), so the host's "timed out"
/// reaches su (and the log) before su gives up on its own.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(55);

/// How often a request that waits for another prompt to close looks again.
const TURN_POLL: Duration = Duration::from_millis(50);

/// What the administrator is asked to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Purpose {
    /// One su call: the command, or a shell.
    #[default]
    Run,
    /// Letting the app run anything as root from now on without asking each time.
    KeepRoot,
}

/// What the administrator prompt is asked about, with what the prompt shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApprovalRequest {
    /// The guest's ID of the su call, unique while its daemon runs; 0 for a level change.
    pub request_id: u64,
    /// The calling app, with its name and icon as the guest last listed them. An app the guest
    /// has not listed has its package as its name and no icon.
    pub app: AppInfo,
    /// The app's Android uid; 0 when unknown (a level change).
    pub uid: u32,
    /// The command su runs, split as a shell would; empty for an interactive shell.
    pub command: Vec<String>,
    /// Whether su has a terminal (an interactive shell).
    pub interactive: bool,
    pub purpose: Purpose,
    /// When the calling app shares its Android user ID (`android:sharedUserId`) with other apps,
    /// every app in the group, so the prompt shows all of them and says they share a user ID.
    pub shared_apps: Vec<AppInfo>,
}

impl ApprovalRequest {
    /// The request a su call makes. `app` is the app as the guest last listed it.
    pub fn from_su(request: &control::SuRequest, app: Option<&AppInfo>) -> ApprovalRequest {
        let packages = package_group(&request.package);
        let shared_apps = if packages.len() > 1 {
            packages
                .iter()
                .map(|pkg| match app {
                    Some(info) if info.package_name == *pkg => info.clone(),
                    _ => AppInfo {
                        package_name: (*pkg).to_string(),
                        ..Default::default()
                    },
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut main_app = app.cloned().unwrap_or_else(|| AppInfo {
            package_name: request.package.clone(),
            ..Default::default()
        });
        if packages.len() > 1 {
            main_app.package_name = request.package.clone();
        }
        ApprovalRequest {
            request_id: request.request_id,
            app: main_app,
            uid: request.uid,
            command: request.command.clone(),
            interactive: request.interactive,
            purpose: Purpose::Run,
            shared_apps,
        }
    }

    /// Sets the apps in a shared user ID group, with their names and icons as the guest listed
    /// them.
    pub fn with_shared_apps(mut self, shared_apps: Vec<AppInfo>) -> ApprovalRequest {
        if shared_apps.len() > 1 {
            self.app.package_name = shared_apps
                .iter()
                .map(|app| app.package_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if self.app.label.is_empty() {
                if let Some(first) = shared_apps.first() {
                    self.app.label = first.label.clone();
                }
            }
            if self.app.icon_png.is_empty() {
                if let Some(first) = shared_apps.iter().find(|app| !app.icon_png.is_empty()) {
                    self.app.icon_png = first.icon_png.clone();
                }
            }
        }
        self.shared_apps = shared_apps;
        self
    }

    /// The request to let `app` keep root without asking each time.
    pub fn keep_root(app: AppInfo) -> ApprovalRequest {
        ApprovalRequest {
            app,
            purpose: Purpose::KeepRoot,
            ..Default::default()
        }
    }

    /// The requesting app's package.
    pub fn package(&self) -> &str {
        &self.app.package_name
    }

    /// The app's launcher icon as a PNG image; empty when the guest has not drawn one.
    pub fn icon_png(&self) -> &[u8] {
        &self.app.icon_png
    }

    /// The app's name for the prompt and the log: its label, else its package. `adb shell`
    /// is named as such, whatever the shell package's label.
    pub fn app_name(&self) -> &str {
        if crate::root_elevation::is_adb_shell(self.package()) {
            return crate::root_elevation::ADB_SHELL_NAME;
        }
        let label = self.app.label.trim();
        if label.is_empty() {
            &self.app.package_name
        } else {
            label
        }
    }

    /// What is asked for, in words for a prompt that shows it on a line of its own: the
    /// command as a shell would read it, "an interactive shell", or keeping root.
    pub fn command_text(&self) -> String {
        match self.purpose {
            Purpose::Run => command_text(&self.command, self.interactive),
            Purpose::KeepRoot => "any command, from now on without asking each time".to_string(),
        }
    }

    /// The command as one line, each word quoted when it needs to be. Empty for a shell.
    pub fn command_line(&self) -> String {
        self.command
            .iter()
            .map(|word| quote_word(word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// What the app wants, in words for a sentence and the log: the command in backticks, or
    /// a shell.
    pub fn what(&self) -> String {
        match self.purpose {
            Purpose::KeepRoot => "root without asking each time".to_string(),
            Purpose::Run if self.command.is_empty() => command_text(&[], self.interactive),
            Purpose::Run => format!("`{}`", self.command_line()),
        }
    }

    /// The sentence the administrator prompt shows: the app by name and package, and what it
    /// wants. When several apps share the user ID, it lists all of them and says they share a
    /// user ID.
    pub fn message(&self) -> String {
        if self.shared_apps.len() > 1 {
            let names = self
                .shared_apps
                .iter()
                .map(|app| {
                    let label = app.label.trim();
                    if label.is_empty() || label == app.package_name {
                        app.package_name.clone()
                    } else {
                        format!("{label} ({})", app.package_name)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            return match self.purpose {
                Purpose::KeepRoot => format!(
                    "The Android apps {names} share a user ID and want to run commands as root \
                     without asking each time."
                ),
                Purpose::Run if self.command.is_empty() => format!(
                    "The Android apps {names} share a user ID and want to open a root shell."
                ),
                Purpose::Run => format!(
                    "The Android apps {names} share a user ID and want to run {} as root.",
                    self.what()
                ),
            };
        }
        let app = if crate::root_elevation::is_adb_shell(self.package()) {
            self.app_name().to_string()
        } else if self.app_name() == self.package() {
            format!("The Android app {}", self.package())
        } else {
            format!("The Android app {} ({})", self.app_name(), self.package())
        };
        match self.purpose {
            Purpose::KeepRoot => {
                format!("{app} wants to run commands as root without asking each time.")
            }
            Purpose::Run if self.command.is_empty() => format!("{app} wants to open a root shell."),
            Purpose::Run => format!("{app} wants to run {} as root.", self.what()),
        }
    }
}

/// Splits a su request's package field into the packages of a shared user ID group: the guest
/// names every app that runs under the caller's uid, separated by commas.
pub fn package_group(package: &str) -> Vec<&str> {
    package
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// What su runs, in words: the command with each word that needs it quoted, or "an
/// interactive shell" when there is no command.
pub fn command_text(command: &[String], interactive: bool) -> String {
    if command.is_empty() {
        return if interactive {
            "an interactive shell".to_string()
        } else {
            "a shell".to_string()
        };
    }
    command
        .iter()
        .map(|word| quote_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A word quoted for a shell when it holds anything but plain characters.
fn quote_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// How the platform's administrator prompt answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAnswer {
    /// An administrator approved: the platform confirmed the answer came with administrator
    /// rights (an elevated process, an authorization right, a polkit decision).
    Approved,
    /// The prompt refused: wrong credentials, a refusal, or the account may not approve.
    Denied,
    /// The person dismissed the prompt without answering.
    Cancelled,
    /// The person at the computer is not one of its administrators, and no administrator
    /// answered for them.
    NotAdministrator,
    /// Nobody answered within the time the prompt had.
    TimedOut,
    /// The prompt could not be shown, or its answer could not be read; the detail says why.
    Unavailable(String),
}

/// The host OS's administrator prompt for one user.
pub trait ApprovalPrompt: Send + Sync {
    /// Shows the prompt for `request` in the user's session, naming the app and showing its
    /// icon where the platform can, and waits at most `timeout` for the answer. It must not
    /// approve on anything less than an answer made with administrator rights. It runs on a
    /// thread of its own, so a prompt that does not return in time only costs that thread.
    fn ask(&self, request: &ApprovalRequest, timeout: Duration) -> PromptAnswer;

    /// Takes down the prompt for `request` after it timed out, where the platform can. Its
    /// later answer, if any, is ignored either way.
    fn withdraw(&self, request: &ApprovalRequest) {
        let _ = request;
    }
}

/// An [`ApprovalPrompt`] backed by a folder on disk, for automated boot checks where no desktop
/// prompt is open. Each prompt appends one JSON line to [`REQUESTS_FILE`] and reads its answer
/// from [`ANSWER_FILE`].
#[derive(Debug, Clone)]
pub struct FolderApprover {
    dir: PathBuf,
}

impl FolderApprover {
    pub fn new(dir: &Path) -> FolderApprover {
        FolderApprover {
            dir: dir.to_path_buf(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn record_request(&self, request: &ApprovalRequest) -> Result<(), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("creating {}: {e}", self.dir.display()))?;
        let path = self.dir.join(REQUESTS_FILE);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("opening {}: {e}", path.display()))?;
        let command = request
            .command
            .iter()
            .map(|arg| format!("\"{}\"", escape_json(arg)))
            .collect::<Vec<_>>()
            .join(",");
        let line = format!(
            "{{\"request_id\":{},\"package\":\"{}\",\"app_name\":\"{}\",\"uid\":{},\"icon_bytes\":{},\"command\":[{}],\"interactive\":{},\"message\":\"{}\"}}\n",
            request.request_id,
            escape_json(request.package()),
            escape_json(request.app_name()),
            request.uid,
            request.icon_png().len(),
            command,
            request.interactive,
            escape_json(&request.message()),
        );
        file.write_all(line.as_bytes())
            .map_err(|e| format!("writing {}: {e}", path.display()))
    }
}

impl ApprovalPrompt for FolderApprover {
    fn ask(&self, request: &ApprovalRequest, timeout: Duration) -> PromptAnswer {
        if let Err(why) = self.record_request(request) {
            return PromptAnswer::Unavailable(why);
        }
        let path = self.dir.join(ANSWER_FILE);
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(text) = fs::read_to_string(&path) {
                let word = text.trim();
                if !word.is_empty() {
                    return match word {
                        "allow" | "approve" | "approved" => PromptAnswer::Approved,
                        "deny" | "refuse" | "refused" => PromptAnswer::Denied,
                        "cancel" | "cancelled" | "dismiss" => PromptAnswer::Cancelled,
                        "not-admin" | "not_administrator" => PromptAnswer::NotAdministrator,
                        "timeout" => {
                            let left = deadline.saturating_duration_since(Instant::now());
                            if !left.is_zero() {
                                thread::sleep(left);
                            }
                            PromptAnswer::TimedOut
                        }
                        other => PromptAnswer::Unavailable(format!(
                            "unknown answer {other:?} in {}",
                            path.display()
                        )),
                    };
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return PromptAnswer::TimedOut;
            }
            thread::sleep(FOLDER_POLL.min(deadline - now));
        }
    }
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The end of one request to the administrator prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    /// Refused by the prompt.
    Denied,
    /// Dismissed by the person: denied too.
    Cancelled,
    /// The person is not an administrator and none answered: denied.
    NotAdministrator,
    /// No answer within the prompt's time.
    TimedOut,
    /// The prompt could not be shown: denied, with the reason.
    Unavailable(String),
}

impl Outcome {
    fn of(answer: PromptAnswer) -> Outcome {
        match answer {
            PromptAnswer::Approved => Outcome::Approved,
            PromptAnswer::Denied => Outcome::Denied,
            PromptAnswer::Cancelled => Outcome::Cancelled,
            PromptAnswer::NotAdministrator => Outcome::NotAdministrator,
            PromptAnswer::TimedOut => Outcome::TimedOut,
            PromptAnswer::Unavailable(why) => Outcome::Unavailable(why),
        }
    }

    /// The result the policy's caller gets: approved, or denied with the reason.
    pub fn result(&self) -> Result<(), DecisionDenied> {
        match self {
            Outcome::Approved => Ok(()),
            Outcome::Denied
            | Outcome::Cancelled
            | Outcome::NotAdministrator
            | Outcome::Unavailable(_) => Err(DecisionDenied::Refused),
            Outcome::TimedOut => Err(DecisionDenied::TimedOut),
        }
    }

    /// The sentence su prints and the log keeps, empty when approved.
    pub fn reason(&self) -> String {
        match self {
            Outcome::Approved => String::new(),
            Outcome::Denied => DecisionDenied::Refused.describe().to_string(),
            Outcome::Cancelled => {
                "the computer's administrator dismissed the root request".to_string()
            }
            Outcome::NotAdministrator => "the root request needs one of the computer's \
                                          administrators, and you are not one"
                .to_string(),
            Outcome::TimedOut => DecisionDenied::TimedOut.describe().to_string(),
            Outcome::Unavailable(why) => {
                format!("the computer could not ask its administrator: {why}")
            }
        }
    }

    /// How it went, for the log.
    fn how(&self, timeout: Duration) -> String {
        match self {
            Outcome::Approved => "approved by the computer's administrator".to_string(),
            Outcome::Denied => "refused by the computer's administrator".to_string(),
            Outcome::Cancelled => "refused: the administrator prompt was dismissed".to_string(),
            Outcome::NotAdministrator => {
                "refused: the person asked is not one of the computer's administrators".to_string()
            }
            Outcome::TimedOut => format!(
                "refused: timed out with no answer within {} seconds",
                timeout.as_secs()
            ),
            Outcome::Unavailable(why) => {
                format!("refused: the administrator prompt could not be shown ({why})")
            }
        }
    }
}

/// Asks `prompt` about `request` and waits at most `timeout` for its answer. A prompt that
/// does not answer in time is withdrawn and the request has timed out; a prompt that fails
/// (its thread panics) leaves the request denied, and the host keeps running.
pub fn ask_admin(
    prompt: &Arc<dyn ApprovalPrompt>,
    request: &ApprovalRequest,
    timeout: Duration,
) -> Outcome {
    let (tx, rx) = mpsc::channel();
    let (asked, question) = (Arc::clone(prompt), request.clone());
    let spawned = thread::Builder::new()
        .name("dsa-admin-prompt".into())
        .spawn(move || {
            let _ = tx.send(asked.ask(&question, timeout));
        });
    if let Err(why) = spawned {
        return Outcome::Unavailable(format!("no thread for the prompt: {why}"));
    }
    match rx.recv_timeout(timeout) {
        Ok(answer) => Outcome::of(answer),
        Err(RecvTimeoutError::Timeout) => {
            prompt.withdraw(request);
            Outcome::TimedOut
        }
        Err(RecvTimeoutError::Disconnected) => {
            Outcome::Unavailable("the prompt ended without an answer".to_string())
        }
    }
}

/// Says whether the feature is on and each app's level, as the settings have them now.
pub type LevelSource = Arc<dyn Fn() -> (bool, Vec<AppLevel>) + Send + Sync>;

/// A [`LevelSource`] that reads `user`'s levels from `store` at each request. A level the
/// store cannot read is off.
pub fn levels_from_settings(store: Arc<dyn SettingsStore>, user: &str) -> LevelSource {
    let user = user.to_string();
    Arc::new(move || crate::root_elevation::stored(&*store, &user))
}

/// Where the decisions are written: the host log.
pub type LogLine = Arc<dyn Fn(&str) + Send + Sync>;

/// The host log of the service: its standard error.
pub fn service_log() -> LogLine {
    Arc::new(|line: &str| eprintln!("{line}"))
}

/// The policy for the running boot, and whether the boot was started with the feature on and
/// has not seen it turned off since.
struct BootPolicy {
    booted_enabled: bool,
    policy: Policy,
}

/// Decides each su call of one user's Android.
pub struct RootApproval {
    user: String,
    levels: LevelSource,
    prompt: Arc<dyn ApprovalPrompt>,
    log: LogLine,
    timeout: Duration,
    boot: Mutex<BootPolicy>,
    /// Held while a prompt is open, so the user sees one prompt at a time and a second call
    /// from the same app finds the grace the first one's approval started.
    turn: Mutex<()>,
}

impl RootApproval {
    /// Decides `user`'s su calls by the settings in `store`, asking through `prompt`, and
    /// writes each decision with `log`. The policy is that of a boot started now.
    pub fn new(
        user: &str,
        store: Arc<dyn SettingsStore>,
        prompt: Arc<dyn ApprovalPrompt>,
        log: LogLine,
    ) -> RootApproval {
        let levels = levels_from_settings(store, user);
        RootApproval::with_levels(user, prompt, levels, log)
    }

    /// The same, with the switch and the levels read from `levels` at each request.
    pub fn with_levels(
        user: &str,
        prompt: Arc<dyn ApprovalPrompt>,
        levels: LevelSource,
        log: LogLine,
    ) -> RootApproval {
        let approval = RootApproval {
            user: user.to_string(),
            levels,
            prompt,
            log,
            timeout: PROMPT_TIMEOUT,
            boot: Mutex::new(BootPolicy {
                booted_enabled: false,
                policy: Policy::new(),
            }),
            turn: Mutex::new(()),
        };
        approval.restart();
        approval
    }

    /// Gives the prompt `timeout` instead of [`PROMPT_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> RootApproval {
        self.timeout = timeout;
        self
    }

    /// Writes each decision with `log` instead.
    pub fn with_log(mut self, log: LogLine) -> RootApproval {
        self.log = log;
        self
    }

    /// Starts the policy of a new boot of Android: the feature as the settings say now, the
    /// stored levels, and no grace for any app.
    pub fn restart(&self) {
        let (enabled, levels) = (self.levels)();
        *self.boot() = BootPolicy {
            booted_enabled: enabled,
            policy: Policy::with_levels(enabled, levels),
        };
    }

    fn boot(&self) -> MutexGuard<'_, BootPolicy> {
        self.boot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Brings the boot's policy up to the settings: the feature turned off since the boot
    /// stops approvals, and a level changed since the last request applies (and clears that
    /// app's grace). Returns the decision for `package` now, and its level.
    ///
    /// Apps that share an Android user ID (`package` holds their comma-separated names) are
    /// decided as one group: if any app in the group is explicitly `Off` in the settings, the
    /// whole group is `Off` and its grace is cleared; otherwise the group takes its level and
    /// grace together.
    fn decide_now(&self, package: &str) -> (Decision, Level) {
        let (enabled, levels) = (self.levels)();
        let mut boot = self.boot();
        // Off applies at once and stays off for the rest of the boot: the guest's su guard
        // and relay are set up at boot, so turning the feature on again waits for the next
        // start of Android.
        if !enabled {
            boot.booted_enabled = false;
        }
        let enabled = boot.booted_enabled;
        let policy = &mut boot.policy;
        if policy.enabled() != enabled {
            policy.set_enabled(enabled);
        }
        let group = package_group(package);
        let group_has_explicit_off = group.len() > 1
            && group.iter().any(|member| {
                levels
                    .iter()
                    .any(|app| app.package == *member && app.level == Level::Off)
            });
        for app in policy.app_levels() {
            if !levels
                .iter()
                .any(|a| a.package == app.package && a.level != Level::Off)
            {
                policy.set_level(&app.package, Level::Off);
            }
        }
        for app in &levels {
            if policy.level(&app.package) != app.level {
                policy.set_level(&app.package, app.level);
            }
        }
        if group.len() <= 1 {
            let member = group.first().copied().unwrap_or(package);
            return (policy.decide(member, Instant::now()), policy.level(member));
        }
        if !enabled || group_has_explicit_off {
            for member in &group {
                if policy.level(member) != Level::Off {
                    policy.set_level(member, Level::Off);
                }
            }
            return (Decision::Deny, Level::Off);
        }
        let mut group_level = Level::Off;
        for member in &group {
            match policy.level(member) {
                Level::Remember => group_level = Level::Remember,
                Level::Ask if group_level == Level::Off => group_level = Level::Ask,
                _ => {}
            }
        }
        let now = Instant::now();
        let decision = match group_level {
            Level::Off => Decision::Deny,
            Level::Remember => Decision::ApproveWithoutPrompt,
            Level::Ask => {
                if group.iter().any(|member| policy.in_grace(member, now)) {
                    Decision::ApproveWithoutPrompt
                } else {
                    Decision::PromptHost
                }
            }
        };
        (decision, group_level)
    }

    fn record_approval(&self, package: &str) {
        let mut boot = self.boot();
        let now = Instant::now();
        let group = package_group(package);
        if group.is_empty() {
            boot.policy.approved(package, now);
            return;
        }
        for member in group {
            boot.policy.approved(member, now);
        }
    }

    /// Decides one su call. `app` is the calling app as the guest last listed it, for the name
    /// and icon the prompt shows. Blocks while the prompt is open, at most the prompt's time.
    pub fn decide(
        &self,
        request: &control::SuRequest,
        app: Option<&AppInfo>,
    ) -> control::SuDecision {
        self.answer(&ApprovalRequest::from_su(request, app))
    }

    /// Answers one su request: by the policy, and by the administrator prompt when the policy
    /// asks for one. Writes one line with the decision to the log.
    pub fn answer(&self, request: &ApprovalRequest) -> control::SuDecision {
        let deadline = Instant::now() + self.timeout;
        let package = request.package();
        let (decision, level) = self.decide_now(package);
        let result = match decision {
            Decision::Deny => Err((
                DecisionDenied::NotAllowed.describe().to_string(),
                "refused: the app may not use root".to_string(),
            )),
            Decision::ApproveWithoutPrompt => Ok(self.approve_without_prompt(package, level)),
            Decision::PromptHost => self.prompt(request, deadline),
        };
        let (approved, reason, how) = match result {
            Ok(how) => (true, String::new(), how),
            Err((reason, how)) => (false, reason, how),
        };
        self.write(request, &how);
        control::SuDecision {
            request_id: request.request_id,
            approved,
            reason,
        }
    }

    /// A call the policy approves at once: a remember app, which also renews its standing
    /// state, or an app within its grace. Says how, for the log.
    fn approve_without_prompt(&self, package: &str, level: Level) -> String {
        if level == Level::Remember {
            self.record_approval(package);
            "approved without asking (the app may keep root)".to_string()
        } else {
            "approved without asking (within the grace of an earlier approval)".to_string()
        }
    }

    /// Waits for the prompt's turn, decides again (an approval in another prompt may have
    /// started the app's grace meanwhile), and asks the administrator. `Ok` says how it was
    /// approved; `Err` gives the reason su prints and how it went for the log.
    fn prompt(
        &self,
        request: &ApprovalRequest,
        deadline: Instant,
    ) -> Result<String, (String, String)> {
        let package = request.package();
        let timed_out = || {
            let outcome = Outcome::TimedOut;
            (outcome.reason(), outcome.how(self.timeout))
        };
        let Some(_turn) = self.wait_for_turn(deadline) else {
            return Err(timed_out());
        };
        let (decision, level) = self.decide_now(package);
        match decision {
            Decision::Deny => {
                return Err((
                    DecisionDenied::NotAllowed.describe().to_string(),
                    "refused: the app may not use root".to_string(),
                ))
            }
            Decision::ApproveWithoutPrompt => {
                return Ok(self.approve_without_prompt(package, level))
            }
            Decision::PromptHost => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out());
        }
        let outcome = ask_admin(&self.prompt, request, left);
        if outcome != Outcome::Approved {
            return Err((outcome.reason(), outcome.how(self.timeout)));
        }
        // The feature or the level may have changed while the prompt was open.
        if self.decide_now(package).0 == Decision::Deny {
            return Err((
                DecisionDenied::NotAllowed.describe().to_string(),
                "refused: the app may no longer use root".to_string(),
            ));
        }
        self.record_approval(package);
        Ok(outcome.how(self.timeout))
    }

    /// The prompt's turn, or `None` when `deadline` passes first.
    fn wait_for_turn(&self, deadline: Instant) -> Option<MutexGuard<'_, ()>> {
        loop {
            match self.turn.try_lock() {
                Ok(turn) => return Some(turn),
                Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(TURN_POLL);
                }
            }
        }
    }

    /// Asks the administrator to let `app` keep root without asking each time, as moving it
    /// to that level needs. Blocks at most the prompt's time. `Err` says why not.
    pub fn approve_keep_root(&self, app: AppInfo) -> Result<(), String> {
        let request = ApprovalRequest::keep_root(app);
        let deadline = Instant::now() + self.timeout;
        let outcome = match self.wait_for_turn(deadline) {
            None => Outcome::TimedOut,
            Some(_turn) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    Outcome::TimedOut
                } else {
                    ask_admin(&self.prompt, &request, left)
                }
            }
        };
        self.write(&request, &outcome.how(self.timeout));
        match outcome {
            Outcome::Approved => Ok(()),
            other => Err(other.reason()),
        }
    }

    /// One line in the host log about `request`, with the time, the user and the app.
    fn write(&self, request: &ApprovalRequest, how: &str) {
        let since_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let id = if request.purpose == Purpose::Run {
            format!(" (request {})", request.request_id)
        } else {
            String::new()
        };
        (self.log)(&format!(
            "root: {} {}: {} ({}, uid {}) asked to run {}{id}: {how}",
            utc_stamp(since_epoch),
            self.user,
            request.app_name(),
            request.package(),
            request.uid,
            request.what(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A prompt that gives the same answer each time, after `delay`, and counts its prompts.
    struct FakePrompt {
        answer: PromptAnswer,
        delay: Duration,
        asked: AtomicUsize,
        withdrawn: AtomicUsize,
        seen: Mutex<Vec<ApprovalRequest>>,
    }

    impl FakePrompt {
        fn new(answer: PromptAnswer) -> Arc<FakePrompt> {
            FakePrompt::slow(answer, Duration::ZERO)
        }
        fn slow(answer: PromptAnswer, delay: Duration) -> Arc<FakePrompt> {
            Arc::new(FakePrompt {
                answer,
                delay,
                asked: AtomicUsize::new(0),
                withdrawn: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    impl ApprovalPrompt for FakePrompt {
        fn ask(&self, request: &ApprovalRequest, _timeout: Duration) -> PromptAnswer {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(request.clone());
            thread::sleep(self.delay);
            self.answer.clone()
        }
        fn withdraw(&self, _request: &ApprovalRequest) {
            self.withdrawn.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct PanickingPrompt;

    impl ApprovalPrompt for PanickingPrompt {
        fn ask(&self, _request: &ApprovalRequest, _timeout: Duration) -> PromptAnswer {
            panic!("the prompt broke");
        }
    }

    fn levels(enabled: bool, apps: &[(&str, Level)]) -> LevelSource {
        let apps: Vec<AppLevel> = apps
            .iter()
            .map(|(package, level)| AppLevel {
                package: package.to_string(),
                level: *level,
            })
            .collect();
        Arc::new(move || (enabled, apps.clone()))
    }

    fn request(package: &str) -> ApprovalRequest {
        ApprovalRequest {
            request_id: 7,
            app: AppInfo {
                package_name: package.to_string(),
                label: "Root Checker".to_string(),
                icon_png: vec![0x89, b'P', b'N', b'G'],
                ..Default::default()
            },
            uid: 10_123,
            command: vec!["id".to_string()],
            interactive: false,
            purpose: Purpose::Run,
            shared_apps: Vec::new(),
        }
    }

    fn quiet() -> LogLine {
        Arc::new(|_: &str| {})
    }

    fn logged() -> (LogLine, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        (
            Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string())),
            lines,
        )
    }

    fn elevation(prompt: Arc<dyn ApprovalPrompt>, source: LevelSource) -> RootApproval {
        RootApproval::with_levels("ann", prompt, source, quiet())
    }

    #[test]
    fn fr35_each_prompt_answer_maps_to_approved_or_denied() {
        let cases = [
            (PromptAnswer::Approved, Outcome::Approved, Ok(())),
            (
                PromptAnswer::Denied,
                Outcome::Denied,
                Err(DecisionDenied::Refused),
            ),
            (
                PromptAnswer::Cancelled,
                Outcome::Cancelled,
                Err(DecisionDenied::Refused),
            ),
            (
                PromptAnswer::NotAdministrator,
                Outcome::NotAdministrator,
                Err(DecisionDenied::Refused),
            ),
            (
                PromptAnswer::TimedOut,
                Outcome::TimedOut,
                Err(DecisionDenied::TimedOut),
            ),
            (
                PromptAnswer::Unavailable("no polkit agent".into()),
                Outcome::Unavailable("no polkit agent".into()),
                Err(DecisionDenied::Refused),
            ),
        ];
        for (answer, outcome, result) in cases {
            let prompt: Arc<dyn ApprovalPrompt> = FakePrompt::new(answer);
            let got = ask_admin(&prompt, &request("com.ask.app"), Duration::from_secs(5));
            assert_eq!(got, outcome);
            assert_eq!(got.result(), result);
        }
    }

    #[test]
    fn fr35_a_prompt_that_does_not_answer_in_time_times_out_and_is_withdrawn() {
        let fake = FakePrompt::slow(PromptAnswer::Approved, Duration::from_secs(2));
        let prompt: Arc<dyn ApprovalPrompt> = fake.clone();
        let got = ask_admin(&prompt, &request("com.ask.app"), Duration::from_millis(50));
        assert_eq!(got, Outcome::TimedOut);
        assert_eq!(got.result(), Err(DecisionDenied::TimedOut));
        assert_eq!(fake.withdrawn.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fr35_a_prompt_that_breaks_denies_and_the_host_keeps_running() {
        let prompt: Arc<dyn ApprovalPrompt> = Arc::new(PanickingPrompt);
        let got = ask_admin(&prompt, &request("com.ask.app"), Duration::from_secs(5));
        assert!(matches!(got, Outcome::Unavailable(_)));
        assert_eq!(got.result(), Err(DecisionDenied::Refused));
    }

    #[test]
    fn fr35_an_ask_app_is_approved_by_the_prompt_and_then_within_grace() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let (log, lines) = logged();
        let root =
            elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)])).with_log(log);
        let first = root.answer(&request("com.ask.app"));
        assert!(first.approved);
        assert_eq!(first.request_id, 7);
        assert!(first.reason.is_empty());
        let second = root.answer(&request("com.ask.app"));
        assert!(second.approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].app_name(), "Root Checker");
        assert_eq!(
            seen[0].message(),
            "The Android app Root Checker (com.ask.app) wants to run `id` as root."
        );
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Root Checker (com.ask.app, uid 10123)"));
        assert!(lines[0].contains("`id`"));
        assert!(lines[0].contains("approved by the computer's administrator"));
        // The time, as an ISO 8601 UTC stamp.
        assert!(lines[0].contains('T') && lines[0].contains("Z "));
        assert!(lines[1].contains("within the grace"));
    }

    #[test]
    fn fr35_adb_shell_asks_for_root_like_an_app() {
        use crate::root_elevation::ADB_SHELL_PACKAGE;
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let root = elevation(
            fake.clone(),
            levels(true, &[(ADB_SHELL_PACKAGE, Level::Ask)]),
        );
        let mut shell = request(ADB_SHELL_PACKAGE);
        shell.app.label = "Shell".into();
        shell.uid = 2000;
        assert!(root.answer(&shell).approved);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].app_name(), "adb shell");
        assert_eq!(seen[0].message(), "adb shell wants to run `id` as root.");
        drop(seen);
        // Refused, su fails.
        let refusing = FakePrompt::new(PromptAnswer::Denied);
        let root = elevation(refusing, levels(true, &[(ADB_SHELL_PACKAGE, Level::Ask)]));
        assert!(!root.answer(&shell).approved);
        // Left out of the levels (developer mode off), adb shell may not ask.
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let root = elevation(fake.clone(), levels(true, &[]));
        assert!(!root.answer(&shell).approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn fr35_a_refused_dismissed_or_timed_out_prompt_fails_su_with_its_reason() {
        for (answer, delay, words) in [
            (PromptAnswer::Denied, Duration::ZERO, "refused"),
            (PromptAnswer::Cancelled, Duration::ZERO, "dismissed"),
            (PromptAnswer::Approved, Duration::from_secs(2), "timed out"),
        ] {
            let fake = FakePrompt::slow(answer, delay);
            let (log, lines) = logged();
            let root = elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)]))
                .with_timeout(Duration::from_millis(100))
                .with_log(log);
            let decision = root.answer(&request("com.ask.app"));
            assert!(!decision.approved);
            assert!(decision.reason.contains(words), "{}", decision.reason);
            assert!(lines.lock().unwrap()[0].contains("refused"));
            // No grace was started: the next request asks again.
            let _ = root.answer(&request("com.ask.app"));
            assert_eq!(fake.asked.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn fr35_remember_and_off_apps_never_reach_the_prompt() {
        let fake = FakePrompt::new(PromptAnswer::Denied);
        let (log, lines) = logged();
        let root = elevation(
            fake.clone(),
            levels(true, &[("com.remember.app", Level::Remember)]),
        )
        .with_log(log);
        assert!(root.answer(&request("com.remember.app")).approved);
        let off = root.answer(&request("com.other.app"));
        assert!(!off.approved);
        assert_eq!(off.reason, DecisionDenied::NotAllowed.describe());
        assert_eq!(fake.asked.load(Ordering::SeqCst), 0);
        let lines = lines.lock().unwrap();
        assert!(lines[0].contains("without asking"));
        assert!(lines[1].contains("may not use root"));
    }

    #[test]
    fn fr35_a_restart_clears_the_grace_and_asks_again() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let root = elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)]));
        root.answer(&request("com.ask.app"));
        root.restart();
        root.answer(&request("com.ask.app"));
        assert_eq!(fake.asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fr35_a_level_change_in_the_settings_clears_the_grace() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let level = Arc::new(Mutex::new(Level::Ask));
        let current = Arc::clone(&level);
        let source: LevelSource = Arc::new(move || {
            (
                true,
                vec![AppLevel {
                    package: "com.ask.app".into(),
                    level: *current.lock().unwrap(),
                }],
            )
        });
        let root = elevation(fake.clone(), source);
        root.answer(&request("com.ask.app"));
        *level.lock().unwrap() = Level::Off;
        assert!(!root.answer(&request("com.ask.app")).approved);
        *level.lock().unwrap() = Level::Ask;
        assert!(root.answer(&request("com.ask.app")).approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fr35_requests_that_arrive_together_get_one_prompt() {
        let fake = FakePrompt::slow(PromptAnswer::Approved, Duration::from_millis(200));
        let root = Arc::new(elevation(
            fake.clone(),
            levels(true, &[("com.ask.app", Level::Ask)]),
        ));
        let others: Vec<_> = (0..3)
            .map(|_| {
                let root = Arc::clone(&root);
                thread::spawn(move || root.answer(&request("com.ask.app")).approved)
            })
            .collect();
        for other in others {
            assert!(other.join().unwrap());
        }
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fr35_the_levels_follow_the_computers_switch_and_the_users_apps() {
        use crate::settings::{MemorySettings, SettingsStore};
        let store = Arc::new(MemorySettings::default());
        store.set_root_app("ann", "com.ask.app", "ask").unwrap();
        store
            .set_root_app("ann", "com.keep.app", "remember")
            .unwrap();
        store.set_root_app("bob", "com.bob.app", "ask").unwrap();
        let source = levels_from_settings(store.clone(), "ann");
        let (enabled, apps) = source();
        assert!(!enabled);
        assert_eq!(apps.len(), 2);
        let mut machine = store.machine();
        machine.root_elevation = true;
        store.set_machine(machine).unwrap();
        let (enabled, apps) = source();
        assert!(enabled);
        assert!(apps.contains(&AppLevel {
            package: "com.keep.app".into(),
            level: Level::Remember,
        }));
        assert!(!apps.iter().any(|a| a.package == "com.bob.app"));
    }

    #[test]
    fn fr35_the_prompt_names_a_shell_and_quotes_the_command() {
        let mut shell = request("com.ask.app");
        shell.command.clear();
        shell.interactive = true;
        shell.app.label.clear();
        assert_eq!(
            shell.message(),
            "The Android app com.ask.app wants to open a root shell."
        );
        let mut quoted = request("com.ask.app");
        quoted.command = vec!["sh".into(), "-c".into(), "echo 'hi' > /x".into()];
        assert_eq!(quoted.command_line(), r"sh -c 'echo '\''hi'\'' > /x'");
    }

    #[test]
    fn fr35_keeping_root_is_asked_of_the_administrator_under_the_apps_name() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let (log, lines) = logged();
        let root = elevation(fake.clone(), levels(true, &[])).with_log(log);
        let app = request("com.keep.app").app;
        assert_eq!(root.approve_keep_root(app.clone()), Ok(()));
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].purpose, Purpose::KeepRoot);
        assert_eq!(seen[0].icon_png(), app.icon_png.as_slice());
        assert_eq!(
            seen[0].message(),
            "The Android app Root Checker (com.keep.app) wants to run commands as root \
             without asking each time."
        );
        assert!(lines.lock().unwrap()[0].contains("approved by the computer's administrator"));

        let refusing = elevation(FakePrompt::new(PromptAnswer::Denied), levels(true, &[]));
        let refused = refusing.approve_keep_root(app).unwrap_err();
        assert!(refused.contains("refused"), "{refused}");
    }

    #[test]
    fn fr35_apps_sharing_a_user_id_are_approved_as_one_group() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let current = Arc::new(Mutex::new(vec![AppLevel {
            package: "com.shared.one".into(),
            level: Level::Ask,
        }]));
        let shared_levels = Arc::clone(&current);
        let source: LevelSource = Arc::new(move || (true, shared_levels.lock().unwrap().clone()));
        let root = elevation(fake.clone(), source);

        let su = control::SuRequest {
            request_id: 11,
            package: "com.shared.one, com.shared.two".into(),
            uid: 10_150,
            command: vec!["id".into()],
            interactive: false,
        };
        let req = ApprovalRequest::from_su(&su, None).with_shared_apps(vec![
            AppInfo {
                package_name: "com.shared.one".into(),
                label: "Shared One".into(),
                ..Default::default()
            },
            AppInfo {
                package_name: "com.shared.two".into(),
                label: "Shared Two".into(),
                ..Default::default()
            },
        ]);
        let first = root.answer(&req);
        assert!(first.approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
        let seen = fake.seen.lock().unwrap();
        assert!(seen[0].message().contains("Shared One (com.shared.one)"));
        assert!(seen[0].message().contains("Shared Two (com.shared.two)"));
        assert!(seen[0].message().contains("share a user ID"));
        drop(seen);

        // The second app in the group shares the grace started by the first approval.
        let second = root.answer(&req);
        assert!(second.approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);

        // If any app in the group is set to Off, the whole group is denied and cannot use root.
        current.lock().unwrap().push(AppLevel {
            package: "com.shared.two".into(),
            level: Level::Off,
        });
        let denied = root.answer(&req);
        assert!(!denied.approved);
        assert_eq!(denied.reason, DecisionDenied::NotAllowed.describe());
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fr35_the_folder_approver_records_each_request_and_answers_from_its_file() {
        let dir = std::env::temp_dir().join(format!(
            "dsa-folder-approver-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let approver = FolderApprover::new(&dir);
        fs::write(dir.join(ANSWER_FILE), "allow\n").unwrap();
        let answer = approver.ask(&request("com.ask.app"), Duration::from_secs(1));
        assert_eq!(answer, PromptAnswer::Approved);
        let recorded = fs::read_to_string(dir.join(REQUESTS_FILE)).unwrap();
        assert!(recorded.contains("\"package\":\"com.ask.app\""));
        assert!(recorded.contains("\"app_name\":\"Root Checker\""));
        assert!(recorded.contains("\"uid\":10123"));
        assert!(recorded.contains("\"icon_bytes\":4"));

        fs::write(dir.join(ANSWER_FILE), "deny\n").unwrap();
        let denied = approver.ask(&request("com.ask.app"), Duration::from_secs(1));
        assert_eq!(denied, PromptAnswer::Denied);

        let _ = fs::remove_dir_all(&dir);
    }
}
