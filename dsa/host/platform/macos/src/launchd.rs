//! The per-user launchd agent that runs `dsa-hostd`.
//!
//! On macOS each user's Android runs in a process of that user: the Virtualization framework
//! needs no privilege, and a VM belongs to the process that made it. launchd starts the
//! agent when the user logs in and again if it ends with an error, as the Windows service
//! manager does for the Windows host. Android itself starts only when something asks for it
//! through the local API.
//!
//! The agent is a property list in `~/Library/LaunchAgents`, loaded into the user's GUI
//! domain (`gui/<uid>`), so it runs in their login session with access to their windows.
//!
//! The window manager (`dsa-wm`) has an agent of its own beside it, so it runs in the same
//! session and launchd gives it a Mach service ([`WM_LABEL`]), through which it hands each
//! task's frame surfaces to the app window hosts. `dsa-hostd` starts it through launchd when
//! Android needs its renderer, and launchd starts it again if it ends with an error.
//!
//! The device lender (`dsa-devices`) has an agent of its own too ([`DEVICES_LABEL`]), started
//! at login: it lends the Mac's sound, cameras, location, controllers and printers to the
//! user's Android from inside the login session, where macOS asks the user for each of them.

use std::path::{Path, PathBuf};

/// The agent's launchd label.
pub const LABEL: &str = "io.github.thisisthepy.dsa.hostd";

/// The window manager agent's launchd label, which is also the name of its Mach service.
pub const WM_LABEL: &str = "io.github.thisisthepy.dsa.wm";

/// The device lender agent's launchd label.
pub const DEVICES_LABEL: &str = "io.github.thisisthepy.dsa.devices";

/// The bundle identifier of DSA.app. Each agent names it, so macOS shows the agents as DSA,
/// with its icon, in the notice of new login items and in Login Items settings, rather than
/// by their programs' file names.
pub const APP_BUNDLE_ID: &str = "org.thisisthepy.dsa";

/// The device lender's program in an installation: `dsa-devices` in DSA.app's helper bundle
/// `Contents/Helpers/DSA Devices.app`, else beside `dsa-hostd`, as in a development build.
pub fn devices_beside(hostd: &Path) -> Option<PathBuf> {
    use crate::app_layout::{helper_program, DEVICES_HELPER, DEVICES_PROGRAM};
    helper_program(hostd, DEVICES_HELPER, DEVICES_PROGRAM)
}

/// Where the device lender agent's property list goes, below the user's home.
pub fn devices_plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{DEVICES_LABEL}.plist"))
}

/// The device lender agent's service target, for `launchctl kickstart` and `bootout`.
pub fn devices_service_target(uid: u32) -> String {
    format!("{}/{DEVICES_LABEL}", domain(uid))
}

/// The device lender agent's property list: `program`, started at login and restarted when
/// it ends with an error, its output appended to `log`.
pub fn devices_plist(program: &Path, log: &Path) -> String {
    let arguments = [program.to_string_lossy().into_owned()];
    agent_plist(DEVICES_LABEL, &arguments, None, log)
}

/// The window manager's program in an installation: `dsa-wm` in DSA.app's helper bundle
/// `Contents/Helpers/DSA Window Manager.app`, else beside `dsa-hostd`, as in a development
/// build.
pub fn window_manager_beside(hostd: &Path) -> Option<PathBuf> {
    use crate::app_layout::{helper_program, WINDOW_MANAGER_HELPER, WINDOW_MANAGER_PROGRAM};
    helper_program(hostd, WINDOW_MANAGER_HELPER, WINDOW_MANAGER_PROGRAM)
}

/// Where the window manager agent's property list goes, below the user's home.
pub fn wm_plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{WM_LABEL}.plist"))
}

/// The window manager agent's service target, for `launchctl kickstart` and `bootout`.
pub fn wm_service_target(uid: u32) -> String {
    format!("{}/{WM_LABEL}", domain(uid))
}

/// Where the agent's property list goes, below the user's home.
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// The launchd domain of the user `uid`'s login session.
pub fn domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// The agent's service target, for `launchctl kickstart` and `bootout`.
pub fn service_target(uid: u32) -> String {
    format!("{}/{LABEL}", domain(uid))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The agent's property list: `program serve [config]`, started at login, restarted when it
/// ends with an error, its output appended to `log`.
pub fn plist(program: &Path, config: Option<&Path>, log: &Path) -> String {
    let mut arguments = vec![program.to_string_lossy().into_owned(), "serve".into()];
    if let Some(config) = config {
        arguments.push(config.to_string_lossy().into_owned());
    }
    agent_plist(LABEL, &arguments, None, log)
}

/// The window manager agent's property list: `program`, started at login and restarted when
/// it ends with an error, with the Mach service [`WM_LABEL`], its output appended to `log`.
pub fn wm_plist(program: &Path, log: &Path) -> String {
    let arguments = [program.to_string_lossy().into_owned()];
    agent_plist(WM_LABEL, &arguments, Some(WM_LABEL), log)
}

fn agent_plist(label: &str, arguments: &[String], mach: Option<&str>, log: &Path) -> String {
    let arguments: String = arguments
        .iter()
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect();
    let log = xml_escape(&log.to_string_lossy());
    let label = xml_escape(label);
    // A Mach service launchd keeps for the program, which it checks in to.
    let mach = match mach {
        Some(name) => format!(
            "\t<key>MachServices</key>\n\t<dict>\n\t\t<key>{}</key>\n\t\t<true/>\n\t</dict>\n",
            xml_escape(name)
        ),
        None => String::new(),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>AssociatedBundleIdentifiers</key>\n\
         \t<array>\n\
         \t\t<string>{APP_BUNDLE_ID}</string>\n\
         \t</array>\n\
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
         \t<key>StandardOutPath</key>\n\
         \t<string>{log}</string>\n\
         \t<key>StandardErrorPath</key>\n\
         \t<string>{log}</string>\n\
         {mach}\
         </dict>\n\
         </plist>\n"
    )
}

/// The `launchctl` invocations that install the agent for user `uid`: unload any earlier
/// copy (which may fail, as there may be none), then load the new one, which starts it.
pub fn install_commands(uid: u32, plist: &Path) -> Vec<Vec<String>> {
    let plist = plist.to_string_lossy().into_owned();
    vec![
        vec!["bootout".into(), service_target(uid)],
        vec!["bootstrap".into(), domain(uid), plist],
    ]
}

/// How many times a bootstrap is tried, and how long to wait between tries, after the earlier
/// copy was booted out. `launchctl bootout` returns before launchd has let go of the service,
/// so a bootstrap right after it fails with "Input/output error" until it has.
pub const BOOTSTRAP_TRIES: u32 = 40;
pub const BOOTSTRAP_PAUSE: std::time::Duration = std::time::Duration::from_millis(250);

/// Runs `attempt` until it succeeds or has failed `tries` times, sleeping through `pause`
/// between tries, and returns the last error.
pub fn retry_bootstrap(
    tries: u32,
    mut pause: impl FnMut(),
    mut attempt: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut last = Err("launchctl bootstrap was never tried".to_string());
    for n in 0..tries.max(1) {
        if n > 0 {
            pause();
        }
        last = attempt();
        if last.is_ok() {
            break;
        }
    }
    last
}

/// The `launchctl` invocation that stops and unloads the agent.
pub fn uninstall_commands(uid: u32) -> Vec<Vec<String>> {
    vec![vec!["bootout".into(), service_target(uid)]]
}

/// The `launchctl` invocation that starts the agent if it is not running.
pub fn start_command(uid: u32) -> Vec<String> {
    vec!["kickstart".into(), service_target(uid)]
}

/// Runs `launchctl` with `arguments`. An error says what launchctl said.
#[cfg(target_os = "macos")]
pub fn launchctl(arguments: &[String]) -> Result<(), String> {
    let output = std::process::Command::new("/bin/launchctl")
        .args(arguments)
        .output()
        .map_err(|e| format!("running launchctl: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "launchctl {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Runs the `bootstrap` in `command`, waiting for launchd to finish unloading an earlier copy.
#[cfg(target_os = "macos")]
fn reload(command: &[String]) -> Result<(), String> {
    retry_bootstrap(
        BOOTSTRAP_TRIES,
        || std::thread::sleep(BOOTSTRAP_PAUSE),
        || launchctl(command),
    )
}

/// Writes the agent's property list for the user whose home is `home` and loads it. A window
/// manager installed beside `program` gets its own agent too.
#[cfg(target_os = "macos")]
pub fn install(
    home: &Path,
    uid: u32,
    program: &Path,
    config: Option<&Path>,
) -> Result<PathBuf, String> {
    let path = plist_path(home);
    let log = crate::host_config::user_root_in(home)
        .join("logs")
        .join("dsa-hostd.log");
    for dir in [path.parent(), log.parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, plist(program, config, &log))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    let commands = install_commands(uid, &path);
    // There may be no earlier copy to unload.
    let _ = launchctl(&commands[0]);
    reload(&commands[1])?;
    if let Some(window_manager) = window_manager_beside(program) {
        install_window_manager(home, uid, &window_manager)?;
    }
    if let Some(devices) = devices_beside(program) {
        install_devices(home, uid, &devices)?;
    }
    Ok(path)
}

/// Writes the device lender agent's property list for the user whose home is `home` and
/// loads it, which starts the lender.
#[cfg(target_os = "macos")]
pub fn install_devices(home: &Path, uid: u32, program: &Path) -> Result<PathBuf, String> {
    let path = devices_plist_path(home);
    let log = crate::host_config::user_root_in(home)
        .join("logs")
        .join("dsa-devices.log");
    for dir in [path.parent(), log.parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, devices_plist(program, &log))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    // There may be no earlier copy to unload.
    let _ = launchctl(&["bootout".to_string(), devices_service_target(uid)]);
    let plist = path.to_string_lossy().into_owned();
    reload(&["bootstrap".to_string(), domain(uid), plist])?;
    Ok(path)
}

/// Writes the window manager agent's property list for the user whose home is `home` and
/// loads it, which starts the window manager.
#[cfg(target_os = "macos")]
pub fn install_window_manager(home: &Path, uid: u32, program: &Path) -> Result<PathBuf, String> {
    let path = wm_plist_path(home);
    let log = crate::host_config::user_root_in(home)
        .join("logs")
        .join("dsa-wm.log");
    for dir in [path.parent(), log.parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, wm_plist(program, &log))
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    // There may be no earlier copy to unload.
    let _ = launchctl(&["bootout".to_string(), wm_service_target(uid)]);
    let plist = path.to_string_lossy().into_owned();
    reload(&["bootstrap".to_string(), domain(uid), plist])?;
    Ok(path)
}

/// Unloads the agents and removes their property lists.
#[cfg(target_os = "macos")]
pub fn uninstall(home: &Path, uid: u32) -> Result<(), String> {
    for command in uninstall_commands(uid) {
        // Not loaded is as good as unloaded.
        let _ = launchctl(&command);
    }
    let _ = launchctl(&["bootout".to_string(), wm_service_target(uid)]);
    let _ = launchctl(&["bootout".to_string(), devices_service_target(uid)]);
    for path in [
        plist_path(home),
        wm_plist_path(home),
        devices_plist_path(home),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("removing {}: {e}", path.display())),
        }
    }
    Ok(())
}
