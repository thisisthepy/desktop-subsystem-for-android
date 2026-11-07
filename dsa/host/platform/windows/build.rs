//! Gives each program its name, version and icon, so Task Manager and the taskbar show
//! "Desktop Subsystem for Android Service" and "DSA Window Manager" rather than a file name.

use dsa_exe_resources::{
    embed, embed_with, icon_resources, names, shell_string_tables, FILES_ICON_FIRST_IMAGE_ID,
    FILES_ICON_GROUP_ID,
};

const ICON: &str = "installer/setup-ui/assets/logo.ico";
/// The icon of Android's entry in Explorer's navigation pane (DSA Files).
const FILES_ICON: &str = "resources/icons/files.ico";

fn main() {
    embed("dsa-hostd", names::SERVICE, None, ICON);
    embed(
        "dsa-wm",
        names::WINDOW_MANAGER,
        Some(names::WINDOW_MANAGER_KO),
        ICON,
    );
    // Each app's copy of the window host gets the app's name and icon instead. The installed
    // one also carries the words Explorer and the Start menu show for DSA, in English and
    // Korean, which the installer points at as indirect strings, and the icon of Android's
    // entry in Explorer's navigation pane as its second icon.
    println!("cargo:rerun-if-changed={FILES_ICON}");
    let mut extra = shell_string_tables();
    let files = std::fs::read(FILES_ICON)
        .map_err(|e| e.to_string())
        .and_then(|file| icon_resources(&file, FILES_ICON_GROUP_ID, FILES_ICON_FIRST_IMAGE_ID))
        .unwrap_or_else(|why| panic!("reading the icon {FILES_ICON}: {why}"));
    extra.extend(files);
    embed_with("dsa-app-window", names::APP_WINDOW, None, ICON, extra);
}
