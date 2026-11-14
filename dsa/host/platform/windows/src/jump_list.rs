//! Each Android app's jump list: its app shortcuts as tasks, and a task that puts a shortcut
//! to the app on the desktop.
//!
//! Android apps publish shortcuts (`ShortcutManager`), such as a messenger's New chat, which a
//! phone's launcher shows when the app's icon is held. Windows shows the same thing as the
//! tasks of the program's jump list, which opens on a right click of its taskbar button or its
//! Start menu entry. The window manager therefore builds a jump list for each app's
//! AppUserModelID, the one its Start menu shortcut and its windows carry, so both places show
//! it: one task per static or dynamic shortcut, by the app's own rank, then "Create desktop
//! shortcut". Each shortcut task runs the app's window program with `--shortcut <id>`, which
//! starts Android first when it is off, as the Start menu entry does; its icon is the
//! shortcut's own, kept in the app's folder as an `.ico` file.
//!
//! A list is committed again only when what it shows changed, and the list of an app that is
//! gone is deleted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dsa_host_core::tasks::{jump_list_shortcuts, AppShortcut};
use serde::{Deserialize, Serialize};

use crate::desktop_shortcuts::{shortcut_arguments, write_icon, CREATE_DESKTOP_SHORTCUT_ARGUMENT};
use crate::start_menu::{app_user_model_id, MenuEntry};

/// One task of a jump list: a link to the app's window program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// What the list shows.
    pub title: String,
    pub arguments: String,
    /// The file the icon comes from and the icon's index in it.
    pub icon: (PathBuf, i32),
    /// The tooltip.
    pub description: String,
}

/// One app's jump list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpList {
    pub app_user_model_id: String,
    /// The program every task runs: the app's window program.
    pub program: PathBuf,
    pub tasks: Vec<Task>,
}

/// The words of the jump list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texts {
    pub create_desktop_shortcut: &'static str,
}

impl Texts {
    pub fn english() -> Texts {
        Texts {
            create_desktop_shortcut: "Create desktop shortcut",
        }
    }

    pub fn korean() -> Texts {
        Texts {
            create_desktop_shortcut: "바탕 화면 바로 가기 만들기",
        }
    }

    /// The words in the user's display language.
    pub fn for_user() -> Texts {
        // SAFETY: reads the user's display language.
        let language = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        if crate::compositor::widget_menu::is_korean(language) {
            Texts::korean()
        } else {
            Texts::english()
        }
    }
}

/// The jump list of the app `entry` opens, whose shortcuts are `shortcuts`, with the
/// shortcuts' icons written in the app's folder `app_dir`.
pub fn jump_list(
    entry: &MenuEntry,
    shortcuts: &[AppShortcut],
    app_dir: &Path,
    texts: &Texts,
) -> JumpList {
    let program_icon = (entry.program.clone(), 0);
    let mut tasks: Vec<Task> = jump_list_shortcuts(shortcuts)
        .into_iter()
        .map(|shortcut| {
            let icon = if shortcut.icon_png.is_empty() {
                None
            } else {
                write_icon(app_dir, &shortcut.icon_png)
            };
            let description = if shortcut.long_label.trim().is_empty() {
                shortcut.label().to_string()
            } else {
                shortcut.long_label.trim().to_string()
            };
            Task {
                title: shortcut.label().to_string(),
                arguments: shortcut_arguments(&entry.package, &shortcut.id),
                icon: icon.map(|icon| (icon, 0)).unwrap_or(program_icon.clone()),
                description,
            }
        })
        .collect();
    tasks.push(Task {
        title: texts.create_desktop_shortcut.to_string(),
        arguments: format!(
            "{CREATE_DESKTOP_SHORTCUT_ARGUMENT} {} {}",
            crate::app_window::PACKAGE_ARGUMENT,
            entry.package
        ),
        icon: program_icon,
        description: format!("{} ({})", texts.create_desktop_shortcut, entry.label),
    });
    JumpList {
        app_user_model_id: app_user_model_id(&entry.package),
        program: entry.program.clone(),
        tasks,
    }
}

/// Commits and deletes jump lists.
pub trait JumpLists {
    /// Makes `list` the jump list of its AppUserModelID.
    fn commit(&self, list: &JumpList) -> Result<(), String>;
    /// Removes the jump list of `app_user_model_id`.
    fn delete(&self, app_user_model_id: &str) -> Result<(), String>;
}

/// What the note keeps of one committed jump list: the AppUserModelID it is under, and a hash
/// of what it shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedList {
    pub app_user_model_id: String,
    pub hash: String,
}

/// The note's content: the committed list of each app, by package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CommittedNote {
    #[serde(default)]
    apps: BTreeMap<String, CommittedList>,
}

/// The note of the jump lists committed: `%LOCALAPPDATA%\DSA\jump-lists.json`.
pub fn default_note() -> Option<PathBuf> {
    crate::paths::UserRoot::current().map(|root| root.jump_lists_note())
}

/// A hash of everything `list` shows, so a list is committed again only when it changed.
pub fn hash_of(list: &JumpList) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(list.app_user_model_id.as_bytes());
    hasher.update([0]);
    hasher.update(list.program.to_string_lossy().as_bytes());
    for task in &list.tasks {
        for part in [
            task.title.as_str(),
            task.arguments.as_str(),
            &task.icon.0.to_string_lossy(),
            &task.icon.1.to_string(),
            task.description.as_str(),
        ] {
            hasher.update([0]);
            hasher.update(part.as_bytes());
        }
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The jump lists the window manager committed, by package, kept in a note so they outlive
/// the window manager: after a restart an unchanged list is not committed again, and the list
/// of an app that went while it was not running is deleted.
#[derive(Debug, Default)]
pub struct Committed {
    lists: BTreeMap<String, CommittedList>,
    /// Where they are kept; `None` keeps them in memory only.
    note: Option<PathBuf>,
}

impl Committed {
    /// Lists kept in memory only.
    pub fn new() -> Committed {
        Committed::default()
    }

    /// The lists the note at `note` records, kept there from now on. A note that is missing
    /// or does not read records none.
    pub fn in_note(note: PathBuf) -> Committed {
        let lists = std::fs::read_to_string(&note)
            .ok()
            .and_then(|text| serde_json::from_str::<CommittedNote>(&text).ok())
            .map(|read| read.apps)
            .unwrap_or_default();
        Committed {
            lists,
            note: Some(note),
        }
    }

    /// The list committed for `package`, if any.
    pub fn get(&self, package: &str) -> Option<&CommittedList> {
        self.lists.get(package)
    }

    fn save(&self) -> Result<(), String> {
        let Some(note) = &self.note else {
            return Ok(());
        };
        if let Some(dir) = note.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&CommittedNote {
            apps: self.lists.clone(),
        })
        .unwrap_or_default();
        let fresh = note.with_extension("json.new");
        std::fs::write(&fresh, text)
            .and_then(|()| std::fs::rename(&fresh, note))
            .map_err(|e| format!("could not write {}: {e}", note.display()))
    }

    /// Makes the jump lists match `lists` (by package): commits each that changed, and deletes
    /// the list of each app committed before that is neither in it nor in `keep` (an app whose
    /// list stays as it is for now). Returns what went wrong, one sentence each; the rest is
    /// done anyway.
    pub fn sync(
        &mut self,
        lists: Vec<(String, JumpList)>,
        keep: &BTreeSet<String>,
        jump_lists: &dyn JumpLists,
    ) -> Vec<String> {
        let mut problems = Vec::new();
        let before = self.lists.clone();
        let wanted: BTreeMap<String, JumpList> = lists.into_iter().collect();
        let gone: Vec<String> = self
            .lists
            .keys()
            .filter(|package| !wanted.contains_key(*package) && !keep.contains(*package))
            .cloned()
            .collect();
        for package in gone {
            if let Some(list) = self.lists.get(&package) {
                match jump_lists.delete(&list.app_user_model_id) {
                    Ok(()) => {
                        self.lists.remove(&package);
                    }
                    Err(why) => {
                        problems.push(format!("could not remove {package}'s jump list: {why}"))
                    }
                }
            }
        }
        for (package, list) in wanted {
            let hash = hash_of(&list);
            if self
                .lists
                .get(&package)
                .is_some_and(|kept| kept.hash == hash)
            {
                continue;
            }
            match jump_lists.commit(&list) {
                Ok(()) => {
                    self.lists.insert(
                        package,
                        CommittedList {
                            app_user_model_id: list.app_user_model_id.clone(),
                            hash,
                        },
                    );
                }
                Err(why) => problems.push(format!("could not make {package}'s jump list: {why}")),
            }
        }
        if self.lists != before {
            if let Err(why) = self.save() {
                problems.push(why);
            }
        }
        problems
    }
}

/// Brings the jump list of every app in `apps` in step, each with the window program the Start
/// menu made for it in `apps_dir`, and deletes those of apps that are gone. An app whose program
/// is not made yet keeps its list until the next sync. Returns what went wrong, one sentence
/// each.
pub fn sync_apps(
    apps: &[dsa_host_core::tasks::AppInfo],
    apps_dir: &Path,
    texts: &Texts,
    committed: &mut Committed,
    jump_lists: &dyn JumpLists,
) -> Vec<String> {
    let mut lists = Vec::new();
    let mut keep = BTreeSet::new();
    for app in apps {
        if !dsa_host_core::compositor::window_hosts::is_valid_package(&app.package_name) {
            continue;
        }
        match crate::desktop_shortcuts::entry_in(apps_dir, &app.package_name, &app.label) {
            Ok(entry) => {
                let app_dir = apps_dir.join(&app.package_name);
                let list = jump_list(&entry, &app.shortcuts, &app_dir, texts);
                lists.push((app.package_name.clone(), list));
            }
            Err(_) => {
                keep.insert(app.package_name.clone());
            }
        }
    }
    committed.sync(lists, &keep, jump_lists)
}

/// Jump lists through the shell's `DestinationList` object. COM must be initialised on the
/// calling thread ([`crate::start_menu::ComApartment`]).
pub struct ShellJumpLists;

impl JumpLists for ShellJumpLists {
    fn commit(&self, list: &JumpList) -> Result<(), String> {
        shell::commit(list)
    }

    fn delete(&self, app_user_model_id: &str) -> Result<(), String> {
        shell::delete(app_user_model_id)
    }
}

mod shell {
    use std::mem::ManuallyDrop;

    use windows::core::{Interface, GUID, HSTRING, PWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{
        DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, ShellLink,
    };

    use super::{JumpList, Task};

    /// `System.Title`, which a jump list shows as a task's name.
    const TITLE: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
        pid: 2,
    };

    fn fail(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
        move |e| format!("{what}: {e}")
    }

    fn link(program: &std::path::Path, task: &Task) -> Result<IShellLinkW, String> {
        // SAFETY: COM objects created and called on a thread with COM initialised, with
        // NUL-terminated strings that outlive each call.
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(fail("the shell cannot make links"))?;
            link.SetPath(&HSTRING::from(program.as_os_str()))
                .map_err(fail("setting the program"))?;
            link.SetArguments(&HSTRING::from(task.arguments.as_str()))
                .map_err(fail("setting the arguments"))?;
            link.SetIconLocation(&HSTRING::from(task.icon.0.as_os_str()), task.icon.1)
                .map_err(fail("setting the icon"))?;
            if let Some(dir) = program.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                    .map_err(fail("setting the folder"))?;
            }
            link.SetDescription(&HSTRING::from(task.description.as_str()))
                .map_err(fail("setting the tooltip"))?;
            let store: IPropertyStore = link.cast().map_err(fail("reading the properties"))?;
            // The value borrows `title`, which outlives it, and the store keeps a copy, so it is
            // never cleared or dropped: that would free memory COM did not allocate.
            let title: Vec<u16> = task
                .title
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
                            pwszVal: PWSTR(title.as_ptr() as *mut u16),
                        },
                    }),
                },
            });
            store
                .SetValue(&TITLE, &*value)
                .map_err(fail("setting the title"))?;
            store.Commit().map_err(fail("setting the title"))?;
            Ok(link)
        }
    }

    pub fn commit(list: &JumpList) -> Result<(), String> {
        // SAFETY: COM objects created and called on a thread with COM initialised.
        unsafe {
            let destinations: ICustomDestinationList =
                CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)
                    .map_err(fail("the shell has no jump lists"))?;
            destinations
                .SetAppID(&HSTRING::from(list.app_user_model_id.as_str()))
                .map_err(fail("naming the app"))?;
            let mut slots = 0u32;
            let _removed: IObjectArray = destinations
                .BeginList(&mut slots)
                .map_err(fail("starting the list"))?;
            let tasks: IObjectCollection =
                CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)
                    .map_err(fail("the shell cannot collect tasks"))?;
            for task in &list.tasks {
                let link = link(&list.program, task)?;
                tasks.AddObject(&link).map_err(fail("adding a task"))?;
            }
            let array: IObjectArray = tasks.cast().map_err(fail("listing the tasks"))?;
            if let Err(e) = destinations.AddUserTasks(&array) {
                let _ = destinations.AbortList();
                return Err(format!("adding the tasks: {e}"));
            }
            destinations
                .CommitList()
                .map_err(fail("committing the list"))
        }
    }

    pub fn delete(app_user_model_id: &str) -> Result<(), String> {
        // SAFETY: a COM object created and called on a thread with COM initialised.
        unsafe {
            let destinations: ICustomDestinationList =
                CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)
                    .map_err(fail("the shell has no jump lists"))?;
            destinations
                .DeleteList(&HSTRING::from(app_user_model_id))
                .map_err(fail("deleting the list"))
        }
    }
}
