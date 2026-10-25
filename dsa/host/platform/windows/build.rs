//! Gives each program its name, version and icon, so Task Manager and the taskbar show
//! "Desktop Subsystem for Android Service" and "DSA Window Manager" rather than a file name.

use dsa_exe_resources::{embed, names};

const ICON: &str = "installer/setup-ui/assets/logo.ico";

fn main() {
    embed("dsa-hostd", names::SERVICE, None, ICON);
    embed(
        "dsa-wm",
        names::WINDOW_MANAGER,
        Some(names::WINDOW_MANAGER_KO),
        ICON,
    );
}
