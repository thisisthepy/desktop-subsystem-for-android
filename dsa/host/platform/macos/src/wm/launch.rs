//! Starting an app's window host: the copy of `dsa-app-window` inside the app's own bundle in
//! `~/Applications/DSA Apps`, so the Dock shows the app with its own name and icon.
//!
//! The bundle is opened through Launch Services (`open -a`), as Launchpad opens it, with
//! `--shown-by-window-manager` so the window host does not ask Android to start the app again.
//! An app without a bundle yet gets one first, from its name and icon. Where no bundle can be
//! made, the window host's helper bundle in DSA.app is run for the app instead, under the
//! helper's own bundle identity, so it never passes for DSA.app (whose program is the settings
//! app) and opening DSA.app always opens the settings.

use std::path::{Path, PathBuf};

use dsa_host_core::tasks::{jump_list_shortcuts, AppInfo, PinShortcutRequest};

use crate::shell::app_stubs::{self, AppEntry, StubShortcut, EXECUTABLE};

/// The argument that tells a window host the window manager started it.
pub const SHOWN_BY_WINDOW_MANAGER: &str = "--shown-by-window-manager";

/// The bundle in `folder` that launches `package`, if there is one.
pub fn bundle_for(folder: &Path, package: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(folder).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path())
        .find(|path| app_stubs::stub_package(path).as_deref() == Some(package))
}

/// The app as its bundle shows it.
pub fn app_entry(app: &AppInfo) -> AppEntry {
    AppEntry {
        package: app.package_name.clone(),
        label: if app.label.is_empty() {
            app.package_name.clone()
        } else {
            app.label.clone()
        },
        version: app.version_name.clone(),
        icon_png: app.icon_png.clone(),
        shortcuts: jump_list_shortcuts(&app.shortcuts)
            .into_iter()
            .map(|shortcut| StubShortcut {
                id: shortcut.id.clone(),
                label: shortcut.label().to_string(),
            })
            .collect(),
    }
}

/// How the window host of `bundle` is opened: `open` with these arguments.
pub fn open_arguments(bundle: &Path) -> Vec<String> {
    vec![
        "-a".into(),
        bundle.to_string_lossy().into_owned(),
        "--args".into(),
        SHOWN_BY_WINDOW_MANAGER.into(),
    ]
}

/// How the window host in DSA.app is run for `package` when the app has no bundle.
pub fn direct_arguments(package: &str) -> Vec<String> {
    vec![
        "--package".into(),
        package.into(),
        SHOWN_BY_WINDOW_MANAGER.into(),
    ]
}

/// The helper bundle in DSA.app's `Contents/Helpers` that holds the window host, with a
/// bundle identifier of its own.
pub const HELPER_APP: &str = "DSA App Window.app";

/// The window host program of an installation, which each app's bundle copies: the one in the
/// helper bundle (`Contents/Helpers/DSA App Window.app` beside the window manager's
/// `Contents/MacOS`), else one right beside the window manager, as in a development build.
pub fn installed_window_host(window_manager: &Path) -> Option<PathBuf> {
    let programs = window_manager.parent()?;
    let helper = programs.parent().map(|contents| {
        contents
            .join("Helpers")
            .join(HELPER_APP)
            .join("Contents/MacOS")
            .join(EXECUTABLE)
    });
    if let Some(helper) = helper.filter(|path| path.is_file()) {
        return Some(helper);
    }
    let program = programs.join(EXECUTABLE);
    program.is_file().then_some(program)
}

/// Starts app window hosts.
pub struct AppLauncher {
    /// `~/Applications/DSA Apps`.
    pub folder: PathBuf,
    /// The window host program each bundle copies.
    pub window_host: Option<PathBuf>,
    /// The user's Desktop, where desktop shortcuts are links to the bundles.
    pub desktop: Option<PathBuf>,
}

impl AppLauncher {
    /// The bundle of `package`: the one there is, else one made now from `app`.
    fn bundle(&self, package: &str, app: Option<&AppInfo>) -> Result<PathBuf, String> {
        if let Some(bundle) = bundle_for(&self.folder, package) {
            return Ok(bundle);
        }
        let window_host = self
            .window_host
            .as_ref()
            .ok_or("dsa-app-window is not installed beside dsa-wm")?;
        let entry = match app {
            Some(app) => app_entry(app),
            None => app_entry(&AppInfo {
                package_name: package.to_string(),
                ..AppInfo::default()
            }),
        };
        let name = app_stubs::bundle_name(&entry.label, package, false);
        std::fs::create_dir_all(&self.folder)
            .map_err(|e| format!("creating {}: {e}", self.folder.display()))?;
        let bundle = app_stubs::write_stub(&self.folder, &name, &entry, window_host)
            .map_err(|e| format!("making {package}'s app bundle: {e}"))?;
        #[cfg(target_os = "macos")]
        {
            let report = app_stubs::SyncReport {
                written: vec![bundle.clone()],
                removed: Vec::new(),
            };
            if let Err(why) = app_stubs::register(&report) {
                super::log(&why);
            }
        }
        Ok(bundle)
    }

    /// Starts the window host of `package`.
    pub fn start(&self, package: &str, app: Option<&AppInfo>) -> Result<(), String> {
        let (program, arguments) = match self.bundle(package, app) {
            Ok(bundle) => (PathBuf::from("/usr/bin/open"), open_arguments(&bundle)),
            Err(why) => {
                let window_host = self
                    .window_host
                    .clone()
                    .ok_or_else(|| format!("{why}; nothing can show {package}'s windows"))?;
                super::log(&format!("{why}; {package} opens under DSA's own icon"));
                (window_host, direct_arguments(package))
            }
        };
        let mut child = std::process::Command::new(&program)
            .args(&arguments)
            .spawn()
            .map_err(|e| format!("running {}: {e}", program.display()))?;
        // The child is waited for, so it never lingers as a zombie.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    /// Puts the shortcut of `request`, which the user agreed to pin, on the Desktop: a
    /// bundle that starts it, and a link to the bundle.
    pub fn pin(&self, request: &PinShortcutRequest) -> Result<PathBuf, String> {
        let window_host = self
            .window_host
            .as_ref()
            .ok_or("dsa-app-window is not installed beside dsa-wm")?;
        let desktop = self
            .desktop
            .as_ref()
            .ok_or("the Desktop is not known: HOME is not set")?;
        let link = app_stubs::place_pinned_in(
            &self.folder,
            desktop,
            &request.package_name,
            &request.shortcut,
            window_host,
        )?;
        #[cfg(target_os = "macos")]
        {
            let bundle = app_stubs::pinned_bundle_path(
                &self.folder,
                &request.package_name,
                &request.shortcut.id,
            );
            let report = app_stubs::SyncReport {
                written: vec![bundle],
                removed: Vec::new(),
            };
            if let Err(why) = app_stubs::register(&report) {
                super::log(&why);
            }
        }
        Ok(link)
    }

    /// Makes the bundles match the apps installed in the guest.
    pub fn sync(&self, apps: &[AppInfo]) -> Result<(), String> {
        let Some(window_host) = &self.window_host else {
            return Err("dsa-app-window is not installed beside dsa-wm".into());
        };
        let entries: Vec<AppEntry> = apps.iter().map(app_entry).collect();
        let report = app_stubs::sync(&self.folder, &entries, window_host)
            .map_err(|e| format!("updating the app bundles: {e}"))?;
        // The Desktop's links follow: an app that is gone takes its desktop shortcuts.
        let packages: Vec<String> = apps.iter().map(|app| app.package_name.clone()).collect();
        for link in app_stubs::sync_desktop(&self.folder, &packages) {
            super::log(&format!("removed the desktop shortcut {}", link.display()));
        }
        #[cfg(target_os = "macos")]
        app_stubs::register(&report)?;
        #[cfg(not(target_os = "macos"))]
        let _ = report;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let process = std::process::id();
        let folder = std::env::temp_dir().join(format!("dsa-wm-launch-{name}-{process}"));
        let _ = std::fs::remove_dir_all(&folder);
        folder
    }

    fn notes() -> AppInfo {
        AppInfo {
            package_name: "com.example.notes".into(),
            label: "Notes".into(),
            ..AppInfo::default()
        }
    }

    #[test]
    fn fr7_a_window_host_opens_from_its_apps_own_bundle() {
        let folder = folder("bundle");
        let host = folder.join("dsa-app-window");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(&host, b"window host").unwrap();
        let entry = app_entry(&notes());
        let bundle = app_stubs::write_stub(&folder, "Notes.app", &entry, &host).unwrap();
        assert_eq!(
            bundle_for(&folder, "com.example.notes"),
            Some(bundle.clone())
        );
        assert_eq!(bundle_for(&folder, "com.example.other"), None);
        let arguments = open_arguments(&bundle);
        assert_eq!(arguments[0], "-a");
        assert_eq!(arguments.last().unwrap(), SHOWN_BY_WINDOW_MANAGER);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn fr7_an_app_without_a_name_is_shown_by_its_package() {
        let app = AppInfo {
            package_name: "com.example.plain".into(),
            ..AppInfo::default()
        };
        assert_eq!(app_entry(&app).label, "com.example.plain");
        assert_eq!(app_entry(&notes()).label, "Notes");
    }

    #[test]
    fn fr17_the_window_host_runs_from_its_helper_bundle_not_dsa_app() {
        let contents = folder("helper").join("DSA.app/Contents");
        let macos = contents.join("MacOS");
        let helper = contents
            .join("Helpers")
            .join(HELPER_APP)
            .join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::create_dir_all(&helper).unwrap();
        std::fs::write(macos.join("dsa-wm"), b"wm").unwrap();
        // A development build keeps the window host beside the window manager.
        std::fs::write(macos.join(EXECUTABLE), b"beside").unwrap();
        assert_eq!(
            installed_window_host(&macos.join("dsa-wm")),
            Some(macos.join(EXECUTABLE))
        );
        // An installed DSA.app has it in the helper bundle, which wins.
        std::fs::write(helper.join(EXECUTABLE), b"helper").unwrap();
        assert_eq!(
            installed_window_host(&macos.join("dsa-wm")),
            Some(helper.join(EXECUTABLE))
        );
        let _ = std::fs::remove_dir_all(contents.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn fr3_without_a_bundle_the_installed_window_host_shows_the_app() {
        let arguments = direct_arguments("com.example.notes");
        assert_eq!(arguments[..2], ["--package", "com.example.notes"]);
        assert!(arguments.contains(&SHOWN_BY_WINDOW_MANAGER.to_string()));
    }
}
