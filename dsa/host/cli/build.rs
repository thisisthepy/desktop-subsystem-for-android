//! Gives `dsa.exe` its name, version and icon.

fn main() {
    dsa_exe_resources::embed(
        "dsa",
        dsa_exe_resources::names::CLI,
        None,
        "../platform/windows/installer/setup-ui/assets/logo.ico",
    );
}
