//! The agents registered through `SMAppService`, from property lists inside DSA.app.
//!
//! An agent whose property list sits in `~/Library/LaunchAgents` is a legacy agent to macOS:
//! the notice of a new background item names it by its program's file name (`dsa-hostd`,
//! `dsa-wm`) whatever bundle it names, unless the app has a Developer ID. An agent that DSA.app
//! registers itself, from `Contents/Library/LaunchAgents/<label>.plist` in its own bundle, is
//! the app's own item: macOS shows it as DSA, with DSA's icon, and lists it under DSA in Login
//! Items settings.
//!
//! The property lists ship inside the app, so they cannot hold a path below the user's home.
//! Each names its program relative to the bundle (`BundleProgram`), and instead of
//! `StandardOutPath` it sets [`LOG_ENV`] to the log's file name: the program opens that file in
//! the user's `logs` folder itself ([`log_to_agent_file`]), so the logs stay where the legacy
//! agents wrote them. `dsa-hostd serve` is given no configuration argument and finds the
//! configuration where it always looks (the user's, then the machine's).
//!
//! Registration runs in `dsa-hostd install-agent`, from DSA.app's `Contents/MacOS`, so the
//! process's main bundle is DSA.app, as `SMAppService` requires. Where that cannot work (a
//! development build outside DSA.app, a configuration named on the command line, a macOS
//! without `SMAppService`, or a registration that fails) the legacy agents are installed
//! instead ([`choose_path`]).

use std::path::{Path, PathBuf};

use crate::launchd::{APP_BUNDLE_ID, DEVICES_LABEL, LABEL, WM_LABEL};

/// The environment variable a bundled agent sets to the file name of its log, which the
/// program opens itself in the user's `logs` folder.
pub const LOG_ENV: &str = "DSA_AGENT_LOG";

/// Where the bundled property lists are, inside DSA.app's `Contents`.
pub const BUNDLED_DIR: &str = "Library/LaunchAgents";

/// One of DSA's three agents, as DSA.app ships it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundledAgent {
    /// The launchd label, also the property list's file name without `.plist`.
    pub label: &'static str,
    /// The program, relative to the bundle (DSA.app).
    pub program: &'static str,
    /// The program's arguments after its name.
    pub arguments: &'static [&'static str],
    /// The Mach service launchd keeps for it, if any.
    pub mach_service: Option<&'static str>,
    /// The log's file name in the user's `logs` folder.
    pub log: &'static str,
}

impl BundledAgent {
    /// The property list's file name, which `SMAppService.agent(plistName:)` takes.
    pub fn plist_name(&self) -> String {
        format!("{}.plist", self.label)
    }
}

/// The service: `dsa-hostd serve`.
pub const HOSTD: BundledAgent = BundledAgent {
    label: LABEL,
    program: "Contents/MacOS/dsa-hostd",
    arguments: &["serve"],
    mach_service: None,
    log: "dsa-hostd.log",
};

/// The window manager, with its Mach service.
pub const WINDOW_MANAGER: BundledAgent = BundledAgent {
    label: WM_LABEL,
    program: "Contents/Helpers/DSA Window Manager.app/Contents/MacOS/dsa-wm",
    arguments: &[],
    mach_service: Some(WM_LABEL),
    log: "dsa-wm.log",
};

/// The device lender.
pub const DEVICES: BundledAgent = BundledAgent {
    label: DEVICES_LABEL,
    program: "Contents/Helpers/DSA Devices.app/Contents/MacOS/dsa-devices",
    arguments: &[],
    mach_service: None,
    log: "dsa-devices.log",
};

/// The three agents, in the order they are registered.
pub const AGENTS: [BundledAgent; 3] = [HOSTD, WINDOW_MANAGER, DEVICES];

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The bundled property list of `agent` for DSA `version`. The packaging keeps a copy of each
/// with `@VERSION@` in place of the version, which a test holds equal to this.
pub fn bundled_plist(agent: &BundledAgent, version: &str) -> String {
    let name = agent.program.rsplit('/').next().unwrap_or(agent.program);
    let arguments: String = std::iter::once(name)
        .chain(agent.arguments.iter().copied())
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect();
    let mach = match agent.mach_service {
        Some(service) => format!(
            "\t<key>MachServices</key>\n\t<dict>\n\t\t<key>{}</key>\n\t\t<true/>\n\t</dict>\n",
            xml_escape(service)
        ),
        None => String::new(),
    };
    let label = xml_escape(agent.label);
    let program = xml_escape(agent.program);
    let log = xml_escape(agent.log);
    let version = xml_escape(version);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- DSA {version} -->\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>AssociatedBundleIdentifiers</key>\n\
         \t<array>\n\
         \t\t<string>{APP_BUNDLE_ID}</string>\n\
         \t</array>\n\
         \t<key>BundleProgram</key>\n\
         \t<string>{program}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {arguments}\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<dict>\n\
         \t\t<key>SuccessfulExit</key>\n\
         \t\t<false/>\n\
         \t</dict>\n\
         \t<key>LimitLoadToSessionType</key>\n\
         \t<string>Aqua</string>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Interactive</string>\n\
         \t<key>EnvironmentVariables</key>\n\
         \t<dict>\n\
         \t\t<key>{LOG_ENV}</key>\n\
         \t\t<string>{log}</string>\n\
         \t</dict>\n\
         {mach}\
         </dict>\n\
         </plist>\n"
    )
}

/// Where the log named `file` (the value of [`LOG_ENV`]) goes for the user whose home is
/// `home`: the same file the legacy agents wrote. Only a bare file name is taken, so the
/// variable cannot point anywhere else.
pub fn agent_log_path(home: &Path, file: &str) -> Option<PathBuf> {
    let name = Path::new(file).file_name()?;
    (name == file && !file.is_empty()).then(|| {
        crate::host_config::user_root_in(home)
            .join("logs")
            .join(name)
    })
}

/// What `install-agent` can see before it tries anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Situation {
    /// This macOS has the `SMAppService` class (13 and later).
    pub service_management: bool,
    /// The program runs from DSA.app, which holds every bundled property list.
    pub bundled_plists: bool,
    /// A configuration was named on the command line, which a bundled property list cannot
    /// carry.
    pub config_argument: bool,
}

/// How the agents are installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPath {
    /// Registered by DSA.app through `SMAppService`.
    AppService,
    /// Written to `~/Library/LaunchAgents` and loaded with `launchctl`, for the reason given.
    Legacy(LegacyReason),
}

/// Why the legacy agents were installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyReason {
    NoServiceManagement,
    NotInApp,
    ConfigArgument,
    RegistrationFailed,
}

impl LegacyReason {
    /// Why, in words for the log.
    pub fn describe(self) -> &'static str {
        match self {
            Self::NoServiceManagement => "this macOS has no SMAppService",
            Self::NotInApp => {
                "this program does not run from DSA.app with its agent property lists"
            }
            Self::ConfigArgument => {
                "a configuration was named, which the agents inside DSA.app cannot pass on"
            }
            Self::RegistrationFailed => "SMAppService did not register the agents",
        }
    }
}

/// The path to try first.
pub fn choose_path(situation: Situation) -> InstallPath {
    if situation.config_argument {
        InstallPath::Legacy(LegacyReason::ConfigArgument)
    } else if !situation.service_management {
        InstallPath::Legacy(LegacyReason::NoServiceManagement)
    } else if !situation.bundled_plists {
        InstallPath::Legacy(LegacyReason::NotInApp)
    } else {
        InstallPath::AppService
    }
}

/// An agent's state as `SMAppService.status` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
    Unknown(isize),
}

impl Status {
    /// The status from `SMAppServiceStatus`'s raw value.
    pub fn from_raw(raw: isize) -> Self {
        match raw {
            0 => Self::NotRegistered,
            1 => Self::Enabled,
            2 => Self::RequiresApproval,
            3 => Self::NotFound,
            other => Self::Unknown(other),
        }
    }

    /// Registered, whether it runs now or once the user allows it in Login Items. Falling back
    /// to a legacy agent beside a registered one would run the program twice.
    pub fn registered(self) -> bool {
        matches!(self, Self::Enabled | Self::RequiresApproval)
    }

    /// The status in words.
    pub fn describe(self) -> String {
        match self {
            Self::NotRegistered => "not registered".into(),
            Self::Enabled => "enabled".into(),
            Self::RequiresApproval => {
                "requires approval in System Settings > General > Login Items".into()
            }
            Self::NotFound => "not found in DSA.app".into(),
            Self::Unknown(raw) => format!("unknown status {raw}"),
        }
    }
}

/// Whether the registration as a whole worked, from each agent's status after it.
pub fn registration_succeeded(statuses: &[Status]) -> bool {
    !statuses.is_empty() && statuses.iter().all(|s| s.registered())
}

/// One step of moving off the legacy agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationStep {
    /// Run `launchctl` with these arguments; it may fail, as the agent may not be loaded.
    Launchctl(Vec<String>),
    /// Remove this property list, if it is there.
    Remove(PathBuf),
}

/// The steps that unload and remove the legacy agents of user `uid` whose home is `home`,
/// each booted out before its property list is removed.
pub fn legacy_migration(home: &Path, uid: u32) -> Vec<MigrationStep> {
    use crate::launchd::{
        devices_plist_path, devices_service_target, plist_path, service_target, wm_plist_path,
        wm_service_target,
    };
    [
        (service_target(uid), plist_path(home)),
        (wm_service_target(uid), wm_plist_path(home)),
        (devices_service_target(uid), devices_plist_path(home)),
    ]
    .into_iter()
    .flat_map(|(target, plist)| {
        [
            MigrationStep::Launchctl(vec!["bootout".into(), target]),
            MigrationStep::Remove(plist),
        ]
    })
    .collect()
}

/// Whether every bundled property list is in the app whose `Contents` is `contents`.
pub fn bundled_plists_in(contents: &Path) -> bool {
    AGENTS.iter().all(|agent| {
        contents
            .join(BUNDLED_DIR)
            .join(agent.plist_name())
            .is_file()
    })
}

/// Whether the program at `program` runs from DSA.app (`Contents/MacOS`) with every bundled
/// property list beside it.
pub fn runs_from_app(program: &Path) -> bool {
    let Some(folder) = program.parent() else {
        return false;
    };
    let Some(contents) = folder.parent() else {
        return false;
    };
    folder.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && contents
            .parent()
            .and_then(Path::extension)
            .is_some_and(|ext| ext == "app")
        && bundled_plists_in(contents)
}

/// Sends this program's output to the log [`LOG_ENV`] names, when a bundled agent started it.
/// Run by hand, the variable is unset and the output stays where it was. A log that cannot be
/// opened leaves it there too, said on the way.
#[cfg(target_os = "macos")]
pub fn log_to_agent_file() {
    use std::os::fd::AsRawFd;
    let Some(file) = std::env::var_os(LOG_ENV) else {
        return;
    };
    // Children (the renderer, the window hosts) are not agents and keep this output.
    std::env::remove_var(LOG_ENV);
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("{LOG_ENV} is set but HOME is not, so the log stays on standard error");
        return;
    };
    let Some(path) = agent_log_path(&home, &file.to_string_lossy()) else {
        eprintln!("{LOG_ENV} is not a file name: {}", file.to_string_lossy());
        return;
    };
    let opened = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        });
    match opened {
        Ok(log) => {
            // SAFETY: dup2 onto the standard descriptors of this process; `log` stays open
            // until both copies are made.
            unsafe {
                libc::dup2(log.as_raw_fd(), 1);
                libc::dup2(log.as_raw_fd(), 2);
            }
        }
        Err(e) => eprintln!("opening the log {}: {e}", path.display()),
    }
}

#[cfg(target_os = "macos")]
mod service_management {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::{BundledAgent, Status};

    #[link(name = "ServiceManagement", kind = "framework")]
    extern "C" {}

    fn class() -> Option<&'static AnyClass> {
        AnyClass::get(c"SMAppService")
    }

    /// Whether this macOS has `SMAppService`.
    pub fn available() -> bool {
        class().is_some()
    }

    fn service(agent: &BundledAgent) -> Result<Retained<AnyObject>, String> {
        let class = class().ok_or("this macOS has no SMAppService")?;
        let name = crate::objc::ns_string(&agent.plist_name());
        let service: Option<Retained<AnyObject>> =
            unsafe { msg_send![class, agentServiceWithPlistName: &*name] };
        service.ok_or_else(|| format!("SMAppService gave no service for {}", agent.label))
    }

    fn describe_error(error: *mut AnyObject) -> String {
        if error.is_null() {
            return "no reason given".into();
        }
        let text: *mut AnyObject = unsafe { msg_send![error, localizedDescription] };
        let code: isize = unsafe { msg_send![error, code] };
        format!("{} (code {code})", crate::objc::string_of(text))
    }

    /// The agent's status.
    pub fn status(agent: &BundledAgent) -> Result<Status, String> {
        let service = service(agent)?;
        let raw: isize = unsafe { msg_send![&*service, status] };
        Ok(Status::from_raw(raw))
    }

    /// Registers the agent, which loads and starts it once the user allows it.
    pub fn register(agent: &BundledAgent) -> Result<(), String> {
        let service = service(agent)?;
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = unsafe { msg_send![&*service, registerAndReturnError: &mut error] };
        if ok {
            Ok(())
        } else {
            Err(format!(
                "registering {}: {}",
                agent.label,
                describe_error(error)
            ))
        }
    }

    /// Unregisters the agent, which stops it.
    pub fn unregister(agent: &BundledAgent) -> Result<(), String> {
        let service = service(agent)?;
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = unsafe { msg_send![&*service, unregisterAndReturnError: &mut error] };
        if ok {
            Ok(())
        } else {
            Err(format!(
                "unregistering {}: {}",
                agent.label,
                describe_error(error)
            ))
        }
    }
}

/// Runs the steps of [`legacy_migration`].
#[cfg(target_os = "macos")]
pub fn remove_legacy_agents(home: &Path, uid: u32) -> Result<(), String> {
    for step in legacy_migration(home, uid) {
        match step {
            MigrationStep::Launchctl(arguments) => {
                // Not loaded is as good as unloaded.
                let _ = crate::launchd::launchctl(&arguments);
            }
            MigrationStep::Remove(path) => match std::fs::remove_file(&path) {
                Ok(()) => eprintln!("removed the legacy agent {}", path.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("removing {}: {e}", path.display())),
            },
        }
    }
    Ok(())
}

/// How often a registration is tried, and how long to wait between tries: launchd may still
/// hold the label of an agent just booted out or unregistered.
const REGISTER_TRIES: u32 = 20;
const REGISTER_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);

/// Registers the bundled agents, after moving off any legacy ones. Each agent is unregistered
/// first, so an upgrade starts the new programs. Returns each agent's status after.
#[cfg(target_os = "macos")]
pub fn register_all(home: &Path, uid: u32) -> Result<Vec<(BundledAgent, Status)>, String> {
    remove_legacy_agents(home, uid)?;
    let mut statuses = Vec::new();
    for agent in AGENTS {
        if service_management::status(&agent)?.registered() {
            let _ = service_management::unregister(&agent);
        }
        let registered = crate::launchd::retry_bootstrap(
            REGISTER_TRIES,
            || std::thread::sleep(REGISTER_PAUSE),
            || service_management::register(&agent),
        );
        let status = service_management::status(&agent)?;
        if let Err(why) = registered {
            if !status.registered() {
                return Err(why);
            }
        }
        statuses.push((agent, status));
    }
    Ok(statuses)
}

/// Installs the agents for the user whose home is `home`: through `SMAppService` when this
/// program runs from DSA.app, else (or when that fails) as legacy agents. Says which path was
/// taken and each agent's status.
#[cfg(target_os = "macos")]
pub fn install(
    home: &Path,
    uid: u32,
    program: &Path,
    config: Option<&Path>,
) -> Result<InstallPath, String> {
    let situation = Situation {
        service_management: service_management::available(),
        bundled_plists: runs_from_app(program),
        config_argument: config.is_some(),
    };
    let reason = match choose_path(situation) {
        InstallPath::AppService => match register_all(home, uid) {
            Ok(statuses) => {
                let ok =
                    registration_succeeded(&statuses.iter().map(|(_, s)| *s).collect::<Vec<_>>());
                for (agent, status) in &statuses {
                    println!("{}: {}", agent.label, status.describe());
                }
                if ok {
                    println!("registered the agents with SMAppService from DSA.app");
                    return Ok(InstallPath::AppService);
                }
                for agent in AGENTS {
                    let _ = service_management::unregister(&agent);
                }
                LegacyReason::RegistrationFailed
            }
            Err(why) => {
                eprintln!("SMAppService registration failed: {why}");
                // Nothing half registered is left beside the legacy agents.
                for agent in AGENTS {
                    let _ = service_management::unregister(&agent);
                }
                LegacyReason::RegistrationFailed
            }
        },
        InstallPath::Legacy(reason) => reason,
    };
    println!(
        "installing legacy agents in ~/Library/LaunchAgents: {}",
        reason.describe()
    );
    let plist = crate::launchd::install(home, uid, program, config)?;
    println!("installed and started {}", plist.display());
    Ok(InstallPath::Legacy(reason))
}

/// Unregisters the bundled agents (where `SMAppService` exists and this is DSA.app) and
/// removes any legacy ones.
#[cfg(target_os = "macos")]
pub fn uninstall(home: &Path, uid: u32) -> Result<(), String> {
    if service_management::available() {
        for agent in AGENTS {
            match service_management::status(&agent) {
                Ok(status) if status.registered() => match service_management::unregister(&agent) {
                    Ok(()) => println!("unregistered {}", agent.label),
                    Err(why) => eprintln!("{why}"),
                },
                Ok(_) => {}
                Err(why) => eprintln!("{why}"),
            }
        }
    }
    crate::launchd::uninstall(home, uid)
}

/// Whether launchd has the agent `target` (`gui/<uid>/<label>`) loaded, by whichever path it
/// was installed.
#[cfg(target_os = "macos")]
pub fn loaded(target: &str) -> bool {
    std::process::Command::new("/bin/launchctl")
        .args(["print", target])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
