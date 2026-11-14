//! Android apps in Launchpad, Spotlight, the Dock and on the Desktop.
//!
//! Launchpad and Spotlight list application bundles, and the Dock shows one icon per running
//! application, so every Android app gets a bundle of its own in `~/Applications/DSA Apps`:
//! its name, its icon (as `.icns`), and as its executable a copy of the window host
//! (`dsa-app-window`). Opened from Launchpad, Spotlight or the Dock, the copy asks DSA to
//! start the app; started by the window manager, it shows the app's windows. Either way the
//! app's windows belong to a process of the app's own bundle, so the Dock shows the app as
//! itself, keeps it on request, and Command-Tab switches to it.
//!
//! The alternatives fall short: one DSA process owning every window would show every
//! Android app under a single Dock icon, and a launcher window of DSA's own would leave the
//! apps out of Launchpad and Spotlight. A bundle per app costs one copy of a small program.
//!
//! A bundle also lists the app's launcher shortcuts (a messenger's "New chat") under
//! [`SHORTCUTS_KEY`], which the running window host shows as its Dock menu. A shortcut the
//! app asked to pin and the user agreed to gets a small bundle of its own in the
//! [`SHORTCUTS_FOLDER`], marked with [`SHORTCUT_KEY`], which starts that shortcut. The
//! Desktop holds links to these bundles: "Create desktop shortcut" makes one for an app, and
//! a pin makes one for its shortcut bundle. A note in the folder ([`DESKTOP_NOTE`]) records
//! each link, so a link follows its bundle when the app is renamed and goes when the app is
//! uninstalled.
//!
//! A bundle is marked as DSA's by its identifier and by the `DSAPackage` key in its
//! `Info.plist`, so [`sync`] never touches an application it did not make, and a Desktop item
//! is only ever removed when it is a link the note records into DSA's folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dsa_host_core::tasks::{is_valid_shortcut_id, AppShortcut};

use super::icns;

/// What every stub's bundle identifier starts with.
pub const BUNDLE_PREFIX: &str = "org.thisisthepy.dsa.app.";

/// The folder, under the user's `~/Applications`, the stubs go in.
pub const FOLDER: &str = "DSA Apps";

/// The folder, in [`FOLDER`], the bundles of pinned shortcuts go in.
pub const SHORTCUTS_FOLDER: &str = "Shortcuts";

/// The `Info.plist` key that names the Android package a stub launches.
pub const PACKAGE_KEY: &str = "DSAPackage";

/// The `Info.plist` key that lists an app's launcher shortcuts, for its Dock menu.
pub const SHORTCUTS_KEY: &str = "DSAShortcuts";

/// The `Info.plist` key of a pinned shortcut's bundle that names the shortcut it starts.
pub const SHORTCUT_KEY: &str = "DSAShortcut";

/// The note, in [`FOLDER`], of the links DSA put on the Desktop.
pub const DESKTOP_NOTE: &str = ".dsa-desktop";

/// The stub's executable, in `Contents/MacOS`: the window host.
pub const EXECUTABLE: &str = "dsa-app-window";

/// One of an app's launcher shortcuts as its Dock menu shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubShortcut {
    pub id: String,
    pub label: String,
}

/// One Android app as the Mac shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub package: String,
    pub label: String,
    pub version: String,
    /// The app's launcher icon, a PNG; empty for none (the bundle then has the generic icon).
    pub icon_png: Vec<u8>,
    /// The shortcuts its Dock menu shows, in order.
    pub shortcuts: Vec<StubShortcut>,
}

/// The default folder of the stubs: `~/Applications/DSA Apps`.
pub fn default_folder(home: &Path) -> PathBuf {
    home.join("Applications").join(FOLDER)
}

/// The user's Desktop: `~/Desktop`.
pub fn default_desktop(home: &Path) -> PathBuf {
    home.join("Desktop")
}

/// The bundle identifier of `package`'s stub: the package with every character a bundle
/// identifier does not take (letters, digits, `-` and `.`) replaced by `-`.
pub fn bundle_identifier(package: &str) -> String {
    let clean: String = package
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{BUNDLE_PREFIX}{clean}")
}

/// A short name for a shortcut's ID that a file name and a bundle identifier can carry: an
/// FNV-1a hash of it, in hex. Android takes any text as an ID.
pub fn shortcut_tag(id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The stub's file name: the app's label, with what a file name cannot hold replaced, and
/// the package after it when two apps share a label.
pub fn bundle_name(label: &str, package: &str, label_taken: bool) -> String {
    let clean: String = label
        .chars()
        .map(|c| match c {
            '/' | ':' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let clean = clean.trim().trim_start_matches('.').trim();
    let base = if clean.is_empty() { package } else { clean };
    if label_taken {
        format!("{base} ({package}).app")
    } else {
        format!("{base}.app")
    }
}

fn xml_escape(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t' | '\n'))
        .fold(String::new(), |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&apos;"),
                c => out.push(c),
            }
            out
        })
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The `<dict>` entries of the shortcuts, for an `Info.plist`.
fn shortcuts_plist(shortcuts: &[StubShortcut]) -> String {
    if shortcuts.is_empty() {
        return String::new();
    }
    let mut text = format!("\t<key>{SHORTCUTS_KEY}</key>\n\t<array>\n");
    for shortcut in shortcuts {
        text.push_str(&format!(
            "\t\t<dict>\n\t\t\t<key>id</key>\n\t\t\t<string>{}</string>\n\t\t\t<key>label</key>\n\t\t\t<string>{}</string>\n\t\t</dict>\n",
            xml_escape(&shortcut.id),
            xml_escape(&shortcut.label)
        ));
    }
    text.push_str("\t</array>\n");
    text
}

/// The stub's `Info.plist`.
pub fn info_plist(app: &AppEntry, has_icon: bool) -> String {
    let icon = if has_icon {
        "\t<key>CFBundleIconFile</key>\n\t<string>icon</string>\n"
    } else {
        ""
    };
    let version = if app.version.is_empty() {
        "1.0"
    } else {
        app.version.as_str()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>{EXECUTABLE}</string>
{icon}	<key>CFBundleIdentifier</key>
	<string>{identifier}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{label}</string>
	<key>CFBundleDisplayName</key>
	<string>{label}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.utilities</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>{PACKAGE_KEY}</key>
	<string>{package}</string>
{shortcuts}</dict>
</plist>
"#,
        identifier = xml_escape(&bundle_identifier(&app.package)),
        label = xml_escape(&app.label),
        version = xml_escape(version),
        package = xml_escape(&app.package),
        shortcuts = shortcuts_plist(&app.shortcuts),
    )
}

/// The `Info.plist` of the bundle of `package`'s pinned shortcut `shortcut`. It starts the
/// shortcut and ends; it shows nothing of its own, so it stays out of the Dock.
pub fn pinned_plist(package: &str, shortcut: &AppShortcut, has_icon: bool) -> String {
    let icon = if has_icon {
        "\t<key>CFBundleIconFile</key>\n\t<string>icon</string>\n"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>{EXECUTABLE}</string>
{icon}	<key>CFBundleIdentifier</key>
	<string>{identifier}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{label}</string>
	<key>CFBundleDisplayName</key>
	<string>{label}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.0</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>{PACKAGE_KEY}</key>
	<string>{package}</string>
	<key>{SHORTCUT_KEY}</key>
	<string>{id}</string>
</dict>
</plist>
"#,
        identifier = xml_escape(&format!(
            "{}.shortcut-{}",
            bundle_identifier(package),
            shortcut_tag(&shortcut.id)
        )),
        label = xml_escape(shortcut.label()),
        package = xml_escape(package),
        id = xml_escape(&shortcut.id),
    )
}

/// The `<string>` that follows `<key>name</key>` in `plist`.
fn plist_string(plist: &str, name: &str) -> Option<String> {
    let key = format!("<key>{name}</key>");
    let rest = &plist[plist.find(&key)? + key.len()..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")? + start;
    Some(xml_unescape(&rest[start..end]))
}

/// The shortcuts a stub's `Info.plist` (its text) lists for the Dock menu, in order.
pub fn shortcuts_in(plist: &str) -> Vec<StubShortcut> {
    let key = format!("<key>{SHORTCUTS_KEY}</key>");
    let Some(at) = plist.find(&key) else {
        return Vec::new();
    };
    let rest = &plist[at + key.len()..];
    let Some(end) = rest.find("</array>") else {
        return Vec::new();
    };
    rest[..end]
        .split("<dict>")
        .skip(1)
        .filter_map(|item| {
            let item = item.split("</dict>").next()?;
            Some(StubShortcut {
                id: plist_string(item, "id")?,
                label: plist_string(item, "label")?,
            })
        })
        .collect()
}

/// The package a bundle at `bundle` launches, if it is one of DSA's stubs.
pub fn stub_package(bundle: &Path) -> Option<String> {
    let plist = fs::read_to_string(bundle.join("Contents").join("Info.plist")).ok()?;
    if !plist.contains(BUNDLE_PREFIX) {
        return None;
    }
    plist_string(&plist, PACKAGE_KEY)
}

/// The shortcut a bundle at `bundle` starts, if it is one of DSA's pinned shortcut bundles.
pub fn stub_shortcut(bundle: &Path) -> Option<String> {
    let plist = fs::read_to_string(bundle.join("Contents").join("Info.plist")).ok()?;
    if !plist.contains(BUNDLE_PREFIX) {
        return None;
    }
    plist_string(&plist, SHORTCUT_KEY)
}

/// The app bundle in `folder` that launches `package`, if there is one.
pub fn app_bundle(folder: &Path, package: &str) -> Option<PathBuf> {
    fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| stub_package(path).as_deref() == Some(package))
}

/// Writes `content` to `path` unless it already holds it.
fn write_if_changed(path: &Path, content: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    fs::write(path, content)
}

/// Puts a copy of `window_host` in the bundle at `macos`, unless it holds the same already.
fn place_executable(macos: &Path, window_host: &Path) -> io::Result<()> {
    let executable = macos.join(EXECUTABLE);
    let same = match (fs::metadata(&executable), fs::metadata(window_host)) {
        (Ok(placed), Ok(installed)) => {
            placed.len() == installed.len()
                && fs::read(&executable).ok() == fs::read(window_host).ok()
        }
        _ => false,
    };
    if !same {
        // A copy, not a link: Launch Services takes only a program inside the bundle as the
        // bundle's own.
        let _ = fs::remove_file(&executable);
        fs::copy(window_host, &executable)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Writes `app`'s stub as `folder/name`, replacing an older one. Its executable is a copy of
/// `window_host`, the installed window host. Only what changed is written, so a running
/// window host reads its new shortcuts without its bundle being rewritten under it.
pub fn write_stub(
    folder: &Path,
    name: &str,
    app: &AppEntry,
    window_host: &Path,
) -> io::Result<PathBuf> {
    let bundle = folder.join(name);
    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    let icon = if app.icon_png.is_empty() {
        None
    } else {
        icns::from_png(&app.icon_png).ok()
    };
    match &icon {
        Some(icon) => write_if_changed(&resources.join("icon.icns"), icon)?,
        None => {
            let _ = fs::remove_file(resources.join("icon.icns"));
        }
    }
    write_if_changed(
        &contents.join("Info.plist"),
        info_plist(app, icon.is_some()).as_bytes(),
    )?;
    write_if_changed(&contents.join("PkgInfo"), b"APPL????")?;
    place_executable(&macos, window_host)?;
    Ok(bundle)
}

/// The bundle of `package`'s pinned shortcut `id`, in `folder`'s [`SHORTCUTS_FOLDER`].
pub fn pinned_bundle_path(folder: &Path, package: &str, id: &str) -> PathBuf {
    folder
        .join(SHORTCUTS_FOLDER)
        .join(format!("{package}.shortcut-{}.app", shortcut_tag(id)))
}

/// Writes the bundle of `package`'s pinned shortcut `shortcut` in `folder`: the shortcut's
/// name, its icon (else `app_icon`, the app's `.icns`), and a copy of `window_host`.
pub fn write_pinned_stub(
    folder: &Path,
    package: &str,
    shortcut: &AppShortcut,
    app_icon: Option<&[u8]>,
    window_host: &Path,
) -> io::Result<PathBuf> {
    let bundle = pinned_bundle_path(folder, package, &shortcut.id);
    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(&resources)?;
    let icon = if shortcut.icon_png.is_empty() {
        app_icon.map(<[u8]>::to_vec)
    } else {
        icns::from_png(&shortcut.icon_png).ok()
    };
    match &icon {
        Some(icon) => write_if_changed(&resources.join("icon.icns"), icon)?,
        None => {
            let _ = fs::remove_file(resources.join("icon.icns"));
        }
    }
    write_if_changed(
        &contents.join("Info.plist"),
        pinned_plist(package, shortcut, icon.is_some()).as_bytes(),
    )?;
    write_if_changed(&contents.join("PkgInfo"), b"APPL????")?;
    place_executable(&macos, window_host)?;
    Ok(bundle)
}

/// What [`sync`] changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub written: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

/// Makes `folder` hold exactly one stub for each of `apps`: writes them all (an app's label,
/// icon or shortcuts may have changed) and removes DSA's stubs of apps no longer installed,
/// their pinned shortcuts' bundles included. Bundles that are not DSA's are left alone.
pub fn sync(folder: &Path, apps: &[AppEntry], window_host: &Path) -> io::Result<SyncReport> {
    fs::create_dir_all(folder)?;
    let mut report = SyncReport::default();
    let mut names = Vec::new();
    for app in apps {
        let lower = app.label.to_lowercase();
        let taken = apps
            .iter()
            .filter(|other| other.label.to_lowercase() == lower)
            .count()
            > 1;
        let name = bundle_name(&app.label, &app.package, taken);
        report
            .written
            .push(write_stub(folder, &name, app, window_host)?);
        names.push(name);
    }
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if names.contains(&name) || stub_package(&path).is_none() {
            continue;
        }
        fs::remove_dir_all(&path)?;
        report.removed.push(path);
    }
    let pinned = folder.join(SHORTCUTS_FOLDER);
    if let Ok(entries) = fs::read_dir(&pinned) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(package) = stub_package(&path) else {
                continue;
            };
            if stub_shortcut(&path).is_some() && !apps.iter().any(|app| app.package == package) {
                fs::remove_dir_all(&path)?;
                report.removed.push(path);
            }
        }
    }
    Ok(report)
}

/// One link DSA put on the Desktop, as the note records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLink {
    pub package: String,
    /// The pinned shortcut the link starts; `None` for a link to the app.
    pub shortcut: Option<String>,
    pub link: PathBuf,
}

/// The links the note in `folder` records.
pub fn read_note(folder: &Path) -> Vec<DesktopLink> {
    let Ok(text) = fs::read_to_string(folder.join(DESKTOP_NOTE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let package = parts.next()?.to_string();
            let shortcut = parts.next()?;
            let link = PathBuf::from(parts.next()?);
            Some(DesktopLink {
                package,
                shortcut: (!shortcut.is_empty()).then(|| shortcut.to_string()),
                link,
            })
        })
        .collect()
}

fn write_note(folder: &Path, links: &[DesktopLink]) -> io::Result<()> {
    let text: String = links
        .iter()
        .map(|link| {
            format!(
                "{}\t{}\t{}\n",
                link.package,
                link.shortcut.as_deref().unwrap_or(""),
                link.link.display()
            )
        })
        .collect();
    fs::create_dir_all(folder)?;
    write_if_changed(&folder.join(DESKTOP_NOTE), text.as_bytes())
}

/// Whether `path` is a link into `folder`, which is all DSA ever removes from the Desktop.
fn is_our_link(path: &Path, folder: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
        && fs::read_link(path).is_ok_and(|target| target.starts_with(folder))
}

#[cfg(unix)]
fn make_link(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn make_link(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "links to app bundles are made on macOS only",
    ))
}

/// Puts a link to `target` on `desktop` named `name`, or another free name, and records it.
fn link_on_desktop(
    folder: &Path,
    desktop: &Path,
    name: &str,
    target: &Path,
    package: &str,
    shortcut: Option<&str>,
) -> Result<PathBuf, String> {
    let mut links = read_note(folder);
    // The same item is not linked twice; a link of it that is still there is the answer.
    if let Some(existing) = links
        .iter()
        .find(|link| link.package == package && link.shortcut.as_deref() == shortcut)
    {
        if is_our_link(&existing.link, folder) {
            return Ok(existing.link.clone());
        }
    }
    links.retain(|link| !(link.package == package && link.shortcut.as_deref() == shortcut));
    fs::create_dir_all(desktop).map_err(|e| format!("{}: {e}", desktop.display()))?;
    let stem = name.strip_suffix(".app").unwrap_or(name);
    let path = (1..100)
        .map(|n| {
            if n == 1 {
                desktop.join(format!("{stem}.app"))
            } else {
                desktop.join(format!("{stem} {n}.app"))
            }
        })
        .find(|path| fs::symlink_metadata(path).is_err())
        .ok_or_else(|| format!("the Desktop has no free name for {stem}"))?;
    make_link(target, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    links.push(DesktopLink {
        package: package.to_string(),
        shortcut: shortcut.map(str::to_string),
        link: path.clone(),
    });
    write_note(folder, &links).map_err(|e| format!("{}: {e}", folder.display()))?;
    Ok(path)
}

/// Puts a link to `package`'s bundle in `folder` on `desktop`: "Create desktop shortcut". It
/// opens the app as Launchpad does.
pub fn place_on_desktop_in(
    folder: &Path,
    desktop: &Path,
    package: &str,
) -> Result<PathBuf, String> {
    let bundle = app_bundle(folder, package).ok_or_else(|| {
        format!(
            "{package} has no app bundle in {} yet; the DSA window manager makes it once it runs",
            folder.display()
        )
    })?;
    let name = bundle
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("{package}.app"));
    link_on_desktop(folder, desktop, &name, &bundle, package, None)
}

/// [`place_on_desktop_in`] in the user's own folders.
pub fn place_on_desktop(package: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("the user's folders are not known: HOME is not set")?;
    place_on_desktop_in(&default_folder(&home), &default_desktop(&home), package)
}

/// Makes the bundle of `package`'s pinned shortcut `shortcut` and puts a link to it on
/// `desktop`, under the shortcut's name.
pub fn place_pinned_in(
    folder: &Path,
    desktop: &Path,
    package: &str,
    shortcut: &AppShortcut,
    window_host: &Path,
) -> Result<PathBuf, String> {
    if !is_valid_shortcut_id(&shortcut.id) {
        return Err(format!(
            "{:?} is not a shortcut ID a bundle can carry",
            shortcut.id
        ));
    }
    let app_icon = app_bundle(folder, package)
        .and_then(|bundle| fs::read(bundle.join("Contents/Resources/icon.icns")).ok());
    let bundle = write_pinned_stub(folder, package, shortcut, app_icon.as_deref(), window_host)
        .map_err(|e| format!("making the shortcut's bundle: {e}"))?;
    let name = bundle_name(shortcut.label(), package, false);
    link_on_desktop(folder, desktop, &name, &bundle, package, Some(&shortcut.id))
}

/// Brings the Desktop links of the note in `folder` in step with the bundles: a link whose
/// app is gone is removed, one whose bundle moved (the app was renamed) points at it again,
/// and one the user deleted is forgotten. Returns the links removed. Nothing that is not a
/// link the note records into `folder` is ever touched.
pub fn sync_desktop(folder: &Path, packages: &[String]) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for link in read_note(folder) {
        if !is_our_link(&link.link, folder) {
            continue;
        }
        let target = if packages.contains(&link.package) {
            match &link.shortcut {
                Some(id) => {
                    let bundle = pinned_bundle_path(folder, &link.package, id);
                    bundle.exists().then_some(bundle)
                }
                None => app_bundle(folder, &link.package),
            }
        } else {
            None
        };
        match target {
            Some(target) => {
                if fs::read_link(&link.link).ok().as_ref() != Some(&target) {
                    let _ = fs::remove_file(&link.link);
                    if make_link(&target, &link.link).is_err() {
                        continue;
                    }
                }
                kept.push(link);
            }
            None => {
                if fs::remove_file(&link.link).is_ok() {
                    removed.push(link.link.clone());
                }
            }
        }
    }
    let _ = write_note(folder, &kept);
    removed
}

/// Tells Launch Services about the stubs `report` wrote and removed, so Launchpad and
/// Spotlight show them at once rather than at their next scan.
#[cfg(target_os = "macos")]
pub fn register(report: &SyncReport) -> Result<(), String> {
    const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/\
                              LaunchServices.framework/Support/lsregister";
    let mut failed = Vec::new();
    for (bundle, flag) in report
        .written
        .iter()
        .map(|b| (b, "-f"))
        .chain(report.removed.iter().map(|b| (b, "-u")))
    {
        let status = std::process::Command::new(LSREGISTER)
            .arg(flag)
            .arg(bundle)
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            failed.push(bundle.display().to_string());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Launch Services did not take these app bundles: {}",
            failed.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(package: &str, label: &str) -> AppEntry {
        AppEntry {
            package: package.into(),
            label: label.into(),
            version: "2.1".into(),
            icon_png: Vec::new(),
            shortcuts: Vec::new(),
        }
    }

    #[test]
    fn fr7_each_app_has_its_own_bundle_identifier() {
        assert_eq!(
            bundle_identifier("com.example.notes"),
            "org.thisisthepy.dsa.app.com.example.notes"
        );
        assert_eq!(
            bundle_identifier("com.example_app.x"),
            "org.thisisthepy.dsa.app.com.example-app.x"
        );
    }

    #[test]
    fn fr7_a_bundle_is_named_after_the_app_and_never_escapes_its_folder() {
        assert_eq!(bundle_name("Notes", "com.n", false), "Notes.app");
        assert_eq!(bundle_name("a/b:c", "com.n", false), "a-b-c.app");
        assert_eq!(bundle_name("../x", "com.n", false), "-x.app");
        assert_eq!(bundle_name("  ", "com.n", false), "com.n.app");
        assert_eq!(bundle_name("Notes", "com.n", true), "Notes (com.n).app");
    }

    #[test]
    fn fr7_the_info_plist_names_the_app_and_escapes_its_text() {
        let plist = info_plist(&app("com.example", "Tom & Jerry <3>"), true);
        assert!(plist.contains("<string>Tom &amp; Jerry &lt;3&gt;</string>"));
        assert!(plist.contains("<string>org.thisisthepy.dsa.app.com.example</string>"));
        assert!(plist.contains("<key>CFBundleIconFile</key>"));
        assert!(plist.contains("<key>DSAPackage</key>\n\t<string>com.example</string>"));
        assert!(!info_plist(&app("p", "l"), false).contains("CFBundleIconFile"));
        assert!(!plist.contains(SHORTCUTS_KEY), "no shortcuts, no list");
    }

    #[test]
    fn fr7_the_info_plist_lists_the_dock_menus_shortcuts() {
        let mut chat = app("com.example.chat", "Chat");
        chat.shortcuts = vec![
            StubShortcut {
                id: "new-chat".into(),
                label: "New chat".into(),
            },
            StubShortcut {
                id: "a&b<c>".into(),
                label: "Tom & \"Jerry\"".into(),
            },
        ];
        let plist = info_plist(&chat, false);
        assert!(plist.contains("<key>DSAShortcuts</key>"));
        assert_eq!(shortcuts_in(&plist), chat.shortcuts);
        assert_eq!(
            plist_string(&plist, PACKAGE_KEY).as_deref(),
            Some("com.example.chat")
        );
        assert!(shortcuts_in(&info_plist(&app("p", "l"), false)).is_empty());
    }

    #[test]
    fn fr7_a_pinned_shortcuts_bundle_names_its_app_and_shortcut() {
        let shortcut = AppShortcut {
            id: "ana".into(),
            short_label: "Ana".into(),
            ..AppShortcut::default()
        };
        let plist = pinned_plist("com.example.chat", &shortcut, true);
        assert_eq!(
            plist_string(&plist, PACKAGE_KEY).as_deref(),
            Some("com.example.chat")
        );
        assert_eq!(plist_string(&plist, SHORTCUT_KEY).as_deref(), Some("ana"));
        assert!(plist.contains("<key>LSUIElement</key>"));
        assert!(plist.contains(&format!(
            "org.thisisthepy.dsa.app.com.example.chat.shortcut-{}",
            shortcut_tag("ana")
        )));
        assert_ne!(shortcut_tag("ana"), shortcut_tag("bob"));
    }

    #[test]
    fn fr23_the_bundles_follow_the_installed_apps_and_leave_others_alone() {
        let root = std::env::temp_dir().join(format!("dsa-app-stubs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join(FOLDER);
        let window_host = root.join("dsa-app-window");
        fs::create_dir_all(&root).unwrap();
        fs::write(&window_host, b"window host").unwrap();
        let foreign = folder.join("Other.app/Contents");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("Info.plist"), "<plist/>").unwrap();

        let mut notes = app("com.example.notes", "Notes");
        notes.icon_png = crate::shell::icns::tests::png_header(128);
        let report = sync(
            &folder,
            &[notes, app("com.example.mail", "Mail")],
            &window_host,
        )
        .unwrap();
        assert_eq!(report.written.len(), 2);
        let executable = folder.join("Notes.app/Contents/MacOS").join(EXECUTABLE);
        assert_eq!(fs::read(&executable).unwrap(), b"window host");
        assert!(folder
            .join("Notes.app/Contents/Resources/icon.icns")
            .exists());
        assert_eq!(
            stub_package(&folder.join("Mail.app")),
            Some("com.example.mail".into())
        );

        // Mail is uninstalled: its bundle goes, and the bundle DSA did not make stays.
        let report = sync(&folder, &[app("com.example.notes", "Notes")], &window_host).unwrap();
        assert_eq!(report.removed, vec![folder.join("Mail.app")]);
        assert!(!folder.join("Mail.app").exists());
        assert!(folder.join("Other.app").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn fr7_app_shortcuts_are_jump_list_tasks_and_desktop_shortcuts() {
        let root = std::env::temp_dir().join(format!("dsa-app-desktop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join(FOLDER);
        let desktop = root.join("Desktop");
        let window_host = root.join("dsa-app-window");
        fs::create_dir_all(&root).unwrap();
        fs::write(&window_host, b"window host").unwrap();
        let mut chat = app("com.example.chat", "Chat");
        chat.shortcuts = vec![StubShortcut {
            id: "new-chat".into(),
            label: "New chat".into(),
        }];
        sync(&folder, std::slice::from_ref(&chat), &window_host).unwrap();
        let plist = fs::read_to_string(folder.join("Chat.app/Contents/Info.plist")).unwrap();
        assert_eq!(
            shortcuts_in(&plist),
            chat.shortcuts,
            "the Dock menu's shortcuts"
        );

        // Create desktop shortcut: a link to the app's bundle, made once.
        let link = place_on_desktop_in(&folder, &desktop, "com.example.chat").unwrap();
        assert_eq!(link, desktop.join("Chat.app"));
        assert_eq!(fs::read_link(&link).unwrap(), folder.join("Chat.app"));
        assert_eq!(
            place_on_desktop_in(&folder, &desktop, "com.example.chat").unwrap(),
            link
        );
        assert!(place_on_desktop_in(&folder, &desktop, "com.example.none").is_err());

        // A pinned shortcut: a bundle of its own that starts it, linked on the Desktop.
        let ana = AppShortcut {
            id: "ana".into(),
            short_label: "Ana".into(),
            ..AppShortcut::default()
        };
        let pinned =
            place_pinned_in(&folder, &desktop, "com.example.chat", &ana, &window_host).unwrap();
        assert_eq!(pinned, desktop.join("Ana.app"));
        let bundle = pinned_bundle_path(&folder, "com.example.chat", "ana");
        assert_eq!(fs::read_link(&pinned).unwrap(), bundle);
        assert_eq!(stub_shortcut(&bundle).as_deref(), Some("ana"));
        assert_eq!(stub_package(&bundle).as_deref(), Some("com.example.chat"));

        // The app is renamed: the link follows its bundle.
        let renamed = AppEntry {
            label: "Messages".into(),
            ..chat.clone()
        };
        sync(&folder, std::slice::from_ref(&renamed), &window_host).unwrap();
        let packages = vec!["com.example.chat".to_string()];
        assert!(sync_desktop(&folder, &packages).is_empty());
        assert_eq!(fs::read_link(&link).unwrap(), folder.join("Messages.app"));

        // The user's own Desktop items, and a link of theirs, are never touched.
        let theirs = desktop.join("notes.txt");
        fs::write(&theirs, "keep").unwrap();
        let their_link = desktop.join("Other.app");
        std::os::unix::fs::symlink(folder.join("Messages.app"), &their_link).unwrap();

        // Uninstalled: the bundles and both links go.
        sync(&folder, &[], &window_host).unwrap();
        let removed = sync_desktop(&folder, &[]);
        assert_eq!(removed.len(), 2);
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(fs::symlink_metadata(&pinned).is_err());
        assert!(!bundle.exists());
        assert!(theirs.exists());
        assert!(fs::symlink_metadata(&their_link).is_ok());
        let _ = fs::remove_dir_all(&root);
    }
}
