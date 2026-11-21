//! Where DSA.app keeps its programs, and how a program finds the rest of the app from where it
//! runs.
//!
//! ```text
//! DSA.app/Contents/MacOS/                 dsa-settings (the app itself), dsa, dsa-hostd, dsa-gfx
//! DSA.app/Contents/Frameworks/            the renderer, the Vulkan loader, MoltenVK
//! DSA.app/Contents/Resources/             geodata, the Vulkan driver description, icons
//! DSA.app/Contents/Helpers/DSA Window Manager.app/Contents/MacOS/dsa-wm
//! DSA.app/Contents/Helpers/DSA Devices.app/Contents/MacOS/dsa-devices
//! DSA.app/Contents/Helpers/DSA App Window.app/Contents/MacOS/dsa-app-window
//! ```
//!
//! The programs that run all session long and talk to the window server (the window manager
//! and the device lender) each live in a helper bundle with a bundle identifier of their own.
//! A program that ran from DSA.app's own `Contents/MacOS` would make Launch Services take
//! DSA.app for running, and opening DSA.app would then only reactivate that program instead
//! of starting the settings app. A development build keeps every program in one folder, and
//! each lookup here falls back to that.

use std::path::{Path, PathBuf};

/// The window manager's helper bundle in `Contents/Helpers`.
pub const WINDOW_MANAGER_HELPER: &str = "DSA Window Manager.app";

/// The device lender's helper bundle in `Contents/Helpers`.
pub const DEVICES_HELPER: &str = "DSA Devices.app";

/// The window manager's program name.
pub const WINDOW_MANAGER_PROGRAM: &str = "dsa-wm";

/// The device lender's program name.
pub const DEVICES_PROGRAM: &str = "dsa-devices";

/// DSA.app's `Contents` folder for a program at `program`, whether the program runs from
/// `Contents/MacOS` or from a helper bundle in `Contents/Helpers`. In a development build it
/// is the folder above the program's own, as before there were helper bundles.
pub fn app_contents(program: &Path) -> Option<PathBuf> {
    let contents = program.parent()?.parent()?;
    let outer = (contents.file_name()? == "Contents")
        .then(|| contents.parent())
        .flatten()
        .filter(|helper| helper.extension().is_some_and(|ext| ext == "app"))
        .and_then(Path::parent)
        .filter(|helpers| helpers.file_name().is_some_and(|name| name == "Helpers"))
        .and_then(Path::parent);
    Some(outer.unwrap_or(contents).to_path_buf())
}

/// The folder of DSA.app's main programs (`Contents/MacOS`) for a program at `program`, also
/// when it runs from a helper bundle. In a development build it is the program's own folder.
pub fn programs_dir(program: &Path) -> Option<PathBuf> {
    let own = program.parent()?;
    let contents = app_contents(program)?;
    if own.parent() == Some(contents.as_path()) {
        Some(own.to_path_buf())
    } else {
        Some(contents.join("MacOS"))
    }
}

/// `name`'s program in the helper bundle `helper` of the app `beside` is in (any program in
/// DSA.app), else `name` right beside it, as in a development build.
pub fn helper_program(beside: &Path, helper: &str, name: &str) -> Option<PathBuf> {
    if let Some(contents) = app_contents(beside) {
        let program = contents
            .join("Helpers")
            .join(helper)
            .join("Contents/MacOS")
            .join(name);
        if program.is_file() {
            return Some(program);
        }
    }
    let program = beside.parent()?.join(name);
    program.is_file().then_some(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dsa-app-layout-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fr2_the_window_manager_and_device_lender_run_from_their_own_helper_bundles() {
        let root = folder("helpers");
        let contents = root.join("DSA.app/Contents");
        let macos = contents.join("MacOS");
        let wm = contents
            .join("Helpers")
            .join(WINDOW_MANAGER_HELPER)
            .join("Contents/MacOS");
        let devices = contents
            .join("Helpers")
            .join(DEVICES_HELPER)
            .join("Contents/MacOS");
        for dir in [&macos, &wm, &devices] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let hostd = macos.join("dsa-hostd");
        std::fs::write(&hostd, b"hostd").unwrap();
        std::fs::write(wm.join(WINDOW_MANAGER_PROGRAM), b"wm").unwrap();
        std::fs::write(devices.join(DEVICES_PROGRAM), b"devices").unwrap();

        assert_eq!(
            helper_program(&hostd, WINDOW_MANAGER_HELPER, WINDOW_MANAGER_PROGRAM),
            Some(wm.join(WINDOW_MANAGER_PROGRAM))
        );
        assert_eq!(
            helper_program(&hostd, DEVICES_HELPER, DEVICES_PROGRAM),
            Some(devices.join(DEVICES_PROGRAM))
        );
        // From inside a helper, the app's Contents and its main programs are found.
        assert_eq!(app_contents(&wm.join("dsa-wm")), Some(contents.clone()));
        assert_eq!(
            app_contents(&devices.join("dsa-devices")),
            Some(contents.clone())
        );
        assert_eq!(programs_dir(&wm.join("dsa-wm")), Some(macos.clone()));
        assert_eq!(app_contents(&hostd), Some(contents.clone()));
        assert_eq!(programs_dir(&hostd), Some(macos.clone()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fr2_a_development_build_keeps_every_program_in_one_folder() {
        let root = folder("dev");
        let release = root.join("target/release");
        std::fs::create_dir_all(&release).unwrap();
        let hostd = release.join("dsa-hostd");
        std::fs::write(&hostd, b"hostd").unwrap();
        assert_eq!(
            helper_program(&hostd, WINDOW_MANAGER_HELPER, WINDOW_MANAGER_PROGRAM),
            None
        );
        std::fs::write(release.join("dsa-wm"), b"wm").unwrap();
        assert_eq!(
            helper_program(&hostd, WINDOW_MANAGER_HELPER, WINDOW_MANAGER_PROGRAM),
            Some(release.join("dsa-wm"))
        );
        assert_eq!(programs_dir(&release.join("dsa-wm")), Some(release.clone()));
        assert_eq!(app_contents(&hostd), Some(root.join("target")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
