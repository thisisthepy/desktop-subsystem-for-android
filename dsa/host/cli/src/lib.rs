//! `dsa`, the command line client of the local API.
//!
//! ```text
//! dsa status
//! dsa start [--wait]
//! dsa stop [--wait]
//! dsa freeze
//! dsa update [--wait]
//! dsa launch <package>
//! dsa install <file.apk> [--progress]
//! dsa apk-info <file.apk>
//! dsa open <file>
//! dsa share <file>
//! dsa tasks
//! dsa settings [set <name> <value>]
//! dsa memory
//! dsa gpus
//! dsa widgets [list | add <provider> | remove <id> | watch]
//! dsa logcat [-d] [-b <buffers>] [<tag>[:<priority>] ...]
//! dsa logs collect -o <file.zip>
//! ```
//!
//! It acts on the Android of the user who runs it: the service learns that user from the
//! operating system, so there is no option to name another one.
//!
//! `dsa launch` starts an app in a window of its own, named by its package (such as
//! `com.android.settings`). `dsa tasks` lists the app tasks Android shows as windows.
//!
//! `dsa install` installs an APK from this computer in Android, starting Android first when it
//! is not running, and says how it went: the app's package once it is installed, or the reason
//! Android gave for refusing it, with exit code 1. The installed app then appears in the Start
//! menu like any other. APKs up to 2 GiB can be installed, one at a time; a second install waits
//! for the first. With `--json` it prints `installed`, `status` (`installed`, or the reason:
//! `incompatible`, `conflicting-signature`, `invalid-apk`, `insufficient-storage`, `downgrade`,
//! `blocked` or `other`), `package` and `message`. A refusal's line on standard error starts
//! with `not installed (<reason>):`. With `--progress` it first prints a line for each step,
//! for programs such as the install window: `stage starting` while Android starts,
//! `stage sending` and then `progress <percent>` as the APK goes to Android, and
//! `stage installing` once Android has all of it.
//!
//! `dsa apk-info` reads an APK on this computer and says what it is: its package, its name and
//! icon, its version, the Android versions it is made for, the permissions it asks for and the
//! certificate it is signed with. When Android is running it also says what Android has of the
//! package, and whether installing the APK would be a new install, an update, the same version
//! again, a downgrade or an APK signed with another key; Android refuses the last two. It never
//! starts Android. With `--json` it prints `package`, `label`, `version_code`, `version_name`,
//! `min_sdk`, `min_android`, `target_sdk`, `target_android`, `size`, `permissions` (each with
//! `name`, `description` and `sensitive`), `icon` (`kind` `image` with `png` in base64, or
//! `adaptive` with a `background` and a `foreground` layer, each an `image`, a `color` as
//! `#AARRGGBB` or `unknown`), `signer` (`scheme` and `sha256`), `abis`, `android` (`running`,
//! `stopped` or `unreachable`), `installed` (`version_code`, `version_name` and `signers`, or
//! null) and `install` (`new`, `update`, `reinstall`, `downgrade`, `signature-mismatch`, or
//! null while Android is not running).
//!
//! `dsa open` sends a file from this computer to Android, which asks which app opens it; that
//! app shows in a window of its own. `dsa share` does the same for sharing the file with an
//! app. Android is started first when it is not running. Files up to 64 MiB can be sent.
//!
//! `dsa widgets list` (or `dsa widgets`) lists the widgets the apps in Android offer, each by
//! its provider (`package/class`), and the widgets placed on the desktop, each by its ID.
//! `dsa widgets add <provider>` places one; when its app asks the user to set it up first, its
//! setup shows in a window and the command waits for the user. `dsa widgets remove <id>`
//! removes one, with its window. Android is started first when it is not running. With
//! `--json`, `list` prints `providers` (each with `provider`, `package`, `label`,
//! `preview_png` in base64, its sizes in dp, `resize_mode`, `needs_configuration` and `app`,
//! the name of the app that offers it) and `widgets` (each with `id`, `provider`, `label`,
//! `app` and its `placement`), `add` prints the placed widget, and `remove` prints `removed`
//! with the widget's ID.
//!
//! `dsa widgets watch` prints the placed widgets, then a line each time one is placed or
//! removed, until it is stopped or Android stops. With `--json` each line is one JSON
//! object: `{"event":"widgets","widgets":[...]}` first, then
//! `{"event":"placed","widget":{...}}` or `{"event":"removed","id":N}`.
//!
//! `dsa freeze` stops every process of the Android until one of its windows is shown
//! again. Its notifications, alarms and widgets stop with it.
//!
//! `dsa logcat` prints Android's log as logcat's `threadtime` format does, with the year and in
//! UTC: the recent records the service holds, then each new one, until it is stopped (Ctrl+C).
//! Android sends its log only while something reads it, and nothing writes it to a file.
//! `-b` names the buffers, comma separated (`main`, `radio`, `events`, `system`, `crash`,
//! `stats`, `security`, `kernel`, or `all`; `main,system,crash` when not given). Each
//! `TAG:P` keeps the records of that tag at priority `P` and above (`V`, `D`, `I`, `W`, `E`,
//! `F`, or `S` for none), and `*:P` sets it for every other tag, as with logcat. `-d` prints
//! what Android's log holds now and ends. With `--json` each record is one JSON object on a
//! line (`time`, `time_ns`, `buffer`, `priority`, `pid`, `tid`, `uid`, `tag`, `message`, or
//! `dropped` for records that were lost because they came faster than they were read).
//!
//! `dsa logs collect -o <file.zip>` saves what a problem report needs as one zip archive:
//! Android's whole log, its tombstones, ANR traces and dropbox, pstore, a summary of the
//! system services' state, and the DSA logs of this computer that concern the user who runs
//! it. It takes a minute or more; `summary.txt` in the archive says what could not be gathered.
//!
//! `dsa settings` shows the settings the service keeps: the GPU the window manager renders on
//! (`render-gpu`: `display`, `integrated`, `discrete` or part of a GPU's name), whether
//! Android may be frozen when idle (`freeze`), whether text copied on the host can be pasted in
//! Android and the reverse (`clipboard`, on by default), whether web links apps in Android open
//! go to apps in Android instead of the default browser (`links-in-android`, off by default),
//! whether DSA (`program-updates`) and the Android image (`image-updates`) are updated, and
//! whether every DSA process writes the input trace (`input-trace`) and each stage of each
//! resize (`resize-trace`) to its log, off by default. Those last four are the computer's, so
//! only an administrator changes them.
//! `dsa settings set <name> <value>` changes one;
//! the switches take `on` or `off`. `dsa memory` shows how much memory the user's Android
//! takes, on the host and inside Android. `dsa gpus` lists the GPUs of this machine, for
//! choosing the render GPU.
//!
//! `dsa status` says `Active` only once Android has finished booting and its agent is connected.
//! An Android that runs but has taken too long is `Stalled`, and a line after the state says why
//! and what to do; so does `problem` in the JSON. `dsa start --wait` fails on it.
//!
//! `--json` prints what `status`, `start`, `stop`, `settings`, `memory` and `gpus` report as
//! one JSON object, for programs such as the settings app. Errors still go to standard error,
//! with exit code 1.
//!
//! `dsa update` installs the newest version of DSA now instead of when no Android is
//! running. It stops every user's Android, since the service restarts. With `--wait` it
//! waits for the service to come back and shows what it says about the update.
//!
//! The service is found at the platform's usual place: the named pipe `\\.\pipe\dsa-host`
//! on Windows, `/run/dsa/host.sock` on Linux, and
//! `~/Library/Application Support/DSA/host.sock` on macOS. `--pipe <name>` (Windows) and
//! `--socket <path>` (Linux, macOS) point elsewhere, as do the `DSA_PIPE` and `DSA_SOCKET`
//! environment variables.

use std::fmt::Write as _;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::apk::{ApkFile, InstallKind, InstallOutcome, InstalledPackage};
use dsa_host_core::apk_info::{signing, ApkInfo, Icon, Layer};
use dsa_host_core::files::{FileAction, FileTransfer};
use dsa_host_core::host_api::{ClientError, HostApiClient, SettingsReport, Status, TaskList};
use dsa_host_core::memory::MemorySnapshot;
use dsa_host_core::session::SessionState;
use dsa_host_core::widgets::{Widget, WidgetEvent, WidgetProvider};
use dsa_transport::Connector;
use serde_json::{json, Value};

pub const USAGE: &str = "usage: dsa <status | start | stop | freeze | update | launch <package> | \
     install <file.apk> [--progress] | apk-info <file.apk> | open <file> | share <file> | \
     tasks | settings [set <name> <value>] | memory | gpus | \
     widgets [list | add <provider> | remove <id> | watch] | \
     logcat [-d] [-b <buffers>] [<tag>[:<priority>] ...] | logs collect -o <file.zip>> \
     [--wait] [--timeout <seconds>] [--json] [--pipe <name> | --socket <path>]";

pub mod logs;

/// What to ask the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Status,
    Start,
    Stop,
    /// Freeze the Android until one of its windows is shown again.
    Freeze,
    Update,
    /// Start an app in a window of its own; the package is in [`Invocation::package`].
    Launch,
    /// Install an APK; the file is in [`Invocation::file`].
    Install,
    /// Say what an APK is, and what installing it would do; the file is in
    /// [`Invocation::file`].
    ApkInfo,
    /// Open a file in an app in Android; the file is in [`Invocation::file`].
    Open,
    /// Share a file with an app in Android; the file is in [`Invocation::file`].
    Share,
    /// List the app tasks Android shows as windows.
    Tasks,
    /// Show the settings, or change one ([`Invocation::change`]).
    Settings,
    /// Show how much memory the user's Android takes.
    Memory,
    /// List this machine's GPUs.
    Gpus,
    /// List, place or remove widgets ([`Invocation::widgets`]).
    Widgets,
    /// Print Android's log ([`Invocation::log_filter`], [`Invocation::dump`]).
    Logcat,
    /// Save the diagnostics archive to [`Invocation::output`].
    LogsCollect,
}

/// What `dsa widgets` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetAction {
    /// List the widgets on offer and the ones placed.
    List,
    /// Place a widget of this provider.
    Add(String),
    /// Remove the widget with this ID.
    Remove(i32),
    /// Print the placed widgets, then each change, until stopped.
    Watch,
}

/// A setting `dsa settings set` changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    RenderGpu(String),
    Freeze(bool),
    LinksInAndroid(bool),
    Clipboard(bool),
    ProgramUpdates(bool),
    ImageUpdates(bool),
    InputTrace(bool),
    ResizeTrace(bool),
}

impl Change {
    fn parse(name: &str, value: &str) -> Result<Change, String> {
        let switch = || match value.to_ascii_lowercase().as_str() {
            "on" | "true" | "yes" | "1" => Ok(true),
            "off" | "false" | "no" | "0" => Ok(false),
            _ => Err(format!("{name} is on or off, not {value:?}")),
        };
        match name {
            "render-gpu" => Ok(Change::RenderGpu(value.to_string())),
            "freeze" => switch().map(Change::Freeze),
            "clipboard" => switch().map(Change::Clipboard),
            "links-in-android" => switch().map(Change::LinksInAndroid),
            "program-updates" => switch().map(Change::ProgramUpdates),
            "image-updates" => switch().map(Change::ImageUpdates),
            "input-trace" => switch().map(Change::InputTrace),
            "resize-trace" => switch().map(Change::ResizeTrace),
            _ => Err(format!(
                "there is no setting {name:?}; the settings are render-gpu, freeze, \
                 clipboard, links-in-android, program-updates, image-updates, input-trace \
                 and resize-trace"
            )),
        }
    }
}

/// What can start a host service or ensure it is running.
pub trait ServiceStarter: Send + Sync {
    /// Starts the service if it is not already running, waiting until it enters the
    /// running state or its local transport is available.
    fn start_service(&self) -> Result<(), String>;
}

#[cfg(windows)]
impl ServiceStarter for dsa_host_windows::scm::WindowsServiceStarter {
    fn start_service(&self) -> Result<(), String> {
        self.start()
    }
}

/// The default service starter for this platform, when using the standard endpoint.
pub fn default_starter(endpoint: Option<&str>) -> Option<Box<dyn ServiceStarter>> {
    if endpoint.is_some() {
        return None;
    }
    #[cfg(windows)]
    {
        Some(Box::new(dsa_host_windows::scm::WindowsServiceStarter::new(
            "DsaHost",
            Duration::from_secs(30),
        )))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// How long the service may take to answer `dsa update`: it may download the setup file
/// first.
const UPDATE_ANSWER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// How long `dsa start --wait` waits unless told otherwise: longer than Android may take to
/// finish booting before it is reported as stalled, so that report is what ends the wait.
const START_TIMEOUT: Duration = Duration::from_secs(360);

/// How long the other waits last unless told otherwise.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub command: Command,
    /// Wait until the Android has started or stopped, instead of returning at once.
    pub wait: bool,
    pub timeout: Duration,
    /// Where the service is, when not at the usual place.
    pub endpoint: Option<String>,
    /// The app to launch, for `dsa launch`.
    pub package: Option<String>,
    /// The file to install, open or share, for `dsa install`, `dsa open` and `dsa share`.
    pub file: Option<std::path::PathBuf>,
    /// The setting to change, for `dsa settings set`.
    pub change: Option<Change>,
    /// What to do with widgets, for `dsa widgets`.
    pub widgets: Option<WidgetAction>,
    /// Print the answer as JSON.
    pub json: bool,
    /// Print each step of `dsa install` as it happens.
    pub progress: bool,
    /// Which records `dsa logcat` prints.
    pub log_filter: Option<dsa_host_core::guest_log::LogFilter>,
    /// `dsa logcat -d`: print what Android's log holds now and end.
    pub dump: bool,
    /// Where `dsa logs collect` saves the archive.
    pub output: Option<std::path::PathBuf>,
}

/// Whether `name` can be an Android package name: dot-separated parts of letters, digits
/// and underscores.
fn is_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut command = None;
    let mut wait = false;
    let mut timeout = None;
    let mut endpoint = None;
    let mut package = None;
    let mut file = None;
    let mut change = None;
    let mut widgets = None;
    let mut json = false;
    let mut progress = false;
    let mut buffers: u32 = 0;
    let mut tags = Vec::new();
    let mut dump = false;
    let mut output = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "logcat" if command.is_none() => command = Some(Command::Logcat),
            "logs" if command.is_none() => match args.next().map(String::as_str) {
                Some("collect") => command = Some(Command::LogsCollect),
                _ => return Err(format!("dsa logs takes collect -o <file.zip>\n{USAGE}")),
            },
            "-d" if command == Some(Command::Logcat) => dump = true,
            "-b" if command == Some(Command::Logcat) => {
                let names = args
                    .next()
                    .ok_or("-b needs the log buffers, such as main,system,crash")?;
                buffers |= dsa_host_core::guest_log::LogFilter::parse_buffers(names)?;
            }
            "-o" | "--output" if command == Some(Command::LogsCollect) => {
                let path = args
                    .next()
                    .ok_or("dsa logs collect -o needs the file to save the archive to")?;
                output = Some(std::path::PathBuf::from(path));
            }
            spec if command == Some(Command::Logcat) && !spec.starts_with('-') => {
                tags.push(dsa_host_core::guest_log::LogFilter::parse_spec(spec)?);
            }
            "status" | "start" | "stop" | "freeze" | "update" | "launch" | "install"
            | "apk-info" | "open" | "share" | "tasks" | "settings" | "memory" | "gpus"
            | "widgets"
                if command.is_none() =>
            {
                command = Some(match arg.as_str() {
                    "status" => Command::Status,
                    "start" => Command::Start,
                    "freeze" => Command::Freeze,
                    "update" => Command::Update,
                    "launch" => Command::Launch,
                    "install" => Command::Install,
                    "apk-info" => Command::ApkInfo,
                    "open" => Command::Open,
                    "share" => Command::Share,
                    "tasks" => Command::Tasks,
                    "settings" => Command::Settings,
                    "memory" => Command::Memory,
                    "gpus" => Command::Gpus,
                    "widgets" => Command::Widgets,
                    _ => Command::Stop,
                });
            }
            "set" if command == Some(Command::Settings) && change.is_none() => {
                let name = args
                    .next()
                    .ok_or("dsa settings set needs a setting and a value")?;
                let value = args
                    .next()
                    .ok_or_else(|| format!("dsa settings set {name} needs a value"))?;
                change = Some(Change::parse(name, value)?);
            }
            "list" if command == Some(Command::Widgets) && widgets.is_none() => {
                widgets = Some(WidgetAction::List);
            }
            "watch" if command == Some(Command::Widgets) && widgets.is_none() => {
                widgets = Some(WidgetAction::Watch);
            }
            "add" if command == Some(Command::Widgets) && widgets.is_none() => {
                let provider = args.next().ok_or(
                    "dsa widgets add needs the widget's provider; dsa widgets list shows them",
                )?;
                if !dsa_host_core::widgets::valid_provider_name(provider) {
                    return Err(format!(
                        "{provider:?} is not a widget's provider; dsa widgets list shows them, \
                         such as com.example.clock/.ClockWidget"
                    ));
                }
                widgets = Some(WidgetAction::Add(provider.clone()));
            }
            "remove" if command == Some(Command::Widgets) && widgets.is_none() => {
                let id = args.next().ok_or(
                    "dsa widgets remove needs the widget's ID; dsa widgets list shows them",
                )?;
                let id: i32 = id
                    .parse()
                    .ok()
                    .filter(|id: &i32| *id > 0)
                    .ok_or_else(|| format!("a widget's ID is a number above 0, not {id:?}"))?;
                widgets = Some(WidgetAction::Remove(id));
            }
            "--json" => json = true,
            "--progress" if command == Some(Command::Install) => progress = true,
            name if command == Some(Command::Launch)
                && package.is_none()
                && !name.starts_with('-') =>
            {
                if !is_package_name(name) {
                    return Err(format!(
                        "{name:?} is not a package name; name the app by its package, such as \
                         com.android.settings"
                    ));
                }
                package = Some(name.to_string());
            }
            name if matches!(command, Some(Command::Install | Command::ApkInfo))
                && file.is_none()
                && !name.starts_with("--") =>
            {
                file = Some(std::path::PathBuf::from(name));
            }
            name if matches!(command, Some(Command::Open | Command::Share))
                && file.is_none()
                && !name.starts_with('-') =>
            {
                file = Some(std::path::PathBuf::from(name));
            }
            "--wait" => wait = true,
            "--timeout" => {
                let value = args.next().ok_or("--timeout needs a number of seconds")?;
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| format!("--timeout needs a number of seconds, not {value:?}"))?;
                timeout = Some(Duration::from_secs(seconds));
            }
            "--pipe" | "--socket" => {
                let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
                endpoint = Some(value.clone());
            }
            other => return Err(format!("unexpected {other:?}\n{USAGE}")),
        }
    }
    let command = command.ok_or(USAGE)?;
    let widgets = (command == Command::Widgets).then(|| widgets.unwrap_or(WidgetAction::List));
    if command == Command::Install && file.is_none() {
        return Err(format!(
            "dsa install needs the APK file to install\n{USAGE}"
        ));
    }
    if command == Command::ApkInfo && file.is_none() {
        return Err(format!("dsa apk-info needs the APK file to read\n{USAGE}"));
    }
    if matches!(command, Command::Open | Command::Share) && file.is_none() {
        return Err(format!(
            "dsa {} needs the file to send to Android\n{USAGE}",
            if command == Command::Open {
                "open"
            } else {
                "share"
            }
        ));
    }
    if command == Command::LogsCollect && output.is_none() {
        return Err(format!(
            "dsa logs collect needs the file to save the archive to, such as -o diag.zip\n{USAGE}"
        ));
    }
    let filter = dsa_host_core::guest_log::LogFilter {
        buffers: if buffers == 0 {
            dsa_host_core::guest_log::DEFAULT_BUFFERS
        } else {
            buffers
        },
        tags,
    };
    let log_filter = (command == Command::Logcat).then_some(filter);
    if command == Command::Launch && package.is_none() {
        return Err(format!(
            "dsa launch needs the package of the app to start, such as com.android.settings\n\
             {USAGE}"
        ));
    }
    Ok(Invocation {
        command,
        wait,
        timeout: timeout.unwrap_or(if matches!(command, Command::Start | Command::Install) {
            START_TIMEOUT
        } else {
            DEFAULT_TIMEOUT
        }),
        endpoint,
        package,
        file,
        change,
        widgets,
        json,
        progress,
        log_filter,
        dump,
        output,
    })
}

/// Encodes `data` in standard base64, for the previews `dsa widgets list --json` prints.
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A widget on offer as one JSON object.
pub fn provider_json(provider: &WidgetProvider) -> Value {
    json!({
        "provider": provider.provider,
        "package": provider.package_name,
        "label": provider.label,
        "preview_png": base64(&provider.preview_png),
        "min_width_dp": provider.min_width_dp,
        "min_height_dp": provider.min_height_dp,
        "default_width_dp": provider.default_width_dp,
        "default_height_dp": provider.default_height_dp,
        "max_width_dp": provider.max_width_dp,
        "max_height_dp": provider.max_height_dp,
        "resize_mode": provider.resize_mode,
        "needs_configuration": provider.needs_configuration,
        "app": provider.app_label,
    })
}

/// A placed widget as one JSON object.
pub fn widget_json(widget: &Widget) -> Value {
    json!({
        "id": widget.widget_id,
        "provider": widget.provider.provider,
        "package": widget.provider.package_name,
        "label": widget.provider.label,
        "app": widget.provider.app_label,
        "placement": {
            "x": widget.placement.x,
            "y": widget.placement.y,
            "width_dp": widget.placement.width_dp,
            "height_dp": widget.placement.height_dp,
        },
    })
}

/// What `dsa widgets list` prints: the widgets on offer, then the ones placed.
pub fn describe_widgets(providers: &[WidgetProvider], placed: &[Widget]) -> String {
    let mut out = String::new();
    if providers.is_empty() {
        out.push_str("No app in Android offers a widget.\n");
    } else {
        out.push_str("Widgets on offer (dsa widgets add <provider>):\n");
        for provider in providers {
            let _ = writeln!(
                out,
                "  {}  {} ({}x{} dp){}",
                provider.provider,
                provider.label,
                provider.default_width_dp,
                provider.default_height_dp,
                if provider.needs_configuration {
                    ", set up when placed"
                } else {
                    ""
                }
            );
        }
    }
    if placed.is_empty() {
        out.push_str("No widget is placed.");
    } else {
        out.push_str("Placed widgets (dsa widgets remove <id>):");
        for widget in placed {
            let _ = write!(
                out,
                "\n  {}  {} ({}) at {},{} {}x{} dp",
                widget.widget_id,
                widget.provider.label,
                widget.provider.provider,
                widget.placement.x,
                widget.placement.y,
                widget.placement.width_dp,
                widget.placement.height_dp
            );
        }
    }
    out
}

/// `dsa widgets`: makes sure Android runs (and, on Windows, the window manager that shows the
/// widgets), then lists, places or removes widgets.
fn widgets(
    invocation: &Invocation,
    mut client: HostApiClient,
    connector: &mut dyn Connector,
) -> Result<String, String> {
    let action = invocation.widgets.clone().unwrap_or(WidgetAction::List);
    #[cfg(windows)]
    if invocation.endpoint.is_none()
        && matches!(action, WidgetAction::Add(_) | WidgetAction::Remove(_))
    {
        ensure_window_manager()?;
    }
    client
        .ensure_running(OPEN_START_TIMEOUT, Duration::from_millis(250), |_| {})
        .map_err(|e| format!("Android did not start: {e}"))?;
    drop(client);
    let mut client = connect(connector, None, false, Duration::from_secs(10))?;
    match action {
        WidgetAction::List => {
            let providers = client.widget_providers().map_err(|e| e.to_string())?;
            let (placed, _) = client.widgets().map_err(|e| e.to_string())?;
            Ok(if invocation.json {
                json!({
                    "providers": providers.iter().map(provider_json).collect::<Vec<_>>(),
                    "widgets": placed.iter().map(widget_json).collect::<Vec<_>>(),
                })
                .to_string()
            } else {
                describe_widgets(&providers, &placed)
            })
        }
        WidgetAction::Add(provider) => {
            let widget = client
                .add_widget(
                    &provider,
                    dsa_host_core::host_api::WIDGET_ADD_WAIT + Duration::from_secs(10),
                )
                .map_err(|e| format!("the widget was not placed: {e}"))?;
            Ok(if invocation.json {
                widget_json(&widget).to_string()
            } else {
                format!(
                    "Placed {} (widget {})",
                    if widget.provider.label.is_empty() {
                        &widget.provider.provider
                    } else {
                        &widget.provider.label
                    },
                    widget.widget_id
                )
            })
        }
        WidgetAction::Remove(id) => {
            client.remove_widget(id).map_err(|e| e.to_string())?;
            Ok(if invocation.json {
                json!({ "removed": id }).to_string()
            } else {
                format!("Removed widget {id}")
            })
        }
        WidgetAction::Watch => {
            let mut stream = client.watch_widgets().map_err(|e| e.to_string())?;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let first = widgets_now_line(stream.current(), invocation.json);
            if !print_line(&mut out, &first) {
                return Ok(String::new());
            }
            loop {
                match stream.next(Duration::from_secs(60)) {
                    Ok(Some(event)) => {
                        if let Some(line) = widget_event_line(&event, invocation.json) {
                            if !print_line(&mut out, &line) {
                                return Ok(String::new());
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => return Err(format!("the watch on the widgets ended: {e}")),
                }
            }
        }
    }
}

/// Writes one line and flushes it, so a program reading the output sees it at once. False
/// once the output is closed.
fn print_line(out: &mut dyn std::io::Write, line: &str) -> bool {
    writeln!(out, "{line}").is_ok() && out.flush().is_ok()
}

/// The first line `dsa widgets watch` prints: the widgets placed now.
pub fn widgets_now_line(placed: &[Widget], json: bool) -> String {
    if json {
        json!({
            "event": "widgets",
            "widgets": placed.iter().map(widget_json).collect::<Vec<_>>(),
        })
        .to_string()
    } else if placed.is_empty() {
        "No widget is placed.".to_string()
    } else {
        let ids: Vec<String> = placed.iter().map(|w| w.widget_id.to_string()).collect();
        format!("Placed widgets: {}", ids.join(", "))
    }
}

/// The line `dsa widgets watch` prints for a change, or None for one it does not report
/// (a widget's new picture).
pub fn widget_event_line(event: &WidgetEvent, json: bool) -> Option<String> {
    match event {
        WidgetEvent::Placed(widget) => Some(if json {
            json!({ "event": "placed", "widget": widget_json(widget) }).to_string()
        } else {
            format!(
                "Placed widget {} ({})",
                widget.widget_id,
                if widget.provider.label.is_empty() {
                    &widget.provider.provider
                } else {
                    &widget.provider.label
                }
            )
        }),
        WidgetEvent::Removed { widget_id } => Some(if json {
            json!({ "event": "removed", "id": widget_id }).to_string()
        } else {
            format!("Removed widget {widget_id}")
        }),
        WidgetEvent::Frame(_) | WidgetEvent::Listed(_) => None,
    }
}

/// `status` as one JSON object.
pub fn status_json(status: &Status) -> Value {
    json!({
        "state": status.state.to_string(),
        "last_reason": status.last_reason,
        "update_notice": status.update_notice,
        "problem": status.problem,
    })
}

/// The settings as one JSON object.
pub fn settings_json(report: &SettingsReport) -> Value {
    json!({
        "render_gpu": report.settings.render_gpu,
        "freeze": report.settings.freeze,
        "links_in_android": report.settings.links_in_android,
        "clipboard": report.settings.clipboard,
        "program_updates": report.settings.program_updates,
        "image_updates": report.settings.image_updates,
        "input_trace": report.settings.input_trace,
        "resize_trace": report.settings.resize_trace,
        "program_updates_available": report.facts.program_updates_available,
        "image_updates_available": report.facts.image_updates_available,
        "version": report.facts.version,
        "log_dir": report.facts.log_dir,
        "android_version": report.facts.android_version,
    })
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

/// The settings, one per line.
pub fn describe_settings(report: &SettingsReport) -> String {
    let settings = &report.settings;
    let facts = &report.facts;
    let available = |value: bool, available: bool| {
        if available {
            on_off(value).to_string()
        } else {
            format!("{} (this installation does not update it)", on_off(value))
        }
    };
    let mut lines = vec![
        format!("Render GPU: {}", settings.render_gpu),
        format!("Freeze when idle: {}", on_off(settings.freeze)),
        format!(
            "Web links open in: {}",
            if settings.links_in_android {
                "Android apps"
            } else {
                "the default browser"
            }
        ),
        format!("Clipboard sharing: {}", on_off(settings.clipboard)),
        format!(
            "Program updates: {}",
            available(settings.program_updates, facts.program_updates_available)
        ),
        format!(
            "Image updates: {}",
            available(settings.image_updates, facts.image_updates_available)
        ),
        format!("Input trace: {}", on_off(settings.input_trace)),
        format!("Resize trace: {}", on_off(settings.resize_trace)),
        format!("Service version: {}", facts.version),
    ];
    if let Some(version) = &facts.android_version {
        lines.push(format!("Android image: {version}"));
    }
    if let Some(dir) = &facts.log_dir {
        lines.push(format!("Service log folder: {dir}"));
    }
    lines.join("\n")
}

/// The memory report as one JSON object. Sizes in KiB; `guest` is null until Android has
/// reported, and `age_seconds` says how old its report is.
pub fn memory_json(memory: &MemorySnapshot) -> Value {
    let guest = memory.guest.as_ref().map(|report| {
        let guest = &report.memory;
        json!({
            "mem_total_kb": guest.mem_total_kb,
            "mem_free_kb": guest.mem_free_kb,
            "mem_available_kb": guest.mem_available_kb,
            "buffers_kb": guest.buffers_kb,
            "cached_kb": guest.cached_kb,
            "anon_pages_kb": guest.anon_pages_kb,
            "shmem_kb": guest.shmem_kb,
            "slab_reclaimable_kb": guest.slab_reclaimable_kb,
            "swap_total_kb": guest.swap_total_kb,
            "swap_free_kb": guest.swap_free_kb,
            "zram_original_kb": guest.zram_original_kb,
            "zram_used_kb": guest.zram_used_kb,
            "total_pss_kb": guest.total_pss_kb,
            "age_seconds": report.age.as_secs(),
            "top_processes": guest.top_processes.iter().map(|p| json!({
                "pid": p.pid,
                "name": p.name,
                "pss_kb": p.pss_kb,
                "swap_pss_kb": p.swap_pss_kb,
            })).collect::<Vec<_>>(),
        })
    });
    json!({
        "guest": guest,
        "host": memory.host.iter().map(|p| json!({
            "role": p.role.name(),
            "pid": p.pid,
            "working_set_kb": p.working_set_kb,
            "private_kb": p.private_kb,
        })).collect::<Vec<_>>(),
        "host_total_kb": memory.host_total_kb(),
        "vm_beyond_guest_kb": memory.vm_beyond_guest_kb(),
    })
}

/// This machine's GPUs, and which one drives the primary monitor.
pub fn gpus_json() -> Value {
    #[cfg(windows)]
    {
        use dsa_host_windows::compositor::display_gpu::{adapter_names, display_sources};
        let adapters = adapter_names();
        let primary = display_sources().ok().and_then(|sources| {
            sources
                .into_iter()
                .find(|source| source.x == 0 && source.y == 0)
                .map(|source| source.adapter)
        });
        json!({
            "gpus": adapters.iter().map(|adapter| json!({
                "name": adapter.name,
                "luid": adapter.luid.to_string(),
                "primary_display": Some(adapter.luid) == primary,
            })).collect::<Vec<_>>(),
        })
    }
    #[cfg(not(windows))]
    {
        json!({ "gpus": [] })
    }
}

fn describe_gpus(gpus: &Value) -> String {
    let list = gpus["gpus"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "no GPU found".into();
    }
    list.iter()
        .map(|gpu| {
            let mut line = gpu["name"].as_str().unwrap_or_default().to_string();
            if gpu["primary_display"].as_bool() == Some(true) {
                line.push_str(" (drives the primary monitor)");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `dsa settings`, and `dsa settings set`.
fn settings(invocation: &Invocation, client: &mut HostApiClient) -> Result<String, String> {
    let mut report = client.settings().map_err(|e| e.to_string())?;
    if let Some(change) = &invocation.change {
        let mut wanted = report.settings.clone();
        match change {
            Change::RenderGpu(value) => wanted.render_gpu = value.clone(),
            Change::Freeze(on) => wanted.freeze = *on,
            Change::LinksInAndroid(on) => wanted.links_in_android = *on,
            Change::Clipboard(on) => wanted.clipboard = *on,
            Change::ProgramUpdates(on) => wanted.program_updates = *on,
            Change::ImageUpdates(on) => wanted.image_updates = *on,
            Change::InputTrace(on) => wanted.input_trace = *on,
            Change::ResizeTrace(on) => wanted.resize_trace = *on,
        }
        report = client.set_settings(&wanted).map_err(|e| e.to_string())?;
    }
    Ok(if invocation.json {
        settings_json(&report).to_string()
    } else {
        describe_settings(&report)
    })
}

/// One line for each task of `list`, or a line saying there are none.
pub fn describe_tasks(list: &TaskList) -> String {
    if list.tasks.is_empty() {
        return "Android has no app tasks".into();
    }
    let lines: Vec<String> = list
        .tasks
        .iter()
        .map(|task| {
            let mut line = format!(
                "Task {}: {} ({}), {}x{}",
                task.task_id,
                if task.label.is_empty() {
                    &task.package_name
                } else {
                    &task.label
                },
                task.package_name,
                task.bounds.width(),
                task.bounds.height()
            );
            if let Some((_, display_id)) = list.frames.iter().find(|(id, _)| *id == task.task_id) {
                let _ = write!(line, ", display {display_id}");
            }
            if task.focused {
                line.push_str(", focused");
            }
            line
        })
        .collect();
    lines.join("\n")
}

/// A line describing `status`, and one more for each piece of news about an update.
///
/// A reason's first line goes in the status line. Any further lines (the end of the guest's
/// console, after a failed start) follow it as they are, so they can be copied whole.
pub fn describe(status: &Status) -> String {
    let mut line = format!("Android: {}", status.state);
    if let Some(reason) = &status.last_reason {
        let mut lines = reason.lines();
        let _ = write!(
            line,
            " (last stopped: {})",
            lines.next().unwrap_or_default()
        );
        for detail in lines {
            let _ = write!(line, "\n{detail}");
        }
    }
    for problem in status.problem.iter().flat_map(|problem| problem.lines()) {
        let _ = write!(line, "\nProblem: {problem}");
    }
    for notice in status
        .update_notice
        .iter()
        .flat_map(|notice| notice.lines())
    {
        let _ = write!(line, "\nUpdate: {notice}");
    }
    line
}

fn build() -> String {
    // The installer build sets DSA_VERSION to the release's version.
    let version = option_env!("DSA_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"));
    format!("dsa {version}")
}

fn connect(
    connector: &mut dyn Connector,
    starter: Option<&dyn ServiceStarter>,
    can_start: bool,
    timeout: Duration,
) -> Result<HostApiClient, String> {
    match HostApiClient::connect(connector, &build(), timeout) {
        Ok(client) => Ok(client),
        Err(ClientError::Connect(why)) if can_start && starter.is_some() => {
            let starter = starter.unwrap();
            match starter.start_service() {
                Ok(()) => {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while Instant::now() < deadline {
                        match HostApiClient::connect(connector, &build(), Duration::from_secs(1)) {
                            Ok(client) => return Ok(client),
                            Err(ClientError::Connect(_)) => {
                                thread::sleep(Duration::from_millis(100));
                            }
                            Err(other) => return Err(other.to_string()),
                        }
                    }
                    Err(format!(
                        "cannot reach the DSA service ({why}): the service started, but its transport did not become ready"
                    ))
                }
                Err(start_err) => Err(format!(
                    "cannot reach the DSA service ({why}): starting the service failed: {start_err}"
                )),
            }
        }
        Err(ClientError::Connect(error)) => Err(format!(
            "cannot reach the DSA service ({error}); check that it is installed and running"
        )),
        Err(other) => Err(other.to_string()),
    }
}

/// `dsa update`: asks the service to install the newest version now and, with `--wait`,
/// waits for the service to go away and come back.
fn update(
    invocation: &Invocation,
    mut client: HostApiClient,
    connector: &mut dyn Connector,
) -> Result<String, String> {
    let update = client
        .update_program(UPDATE_ANSWER_TIMEOUT)
        .map_err(|e| e.to_string())?;
    drop(client);
    if !invocation.wait || update.installing.is_none() {
        return Ok(update.detail);
    }
    let deadline = Instant::now() + invocation.timeout;
    let mut went_away = false;
    let mut last = None;
    loop {
        match connect(connector, None, false, Duration::from_secs(2))
            .and_then(|mut client| client.status().map_err(|e| e.to_string()))
        {
            Ok(status) if went_away => {
                return Ok(format!("{}\n{}", update.detail, describe(&status)))
            }
            Ok(status) => last = Some(status),
            Err(_) => went_away = true,
        }
        if Instant::now() >= deadline {
            // A setup that failed before it stopped the service leaves it running, and the
            // service says so in its status.
            let now = last
                .as_ref()
                .map(|status| format!("\n{}", describe(status)))
                .unwrap_or_default();
            return Err(format!(
                "{}\nthe service has not restarted after {} seconds{now}",
                update.detail,
                invocation.timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// How an install ended, as one JSON object.
pub fn install_json(outcome: &InstallOutcome) -> Value {
    json!({
        "installed": outcome.installed(),
        "status": outcome.status.code(),
        "package": outcome.package_name,
        "message": outcome.message,
    })
}

/// The line `dsa install` prints when Android refused the APK. It starts with the reason's
/// stable code, which the settings app reads to say the reason in the user's language.
pub fn describe_refusal(name: &str, outcome: &InstallOutcome) -> String {
    format!(
        "not installed ({}): {name}: {outcome}",
        outcome.status.code()
    )
}

/// `dsa install`: opens the APK, makes sure Android runs (and, on Windows, the window manager
/// that shows the app once it is opened), then streams the APK to the service.
fn install(
    invocation: &Invocation,
    mut client: HostApiClient,
    connector: &mut dyn Connector,
) -> Result<String, String> {
    let path = invocation
        .file
        .as_deref()
        .unwrap_or(std::path::Path::new(""));
    // A file that cannot be installed is refused before Android is started for it.
    let mut apk = ApkFile::open(path)?;
    let step = |line: &str| {
        if invocation.progress {
            use std::io::Write as _;
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    };
    #[cfg(windows)]
    if invocation.endpoint.is_none() {
        ensure_window_manager()?;
    }
    if !client.status().is_ok_and(|status| {
        matches!(
            status.state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        )
    }) {
        step("stage starting");
    }
    client
        .ensure_running(invocation.timeout, Duration::from_millis(250), |_| {})
        .map_err(|e| format!("Android did not start: {e}"))?;
    drop(client);
    let name = apk.name.clone();
    let size = apk.size;
    step("stage sending");
    let mut sending = Progress {
        inner: apk.reader(),
        sent: 0,
        size,
        shown: None,
        step: &step,
    };
    let outcome = connect(connector, None, false, Duration::from_secs(10))?
        .install_apk(&name, size, &mut sending)
        .map_err(|e| format!("{name} was not installed: {e}"))?;
    if !outcome.installed() {
        return Err(describe_refusal(&name, &outcome));
    }
    Ok(if invocation.json {
        install_json(&outcome).to_string()
    } else {
        format!(
            "Installed {} from {name}",
            outcome.package_name.as_deref().unwrap_or("the app")
        )
    })
}

/// The APK as it is read to be sent, saying how much of it has gone.
struct Progress<'a, F: Fn(&str)> {
    inner: &'a mut dyn std::io::Read,
    sent: u64,
    size: u64,
    shown: Option<u64>,
    step: &'a F,
}

impl<F: Fn(&str)> std::io::Read for Progress<'_, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.sent += read as u64;
        let percent = (self.sent * 100)
            .checked_div(self.size)
            .unwrap_or(100)
            .min(100);
        if self.shown != Some(percent) {
            self.shown = Some(percent);
            (self.step)(&format!("progress {percent}"));
            if percent == 100 {
                (self.step)("stage installing");
            }
        }
        Ok(read)
    }
}

/// What `dsa apk-info` says of an APK, as one JSON object.
pub fn apk_info_json(
    info: &ApkInfo,
    android: &str,
    installed: Option<&InstalledPackage>,
    kind: Option<InstallKind>,
) -> Value {
    let layer = |layer: &Layer| match layer {
        Layer::Image(png) => json!({ "kind": "image", "png": base64(png) }),
        Layer::Color(argb) => json!({ "kind": "color", "color": format!("#{argb:08X}") }),
        Layer::Unknown => json!({ "kind": "unknown" }),
    };
    let icon = match &info.icon {
        Some(Icon::Image(png)) => json!({ "kind": "image", "png": base64(png) }),
        Some(Icon::Adaptive {
            background,
            foreground,
        }) => json!({
            "kind": "adaptive",
            "background": layer(background),
            "foreground": layer(foreground),
        }),
        None => Value::Null,
    };
    json!({
        "package": info.package,
        "label": info.label,
        "version_code": info.version_code,
        "version_name": info.version_name,
        "min_sdk": info.min_sdk,
        "min_android": dsa_host_core::apk_info::permissions::android_version(info.min_sdk),
        "target_sdk": info.target_sdk,
        "target_android": dsa_host_core::apk_info::permissions::android_version(info.target_sdk),
        "size": info.size,
        "permissions": info.permissions.iter().map(|p| json!({
            "name": p.name,
            "description": p.description,
            "sensitive": p.sensitive,
        })).collect::<Vec<_>>(),
        "icon": icon,
        "signer": info.signer.as_ref().map(|signer| json!({
            "scheme": signer.scheme.name(),
            "sha256": signing::fingerprint(&signer.sha256()),
        })),
        "abis": info.abis,
        "android": android,
        "installed": installed.map(|installed| json!({
            "version_code": installed.version_code,
            "version_name": installed.version_name,
            "signers": installed
                .signer_sha256
                .iter()
                .map(|digest| signing::fingerprint(digest))
                .collect::<Vec<_>>(),
        })),
        "install": kind.map(InstallKind::code),
    })
}

/// What `dsa apk-info` prints of an APK for a person.
pub fn describe_apk(
    info: &ApkInfo,
    installed: Option<&InstalledPackage>,
    kind: Option<InstallKind>,
) -> String {
    let android = |sdk: u32| {
        dsa_host_core::apk_info::permissions::android_version(sdk)
            .map(|version| format!("Android {version} (API {sdk})"))
            .unwrap_or_else(|| format!("API {sdk}"))
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} ({})",
        info.label.as_deref().unwrap_or(&info.package),
        info.package
    );
    let _ = writeln!(
        out,
        "version {} ({})",
        info.version_name.as_deref().unwrap_or("-"),
        info.version_code
    );
    let _ = writeln!(
        out,
        "needs {}, made for {}",
        android(info.min_sdk),
        android(info.target_sdk)
    );
    let _ = writeln!(out, "{} bytes", info.size);
    match &info.signer {
        Some(signer) => {
            let _ = writeln!(
                out,
                "signed ({}) by SHA-256 {}",
                signer.scheme.name(),
                signing::fingerprint(&signer.sha256())
            );
        }
        None => {
            let _ = writeln!(out, "not signed; Android will not install it");
        }
    }
    if info.permissions.is_empty() {
        let _ = writeln!(out, "asks for no permissions");
    } else {
        let _ = writeln!(out, "asks for:");
        for permission in &info.permissions {
            let _ = writeln!(out, "  {} ({})", permission.description, permission.name);
        }
    }
    let _ = write!(
        out,
        "{}",
        match (kind, installed) {
            (None, _) => "Android is not running, so what it has of the app is not known".to_string(),
            (Some(InstallKind::New), _) => "not installed yet".to_string(),
            (Some(kind), Some(installed)) => format!(
                "installed: version {} ({}); {}",
                installed.version_name,
                installed.version_code,
                match kind {
                    InstallKind::Update => "this APK updates it",
                    InstallKind::Reinstall => "this APK installs the same version again",
                    InstallKind::Downgrade =>
                        "this APK is an older version, which Android does not install over it",
                    InstallKind::SignatureMismatch =>
                        "this APK is signed with another key, which Android does not install over it",
                    InstallKind::New => "",
                }
            ),
            (Some(_), None) => String::new(),
        }
    );
    out
}

/// `dsa apk-info`: reads the APK, and when Android runs, asks what it has of the package.
/// Never starts Android or the service.
fn apk_info(invocation: &Invocation, connector: &mut dyn Connector) -> Result<String, String> {
    let path = invocation
        .file
        .as_deref()
        .unwrap_or(std::path::Path::new(""));
    let info = ApkInfo::read(path)?;
    let mut android = "unreachable";
    let mut installed = None;
    let mut kind = None;
    if let Ok(mut client) = connect(connector, None, false, Duration::from_secs(5)) {
        android = "stopped";
        if client
            .status()
            .is_ok_and(|status| matches!(status.state, SessionState::Active | SessionState::Dozing))
        {
            if let Ok(state) = client.package_state(&info.package) {
                android = "running";
                kind = Some(InstallKind::decide(
                    info.version_code,
                    &info.signers(),
                    state.as_ref(),
                ));
                installed = state;
            }
        }
    }
    Ok(if invocation.json {
        apk_info_json(&info, android, installed.as_ref(), kind).to_string()
    } else {
        describe_apk(&info, installed.as_ref(), kind)
    })
}

/// How long `dsa open` and `dsa share` wait for Android to start when it is not running:
/// as long as `dsa start --wait`.
const OPEN_START_TIMEOUT: Duration = START_TIMEOUT;

/// `dsa open` and `dsa share`: reads the file, makes sure Android runs (and, on Windows, the
/// window manager that shows the app Android opens it in), then sends the file.
fn open_file(
    invocation: &Invocation,
    mut client: HostApiClient,
    connector: &mut dyn Connector,
) -> Result<String, String> {
    let path = invocation
        .file
        .as_deref()
        .unwrap_or(std::path::Path::new(""));
    let action = if invocation.command == Command::Share {
        FileAction::Send
    } else {
        FileAction::View
    };
    // A file that cannot be sent is refused before Android is started for it.
    let file = FileTransfer::read(path, action)?;
    #[cfg(windows)]
    if invocation.endpoint.is_none() {
        ensure_window_manager()?;
    }
    client
        .ensure_running(OPEN_START_TIMEOUT, Duration::from_millis(250), |_| {})
        .map_err(|e| format!("Android did not start: {e}"))?;
    drop(client);
    connect(connector, None, false, Duration::from_secs(10))?
        .open_file(&file)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "Android asks which app to {} {} with",
        if action == FileAction::Send {
            "share"
        } else {
            "open"
        },
        file.name
    ))
}

/// Starts this session's window manager when it is not running, as opening an app does.
#[cfg(windows)]
fn ensure_window_manager() -> Result<(), String> {
    use dsa_host_windows::app_window::launch::{
        ensure_window_manager, pipe_exists, run_program, window_manager_path, POLL,
        START_WINDOW_MANAGER,
    };
    use dsa_host_windows::compositor::{current_session, hosts::pipe_name};
    let pipe = pipe_name(current_session());
    let program = std::env::current_exe().ok();
    ensure_window_manager(
        || pipe_exists(&pipe),
        || match window_manager_path(program.as_deref()) {
            Some(window_manager) => run_program(&window_manager, ""),
            None => Err("the DSA Window Manager is not installed".into()),
        },
        START_WINDOW_MANAGER,
        POLL,
    )
}

/// Runs `invocation` against the service `connector` reaches. Returns what to print.
pub fn run(invocation: &Invocation, connector: &mut dyn Connector) -> Result<String, String> {
    let starter = default_starter(invocation.endpoint.as_deref());
    run_with_starter(invocation, connector, starter.as_deref())
}

/// Runs `invocation` against the service `connector` reaches, using `starter` to start it
/// if stopped.
pub fn run_with_starter(
    invocation: &Invocation,
    connector: &mut dyn Connector,
    starter: Option<&dyn ServiceStarter>,
) -> Result<String, String> {
    if invocation.command == Command::ApkInfo {
        return apk_info(invocation, connector);
    }
    if invocation.command == Command::Gpus {
        let gpus = gpus_json();
        return Ok(if invocation.json {
            gpus.to_string()
        } else {
            describe_gpus(&gpus)
        });
    }
    // Stopping or freezing needs an Android that runs, so neither starts the service.
    let can_start = !matches!(invocation.command, Command::Stop | Command::Freeze);
    let mut client = connect(connector, starter, can_start, Duration::from_secs(10))?;
    let show = |status: &Status| {
        if invocation.json {
            status_json(status).to_string()
        } else {
            describe(status)
        }
    };
    let status = match invocation.command {
        Command::Status => client.status(),
        Command::Start => client.start(),
        Command::Stop => client.stop(),
        Command::Freeze => client.freeze(),
        Command::Update => return update(invocation, client, connector),
        Command::Launch => {
            let package = invocation.package.as_deref().unwrap_or_default();
            client.launch_app(package).map_err(|e| e.to_string())?;
            return Ok(format!("Launching {package}"));
        }
        Command::Install => return install(invocation, client, connector),
        Command::Widgets => return widgets(invocation, client, connector),
        Command::Open | Command::Share => {
            return open_file(invocation, client, connector);
        }
        Command::Tasks => {
            let list = client.tasks().map_err(|e| e.to_string())?;
            return Ok(describe_tasks(&list));
        }
        Command::Settings => return settings(invocation, &mut client),
        Command::Memory => {
            let memory = client.memory().map_err(|e| e.to_string())?;
            return Ok(if invocation.json {
                memory_json(&memory).to_string()
            } else {
                memory.to_string().trim_end().to_string()
            });
        }
        Command::Logcat => {
            let filter = invocation.log_filter.clone().unwrap_or_default();
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            return logs::logcat(client, &filter, invocation.dump, invocation.json, &mut out);
        }
        Command::LogsCollect => {
            let path = invocation.output.clone().unwrap_or_default();
            return logs::collect(client, &path);
        }
        Command::Gpus | Command::ApkInfo => unreachable!("answered before connecting"),
    }
    .map_err(|e| e.to_string())?;
    if !invocation.wait || matches!(invocation.command, Command::Status | Command::Freeze) {
        return Ok(show(&status));
    }
    let deadline = Instant::now() + invocation.timeout;
    // The service answers a start once the session has moved on, so a Stopped answer
    // means the start has already failed.
    let (done, failed): (&[SessionState], &[SessionState]) = match invocation.command {
        Command::Start => (
            &[SessionState::Active, SessionState::Dozing],
            &[SessionState::Stopped, SessionState::Stalled],
        ),
        _ => (&[SessionState::Stopped], &[]),
    };
    let goal = if invocation.command == Command::Start {
        "started"
    } else {
        "stopped"
    };
    let mut status = status;
    loop {
        if done.contains(&status.state) {
            return Ok(show(&status));
        }
        if failed.contains(&status.state) {
            return Err(format!("Android did not start: {}", describe(&status)));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Android has not {goal} after {} seconds: {}",
                invocation.timeout.as_secs(),
                describe(&status)
            ));
        }
        thread::sleep(Duration::from_millis(250));
        status = client.status().map_err(|e| e.to_string())?;
    }
}

/// The connector for the platform's usual place, or for `endpoint`.
pub fn connector(endpoint: Option<&str>) -> Result<Box<dyn Connector>, String> {
    #[cfg(windows)]
    {
        let name = endpoint
            .map(str::to_string)
            .or_else(|| std::env::var("DSA_PIPE").ok())
            .unwrap_or_else(|| "dsa-host".into());
        Ok(Box::new(dsa_host_windows::pipe::NamedPipeConnector {
            name,
        }))
    }
    #[cfg(unix)]
    {
        let path = match endpoint
            .map(str::to_string)
            .or_else(|| std::env::var("DSA_SOCKET").ok())
        {
            Some(path) => std::path::PathBuf::from(path),
            None => default_socket()?,
        };
        Ok(Box::new(dsa_host_unix::socket::UnixLocalConnector { path }))
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = endpoint;
        Err("this platform has no DSA service".into())
    }
}

#[cfg(unix)]
fn default_socket() -> Result<std::path::PathBuf, String> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        Ok(std::path::PathBuf::from(home).join("Library/Application Support/DSA/host.sock"))
    } else {
        Ok("/run/dsa/host.sock".into())
    }
}
