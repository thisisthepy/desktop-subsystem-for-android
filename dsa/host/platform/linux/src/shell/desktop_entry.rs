//! A launcher entry (a `.desktop` file) and an icon for each app installed in the guest.
//!
//! The entries go in the user's `applications` folder, where every desktop's app menu and
//! search look, each with the app's own name and icon. Starting one asks the guest to start
//! the app, which opens its window. The entry's ID is also the `app_id` of the app's windows,
//! so the desktop groups the windows under the app's icon in its dock or task bar, and its
//! actions open the app's settings or uninstall it, as the Windows Start menu does. The app's
//! own launcher shortcuts (a messenger's "New chat") are actions of the entry too, the way a
//! dock's right-click menu shows them, each with its own name and icon.
//!
//! A copy of an app's entry can also sit in the user's desktop folder (the one
//! `xdg-user-dirs` names), which is what "Create desktop shortcut" makes, and an app's
//! request to pin one of its shortcuts makes an entry there that starts that shortcut.
//!
//! Every entry this host writes carries [`PACKAGE_KEY`], so an entry of an app that is gone
//! is found and removed, in both folders, without touching anything else in them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dsa_host_core::compositor::window_hosts::is_valid_package;
use dsa_host_core::tasks::{is_valid_shortcut_id, jump_list_shortcuts, AppInfo, AppShortcut};

/// The key each entry names its package with.
pub const PACKAGE_KEY: &str = "X-DSA-Package";

/// The key a pinned shortcut's entry names the shortcut's ID with.
pub const SHORTCUT_KEY: &str = "X-DSA-Shortcut";

/// The prefix of every entry's ID.
pub const PREFIX: &str = "dsa-";

/// The argument of this host's compositor that starts one of an app's shortcuts:
/// `--launch-shortcut <package> <id>`.
pub const LAUNCH_SHORTCUT: &str = "--launch-shortcut";

/// The ID of the app's entry and of its windows (`xdg_toplevel.set_app_id`).
pub fn app_id(package: &str) -> String {
    format!("{PREFIX}{package}")
}

/// The file name of the app's entry.
pub fn file_name(package: &str) -> String {
    format!("{}.desktop", app_id(package))
}

/// A short name for a shortcut's ID that any file name can carry: an FNV-1a hash of it, in
/// hex. Android takes any text as an ID, which a file name cannot always.
pub fn shortcut_tag(id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The file name of the desktop entry of the app's pinned shortcut `id`.
pub fn pinned_file_name(package: &str, id: &str) -> String {
    format!("{}-shortcut-{}.desktop", app_id(package), shortcut_tag(id))
}

/// Where the icon of the app's shortcut `id` is kept, in the icons folder `icons`.
pub fn shortcut_icon_path(icons: &Path, package: &str, id: &str) -> PathBuf {
    icons.join(format!("{package}.shortcut-{}.png", shortcut_tag(id)))
}

/// A value as a `.desktop` string: the spec's escapes for backslash and line breaks.
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => {}
            c => escaped.push(c),
        }
    }
    escaped
}

/// An argument of `Exec`, quoted when it has to be, as the spec quotes them.
fn exec_argument(argument: &str) -> String {
    let plain = argument
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/-_.+=:,@".contains(c));
    if plain && !argument.is_empty() {
        return argument.to_string();
    }
    let mut quoted = String::from("\"");
    for c in argument.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        if c == '%' {
            quoted.push('%');
        }
        quoted.push(c);
    }
    quoted.push('"');
    // The spec reads the whole value as a string first, so every backslash doubles again.
    quoted.replace('\\', "\\\\")
}

/// The command line that runs `program` with `arguments`, for `Exec`.
fn exec(program: &Path, arguments: &[&str]) -> String {
    let mut line = exec_argument(&program.to_string_lossy());
    for argument in arguments {
        line.push(' ');
        line.push_str(&exec_argument(argument));
    }
    line
}

/// The entry of `app`, started through `program` (this host's compositor), with its icon at
/// `icon` when it has one. Its shortcuts are actions without icons of their own.
pub fn entry(app: &AppInfo, program: &Path, icon: Option<&Path>) -> String {
    entry_with(app, program, icon, None)
}

/// [`entry`], with each shortcut's icon kept in the icons folder `icons` when it has one.
pub fn entry_with(
    app: &AppInfo,
    program: &Path,
    icon: Option<&Path>,
    icons: Option<&Path>,
) -> String {
    let package = &app.package_name;
    let name = if app.label.trim().is_empty() {
        package.as_str()
    } else {
        app.label.as_str()
    };
    let shortcuts = jump_list_shortcuts(&app.shortcuts);
    let mut text = String::new();
    text.push_str("[Desktop Entry]\n");
    text.push_str("Type=Application\n");
    text.push_str("Version=1.5\n");
    text.push_str(&format!("Name={}\n", escape(name)));
    text.push_str("Comment=Desktop Subsystem for Android\n");
    if let Some(icon) = icon {
        text.push_str(&format!("Icon={}\n", escape(&icon.to_string_lossy())));
    }
    text.push_str(&format!("Exec={}\n", exec(program, &["--launch", package])));
    text.push_str("Terminal=false\n");
    text.push_str("Categories=Utility;\n");
    text.push_str(&format!("StartupWMClass={}\n", app_id(package)));
    text.push_str("StartupNotify=false\n");
    // The app's shortcuts first, as a launcher lists them above its own items.
    let mut actions: String = (0..shortcuts.len())
        .map(|index| format!("shortcut-{index};"))
        .collect();
    actions.push_str("settings;uninstall;");
    text.push_str(&format!("Actions={actions}\n"));
    text.push_str(&format!("{PACKAGE_KEY}={package}\n"));
    for (index, shortcut) in shortcuts.iter().enumerate() {
        text.push_str(&format!("\n[Desktop Action shortcut-{index}]\n"));
        text.push_str(&format!("Name={}\n", escape(shortcut.label())));
        if let Some(icons) = icons.filter(|_| !shortcut.icon_png.is_empty()) {
            let path = shortcut_icon_path(icons, package, &shortcut.id);
            text.push_str(&format!("Icon={}\n", escape(&path.to_string_lossy())));
        }
        text.push_str(&format!(
            "Exec={}\n",
            exec(program, &[LAUNCH_SHORTCUT, package, &shortcut.id])
        ));
    }
    text.push_str("\n[Desktop Action settings]\n");
    text.push_str("Name=App settings\n");
    text.push_str(&format!(
        "Exec={}\n",
        exec(program, &["--app-settings", package])
    ));
    text.push_str("\n[Desktop Action uninstall]\n");
    text.push_str("Name=Uninstall\n");
    text.push_str(&format!(
        "Exec={}\n",
        exec(program, &["--uninstall", package])
    ));
    text
}

/// The desktop entry of `shortcut`, an app's pinned shortcut: it starts the shortcut through
/// `program`, under the shortcut's name and icon (`icon`, when it has one).
pub fn pinned_entry(
    package: &str,
    shortcut: &AppShortcut,
    program: &Path,
    icon: Option<&Path>,
) -> String {
    let mut text = String::new();
    text.push_str("[Desktop Entry]\n");
    text.push_str("Type=Application\n");
    text.push_str("Version=1.5\n");
    text.push_str(&format!("Name={}\n", escape(shortcut.label())));
    if !shortcut.long_label.trim().is_empty() {
        text.push_str(&format!("Comment={}\n", escape(&shortcut.long_label)));
    }
    if let Some(icon) = icon {
        text.push_str(&format!("Icon={}\n", escape(&icon.to_string_lossy())));
    }
    text.push_str(&format!(
        "Exec={}\n",
        exec(program, &[LAUNCH_SHORTCUT, package, &shortcut.id])
    ));
    text.push_str("Terminal=false\n");
    text.push_str(&format!("StartupWMClass={}\n", app_id(package)));
    text.push_str("StartupNotify=false\n");
    text.push_str(&format!("{PACKAGE_KEY}={package}\n"));
    text.push_str(&format!("{SHORTCUT_KEY}={}\n", escape(&shortcut.id)));
    text
}

/// The package an entry this host wrote is for; `None` for any other file.
pub fn package_of(text: &str) -> Option<&str> {
    text.lines()
        .find_map(|line| line.strip_prefix(PACKAGE_KEY)?.strip_prefix('='))
        .map(str::trim)
}

/// Whether an entry this host wrote is a pinned shortcut's.
pub fn is_pinned_entry(text: &str) -> bool {
    text.lines().any(|line| {
        line.strip_prefix(SHORTCUT_KEY)
            .is_some_and(|rest| rest.starts_with('='))
    })
}

/// The desktop folder `user-dirs.dirs` (its text, when there is one) gives, below `home`:
/// `XDG_DESKTOP_DIR`, else `~/Desktop`. A value that names the home itself, or is neither
/// `$HOME/...` nor absolute, falls back to `~/Desktop` too, as `xdg-user-dir` does.
pub fn desktop_dir_in(user_dirs: Option<&str>, home: &Path) -> PathBuf {
    let fallback = home.join("Desktop");
    let Some(text) = user_dirs else {
        return fallback;
    };
    let value = text.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let rest = line
            .strip_prefix("XDG_DESKTOP_DIR")?
            .trim_start()
            .strip_prefix('=')?;
        Some(rest.trim().trim_matches('"').to_string())
    });
    let Some(value) = value else {
        return fallback;
    };
    let path = if let Some(below) = value.strip_prefix("$HOME") {
        if !below.is_empty() && !below.starts_with('/') {
            return fallback;
        }
        home.join(below.trim_start_matches('/'))
    } else if value.starts_with('/') {
        PathBuf::from(value)
    } else {
        return fallback;
    };
    if path.components().eq(home.components()) {
        fallback
    } else {
        path
    }
}

/// The user's desktop folder, from `$XDG_CONFIG_HOME/user-dirs.dirs` (`~/.config` when
/// unset). `None` without a home.
pub fn desktop_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.has_root())?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.has_root())
        .unwrap_or_else(|| home.join(".config"));
    let text = fs::read_to_string(config.join("user-dirs.dirs")).ok();
    Some(desktop_dir_in(text.as_deref(), &home))
}

/// Where the entries and the icons go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folders {
    /// `$XDG_DATA_HOME/applications`.
    pub applications: PathBuf,
    /// The folder the icons are kept in.
    pub icons: PathBuf,
    /// `$XDG_DATA_HOME/icons/hicolor`: the user's part of the theme every icon theme falls
    /// back to, where each app's icon is also kept by its name ([`icon_name`]), for what
    /// takes an icon by name rather than by path, such as polkit's dialog.
    pub theme_icons: PathBuf,
    /// The user's desktop folder, where desktop shortcuts go; `None` when it is not known.
    pub desktop: Option<PathBuf>,
}

impl Folders {
    /// The user's folders: `$XDG_DATA_HOME` (`~/.local/share` when unset). The desktop
    /// folder is left out; [`Folders::from_env`] reads it.
    pub fn of_user(data_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<Folders> {
        let data = data_home
            .filter(|path| path.has_root())
            .or_else(|| home.map(|home| home.join(".local/share")))?;
        Some(Folders {
            applications: data.join("applications"),
            icons: data.join("dsa/icons"),
            theme_icons: data.join("icons/hicolor"),
            desktop: None,
        })
    }

    /// The user's folders, from the environment, the desktop folder included.
    pub fn from_env() -> Option<Folders> {
        Folders::of_user(
            std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
        .map(|folders| Folders {
            desktop: desktop_dir(),
            ..folders
        })
    }
}

/// Writes the entries of `apps` and removes those of apps no longer installed, in the
/// applications folder and the desktop folder. An app's desktop copy is kept in step with its
/// entry. An app with a name no file can take is left out. Returns how many entries were
/// written to the applications folder.
pub fn sync(folders: &Folders, apps: &[AppInfo], program: &Path) -> io::Result<usize> {
    fs::create_dir_all(&folders.applications)?;
    fs::create_dir_all(&folders.icons)?;
    let mut written = 0;
    for app in apps {
        if !is_valid_package(&app.package_name) {
            continue;
        }
        let icon = if app.icon_png.is_empty() {
            None
        } else {
            let path = folders.icons.join(format!("{}.png", app.package_name));
            write_if_changed(&path, &app.icon_png)?;
            write_theme_icon(&folders.theme_icons, &app.package_name, &app.icon_png)?;
            Some(path)
        };
        let mut kept_icons = Vec::new();
        for shortcut in jump_list_shortcuts(&app.shortcuts) {
            if !shortcut.icon_png.is_empty() {
                let path = shortcut_icon_path(&folders.icons, &app.package_name, &shortcut.id);
                write_if_changed(&path, &shortcut.icon_png)?;
                kept_icons.push(path);
            }
        }
        remove_shortcut_icons(folders, &app.package_name, &kept_icons);
        let text = entry_with(app, program, icon.as_deref(), Some(&folders.icons));
        write_if_changed(
            &folders.applications.join(file_name(&app.package_name)),
            text.as_bytes(),
        )?;
        if let Some(desktop) = &folders.desktop {
            // A copy the user put on the desktop follows the app's name, icon and shortcuts.
            let copy = desktop.join(file_name(&app.package_name));
            let ours = fs::read_to_string(&copy)
                .ok()
                .is_some_and(|old| package_of(&old) == Some(app.package_name.as_str()));
            if ours {
                write_desktop_file(&copy, text.as_bytes())?;
            }
        }
        written += 1;
    }
    let gone = |package: &str| !apps.iter().any(|app| app.package_name == package);
    for folder in std::iter::once(&folders.applications).chain(folders.desktop.as_ref()) {
        let Ok(items) = fs::read_dir(folder) else {
            continue;
        };
        for item in items.flatten() {
            let path = item.path();
            let named_ours = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(PREFIX) && name.ends_with(".desktop"));
            if !named_ours {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(package) = package_of(&text) {
                if gone(package) {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
    // The icons of the apps that are gone, their shortcuts' and pinned shortcuts' included.
    if let Ok(items) = fs::read_dir(&folders.icons) {
        for item in items.flatten() {
            let Some(name) = item.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(package) = icon_package(&name) else {
                continue;
            };
            if gone(package) {
                let _ = fs::remove_file(item.path());
                for size in THEME_SIZES {
                    let _ = fs::remove_file(theme_icon_path(&folders.theme_icons, package, size));
                }
            }
        }
    }
    Ok(written)
}

/// The package an icon in the icons folder is of: `<package>.png` or
/// `<package>.shortcut-<tag>.png`.
fn icon_package(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".png")?;
    let package = match stem.rfind(".shortcut-") {
        Some(at) => &stem[..at],
        None => stem,
    };
    is_valid_package(package).then_some(package)
}

/// Removes the icons of `package`'s shortcuts that are no longer in its jump list, but not
/// one a pinned shortcut's desktop entry still shows.
fn remove_shortcut_icons(folders: &Folders, package: &str, kept: &[PathBuf]) {
    let prefix = format!("{package}.shortcut-");
    let pinned = pinned_icons(folders, package);
    let Ok(items) = fs::read_dir(&folders.icons) else {
        return;
    };
    for item in items.flatten() {
        let path = item.path();
        let is_shortcut_icon = item
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".png"));
        if is_shortcut_icon && !kept.contains(&path) && !pinned.contains(&path) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// The icons the pinned shortcuts of `package` on the desktop show.
fn pinned_icons(folders: &Folders, package: &str) -> Vec<PathBuf> {
    let Some(desktop) = &folders.desktop else {
        return Vec::new();
    };
    let Ok(items) = fs::read_dir(desktop) else {
        return Vec::new();
    };
    items
        .flatten()
        .filter_map(|item| fs::read_to_string(item.path()).ok())
        .filter(|text| is_pinned_entry(text) && package_of(text) == Some(package))
        .filter_map(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("Icon="))
                .map(PathBuf::from)
        })
        .collect()
}

/// Puts a copy of `app`'s entry on the desktop: the "Create desktop shortcut" of the
/// settings apps. The entry the compositor keeps in the applications folder is copied, so
/// the copy starts the app as the app menu does; it follows the app from then on and goes
/// with it ([`sync`]).
pub fn place_on_desktop_in(folders: &Folders, app: &AppInfo) -> Result<PathBuf, String> {
    if !is_valid_package(&app.package_name) {
        return Err(format!("{:?} is not a package name", app.package_name));
    }
    let desktop = folders
        .desktop
        .as_ref()
        .ok_or("the desktop folder is not known: HOME is not set")?;
    let source = folders.applications.join(file_name(&app.package_name));
    let text = fs::read_to_string(&source).map_err(|e| {
        format!(
            "the app menu entry of {} is not made yet ({}: {e}); the DSA compositor makes it \
             once it runs",
            app.label,
            source.display()
        )
    })?;
    if package_of(&text) != Some(app.package_name.as_str()) {
        return Err(format!("{} is not DSA's entry", source.display()));
    }
    fs::create_dir_all(desktop).map_err(|e| format!("{}: {e}", desktop.display()))?;
    let path = desktop.join(file_name(&app.package_name));
    if path.exists()
        && fs::read_to_string(&path)
            .ok()
            .and_then(|old| package_of(&old).map(str::to_string))
            .as_deref()
            != Some(app.package_name.as_str())
    {
        return Err(format!(
            "{} is another program's; it is left as it is",
            path.display()
        ));
    }
    write_desktop_file(&path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// [`place_on_desktop_in`] in the user's own folders.
pub fn place_on_desktop(app: &AppInfo) -> Result<PathBuf, String> {
    let folders = Folders::from_env().ok_or("the user's folders are not known: HOME is not set")?;
    place_on_desktop_in(&folders, app)
}

/// Puts an entry that starts `shortcut`, one of `package`'s shortcuts the user agreed to pin,
/// on the desktop, started through `program`. Its icon is the shortcut's, else the app's.
pub fn place_pinned_in(
    folders: &Folders,
    package: &str,
    shortcut: &AppShortcut,
    program: &Path,
) -> Result<PathBuf, String> {
    if !is_valid_package(package) {
        return Err(format!("{package:?} is not a package name"));
    }
    if !is_valid_shortcut_id(&shortcut.id) {
        return Err(format!(
            "{:?} is not a shortcut ID a desktop entry can carry",
            shortcut.id
        ));
    }
    let desktop = folders
        .desktop
        .as_ref()
        .ok_or("the desktop folder is not known: HOME is not set")?;
    fs::create_dir_all(&folders.icons).map_err(|e| format!("{}: {e}", folders.icons.display()))?;
    let icon = if shortcut.icon_png.is_empty() {
        let app_icon = folders.icons.join(format!("{package}.png"));
        app_icon.exists().then_some(app_icon)
    } else {
        let path = shortcut_icon_path(&folders.icons, package, &shortcut.id);
        write_if_changed(&path, &shortcut.icon_png)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Some(path)
    };
    fs::create_dir_all(desktop).map_err(|e| format!("{}: {e}", desktop.display()))?;
    let path = desktop.join(pinned_file_name(package, &shortcut.id));
    let text = pinned_entry(package, shortcut, program, icon.as_deref());
    write_desktop_file(&path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Writes a desktop entry to the desktop folder: executable, as file managers want before
/// they start one, and marked trusted for GNOME's, which asks otherwise.
fn write_desktop_file(path: &Path, content: &[u8]) -> io::Result<()> {
    write_if_changed(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(target_os = "linux")]
    {
        // Nautilus reads the mark from GIO's metadata; a desktop without GIO has no use for
        // it, so a failure here changes nothing.
        let _ = std::process::Command::new("gio")
            .arg("set")
            .arg(path)
            .arg("metadata::trusted")
            .arg("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    Ok(())
}

/// The icon theme's sizes, as hicolor's own `index.theme` lists its `apps` folders. A theme
/// reads only the folders its index lists, so an icon goes in the one nearest its size.
pub const THEME_SIZES: [u32; 13] = [16, 22, 24, 32, 36, 48, 64, 72, 96, 128, 192, 256, 512];

/// The icon theme name of the app's icon: the same as its entry's ID.
pub fn icon_name(package: &str) -> String {
    app_id(package)
}

/// Where the app's icon is kept in the theme folder `theme` at `size`.
pub fn theme_icon_path(theme: &Path, package: &str, size: u32) -> PathBuf {
    theme
        .join(format!("{size}x{size}"))
        .join("apps")
        .join(format!("{}.png", icon_name(package)))
}

/// The theme size nearest a PNG image's width, read from its header; 256 when the header
/// cannot be read.
pub fn theme_size(png: &[u8]) -> u32 {
    let width = png
        .get(16..20)
        .filter(|_| png.starts_with(b"\x89PNG\r\n\x1a\n"))
        .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    let Some(width) = width.filter(|width| *width > 0) else {
        return 256;
    };
    THEME_SIZES
        .into_iter()
        .min_by_key(|size| size.abs_diff(width))
        .unwrap_or(256)
}

/// Keeps the app's icon in the theme folder `theme`, at the size nearest its own, and in no
/// other size.
fn write_theme_icon(theme: &Path, package: &str, png: &[u8]) -> io::Result<()> {
    let size = theme_size(png);
    let path = theme_icon_path(theme, package, size);
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    write_if_changed(&path, png)?;
    for other in THEME_SIZES.into_iter().filter(|other| *other != size) {
        let _ = fs::remove_file(theme_icon_path(theme, package, other));
    }
    Ok(())
}

/// Writes `content` to `path` unless it already holds it, through a file beside it so a
/// desktop reading the folder never sees half an entry.
fn write_if_changed(path: &Path, content: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    let partial = path.with_extension("partial");
    fs::write(&partial, content)?;
    fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(package: &str, label: &str) -> AppInfo {
        AppInfo {
            package_name: package.into(),
            label: label.into(),
            ..AppInfo::default()
        }
    }

    fn shortcut(id: &str, label: &str, rank: u32) -> AppShortcut {
        AppShortcut {
            id: id.into(),
            short_label: label.into(),
            icon_png: vec![0x89, b'P', b'N', b'G', rank as u8],
            rank,
            dynamic: true,
            ..AppShortcut::default()
        }
    }

    /// A fresh folder of the user's folders under the system's temporary folder.
    fn temp_folders(name: &str) -> (PathBuf, Folders) {
        let root = std::env::temp_dir().join(format!("dsa-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut folders = Folders::of_user(Some(root.join("data")), None).unwrap();
        folders.desktop = Some(root.join("Desktop"));
        (root, folders)
    }

    #[test]
    fn fr7_each_app_gets_an_entry_with_its_name_icon_and_window_class() {
        let text = entry(
            &app("com.example.notes", "Notes"),
            Path::new("/usr/lib/dsa/dsa-compositor"),
            Some(Path::new(
                "/home/me/.local/share/dsa/icons/com.example.notes.png",
            )),
        );
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("\nName=Notes\n"));
        assert!(text.contains("\nIcon=/home/me/.local/share/dsa/icons/com.example.notes.png\n"));
        assert!(text.contains("\nExec=/usr/lib/dsa/dsa-compositor --launch com.example.notes\n"));
        assert!(text.contains("\nStartupWMClass=dsa-com.example.notes\n"));
        assert!(text.contains("--app-settings com.example.notes"));
        assert!(text.contains("--uninstall com.example.notes"));
        assert!(text.contains("\nActions=settings;uninstall;\n"));
        assert_eq!(package_of(&text), Some("com.example.notes"));
    }

    #[test]
    fn fr7_a_name_or_a_path_with_special_characters_is_escaped() {
        let text = entry(
            &app("com.example.a", "Two\nLines \\ here"),
            Path::new("/opt/My Apps/dsa-compositor"),
            None,
        );
        assert!(text.contains("\nName=Two\\nLines \\\\ here\n"));
        assert!(text.contains("\nExec=\"/opt/My Apps/dsa-compositor\" --launch com.example.a\n"));
        assert!(!text.contains("Icon="));
    }

    #[test]
    fn fr7_an_app_without_a_label_is_named_by_its_package() {
        let text = entry(&app("com.example.b", " "), Path::new("/x"), None);
        assert!(text.contains("\nName=com.example.b\n"));
    }

    #[test]
    fn fr7_the_window_class_matches_the_entry() {
        assert_eq!(app_id("com.example.c"), "dsa-com.example.c");
        assert_eq!(file_name("com.example.c"), "dsa-com.example.c.desktop");
    }

    #[test]
    fn fr7_entries_go_in_the_users_applications_folder() {
        let folders = Folders::of_user(None, Some(PathBuf::from("/home/me"))).unwrap();
        assert_eq!(
            folders.applications,
            PathBuf::from("/home/me/.local/share/applications")
        );
        let folders = Folders::of_user(Some(PathBuf::from("/data")), None).unwrap();
        assert_eq!(folders.icons, PathBuf::from("/data/dsa/icons"));
        assert_eq!(folders.theme_icons, PathBuf::from("/data/icons/hicolor"));
        assert_eq!(
            Folders::of_user(Some(PathBuf::from("relative")), None),
            None,
            "a relative XDG_DATA_HOME is ignored, as the spec says"
        );
    }

    #[test]
    fn fr7_each_jump_list_shortcut_is_an_action_of_the_apps_entry() {
        let mut chat = app("com.example.chat", "Chat");
        chat.shortcuts = vec![
            shortcut("ana", "Ana", 1),
            shortcut("new-chat", "New chat", 0),
            AppShortcut {
                pinned: true,
                dynamic: false,
                ..shortcut("pinned-only", "Pinned", 0)
            },
        ];
        let text = entry_with(
            &chat,
            Path::new("/usr/lib/dsa/dsa-compositor"),
            None,
            Some(Path::new("/icons")),
        );
        assert!(text.contains("\nActions=shortcut-0;shortcut-1;settings;uninstall;\n"));
        let first = text.find("[Desktop Action shortcut-0]").unwrap();
        let second = text.find("[Desktop Action shortcut-1]").unwrap();
        assert!(first < second);
        assert!(text[first..second].contains("\nName=New chat\n"));
        // The icon is the file `shortcut_icon_path` names in the icons folder. A literal with
        // `/` holds only where `/` is the separator, and the tests also run on Windows.
        let icon = shortcut_icon_path(Path::new("/icons"), "com.example.chat", "new-chat");
        assert_eq!(
            icon,
            Path::new("/icons").join(format!(
                "com.example.chat.shortcut-{}.png",
                shortcut_tag("new-chat")
            ))
        );
        // Off Unix the entry also escapes the separator, so there the line is checked only
        // for the file it names.
        let icon_line = text[first..second]
            .lines()
            .find(|line| line.starts_with("Icon="))
            .expect("the shortcut's action has an icon");
        let file = icon.file_name().unwrap().to_string_lossy().into_owned();
        assert!(icon_line.ends_with(&file), "{icon_line} names {file}");
        #[cfg(unix)]
        assert!(text[first..second].contains(&format!(
            "\nIcon=/icons/com.example.chat.shortcut-{}.png\n",
            shortcut_tag("new-chat")
        )));
        assert!(text[first..second].contains(
            "\nExec=/usr/lib/dsa/dsa-compositor --launch-shortcut com.example.chat new-chat\n"
        ));
        assert!(text[second..].contains("\nName=Ana\n"));
        assert!(
            !text.contains("Pinned"),
            "a pinned-only shortcut is no action"
        );
    }

    #[test]
    fn fr7_an_odd_shortcut_id_is_quoted_so_no_shell_reads_it() {
        let mut chat = app("com.example.chat", "Chat");
        chat.shortcuts = vec![shortcut("a b\"$(rm)`%", "Odd", 0)];
        let text = entry(&chat, Path::new("/x"), None);
        assert!(text.contains(
            "\nExec=/x --launch-shortcut com.example.chat \"a b\\\\\"\\\\$(rm)\\\\`%%\"\n"
        ));
        // Two IDs never share an icon or a pinned entry's file.
        assert_ne!(shortcut_tag("a"), shortcut_tag("b"));
        assert_eq!(shortcut_tag("a").len(), 16);
    }

    #[test]
    fn fr7_the_desktop_folder_comes_from_user_dirs() {
        let home = Path::new("/home/me");
        let dirs = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/바탕화면\"\n";
        assert_eq!(desktop_dir_in(Some(dirs), home), home.join("바탕화면"));
        assert_eq!(
            desktop_dir_in(Some("XDG_DESKTOP_DIR=\"/srv/desk\""), home),
            PathBuf::from("/srv/desk")
        );
        assert_eq!(desktop_dir_in(None, home), home.join("Desktop"));
        assert_eq!(
            desktop_dir_in(Some("XDG_DOWNLOAD_DIR=\"$HOME/Downloads\""), home),
            home.join("Desktop")
        );
        assert_eq!(
            desktop_dir_in(Some("XDG_DESKTOP_DIR=\"$HOME\""), home),
            home.join("Desktop"),
            "a desktop that is the home falls back"
        );
        assert_eq!(
            desktop_dir_in(Some("XDG_DESKTOP_DIR=\"relative\""), home),
            home.join("Desktop")
        );
        assert_eq!(
            desktop_dir_in(Some("#XDG_DESKTOP_DIR=\"/nope\""), home),
            home.join("Desktop")
        );
    }

    #[test]
    fn fr7_app_shortcuts_are_jump_list_tasks_and_desktop_shortcuts() {
        let (root, folders) = temp_folders("fr7-desktop");
        let program = Path::new("/usr/lib/dsa/dsa-compositor");
        let mut chat = app("com.example.chat", "Chat");
        chat.icon_png = vec![0x89, b'P', b'N', b'G'];
        chat.shortcuts = vec![shortcut("new-chat", "New chat", 0)];
        let notes = app("com.example.notes", "Notes");
        sync(&folders, &[chat.clone(), notes.clone()], program).unwrap();
        let entry_text =
            fs::read_to_string(folders.applications.join(file_name("com.example.chat"))).unwrap();
        assert!(entry_text.contains("--launch-shortcut com.example.chat new-chat"));
        let shortcut_icon = shortcut_icon_path(&folders.icons, "com.example.chat", "new-chat");
        assert!(shortcut_icon.is_file());

        // Create desktop shortcut: a copy of the app's entry on the desktop.
        let desktop = folders.desktop.clone().unwrap();
        let placed = place_on_desktop_in(&folders, &chat).unwrap();
        assert_eq!(placed, desktop.join("dsa-com.example.chat.desktop"));
        assert_eq!(fs::read_to_string(&placed).unwrap(), entry_text);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&placed).unwrap().permissions().mode() & 0o111,
                0o111
            );
        }
        // An app the compositor has not made an entry for yet is refused with the reason.
        assert!(place_on_desktop_in(&folders, &app("com.example.other", "Other")).is_err());

        // A pinned shortcut the user agreed to: its own entry, which starts the shortcut.
        let ana = shortcut("ana", "Ana", 1);
        let pinned = place_pinned_in(&folders, "com.example.chat", &ana, program).unwrap();
        let pinned_text = fs::read_to_string(&pinned).unwrap();
        assert!(pinned_text.contains("\nName=Ana\n"));
        assert!(pinned_text.contains("--launch-shortcut com.example.chat ana"));
        assert!(pinned_text.contains("\nX-DSA-Shortcut=ana\n"));
        assert!(is_pinned_entry(&pinned_text));

        // The app's shortcuts change: the entry and its desktop copy follow, and the pinned
        // shortcut keeps its icon though it left the jump list.
        chat.shortcuts = vec![shortcut("archive", "Archive", 0)];
        sync(&folders, &[chat.clone(), notes.clone()], program).unwrap();
        assert!(fs::read_to_string(&placed)
            .unwrap()
            .contains("Name=Archive"));
        assert!(
            !shortcut_icon.exists(),
            "a shortcut that is gone takes its icon"
        );
        assert!(shortcut_icon_path(&folders.icons, "com.example.chat", "ana").is_file());

        // Someone else's files on the desktop are never touched.
        let theirs = desktop.join("dsa-mine.desktop");
        fs::write(&theirs, "[Desktop Entry]\nName=Mine\n").unwrap();
        let other = desktop.join("notes.txt");
        fs::write(&other, "keep").unwrap();

        // Uninstalled: the entry, the desktop shortcut, the pinned shortcut and the icons go.
        sync(&folders, &[notes], program).unwrap();
        assert!(!placed.exists());
        assert!(!pinned.exists());
        assert!(!folders
            .applications
            .join(file_name("com.example.chat"))
            .exists());
        assert!(!shortcut_icon_path(&folders.icons, "com.example.chat", "ana").exists());
        assert!(!folders.icons.join("com.example.chat.png").exists());
        assert!(theirs.exists() && other.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fr7_a_desktop_file_of_another_program_is_not_replaced() {
        let (root, folders) = temp_folders("fr7-desktop-theirs");
        let chat = app("com.example.chat", "Chat");
        sync(&folders, std::slice::from_ref(&chat), Path::new("/x")).unwrap();
        let desktop = folders.desktop.clone().unwrap();
        fs::create_dir_all(&desktop).unwrap();
        let theirs = desktop.join(file_name("com.example.chat"));
        fs::write(&theirs, "[Desktop Entry]\nName=Theirs\n").unwrap();
        assert!(place_on_desktop_in(&folders, &chat).is_err());
        assert_eq!(
            fs::read_to_string(&theirs).unwrap(),
            "[Desktop Entry]\nName=Theirs\n"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// A PNG header for an image `width` pixels wide, enough for [`theme_size`].
    fn png_of_width(width: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&width.to_be_bytes());
        png
    }

    #[test]
    fn fr35_each_apps_icon_is_also_in_the_icon_theme_by_its_name() {
        assert_eq!(icon_name("com.example.d"), "dsa-com.example.d");
        assert_eq!(theme_size(&png_of_width(192)), 192);
        assert_eq!(theme_size(&png_of_width(200)), 192);
        assert_eq!(theme_size(&png_of_width(1000)), 512);
        assert_eq!(theme_size(b"not a png"), 256);

        let root = std::env::temp_dir().join(format!("dsa-theme-icons-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folders = Folders::of_user(Some(root.clone()), None).unwrap();
        let mut notes = app("com.example.d", "Notes");
        notes.icon_png = png_of_width(96);
        sync(&folders, &[notes], Path::new("/x")).unwrap();
        let themed = theme_icon_path(&folders.theme_icons, "com.example.d", 96);
        assert_eq!(
            themed,
            root.join("icons/hicolor/96x96/apps/dsa-com.example.d.png")
        );
        assert!(themed.is_file());
        // Gone with the app.
        sync(&folders, &[], Path::new("/x")).unwrap();
        assert!(!themed.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
