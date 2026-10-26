//! Each app's own window program: a copy of the built app window host, rewritten to carry the
//! app's name and icon, read back the way Windows reads them.
#![cfg(windows)]

use std::path::{Path, PathBuf};

use dsa_exe_resources::{parse_ico, LANG_EN_US, RT_GROUP_ICON, RT_ICON, RT_VERSION};
use dsa_host_core::tasks::AppInfo;
use dsa_host_windows::app_exe::ensure;
use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceExW, LoadLibraryExW, LoadResource, LockResource, SizeofResource,
    LOAD_LIBRARY_AS_DATAFILE,
};

const TEMPLATE: &str = env!("CARGO_BIN_EXE_dsa-app-window");

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("app-exe")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The 256-pixel PNG of the DSA icon, standing in for an app's launcher icon.
fn png() -> Vec<u8> {
    let icon = include_bytes!("../installer/setup-ui/assets/logo.ico");
    parse_ico(icon)
        .unwrap()
        .into_iter()
        .find(|image| image.width == 0)
        .unwrap()
        .data
}

fn clock() -> AppInfo {
    AppInfo {
        package_name: "com.example.clock".into(),
        label: "Clock".into(),
        icon_png: png(),
    }
}

/// The file's `FileDescription`, as Task Manager reads it.
fn description(path: &Path) -> String {
    let path = wide(&path.to_string_lossy());
    unsafe {
        let size = GetFileVersionInfoSizeW(path.as_ptr(), std::ptr::null_mut());
        assert!(size > 0, "no version information");
        let mut data = vec![0u8; size as usize];
        assert_ne!(
            GetFileVersionInfoW(path.as_ptr(), 0, size, data.as_mut_ptr().cast()),
            0
        );
        let query = wide("\\StringFileInfo\\040904B0\\FileDescription");
        let mut value: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut length = 0u32;
        assert_ne!(
            VerQueryValueW(
                data.as_ptr().cast(),
                query.as_ptr(),
                &mut value,
                &mut length
            ),
            0,
            "no English FileDescription"
        );
        let text = std::slice::from_raw_parts(value as *const u16, length as usize);
        String::from_utf16_lossy(text)
            .trim_end_matches('\0')
            .to_string()
    }
}

/// The bytes of one resource of the file, or `None` when it has none.
fn resource(path: &Path, kind: u16, id: u16, language: u16) -> Option<Vec<u8>> {
    let path = wide(&path.to_string_lossy());
    unsafe {
        let module = LoadLibraryExW(
            path.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_AS_DATAFILE,
        );
        assert!(!module.is_null());
        let found = FindResourceExW(
            module,
            kind as usize as *const u16,
            id as usize as *const u16,
            language,
        );
        let bytes = (!found.is_null()).then(|| {
            let loaded = LoadResource(module, found);
            let size = SizeofResource(module, found) as usize;
            std::slice::from_raw_parts(LockResource(loaded) as *const u8, size).to_vec()
        });
        FreeLibrary(module);
        bytes
    }
}

#[test]
fn fr3_the_installed_window_host_carries_its_own_name() {
    assert_eq!(description(Path::new(TEMPLATE)), "DSA app window");
}

#[test]
fn fr3_an_apps_window_program_carries_its_name_and_icon() {
    let dir = fresh_dir("named");
    let (program, note) = ensure(
        Path::new(TEMPLATE),
        &dir,
        "com.example.clock",
        Some(&clock()),
    )
    .expect("making the window program");
    assert!(note.is_none());
    assert_eq!(program, dir.join("com.example.clock.exe"));
    assert_eq!(description(&program), "Clock");
    // The icon is the app's alone: its PNG is the first image, and the group lists only it.
    assert_eq!(resource(&program, RT_ICON, 1, 0), Some(png()));
    assert_eq!(resource(&program, RT_ICON, 2, 0), None);
    let group = resource(&program, RT_GROUP_ICON, 1, 0).expect("an icon group");
    assert_eq!(u16::from_le_bytes([group[4], group[5]]), 1);
    assert!(resource(&program, RT_VERSION, 1, LANG_EN_US).is_some());
}

#[test]
fn fr3_an_app_without_a_name_or_icon_is_named_by_its_package() {
    let dir = fresh_dir("bare");
    let (program, _) =
        ensure(Path::new(TEMPLATE), &dir, "org.example.bare", None).expect("making it");
    assert_eq!(description(&program), "org.example.bare");
    // It keeps the window host's own icon.
    assert!(resource(&program, RT_ICON, 2, 0).is_some());
}

#[test]
fn fr3_a_window_program_is_made_again_only_when_its_app_changes() {
    let dir = fresh_dir("again");
    let template = Path::new(TEMPLATE);
    let (program, _) = ensure(template, &dir, "com.example.clock", Some(&clock())).unwrap();
    let made = std::fs::metadata(&program).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    ensure(template, &dir, "com.example.clock", Some(&clock())).unwrap();
    assert_eq!(
        std::fs::metadata(&program).unwrap().modified().unwrap(),
        made
    );

    let renamed = AppInfo {
        label: "World Clock".into(),
        ..clock()
    };
    ensure(template, &dir, "com.example.clock", Some(&renamed)).unwrap();
    assert_eq!(description(&program), "World Clock");
}

#[test]
fn nfr5_a_package_that_is_not_one_never_becomes_a_file() {
    let dir = fresh_dir("bad");
    for bad in [r"..\evil", "a/b", "", "con."] {
        assert!(
            ensure(Path::new(TEMPLATE), &dir, bad, None).is_err(),
            "{bad}"
        );
    }
    assert!(!dir.exists());
}

/// The windows of process `pid` titled `title`.
fn windows_titled(pid: u32, title: &str) -> Vec<isize> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId,
    };
    struct Search {
        pid: u32,
        title: String,
        found: Vec<isize>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> i32 {
        let search = &mut *(lparam as *mut Search);
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner == search.pid {
            let mut buffer = [0u16; 256];
            let length = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            if String::from_utf16_lossy(&buffer[..length.max(0) as usize]) == search.title {
                search.found.push(hwnd as isize);
            }
        }
        1
    }
    let mut search = Search {
        pid,
        title: title.to_string(),
        found: Vec::new(),
    };
    unsafe { EnumWindows(Some(each), &mut search as *mut Search as LPARAM) };
    search.found
}

/// The real thing: the app's own program, started as the window manager starts it, opens its
/// window and shows a frame from a texture another process made, over the real pipe.
#[test]
fn fr3_an_apps_window_program_shows_a_frame_made_in_another_process() {
    use std::time::{Duration, Instant};

    use dsa_host_core::compositor::window_hosts::{accept_window_host, answer_attached};
    use dsa_host_windows::compositor::present::Gpu;
    use dsa_host_windows::pipe::{current_user_only_sddl, NamedPipeListener};
    use dsa_protocol::window_host::types::{
        envelope::Body, CloseWindow, Frame, FrameTexture, ShowWindow,
    };
    use dsa_protocol::window_host::{decode, send};
    use dsa_transport::local::LocalListener;
    use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
    };

    const WAIT: Duration = Duration::from_secs(10);
    let dir = fresh_dir("process");
    let (program, _) = ensure(
        Path::new(TEMPLATE),
        &dir,
        "com.example.clock",
        Some(&clock()),
    )
    .unwrap();
    let pipe = format!("dsa-test-wm-{}", std::process::id());
    let mut listener = NamedPipeListener::bind(&pipe, &current_user_only_sddl().unwrap()).unwrap();
    let mut child = std::process::Command::new(&program)
        .args(["--started-by-window-manager", "--pipe", &pipe])
        .spawn()
        .unwrap();

    let (mut connection, _) = listener.accept(WAIT).expect("the window host connects");
    let attach = accept_window_host(&mut *connection, "test", WAIT).unwrap();
    assert_eq!(attach.package_name, "com.example.clock");
    assert_eq!(attach.process_id, child.id());
    assert!(!attach.launch);
    answer_attached(&mut *connection, None).unwrap();
    send(
        &mut *connection,
        Body::ShowWindow(ShowWindow {
            task_id: 1,
            title: "Alarm".into(),
            width: 400,
            height: 300,
            focused: false,
        }),
    )
    .unwrap();
    let deadline = Instant::now() + WAIT;
    while windows_titled(child.id(), "Alarm").is_empty() {
        assert!(Instant::now() < deadline, "the window did not open");
        std::thread::sleep(Duration::from_millis(20));
    }

    match Gpu::new(None, false) {
        Err(why) => eprintln!("skipping the frame: no Direct3D 11 device here ({why})"),
        Ok(gpu) => {
            let texture = gpu.shared_texture(400, 300).unwrap();
            let mut placed = std::ptr::null_mut();
            unsafe {
                let process = OpenProcess(PROCESS_DUP_HANDLE, 0, child.id());
                assert!(!process.is_null());
                assert_ne!(
                    DuplicateHandle(
                        GetCurrentProcess(),
                        texture.handle() as _,
                        process,
                        &mut placed,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    ),
                    0
                );
                CloseHandle(process);
            }
            send(
                &mut *connection,
                Body::FrameTexture(FrameTexture {
                    task_id: 1,
                    texture_id: 1,
                    handle: placed as usize as u64,
                    width: 400,
                    height: 300,
                }),
            )
            .unwrap();
            send(
                &mut *connection,
                Body::Frame(Frame {
                    task_id: 1,
                    texture_id: 1,
                    sequence: 1,
                }),
            )
            .unwrap();
            let presented = loop {
                let frame = connection
                    .recv(Some(WAIT))
                    .expect("the window host answers");
                if let Some(Body::FramePresented(presented)) = decode(&frame).unwrap().body {
                    break presented;
                }
            };
            assert_eq!((presented.task_id, presented.sequence), (1, 1));
        }
    }

    send(
        &mut *connection,
        Body::CloseWindow(CloseWindow { task_id: 1 }),
    )
    .unwrap();
    connection.close();
    drop(connection);
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the window host did not end");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status}");
}
