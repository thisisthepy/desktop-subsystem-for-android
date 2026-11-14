//! Shortcuts to Android apps, and to their app shortcuts, on the user's desktop.
//!
//! An app's icon is handled the way Windows handles any program's, not the way Android's home
//! screen does: the user may put a shortcut to the app on the desktop (from the settings app's
//! page of the app, or from the jump list of the app's taskbar button), and an app's request to
//! pin one of its shortcuts (`requestPinShortcut`) becomes a desktop shortcut once the user
//! agrees.
//!
//! A shortcut to the app is the app's Start menu shortcut ([`MenuEntry::shortcut`]) under the
//! app's name: the app's window program with `--package`, its icon and its AppUserModelID, so
//! it opens the app exactly as the Start menu does and its windows group on the app's taskbar
//! button. A shortcut to one of the app's shortcuts runs the same program with `--shortcut
//! <id>` too, under the shortcut's name and with the shortcut's own icon.
//!
//! The desktop is the user's own, so a file that is already there is never replaced: a name
//! that is taken gets the app's name after it, then a number. Every shortcut made is recorded
//! in a note (`desktop-shortcuts.json` in `%LOCALAPPDATA%\DSA`), and only those are ever
//! changed or removed: when the app is uninstalled, and when DSA is.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dsa_host_core::tasks::{is_valid_shortcut_id, AppShortcut};
use serde::{Deserialize, Serialize};

use crate::start_menu::{MenuEntry, Shortcut, Shortcuts};

/// The argument that names the shortcut of the app the window program starts.
pub const SHORTCUT_ARGUMENT: &str = "--shortcut";

/// The argument that makes the window program put a shortcut to its app on the desktop and
/// exit, which the app's jump list runs.
pub const CREATE_DESKTOP_SHORTCUT_ARGUMENT: &str = "--create-desktop-shortcut";

/// The shortcut `args` ask the window program to start (`--shortcut <id>`), if they name one
/// a command line can carry.
pub fn shortcut_of(args: &[String]) -> Option<String> {
    crate::app_window::argument(args, SHORTCUT_ARGUMENT).filter(|id| is_valid_shortcut_id(id))
}

/// Whether `args` ask the window program to put a shortcut to its app on the desktop.
pub fn creates_desktop_shortcut(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == CREATE_DESKTOP_SHORTCUT_ARGUMENT)
}

/// Puts a shortcut to the app whose window program is `program` (this one, named after the
/// app) on the user's desktop, as the app's jump list asks; returns its path.
pub fn place_program_on_desktop(package: &str, program: &Path) -> Result<PathBuf, String> {
    let label = program
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| package.to_string());
    let entry = MenuEntry {
        package: package.to_string(),
        label,
        program: program.to_path_buf(),
    };
    let desktop = Desktop::default_for_user().ok_or("the desktop folder cannot be found")?;
    let _com = crate::start_menu::ComApartment::enter();
    desktop.place_app(&entry, &crate::start_menu::ShellLinks)
}

/// The longest name a desktop shortcut gets, in characters, before `.lnk`.
const MAX_NAME: usize = 64;

/// How many numbered names are tried before a shortcut is given up on.
const MAX_TRIES: usize = 20;

/// The user's desktop folder, wherever they moved it.
pub fn default_desktop() -> Option<PathBuf> {
    use windows_sys::Win32::UI::Shell::{FOLDERID_Desktop, KF_FLAG_DEFAULT};
    crate::app_window::screenshot::known_folder(&FOLDERID_Desktop, KF_FLAG_DEFAULT)
}

/// The note of the desktop shortcuts made: `%LOCALAPPDATA%\DSA\desktop-shortcuts.json`.
pub fn default_note() -> Option<PathBuf> {
    crate::paths::UserRoot::current().map(|root| root.desktop_shortcuts_note())
}

/// What was made for one app: the path of its own shortcut, and of each of its shortcuts by
/// ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Made {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shortcuts: BTreeMap<String, PathBuf>,
}

impl Made {
    fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.app.iter().chain(self.shortcuts.values())
    }
}

/// The note: what was made, by package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    #[serde(default)]
    pub apps: BTreeMap<String, Made>,
}

/// Reads the note at `path`; an empty one when there is none or it does not read.
pub fn read_note(path: &Path) -> Note {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_note(path: &Path, note: &Note) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(note).unwrap_or_default();
    let fresh = path.with_extension("json.new");
    fs::write(&fresh, text)
        .and_then(|()| fs::rename(&fresh, path))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Whether `path` is one a note may name: an absolute path to a `.lnk` file. A damaged note
/// can then never make anything else be removed.
fn is_shortcut_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
        && path.file_stem().is_some_and(|stem| !stem.is_empty())
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A desktop shortcut's name: `label` with what Windows does not allow in a file name taken
/// out, at most [`MAX_NAME`] characters. Empty when nothing usable is left.
pub fn clean_name(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .filter(|c| !c.is_control() && !"<>:\"/\\|?*".contains(*c))
        .collect();
    let mut name: String = cleaned.trim().chars().take(MAX_NAME).collect();
    name.truncate(name.trim_end_matches(['.', ' ']).len());
    name.trim_start().to_string()
}

/// The names a shortcut called `name` may get, in the order they are tried: the name, the
/// name with `qualifier` after it, then numbered.
pub fn candidate_names(name: &str, qualifier: &str) -> Vec<String> {
    let mut names = vec![format!("{name}.lnk")];
    if !qualifier.is_empty() && qualifier != name {
        names.push(format!("{name} ({qualifier}).lnk"));
    }
    names.extend((2..MAX_TRIES).map(|n| format!("{name} ({n}).lnk")));
    names
}

/// The arguments that start `shortcut_id` of `package` through the app's window program.
pub fn shortcut_arguments(package: &str, shortcut_id: &str) -> String {
    format!(
        "{} {package} {SHORTCUT_ARGUMENT} {}",
        crate::app_window::PACKAGE_ARGUMENT,
        quote_argument(shortcut_id)
    )
}

/// One argument as Windows' command line parsing reads it back unchanged
/// (`CommandLineToArgvW`): quoted when it has spaces, quotes or nothing in it.
pub fn quote_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '"']) {
        return argument.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for c in argument.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            c => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The shortcut that starts `shortcut` of the app `entry` opens, with its icon at `icon` (an
/// `.ico` file) when it has one of its own, else the app's.
pub fn shortcut_link(entry: &MenuEntry, shortcut: &AppShortcut, icon: Option<&Path>) -> Shortcut {
    let app = entry.shortcut();
    let description = if shortcut.long_label.trim().is_empty() {
        format!("{} ({})", shortcut.label(), entry.label)
    } else {
        format!("{} ({})", shortcut.long_label.trim(), entry.label)
    };
    Shortcut {
        arguments: shortcut_arguments(&entry.package, &shortcut.id),
        icon: icon.map(|icon| (icon.to_path_buf(), 0)).unwrap_or(app.icon),
        description,
        ..app
    }
}

/// An `.ico` file that holds `png` as its one image, as Windows reads icons since Vista.
/// `None` when `png` is not a PNG image.
pub fn ico_from_png(png: &[u8]) -> Option<Vec<u8>> {
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") || png.len() < 24 {
        return None;
    }
    let side = |at: usize| {
        let value = u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
        // 0 stands for 256 and more.
        if value >= 256 {
            0
        } else {
            value as u8
        }
    };
    let (width, height) = (side(16), side(20));
    let size = u32::try_from(png.len()).ok()?;
    let mut ico = Vec::with_capacity(22 + png.len());
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    ico.extend_from_slice(&[width, height, 0, 0]);
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&size.to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    ico.extend_from_slice(png);
    Some(ico)
}

/// Where the icon of a shortcut whose PNG is `png` is kept, in the app's folder `app_dir`:
/// `shortcuts\<hash of the image>.ico`, so the same image is written once and an image that
/// changes gets a path of its own (Explorer and the taskbar cache icons by path).
pub fn icon_path(app_dir: &Path, png: &[u8]) -> PathBuf {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(png)
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    app_dir.join("shortcuts").join(format!("{hash}.ico"))
}

/// Writes the icon of `png` in `app_dir` unless it is there already; its path, or `None` when
/// the image is not a PNG or could not be written.
pub fn write_icon(app_dir: &Path, png: &[u8]) -> Option<PathBuf> {
    let ico = ico_from_png(png)?;
    let path = icon_path(app_dir, png);
    if fs::read(&path).is_ok_and(|old| old == ico) {
        return Some(path);
    }
    fs::create_dir_all(path.parent()?).ok()?;
    let fresh = path.with_extension("ico.new");
    fs::write(&fresh, &ico).ok()?;
    fs::rename(&fresh, &path).ok()?;
    Some(path)
}

/// The user's desktop, as far as DSA's shortcuts go.
pub struct Desktop {
    /// Where the shortcuts go.
    pub dir: PathBuf,
    /// The note of the shortcuts made there.
    pub note: PathBuf,
}

impl Desktop {
    /// The user's own desktop and note. `None` when either cannot be told.
    pub fn default_for_user() -> Option<Desktop> {
        Some(Desktop {
            dir: default_desktop()?,
            note: default_note()?,
        })
    }

    /// What the note says was made.
    pub fn made(&self) -> Note {
        read_note(&self.note)
    }

    /// Puts `link` on the desktop as one of `package`'s, where `previous` was put before (it is
    /// replaced, being DSA's), else under the first free name of `names`. Returns its path.
    fn place(
        &self,
        names: Vec<String>,
        previous: Option<PathBuf>,
        link: &Shortcut,
        shortcuts: &dyn Shortcuts,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("could not create {}: {e}", self.dir.display()))?;
        let ours = |path: &Path| previous.as_deref().is_some_and(|previous| previous == path);
        let path = names
            .into_iter()
            .map(|name| self.dir.join(name))
            .find(|path| ours(path) || !path.exists())
            .ok_or_else(|| "every name it could take is taken on the desktop".to_string())?;
        if shortcuts.read(&path).ok().as_ref() != Some(link) {
            shortcuts.write(&path, link)?;
        }
        if let Some(previous) = previous.filter(|previous| previous != &path) {
            let _ = remove(&previous);
        }
        Ok(path)
    }

    /// Puts a shortcut to the app `entry` opens on the desktop, or brings the one made before
    /// in step; returns its path.
    pub fn place_app(
        &self,
        entry: &MenuEntry,
        shortcuts: &dyn Shortcuts,
    ) -> Result<PathBuf, String> {
        let mut note = self.made();
        let made = note.apps.entry(entry.package.clone()).or_default();
        let previous = made
            .app
            .clone()
            .filter(|path| is_shortcut_path(path) && path.parent() == Some(self.dir.as_path()));
        let name = clean_name(&entry.label);
        let name = if name.is_empty() {
            entry.package.clone()
        } else {
            name
        };
        let path = self.place(
            candidate_names(&name, &entry.package),
            previous,
            &entry.shortcut(),
            shortcuts,
        )?;
        made.app = Some(path.clone());
        write_note(&self.note, &note)?;
        Ok(path)
    }

    /// Puts a shortcut that starts `shortcut` of the app `entry` opens on the desktop, with
    /// its own icon kept in the app's folder `app_dir`; returns its path.
    pub fn place_shortcut(
        &self,
        entry: &MenuEntry,
        shortcut: &AppShortcut,
        app_dir: &Path,
        shortcuts: &dyn Shortcuts,
    ) -> Result<PathBuf, String> {
        if !is_valid_shortcut_id(&shortcut.id) {
            return Err(format!(
                "{}'s shortcut has an ID no command line can carry",
                entry.package
            ));
        }
        let icon = if shortcut.icon_png.is_empty() {
            None
        } else {
            write_icon(app_dir, &shortcut.icon_png)
        };
        let link = shortcut_link(entry, shortcut, icon.as_deref());
        let mut note = self.made();
        let made = note.apps.entry(entry.package.clone()).or_default();
        let previous = made
            .shortcuts
            .get(&shortcut.id)
            .cloned()
            .filter(|path| is_shortcut_path(path) && path.parent() == Some(self.dir.as_path()));
        let name = clean_name(shortcut.label());
        let name = if name.is_empty() {
            clean_name(&entry.label)
        } else {
            name
        };
        let name = if name.is_empty() {
            entry.package.clone()
        } else {
            name
        };
        let path = self.place(
            candidate_names(&name, &clean_name(&entry.label)),
            previous,
            &link,
            shortcuts,
        )?;
        made.shortcuts.insert(shortcut.id.clone(), path.clone());
        write_note(&self.note, &note)?;
        Ok(path)
    }

    /// Removes the shortcuts of every app that is not in `installed`. Returns what went wrong,
    /// one sentence each; the rest is done anyway.
    pub fn keep_only(&self, installed: &BTreeSet<String>) -> Vec<String> {
        let mut note = self.made();
        let gone: Vec<String> = note
            .apps
            .keys()
            .filter(|package| !installed.contains(*package))
            .cloned()
            .collect();
        if gone.is_empty() {
            return Vec::new();
        }
        let mut problems = Vec::new();
        for package in gone {
            let Some(made) = note.apps.remove(&package) else {
                continue;
            };
            for path in made.paths().filter(|path| is_shortcut_path(path)) {
                if let Err(e) = remove(path) {
                    problems.push(format!(
                        "could not remove {package}'s desktop shortcut {}: {e}",
                        path.display()
                    ));
                }
            }
        }
        if let Err(why) = write_note(&self.note, &note) {
            problems.push(why);
        }
        problems
    }
}

/// The Start menu entry of the installed app `package` named `label`, whose window program
/// the window manager made in `apps_dir`. An error when the program is not there yet.
pub fn entry_in(apps_dir: &Path, package: &str, label: &str) -> Result<MenuEntry, String> {
    let label = if label.trim().is_empty() {
        package
    } else {
        label.trim()
    };
    let named = crate::app_exe::program_path(apps_dir, package, label);
    // A program not yet renamed after the app's new name is still the app's.
    let program = if named.exists() {
        Some(named)
    } else {
        fs::read_dir(apps_dir.join(package))
            .ok()
            .and_then(|entries| {
                entries.flatten().map(|entry| entry.path()).find(|path| {
                    path.extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
                })
            })
    };
    let Some(program) = program else {
        return Err(format!(
            "{label} has no program to start yet; it is made once the DSA Window Manager \
             has seen the app, so open DSA and try again"
        ));
    };
    Ok(MenuEntry {
        package: package.to_string(),
        label: label.to_string(),
        program,
    })
}

/// Puts a shortcut to the installed `app` on the user's desktop, as `dsa apps desktop-shortcut`
/// does; returns its path.
pub fn place_app_on_desktop(app: &dsa_host_core::tasks::AppInfo) -> Result<PathBuf, String> {
    let apps_dir =
        crate::app_exe::default_apps_dir().ok_or("the user's DSA folder cannot be found")?;
    let entry = entry_in(&apps_dir, &app.package_name, &app.label)?;
    let desktop = Desktop::default_for_user().ok_or("the user's desktop cannot be found")?;
    let _com = crate::start_menu::ComApartment::enter();
    desktop.place_app(&entry, &crate::start_menu::ShellLinks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr7_a_desktop_shortcut_is_named_after_its_label_without_what_windows_refuses() {
        assert_eq!(clean_name("New chat"), "New chat");
        assert_eq!(clean_name(" a/b:c*? "), "abc");
        assert_eq!(clean_name("Notes..."), "Notes");
        assert_eq!(clean_name("\u{7}"), "");
        assert_eq!(
            candidate_names("Chat", "com.example.chat")[..3],
            [
                "Chat.lnk".to_string(),
                "Chat (com.example.chat).lnk".to_string(),
                "Chat (2).lnk".to_string()
            ]
        );
    }

    #[test]
    fn fr7_a_shortcut_id_is_quoted_as_windows_reads_it_back() {
        assert_eq!(quote_argument("new-chat"), "new-chat");
        assert_eq!(quote_argument("a b"), "\"a b\"");
        assert_eq!(quote_argument(""), "\"\"");
        assert_eq!(quote_argument("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_argument("end\\"), "end\\");
        assert_eq!(quote_argument("a b\\"), "\"a b\\\\\"");
        assert_eq!(
            shortcut_arguments("com.example.chat", "new chat"),
            "--package com.example.chat --shortcut \"new chat\""
        );
    }

    #[test]
    fn fr7_a_shortcuts_png_becomes_a_one_image_icon_file() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&96u32.to_be_bytes());
        png.extend_from_slice(&96u32.to_be_bytes());
        let ico = ico_from_png(&png).unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!((ico[6], ico[7]), (96, 96));
        assert_eq!(
            u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize,
            png.len()
        );
        assert_eq!(&ico[22..], &png[..]);
        assert_eq!(ico_from_png(b"not a png at all, really"), None);
        let dir = Path::new("C:\\apps\\com.example.chat");
        assert_eq!(icon_path(dir, &png), icon_path(dir, &png));
        assert_ne!(icon_path(dir, &png), icon_path(dir, b"other"));
    }

    #[test]
    fn fr7_the_window_program_reads_its_shortcut_and_desktop_shortcut_arguments() {
        let args = |line: &str| -> Vec<String> { line.split(' ').map(String::from).collect() };
        assert_eq!(
            shortcut_of(&args("--package com.example.chat --shortcut new-chat")),
            Some("new-chat".into())
        );
        assert_eq!(shortcut_of(&args("--package com.example.chat")), None);
        assert_eq!(shortcut_of(&args("--shortcut")), None);
        assert_eq!(
            shortcut_of(&["--shortcut".to_string(), "bad\nid".to_string()]),
            None
        );
        assert!(creates_desktop_shortcut(&args(
            "--create-desktop-shortcut --package com.example.chat"
        )));
        assert!(!creates_desktop_shortcut(&args(
            "--package com.example.chat"
        )));
    }

    #[test]
    fn fr7_a_damaged_note_never_names_anything_but_a_shortcut() {
        assert!(is_shortcut_path(Path::new(
            "C:\\Users\\me\\Desktop\\Chat.lnk"
        )));
        assert!(!is_shortcut_path(Path::new("Chat.lnk")));
        assert!(!is_shortcut_path(Path::new(
            "C:\\Users\\me\\Desktop\\notes.txt"
        )));
        assert!(!is_shortcut_path(Path::new("C:\\Users\\me\\Desktop\\.lnk")));
    }
}
