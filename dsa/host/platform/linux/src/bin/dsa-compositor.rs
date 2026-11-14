//! The DSA compositor for Linux (`dsa-compositor`): runs in the user's Wayland session and
//! shows each Android task in a window of its own.
//!
//! - `dsa-compositor`: runs the compositor. A second one in the same session exits at once.
//! - `dsa-compositor --launch <package>`: starts the app, as its launcher entry does.
//! - `dsa-compositor --app-settings <package>` and `--uninstall <package>`: the launcher
//!   entry's actions.
//! - `dsa-compositor --launch-shortcut <package> <id>`: starts one of the app's shortcuts, as
//!   the entry's shortcut actions and a pinned shortcut on the desktop do.
//! - `dsa-compositor --install-autostart`: starts the compositor with every session.

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("dsa-compositor runs on Linux only");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;

    use dsa_host_linux::compositor::session;

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some(action @ ("--launch" | "--app-settings" | "--uninstall")) => {
            let Some(package) = args.get(1) else {
                eprintln!("{action} needs the app's package name");
                return ExitCode::FAILURE;
            };
            match session::one_shot(action, package) {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => {
                    session::log(&why);
                    ExitCode::FAILURE
                }
            }
        }
        Some("--launch-shortcut") => {
            let (Some(package), Some(id)) = (args.get(1), args.get(2)) else {
                eprintln!("--launch-shortcut needs the app's package name and the shortcut's ID");
                return ExitCode::FAILURE;
            };
            match session::launch_shortcut(package, id) {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => {
                    session::log(&why);
                    ExitCode::FAILURE
                }
            }
        }
        Some("--install-autostart") => match install_autostart() {
            Ok(path) => {
                println!(
                    "The compositor starts with every session: {}",
                    path.display()
                );
                ExitCode::SUCCESS
            }
            Err(why) => {
                eprintln!("{why}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("unknown argument {other}");
            ExitCode::FAILURE
        }
        None => {
            let _lock = match single_instance() {
                Ok(Some(lock)) => lock,
                Ok(None) => return ExitCode::SUCCESS,
                Err(why) => {
                    session::log(&why);
                    return ExitCode::FAILURE;
                }
            };
            match session::run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(why) => {
                    session::log(&why);
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Holds a lock in the session's runtime folder while the compositor runs; `None` when
/// another compositor holds it.
#[cfg(target_os = "linux")]
fn single_instance() -> Result<Option<std::fs::File>, String> {
    use std::os::fd::AsRawFd;

    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .ok_or("XDG_RUNTIME_DIR is not set; the compositor runs in a desktop session")?;
    let path = dir.join("dsa-compositor.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    // SAFETY: locks a file this process holds open.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(locked.then_some(file))
}

/// Writes an XDG autostart entry that starts this program with the session.
#[cfg(target_os = "linux")]
fn install_autostart() -> Result<std::path::PathBuf, String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config"))
        })
        .ok_or("neither XDG_CONFIG_HOME nor HOME is set")?;
    let dir = config.join("autostart");
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let program = std::env::current_exe().map_err(|error| error.to_string())?;
    let path = dir.join("dsa-compositor.desktop");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Desktop Subsystem for Android\n\
         Exec=\"{}\"\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
        program.display()
    );
    std::fs::write(&path, entry).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}
