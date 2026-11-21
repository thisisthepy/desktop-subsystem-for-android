//! The renderer opens the Vulkan loader at run time by its bare name, `libvulkan.dylib`, so
//! linking never lists it and the loader has to be put into the app on purpose, under that
//! name, and opened before the renderer.
//!
//! The loader here is a small library built with the system compiler. The packaging script
//! puts it into a `Frameworks` folder, and the renderer's loading step opens it from there,
//! after which a bare-name open (as the renderer's) finds it without any environment setting.

#![cfg(target_os = "macos")]

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::Command;

use dsa_host_macos::compositor::renderer::preload_vulkan_loader;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsa-vkloader-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A library with the loader's file name under an install name that is not the bare one,
/// as Homebrew's is.
fn fake_loader(dir: &Path) -> PathBuf {
    let source = dir.join("loader.c");
    std::fs::write(&source, "int vkDsaProbe(void) { return 7; }\n").unwrap();
    let library = dir.join("libvulkan.1.dylib");
    let status = Command::new("cc")
        .args([
            "-dynamiclib",
            "-install_name",
            "/opt/homebrew/opt/x/libvulkan.1.dylib",
            "-o",
        ])
        .arg(&library)
        .arg(&source)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the system compiler built the stand-in loader"
    );
    library
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/bundle-vulkan-loader.sh")
}

#[test]
fn fr4_the_package_carries_the_vulkan_loader_under_the_name_the_renderer_opens() {
    let dir = scratch("package");
    let loader = fake_loader(&dir);
    let frameworks = dir.join("Frameworks");
    std::fs::create_dir_all(&frameworks).unwrap();
    let status = Command::new("bash")
        .arg(script())
        .arg(&loader)
        .arg(&frameworks)
        .status()
        .unwrap();
    assert!(status.success(), "the packaging step ran");
    let bundled = frameworks.join("libvulkan.dylib");
    assert!(bundled.is_file(), "Frameworks holds libvulkan.dylib");
    let id = Command::new("otool")
        .arg("-D")
        .arg(&bundled)
        .output()
        .unwrap();
    let id = String::from_utf8_lossy(&id.stdout).into_owned();
    assert!(
        id.lines().last().map(str::trim) == Some("libvulkan.dylib"),
        "the install name is the bare name the renderer opens, got: {id}"
    );
    let verify = Command::new("codesign")
        .args(["--verify"])
        .arg(&bundled)
        .status()
        .unwrap();
    assert!(
        verify.success(),
        "the copy is signed again after its install name changed"
    );
}

#[test]
fn fr4_a_bare_name_open_finds_the_loader_the_renderer_preloaded_from_its_folder() {
    let dir = scratch("preload");
    let loader = fake_loader(&dir);
    let frameworks = dir.join("Frameworks");
    std::fs::create_dir_all(&frameworks).unwrap();
    assert!(Command::new("bash")
        .arg(script())
        .arg(&loader)
        .arg(&frameworks)
        .status()
        .unwrap()
        .success());

    let bare = CString::new("libvulkan.dylib").unwrap();
    let before = unsafe { libc::dlopen(bare.as_ptr(), libc::RTLD_NOW) };
    assert!(
        before.is_null(),
        "nothing answers the bare name before the preload"
    );

    assert_eq!(preload_vulkan_loader(&frameworks), Ok(true));
    let after = unsafe { libc::dlopen(bare.as_ptr(), libc::RTLD_NOW) };
    assert!(
        !after.is_null(),
        "the bare name now finds the bundled loader"
    );
}

#[test]
fn fr4_a_folder_without_a_loader_is_left_to_the_systems_search() {
    let dir = scratch("none");
    assert_eq!(preload_vulkan_loader(&dir), Ok(false));
}
