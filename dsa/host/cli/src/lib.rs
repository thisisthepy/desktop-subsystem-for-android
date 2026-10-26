//! `dsa`, the command line client of the local API.
//!
//! ```text
//! dsa status
//! dsa start [--wait]
//! dsa stop [--wait]
//! dsa update [--wait]
//! dsa launch <package>
//! dsa tasks
//! dsa settings [set <name> <value>]
//! dsa memory
//! dsa gpus
//! ```
//!
//! It acts on the Android of the user who runs it: the service learns that user from the
//! operating system, so there is no option to name another one.
//!
//! `dsa launch` starts an app in a window of its own, named by its package (such as
//! `com.android.settings`). `dsa tasks` lists the app tasks Android shows as windows.
//!
//! `dsa settings` shows the settings the service keeps: the GPU the window manager renders on
//! (`render-gpu`: `display`, `integrated`, `discrete` or part of a GPU's name), whether
//! Android may be frozen when idle (`freeze`), and whether DSA (`program-updates`) and the
//! Android image (`image-updates`) are updated. `dsa settings set <name> <value>` changes one;
//! the switches take `on` or `off`. `dsa memory` shows how much memory the user's Android
//! takes, on the host and inside Android. `dsa gpus` lists the GPUs of this machine, for
//! choosing the render GPU.
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

use dsa_host_core::host_api::{ClientError, HostApiClient, SettingsReport, Status, TaskList};
use dsa_host_core::memory::Memory;
use dsa_host_core::session::SessionState;
use dsa_transport::Connector;
use serde_json::{json, Value};

pub const USAGE: &str = "usage: dsa <status | start | stop | update | launch <package> | tasks | \
     settings [set <name> <value>] | memory | gpus> [--wait] [--timeout <seconds>] [--json] \
     [--pipe <name> | --socket <path>]";

/// What to ask the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Status,
    Start,
    Stop,
    Update,
    /// Start an app in a window of its own; the package is in [`Invocation::package`].
    Launch,
    /// List the app tasks Android shows as windows.
    Tasks,
    /// Show the settings, or change one ([`Invocation::change`]).
    Settings,
    /// Show how much memory the user's Android takes.
    Memory,
    /// List this machine's GPUs.
    Gpus,
}

/// A setting `dsa settings set` changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    RenderGpu(String),
    Freeze(bool),
    ProgramUpdates(bool),
    ImageUpdates(bool),
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
            "program-updates" => switch().map(Change::ProgramUpdates),
            "image-updates" => switch().map(Change::ImageUpdates),
            _ => Err(format!(
                "there is no setting {name:?}; the settings are render-gpu, freeze, \
                 program-updates and image-updates"
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
    /// The setting to change, for `dsa settings set`.
    pub change: Option<Change>,
    /// Print the answer as JSON.
    pub json: bool,
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
    let mut timeout = Duration::from_secs(120);
    let mut endpoint = None;
    let mut package = None;
    let mut change = None;
    let mut json = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "status" | "start" | "stop" | "update" | "launch" | "tasks" | "settings" | "memory"
            | "gpus"
                if command.is_none() =>
            {
                command = Some(match arg.as_str() {
                    "status" => Command::Status,
                    "start" => Command::Start,
                    "update" => Command::Update,
                    "launch" => Command::Launch,
                    "tasks" => Command::Tasks,
                    "settings" => Command::Settings,
                    "memory" => Command::Memory,
                    "gpus" => Command::Gpus,
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
            "--json" => json = true,
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
            "--wait" => wait = true,
            "--timeout" => {
                let value = args.next().ok_or("--timeout needs a number of seconds")?;
                let seconds: u64 = value
                    .parse()
                    .map_err(|_| format!("--timeout needs a number of seconds, not {value:?}"))?;
                timeout = Duration::from_secs(seconds);
            }
            "--pipe" | "--socket" => {
                let value = args.next().ok_or_else(|| format!("{arg} needs a value"))?;
                endpoint = Some(value.clone());
            }
            other => return Err(format!("unexpected {other:?}\n{USAGE}")),
        }
    }
    let command = command.ok_or(USAGE)?;
    if command == Command::Launch && package.is_none() {
        return Err(format!(
            "dsa launch needs the package of the app to start, such as com.android.settings\n\
             {USAGE}"
        ));
    }
    Ok(Invocation {
        command,
        wait,
        timeout,
        endpoint,
        package,
        change,
        json,
    })
}

/// `status` as one JSON object.
pub fn status_json(status: &Status) -> Value {
    json!({
        "state": status.state.to_string(),
        "last_reason": status.last_reason,
        "update_notice": status.update_notice,
    })
}

/// The settings as one JSON object.
pub fn settings_json(report: &SettingsReport) -> Value {
    json!({
        "render_gpu": report.settings.render_gpu,
        "freeze": report.settings.freeze,
        "program_updates": report.settings.program_updates,
        "image_updates": report.settings.image_updates,
        "program_updates_available": report.facts.program_updates_available,
        "image_updates_available": report.facts.image_updates_available,
        "version": report.facts.version,
        "log_dir": report.facts.log_dir,
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
            "Program updates: {}",
            available(settings.program_updates, facts.program_updates_available)
        ),
        format!(
            "Image updates: {}",
            available(settings.image_updates, facts.image_updates_available)
        ),
        format!("Service version: {}", facts.version),
    ];
    if let Some(dir) = &facts.log_dir {
        lines.push(format!("Service log folder: {dir}"));
    }
    lines.join("\n")
}

/// The memory report as one JSON object.
pub fn memory_json(memory: &Memory) -> Value {
    let guest = memory.guest.as_ref().map(|guest| {
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
            "age_seconds": guest.age_seconds,
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
            "role": p.role.as_str(),
            "pid": p.pid,
            "working_set_kb": p.working_set_kb,
            "private_kb": p.private_kb,
        })).collect::<Vec<_>>(),
        "host_total_kb": memory.host_total_kb(),
    })
}

fn megabytes(kb: u64) -> String {
    format!("{:.1} MB", kb as f64 / 1024.0)
}

/// The memory report, one line per process and per part of the guest.
pub fn describe_memory(memory: &Memory) -> String {
    let mut lines = Vec::new();
    if memory.host.is_empty() {
        lines.push("Host: no process of this user's Android is running".to_string());
    } else {
        lines.push(format!(
            "Host: {} in RAM",
            megabytes(memory.host_total_kb())
        ));
        for p in &memory.host {
            lines.push(format!(
                "  {} (process {}): {} in RAM, {} private",
                p.role.as_str(),
                p.pid,
                megabytes(p.working_set_kb),
                megabytes(p.private_kb)
            ));
        }
    }
    match &memory.guest {
        None => lines.push("Android: no report yet".to_string()),
        Some(guest) => {
            lines.push(format!(
                "Android: {} of {} available ({} seconds ago)",
                megabytes(guest.mem_available_kb),
                megabytes(guest.mem_total_kb),
                guest.age_seconds
            ));
            lines.push(format!(
                "  apps and system: {}",
                megabytes(guest.total_pss_kb)
            ));
            lines.push(format!(
                "  cache: {}",
                megabytes(guest.cached_kb + guest.buffers_kb)
            ));
            lines.push(format!(
                "  kernel caches it can free: {}",
                megabytes(guest.slab_reclaimable_kb)
            ));
            for p in &guest.top_processes {
                lines.push(format!(
                    "  {} (process {}): {}",
                    p.name,
                    p.pid,
                    megabytes(p.pss_kb)
                ));
            }
        }
    }
    lines.join("\n")
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
            Change::ProgramUpdates(on) => wanted.program_updates = *on,
            Change::ImageUpdates(on) => wanted.image_updates = *on,
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
    if invocation.command == Command::Gpus {
        let gpus = gpus_json();
        return Ok(if invocation.json {
            gpus.to_string()
        } else {
            describe_gpus(&gpus)
        });
    }
    let can_start = invocation.command != Command::Stop;
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
        Command::Update => return update(invocation, client, connector),
        Command::Launch => {
            let package = invocation.package.as_deref().unwrap_or_default();
            client.launch_app(package).map_err(|e| e.to_string())?;
            return Ok(format!("Launching {package}"));
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
                describe_memory(&memory)
            });
        }
        Command::Gpus => unreachable!("answered before connecting"),
    }
    .map_err(|e| e.to_string())?;
    if !invocation.wait || invocation.command == Command::Status {
        return Ok(show(&status));
    }
    let deadline = Instant::now() + invocation.timeout;
    // The service answers a start once the session has moved on, so a Stopped answer
    // means the start has already failed.
    let (done, failed): (&[SessionState], &[SessionState]) = match invocation.command {
        Command::Start => (
            &[SessionState::Active, SessionState::Dozing],
            &[SessionState::Stopped],
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
