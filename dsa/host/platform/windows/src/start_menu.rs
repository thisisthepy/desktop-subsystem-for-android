//! The Start menu entries of the apps installed in the user's Android.
//!
//! Each app a launcher lists in Android gets a shortcut in the user's Start menu, so Windows
//! search finds it by name and the Start menu lists it. The shortcut runs the app's own window
//! program (see [`crate::app_exe`]), named and drawn as the app, with `--package <package>`,
//! and the program asks for the app to be started, starting Android first if it is not running.
//!
//! The shortcuts go straight into the user's `Programs` folder rather than a folder of their
//! own. Windows search finds a shortcut in a subfolder just as well, but the Start menu's list
//! of all apps then shows them inside one folder, apart from every other app; directly in
//! `Programs` an Android app sits among the Windows apps under its own name, as it would on a
//! phone. A name another program's shortcut already has gets the package after it, so nothing
//! of anyone else's is replaced.
//!
//! Each shortcut carries the app's own AppUserModelID ([`app_user_model_id`]), which the app's
//! window program also takes for itself, so the taskbar groups the app's windows on one button
//! with the app's icon, and pinning that button pins the shortcut.
//!
//! The window manager keeps the shortcuts in step with the apps whenever Android says which it
//! has. It records the shortcuts it made in a note (`start-menu.json` in
//! `%LOCALAPPDATA%\DSA`) and only ever changes or removes those: a shortcut it did not make is
//! never touched.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use dsa_host_core::compositor::window_hosts::is_valid_package;
use dsa_host_core::tasks::AppInfo;
use serde::{Deserialize, Serialize};

use crate::app_exe::program_name;
use crate::app_window::PACKAGE_ARGUMENT;
use crate::compositor::log::{note, CompositorLogger};

/// The longest AppUserModelID Windows takes, in characters.
const MAX_APP_ID: usize = 128;

/// What every app's AppUserModelID starts with. Microsoft asks for IDs of the form
/// `CompanyName.ProductName.SubProduct`, so another vendor's IDs never collide with these:
/// the company is thisisthepy, the product DSA, and the sub-product the app's package.
pub const APP_ID_PREFIX: &str = "Thisisthepy.DSA.";

/// What earlier versions started each app's AppUserModelID with. Their registry keys are
/// removed on upgrade and uninstall.
pub const OLD_APP_ID_PREFIX: &str = "DSA.";

/// DSA's own AppUserModelID, which its own toasts (the news about updates) are shown under, so
/// Windows names them DSA rather than an Android app. No package is one word, so no app's ID
/// is this one; it starts with [`APP_ID_PREFIX`], so uninstalling removes its key with the
/// apps' keys.
pub const DSA_APP_ID: &str = "Thisisthepy.DSA.Notices";

/// The AppUserModelID of `package`'s windows and Start menu shortcut. Every app has its own,
/// so the taskbar never groups two apps together.
pub fn app_user_model_id(package: &str) -> String {
    format!("{APP_ID_PREFIX}{package}")
        .chars()
        .take(MAX_APP_ID)
        .collect()
}

/// The folder of the user's Start menu programs: `%APPDATA%\Microsoft\Windows\Start
/// Menu\Programs`.
pub fn default_programs_dir() -> Option<PathBuf> {
    crate::paths::start_menu_programs()
}

/// The note of the shortcuts the window manager made: `%LOCALAPPDATA%\DSA\start-menu.json`.
pub fn default_note() -> Option<PathBuf> {
    crate::paths::UserRoot::current().map(|root| root.start_menu_note())
}

/// The note's file name.
pub const NOTE_NAME: &str = "start-menu.json";

/// What a shortcut holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    /// The program it runs.
    pub target: PathBuf,
    pub arguments: String,
    /// The file the icon comes from and the icon's index in it.
    pub icon: (PathBuf, i32),
    /// The folder the program starts in.
    pub working_dir: PathBuf,
    /// The tooltip the Start menu shows.
    pub description: String,
    /// `System.AppUserModel.ID`.
    pub app_user_model_id: String,
}

/// One app's Start menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub package: String,
    /// The app's name, which the shortcut is named after.
    pub label: String,
    /// The app's window program.
    pub program: PathBuf,
}

impl MenuEntry {
    /// The shortcut that opens the app.
    pub fn shortcut(&self) -> Shortcut {
        Shortcut {
            target: self.program.clone(),
            arguments: format!("{PACKAGE_ARGUMENT} {}", self.package),
            icon: (self.program.clone(), 0),
            working_dir: self
                .program
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            description: format!("{} (Desktop Subsystem for Android)", self.label),
            app_user_model_id: app_user_model_id(&self.package),
        }
    }
}

/// The shortcuts the window manager made, by package: the file name of each, in the
/// `Programs` folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Note {
    shortcuts: BTreeMap<String, String>,
}

/// Writes and reads shortcut files.
pub trait Shortcuts {
    fn write(&self, path: &Path, shortcut: &Shortcut) -> Result<(), String>;
    /// What the shortcut at `path` holds; an error when there is none or it cannot be read.
    fn read(&self, path: &Path) -> Result<Shortcut, String>;
}

/// The user's Start menu, as far as DSA's shortcuts go.
pub struct StartMenu {
    /// Where the shortcuts go.
    pub dir: PathBuf,
    /// The note of the shortcuts made there.
    pub note: PathBuf,
}

impl StartMenu {
    fn load(&self) -> Note {
        fs::read_to_string(&self.note)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, note: &Note) -> Result<(), String> {
        if let Some(dir) = self.note.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(note).unwrap_or_default();
        let fresh = self.note.with_extension("json.new");
        fs::write(&fresh, text)
            .and_then(|()| fs::rename(&fresh, &self.note))
            .map_err(|e| format!("could not write {}: {e}", self.note.display()))
    }

    /// Makes the shortcuts match `entries`: one for each app, named after it, and none for an
    /// app that is gone. Returns what went wrong, one sentence each; the rest is done anyway.
    pub fn update(&self, entries: &[MenuEntry], shortcuts: &dyn Shortcuts) -> Vec<String> {
        let mut problems = Vec::new();
        let mut note = self.load();
        if let Err(e) = fs::create_dir_all(&self.dir) {
            return vec![format!("could not create {}: {e}", self.dir.display())];
        }
        let wanted: BTreeSet<&str> = entries.iter().map(|e| e.package.as_str()).collect();
        // The shortcuts of the apps that are gone.
        let gone: Vec<String> = note
            .shortcuts
            .keys()
            .filter(|package| !wanted.contains(package.as_str()))
            .cloned()
            .collect();
        for package in gone {
            if let Some(file) = note.shortcuts.remove(&package) {
                if let Err(why) = remove(&self.dir.join(&file)) {
                    problems.push(format!("could not remove {package}'s shortcut: {why}"));
                    note.shortcuts.insert(package, file);
                }
            }
        }
        for entry in entries {
            let ours: BTreeSet<String> = note
                .shortcuts
                .iter()
                .filter(|(package, _)| **package != entry.package)
                .map(|(_, file)| file.to_lowercase())
                .collect();
            let current = note.shortcuts.get(&entry.package).cloned();
            let Some(file) = self.file_name(entry, current.as_deref(), &ours) else {
                problems.push(format!(
                    "{}'s shortcut is not made: other programs' shortcuts have its name",
                    entry.package
                ));
                continue;
            };
            let path = self.dir.join(&file);
            let shortcut = entry.shortcut();
            if shortcuts.read(&path).ok().as_ref() != Some(&shortcut) {
                if let Err(why) = shortcuts.write(&path, &shortcut) {
                    problems.push(format!(
                        "could not make {}'s shortcut: {why}",
                        entry.package
                    ));
                    continue;
                }
            }
            if let Some(old) = current.filter(|old| !old.eq_ignore_ascii_case(&file)) {
                let _ = remove(&self.dir.join(old));
            }
            note.shortcuts.insert(entry.package.clone(), file);
        }
        if let Err(why) = self.save(&note) {
            problems.push(why);
        }
        problems
    }

    /// The file name `entry`'s shortcut gets: the app's name, else the name with the package
    /// after it when another program's shortcut has the name already. `None` when both are
    /// taken by shortcuts that are not DSA's.
    fn file_name(
        &self,
        entry: &MenuEntry,
        current: Option<&str>,
        others_of_ours: &BTreeSet<String>,
    ) -> Option<String> {
        let name = program_name(&entry.package, &entry.label);
        let candidates = [
            format!("{name}.lnk"),
            format!("{name} ({}).lnk", entry.package),
        ];
        candidates.into_iter().find(|file| {
            let mine = current.is_some_and(|current| current.eq_ignore_ascii_case(file));
            mine || (!others_of_ours.contains(&file.to_lowercase())
                && !self.dir.join(file).exists())
        })
    }
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Shortcut files through the shell's own `ShellLink` object.
pub struct ShellLinks;

impl Shortcuts for ShellLinks {
    fn write(&self, path: &Path, shortcut: &Shortcut) -> Result<(), String> {
        shell::write(path, shortcut)
    }

    fn read(&self, path: &Path) -> Result<Shortcut, String> {
        shell::read(path)
    }
}

/// The calls into the shell. COM must be initialised on the calling thread
/// ([`ComApartment`]).
mod shell {
    use std::path::{Path, PathBuf};

    use std::mem::ManuallyDrop;

    use windows::core::{Interface, GUID, HSTRING, PWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::{
        PropVariantClear, PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    use super::Shortcut;

    /// `System.AppUserModel.ID`.
    const APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };

    /// The longest text a shortcut field is read back with.
    const MAX_TEXT: usize = 1024;

    fn link() -> Result<IShellLinkW, String> {
        // SAFETY: a COM object created on a thread with COM initialised.
        unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("the shell cannot make shortcuts: {e}"))
    }

    fn text(buffer: &[u16]) -> String {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }

    pub fn write(path: &Path, shortcut: &Shortcut) -> Result<(), String> {
        let link = link()?;
        let fail = |what: &str, e: windows::core::Error| format!("setting the {what}: {e}");
        // SAFETY: calls on a live COM object with NUL-terminated strings that outlive them.
        unsafe {
            link.SetPath(&HSTRING::from(shortcut.target.as_os_str()))
                .map_err(|e| fail("program", e))?;
            link.SetArguments(&HSTRING::from(shortcut.arguments.as_str()))
                .map_err(|e| fail("arguments", e))?;
            link.SetIconLocation(&HSTRING::from(shortcut.icon.0.as_os_str()), shortcut.icon.1)
                .map_err(|e| fail("icon", e))?;
            link.SetWorkingDirectory(&HSTRING::from(shortcut.working_dir.as_os_str()))
                .map_err(|e| fail("folder", e))?;
            link.SetDescription(&HSTRING::from(shortcut.description.as_str()))
                .map_err(|e| fail("description", e))?;
            let store: IPropertyStore = link.cast().map_err(|e| fail("properties", e))?;
            // The shell reads the ID as a plain wide string. The value borrows `id`, which
            // outlives it, and the store keeps a copy, so it is never cleared or dropped: that
            // would free memory COM did not allocate.
            let id: Vec<u16> = shortcut
                .app_user_model_id
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let value = ManuallyDrop::new(PROPVARIANT {
                Anonymous: PROPVARIANT_0 {
                    Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                        vt: VT_LPWSTR,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: PROPVARIANT_0_0_0 {
                            pwszVal: PWSTR(id.as_ptr() as *mut u16),
                        },
                    }),
                },
            });
            store
                .SetValue(&APP_USER_MODEL_ID, &*value)
                .map_err(|e| fail("app ID", e))?;
            store.Commit().map_err(|e| fail("app ID", e))?;
            let file: IPersistFile = link.cast().map_err(|e| fail("file", e))?;
            // Written beside the shortcut first, so a half-written one never shows.
            let mut fresh = path.as_os_str().to_owned();
            fresh.push(".new");
            let fresh = PathBuf::from(fresh);
            file.Save(&HSTRING::from(fresh.as_os_str()), true)
                .map_err(|e| format!("saving {}: {e}", fresh.display()))?;
            std::fs::rename(&fresh, path).map_err(|e| {
                let _ = std::fs::remove_file(&fresh);
                format!("putting {} in place: {e}", path.display())
            })
        }
    }

    pub fn read(path: &Path) -> Result<Shortcut, String> {
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        let link = link()?;
        let fail = |what: &str, e: windows::core::Error| format!("reading the {what}: {e}");
        // SAFETY: calls on a live COM object, writing into buffers they are given the size of.
        unsafe {
            let file: IPersistFile = link.cast().map_err(|e| fail("file", e))?;
            file.Load(&HSTRING::from(path.as_os_str()), STGM_READ)
                .map_err(|e| fail("file", e))?;
            let mut buffer = vec![0u16; MAX_TEXT];
            link.GetPath(&mut buffer, std::ptr::null_mut(), 0)
                .map_err(|e| fail("program", e))?;
            let target = PathBuf::from(text(&buffer));
            buffer.fill(0);
            link.GetArguments(&mut buffer)
                .map_err(|e| fail("arguments", e))?;
            let arguments = text(&buffer);
            buffer.fill(0);
            let mut index = 0;
            link.GetIconLocation(&mut buffer, &mut index)
                .map_err(|e| fail("icon", e))?;
            let icon = (PathBuf::from(text(&buffer)), index);
            buffer.fill(0);
            link.GetWorkingDirectory(&mut buffer)
                .map_err(|e| fail("folder", e))?;
            let working_dir = PathBuf::from(text(&buffer));
            buffer.fill(0);
            link.GetDescription(&mut buffer)
                .map_err(|e| fail("description", e))?;
            let description = text(&buffer);
            let store: IPropertyStore = link.cast().map_err(|e| fail("properties", e))?;
            // Cleared once below, and never dropped as well.
            let mut value = ManuallyDrop::new(
                store
                    .GetValue(&APP_USER_MODEL_ID)
                    .map_err(|e| fail("app ID", e))?,
            );
            let inner = &value.Anonymous.Anonymous;
            let app_user_model_id = if inner.vt == VT_LPWSTR {
                inner.Anonymous.pwszVal.to_string().unwrap_or_default()
            } else {
                String::new()
            };
            let _ = PropVariantClear(&mut *value);
            Ok(Shortcut {
                target,
                arguments,
                icon,
                working_dir,
                description,
                app_user_model_id,
            })
        }
    }
}

/// Where the window manager makes the apps' window programs and their shortcuts.
pub struct StartMenuSetup {
    /// The installed app window host, which each app's program is a copy of.
    pub template: PathBuf,
    /// Where each app's program goes.
    pub apps_dir: PathBuf,
    pub menu: StartMenu,
}

impl StartMenuSetup {
    /// The user's own folders, beside the installed window host. `None` when they cannot be
    /// told.
    pub fn default_for_user() -> Option<StartMenuSetup> {
        Some(StartMenuSetup {
            template: crate::app_exe::default_template()?,
            apps_dir: crate::app_exe::default_apps_dir()?,
            menu: StartMenu {
                dir: default_programs_dir()?,
                note: default_note()?,
            },
        })
    }

    /// Makes each app's window program and its shortcut, and removes the shortcuts of the apps
    /// that are gone. Returns what went wrong, one sentence each; the rest is done anyway.
    pub fn sync(&self, apps: &[AppInfo], shortcuts: &dyn Shortcuts) -> Vec<String> {
        self.sync_waiting(apps, shortcuts).0
    }

    /// [`StartMenuSetup::sync`], and whether a program is still waiting to take its app's name
    /// and icon because it runs, so the sync is worth trying again once its windows close.
    pub fn sync_waiting(&self, apps: &[AppInfo], shortcuts: &dyn Shortcuts) -> (Vec<String>, bool) {
        let mut problems = Vec::new();
        let mut entries = Vec::new();
        let mut waiting = false;
        for app in apps {
            if !is_valid_package(&app.package_name) {
                continue;
            }
            match crate::app_exe::ensure_installed(&self.template, &self.apps_dir, app) {
                Ok((program, note)) => {
                    waiting |= note.is_some();
                    problems.extend(note);
                    let label = app.label.trim();
                    entries.push(MenuEntry {
                        package: app.package_name.clone(),
                        label: if label.is_empty() {
                            app.package_name.clone()
                        } else {
                            label.to_string()
                        },
                        program,
                    });
                }
                Err(why) => problems.push(why),
            }
        }
        problems.extend(self.menu.update(&entries, shortcuts));
        (problems, waiting)
    }

    /// Removes the window program of each app in `gone`, an app that was installed and is no
    /// longer. A program that still runs cannot be removed; it stays until it is made again.
    pub fn remove_programs(&self, gone: &BTreeSet<String>) {
        for package in gone {
            if is_valid_package(package) {
                let _ = fs::remove_dir_all(self.apps_dir.join(package));
            }
        }
    }
}

/// The settings app of the installation whose window manager is `wm`
/// (`<install>\settings\DsaSettings.exe`), which Windows' list of installed apps runs to
/// uninstall an app. It is a windowed program, unlike `dsa.exe`, so no console flashes up.
pub fn uninstaller_beside(wm: &std::path::Path) -> Option<PathBuf> {
    wm.parent()
        .map(|dir| dir.join("settings").join("DsaSettings.exe"))
}

/// The settings app installed with this window manager.
pub fn default_uninstaller() -> Option<PathBuf> {
    uninstaller_beside(&std::env::current_exe().ok()?)
}

/// How often the Start menu is brought in step again while an app's program could not take
/// the app's name and icon because the app's windows were open.
pub const RETRY_WHILE_IN_USE: std::time::Duration = std::time::Duration::from_secs(15);

/// Keeps the Start menu, and Windows' list of installed apps, in step with each list of
/// installed apps that arrives on `installed`, until it closes. When several lists are waiting,
/// only the latest is acted on. A program that runs, and so could not take its app's new name
/// or icon, is tried again every [`RETRY_WHILE_IN_USE`] until its windows have closed.
pub fn keep(
    installed: Receiver<Vec<AppInfo>>,
    setup: StartMenuSetup,
    logger: Option<Arc<CompositorLogger>>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    let _com = ComApartment::enter();
    let mut previous: Option<BTreeSet<String>> = None;
    let uninstaller = default_uninstaller();
    let keys = crate::installed_apps::RegistryKeys::current_user();
    let mut last: Option<Vec<AppInfo>> = None;
    let mut waiting = false;
    // What was logged at the last sync, so a retry says only what is new.
    let mut told: BTreeSet<String> = BTreeSet::new();
    // Each app's jump list follows its shortcuts, and its desktop shortcuts go with it.
    // Kept in a note, so a restart neither commits an unchanged list again nor forgets the
    // list of an app uninstalled while the window manager was not running.
    let mut jump_lists = match crate::jump_list::default_note() {
        Some(note) => crate::jump_list::Committed::in_note(note),
        None => crate::jump_list::Committed::new(),
    };
    let texts = crate::jump_list::Texts::for_user();
    let desktop = crate::desktop_shortcuts::Desktop::default_for_user();
    loop {
        let received = if waiting {
            installed.recv_timeout(RETRY_WHILE_IN_USE)
        } else {
            installed.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        let fresh = received.is_ok();
        let mut apps = match received {
            Ok(apps) => apps,
            Err(RecvTimeoutError::Timeout) => match last.clone() {
                Some(apps) => apps,
                None => {
                    waiting = false;
                    continue;
                }
            },
            Err(RecvTimeoutError::Disconnected) => break,
        };
        while let Ok(newer) = installed.try_recv() {
            apps = newer;
        }
        let (mut problems, still_waiting) = setup.sync_waiting(&apps, &ShellLinks);
        waiting = still_waiting;
        // After the Start menu, whose programs the jump lists' tasks run.
        problems.extend(
            crate::jump_list::sync_apps(
                &apps,
                &setup.apps_dir,
                &texts,
                &mut jump_lists,
                &crate::jump_list::ShellJumpLists,
            )
            .into_iter()
            .map(|why| format!("jump list: {why}")),
        );
        let now_told: BTreeSet<String> = problems.iter().cloned().collect();
        for problem in &problems {
            if !told.contains(problem) {
                note(logger.as_deref(), &format!("Start menu: {problem}"));
            }
        }
        told = now_told;
        last = Some(apps.clone());
        if !fresh {
            // Only the programs were tried again; the rest is in step with this list already.
            continue;
        }
        if let Some(uninstaller) = &uninstaller {
            let entries = crate::installed_apps::entries(&apps, &setup.apps_dir, uninstaller);
            for problem in crate::installed_apps::sync(&entries, &keys) {
                note(logger.as_deref(), &format!("Installed apps: {problem}"));
            }
        }
        let now: BTreeSet<String> = apps.iter().map(|app| app.package_name.clone()).collect();
        if let Some(desktop) = &desktop {
            for problem in desktop.keep_only(&now) {
                note(logger.as_deref(), &format!("Desktop: {problem}"));
            }
        }
        if let Some(previous) = &previous {
            setup.remove_programs(&previous.difference(&now).cloned().collect());
        }
        previous = Some(now);
    }
}

/// COM, initialised on this thread for as long as it lives, as the shell's objects need.
pub struct ComApartment {
    initialised: bool,
}

impl ComApartment {
    pub fn enter() -> ComApartment {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: initialises COM for this thread; undone on drop when it succeeded.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComApartment {
            initialised: result.is_ok(),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialised {
            // SAFETY: balances the successful CoInitializeEx of this thread.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}
