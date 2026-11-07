//! The apps of the user's Android that keep running with no window, in the notification area
//! of the taskbar and in Windows' media controls.
//!
//! An Android app that runs a foreground service (music playback, a download, a VPN) keeps
//! running after its last window closes. Android would show it in its status bar; the guest
//! has none, so the window manager shows each such app as an icon in the notification area
//! (the system tray) instead. The icon is the app's, and its tooltip is the title and text of
//! the notification the service keeps shown. A left click opens the app's window; the right
//! click menu has Open, the notification's actions (Pause and Next for music) and Stop, which
//! ends the app after the user confirms it, unless they chose not to be asked again.
//!
//! Each icon has a GUID of its own, made from the app's package ([`tray_guid`]), so Windows
//! remembers for each app whether its icon shows on the taskbar or in the overflow, as it
//! does for every other program's icon.
//!
//! An app with an active media session also gets Windows' media controls
//! (SystemMediaTransportControls): the media keys, and the media flyout with the title,
//! artist and album art. Each session has a hidden window of its own that carries the app's
//! AppUserModelID, so the flyout names the app rather than the window manager.
//!
//! The window manager watches the service ([`sync`]); what the user does goes back to it on
//! new connections. The Win32 side is [`WindowsTray`]; a test's [`Tray`] records what it is
//! asked.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::background::{
    BackgroundApp, BackgroundCommand, BackgroundEvent, MediaCommand, MediaSession,
};
use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;
use sha2::{Digest, Sha256};

use crate::compositor::log::{note, CompositorLogger};

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm background apps";

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long to wait before connecting again after the service could not be reached.
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch waits for the service before it looks at what the user did again.
const POLL: Duration = Duration::from_millis(100);

/// The longest tooltip the notification area shows, in UTF-16 units, without its NUL.
pub const MAX_TOOLTIP: usize = 127;

/// The menu item that opens the app.
pub const MENU_OPEN: u32 = 1;
/// The menu item that stops the app.
pub const MENU_STOP: u32 = 2;
/// The menu item of the notification's first action; the others follow it.
pub const MENU_FIRST_ACTION: u32 = 100;

/// The GUID of `package`'s icon in the notification area: the same for an app every time, and
/// different for every app. It is a name-based UUID (version 5 layout) made from a SHA-256 of
/// the package under a prefix of this program's own.
pub fn tray_guid(package: &str) -> u128 {
    let digest = Sha256::digest(format!("Thisisthepy.DSA.tray.{package}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    u128::from_be_bytes(bytes)
}

/// The name an app goes by: its label, or its package when it has none.
pub fn app_name(app: &BackgroundApp) -> &str {
    if app.app_label.trim().is_empty() {
        &app.package_name
    } else {
        &app.app_label
    }
}

/// The tooltip of `app`'s icon: its name, then the notification's title and text on lines of
/// their own, cut to what the notification area shows.
pub fn tooltip(app: &BackgroundApp) -> String {
    let mut lines = vec![app_name(app).to_string()];
    for line in [&app.title, &app.text] {
        let line = line.trim();
        if !line.is_empty() && line != app_name(app) {
            lines.push(line.to_string());
        }
    }
    let text = lines.join("\n");
    let mut out = String::new();
    let mut units = 0;
    for c in text.chars() {
        let more = c.len_utf16();
        if units + more > MAX_TOOLTIP {
            break;
        }
        units += more;
        out.push(c);
    }
    out
}

/// The words the tray and its question use, in the user's language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texts {
    pub open: &'static str,
    pub stop: &'static str,
    pub cancel: &'static str,
    pub do_not_ask: &'static str,
    korean: bool,
}

impl Texts {
    pub fn english() -> Texts {
        Texts {
            open: "Open",
            stop: "Stop",
            cancel: "Cancel",
            do_not_ask: "Do not ask again",
            korean: false,
        }
    }

    pub fn korean() -> Texts {
        Texts {
            open: "열기",
            stop: "중지",
            cancel: "취소",
            do_not_ask: "다시 묻지 않음",
            korean: true,
        }
    }

    /// The question asked before `name` is stopped.
    pub fn stop_question(&self, name: &str) -> String {
        if self.korean {
            format!(
                "{name}을(를) 중지할까요?\n\n백그라운드에서 하던 일(음악 재생 등)을 모두 \
                 멈추고, 앱이 쓰던 메모리를 돌려받습니다."
            )
        } else {
            format!(
                "Stop {name}?\n\nIt stops everything it does in the background, such as playing \
                 music, and the memory it used is freed."
            )
        }
    }
}

/// One entry of an icon's right click menu: its ID and its text. ID 0 is a separator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    pub id: u32,
    pub text: String,
}

/// The right click menu of `app`'s icon: Open, the notification's actions, and Stop.
pub fn menu_entries(app: &BackgroundApp, texts: &Texts) -> Vec<MenuEntry> {
    let mut entries = vec![MenuEntry {
        id: MENU_OPEN,
        text: texts.open.to_string(),
    }];
    let actions: Vec<MenuEntry> = app
        .actions
        .iter()
        .enumerate()
        .filter(|(_, title)| !title.trim().is_empty())
        .map(|(index, title)| MenuEntry {
            id: MENU_FIRST_ACTION + index as u32,
            // An ampersand would mark the next letter as the item's access key.
            text: title.trim().replace('&', "&&"),
        })
        .collect();
    if !actions.is_empty() {
        entries.push(MenuEntry {
            id: 0,
            text: String::new(),
        });
        entries.extend(actions);
    }
    entries.push(MenuEntry {
        id: 0,
        text: String::new(),
    });
    entries.push(MenuEntry {
        id: MENU_STOP,
        text: texts.stop.to_string(),
    });
    entries
}

/// What the user chose in a menu, as a request for the service: `None` for nothing.
pub fn chosen(id: u32) -> Option<BackgroundCommand> {
    match id {
        MENU_OPEN => Some(BackgroundCommand::Open),
        MENU_STOP => Some(BackgroundCommand::Stop),
        id if (MENU_FIRST_ACTION
            ..MENU_FIRST_ACTION + dsa_host_core::background::MAX_ACTIONS as u32)
            .contains(&id) =>
        {
            Some(BackgroundCommand::Action(id - MENU_FIRST_ACTION))
        }
        _ => None,
    }
}

/// What the tray is asked to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    /// Shows `app`'s icon, or changes it.
    Show(BackgroundApp),
    /// Removes the icon of the package.
    Remove(String),
    /// Shows a media session in Windows' media controls, or changes it.
    Media(MediaSession),
    /// Removes the package's media session from them.
    MediaGone(String),
    /// Whether Stop asks first.
    AskBeforeStop(bool),
}

/// What the user did with the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayReport {
    /// Open, an action or Stop for the package, Stop confirmed when it asked.
    Command(String, BackgroundCommand),
    /// A media key or a button of the media flyout.
    Media(String, MediaCommand),
    /// The user ticked "do not ask again" when confirming a stop.
    NeverAskBeforeStop,
}

/// The notification area and the media controls. The window manager's is [`WindowsTray`].
pub trait Tray {
    fn command(&mut self, command: TrayCommand);
    /// What the user did since the last call.
    fn reports(&mut self) -> Vec<TrayReport>;
}

/// Shows the apps that run in the background in the user's Android until `stop`,
/// reconnecting to the service whenever the connection ends, and passes what the user does
/// with them back to it.
pub fn sync(
    mut connector: Box<dyn Connector>,
    tray: &mut dyn Tray,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut last_error = String::new();
    // What the tray shows, so a new connection removes what is gone and shows what is new.
    let mut apps: BTreeMap<String, BackgroundApp> = BTreeMap::new();
    let mut media: BTreeMap<String, MediaSession> = BTreeMap::new();
    while !stop.load(Ordering::Relaxed) {
        let watched = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_background());
        let mut stream = match watched {
            Ok(stream) => stream,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    note(
                        logger.as_deref(),
                        &format!(
                            "the apps that run in the background cannot reach the service: {error}"
                        ),
                    );
                    last_error = error;
                }
                handle_reports(&mut *connector, tray, logger.as_deref());
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        if let Ok(settings) = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|mut client| client.settings())
        {
            tray.command(TrayCommand::AskBeforeStop(
                !settings.settings.stop_without_asking,
            ));
        }
        let current: BTreeMap<String, BackgroundApp> = std::mem::take(&mut stream.apps)
            .into_iter()
            .map(|app| (app.package_name.clone(), app))
            .collect();
        let current_media: BTreeMap<String, MediaSession> = std::mem::take(&mut stream.media)
            .into_iter()
            .map(|session| (session.package_name.clone(), session))
            .collect();
        for package in apps.keys().filter(|p| !current.contains_key(*p)) {
            tray.command(TrayCommand::Remove(package.clone()));
        }
        for package in media.keys().filter(|p| !current_media.contains_key(*p)) {
            tray.command(TrayCommand::MediaGone(package.clone()));
        }
        for app in current.values() {
            if apps.get(&app.package_name) != Some(app) {
                tray.command(TrayCommand::Show(app.clone()));
            }
        }
        for session in current_media.values() {
            if media.get(&session.package_name) != Some(session) {
                tray.command(TrayCommand::Media(session.clone()));
            }
        }
        apps = current;
        media = current_media;
        while !stop.load(Ordering::Relaxed) {
            handle_reports(&mut *connector, tray, logger.as_deref());
            match stream.next(POLL) {
                Ok(Some(event)) => match event {
                    BackgroundEvent::App(app) => {
                        apps.insert(app.package_name.clone(), app.clone());
                        tray.command(TrayCommand::Show(app));
                    }
                    BackgroundEvent::AppRemoved { package_name } => {
                        apps.remove(&package_name);
                        tray.command(TrayCommand::Remove(package_name));
                    }
                    BackgroundEvent::Media(session) => {
                        media.insert(session.package_name.clone(), session.clone());
                        tray.command(TrayCommand::Media(session));
                    }
                    BackgroundEvent::MediaRemoved { package_name } => {
                        media.remove(&package_name);
                        tray.command(TrayCommand::MediaGone(package_name));
                    }
                },
                Ok(None) => {}
                Err(error) => {
                    note(
                        logger.as_deref(),
                        &format!("the background apps' connection to the service ended: {error}"),
                    );
                    break;
                }
            }
        }
    }
    for package in apps.into_keys() {
        tray.command(TrayCommand::Remove(package));
    }
    for package in media.into_keys() {
        tray.command(TrayCommand::MediaGone(package));
    }
}

/// Passes what the user did with the tray to the service.
fn handle_reports(
    connector: &mut dyn Connector,
    tray: &mut dyn Tray,
    logger: Option<&CompositorLogger>,
) {
    for report in tray.reports() {
        let result =
            HostApiClient::connect(connector, BUILD, ANSWER).and_then(|mut client| match &report {
                TrayReport::Command(package, command) => {
                    client.act_on_background(package, *command)
                }
                TrayReport::Media(package, command) => client.control_media(package, *command),
                TrayReport::NeverAskBeforeStop => {
                    let mut settings = client.settings()?.settings;
                    settings.stop_without_asking = true;
                    client.set_settings(&settings).map(|_| ())
                }
            });
        if let Err(error) = result {
            note(
                logger,
                &format!("what the user did with an app that runs in the background did not reach Android: {error}"),
            );
        }
    }
}

/// The dialog template of the question asked before a stop, as `DialogBoxIndirectParamW`
/// takes it: a `DLGTEMPLATE` with its font, then a static text, the "do not ask again" check
/// box, and the Stop and Cancel buttons, each a `DLGITEMTEMPLATE` on a 4-byte boundary.
pub fn stop_dialog_template(question: &str, texts: &Texts, title: &str) -> Vec<u32> {
    fn word(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    fn dword(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    fn text(out: &mut Vec<u8>, value: &str) {
        for unit in value.encode_utf16() {
            word(out, unit);
        }
        word(out, 0);
    }
    fn align(out: &mut Vec<u8>) {
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    const DS_SETFONT: u32 = 0x40;
    const DS_MODALFRAME: u32 = 0x80;
    const DS_CENTER: u32 = 0x0800;
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_CAPTION: u32 = 0x00C0_0000;
    const WS_SYSMENU: u32 = 0x0008_0000;
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_TABSTOP: u32 = 0x0001_0000;
    const WS_EX_TOPMOST: u32 = 0x0000_0008;
    const BS_DEFPUSHBUTTON: u32 = 0x1;
    const BS_AUTOCHECKBOX: u32 = 0x3;
    const BUTTON: u16 = 0x0080;
    const STATIC: u16 = 0x0082;
    type Item<'a> = (u32, [i16; 4], u16, u16, &'a str);
    let items: [Item<'_>; 4] = [
        (0, [10, 10, 240, 52], STATIC_ID, STATIC, question),
        (
            BS_AUTOCHECKBOX | WS_TABSTOP,
            [10, 68, 240, 12],
            DO_NOT_ASK_ID,
            BUTTON,
            texts.do_not_ask,
        ),
        (
            BS_DEFPUSHBUTTON | WS_TABSTOP,
            [146, 88, 50, 14],
            STOP_ID,
            BUTTON,
            texts.stop,
        ),
        (
            WS_TABSTOP,
            [200, 88, 50, 14],
            CANCEL_ID,
            BUTTON,
            texts.cancel,
        ),
    ];
    let mut out = Vec::new();
    dword(
        &mut out,
        DS_SETFONT | DS_MODALFRAME | DS_CENTER | WS_POPUP | WS_CAPTION | WS_SYSMENU,
    );
    dword(&mut out, WS_EX_TOPMOST);
    word(&mut out, items.len() as u16);
    for value in [0i16, 0, 260, 110] {
        word(&mut out, value as u16);
    }
    word(&mut out, 0); // no menu
    word(&mut out, 0); // the standard dialog class
    text(&mut out, title);
    word(&mut out, 9); // points
    text(&mut out, "Segoe UI");
    for (style, place, id, class, caption) in items {
        align(&mut out);
        dword(&mut out, WS_CHILD | WS_VISIBLE | style);
        dword(&mut out, 0);
        for value in place {
            word(&mut out, value as u16);
        }
        word(&mut out, id);
        word(&mut out, 0xFFFF);
        word(&mut out, class);
        text(&mut out, caption);
        word(&mut out, 0); // no creation data
    }
    align(&mut out);
    out.chunks(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// The controls of the question asked before a stop.
pub const STATIC_ID: u16 = 100;
pub const DO_NOT_ASK_ID: u16 = 101;
/// `IDOK` and `IDCANCEL`, so Enter stops and Escape cancels.
pub const STOP_ID: u16 = 1;
pub const CANCEL_ID: u16 = 2;

/// The notification area icons and media controls of the window manager: Win32 windows on a
/// thread of their own, which the watch drives through a channel.
#[cfg(windows)]
pub struct WindowsTray {
    commands: std::sync::mpsc::Sender<TrayCommand>,
    reports: std::sync::mpsc::Receiver<TrayReport>,
    controller: Arc<std::sync::atomic::AtomicIsize>,
    thread: Option<thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl WindowsTray {
    /// Starts the tray's thread.
    pub fn open(logger: Option<Arc<CompositorLogger>>) -> Result<WindowsTray, String> {
        let (commands, command_rx) = std::sync::mpsc::channel();
        let (report_tx, reports) = std::sync::mpsc::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let controller = Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let slot = Arc::clone(&controller);
        let thread = thread::Builder::new()
            .name("dsa-wm-tray".into())
            .spawn(move || win::run(slot, command_rx, report_tx, ready_tx, logger))
            .map_err(|e| format!("could not start the tray's thread: {e}"))?;
        match ready.recv() {
            Ok(Ok(())) => Ok(WindowsTray {
                commands,
                reports,
                controller,
                thread: Some(thread),
            }),
            Ok(Err(why)) => {
                let _ = thread.join();
                Err(why)
            }
            Err(_) => {
                let _ = thread.join();
                Err("the tray's thread ended before it was ready".into())
            }
        }
    }
}

#[cfg(windows)]
impl Tray for WindowsTray {
    fn command(&mut self, command: TrayCommand) {
        if self.commands.send(command).is_ok() {
            win::wake(self.controller.load(Ordering::SeqCst));
        }
    }

    fn reports(&mut self) -> Vec<TrayReport> {
        self.reports.try_iter().collect()
    }
}

#[cfg(windows)]
impl Drop for WindowsTray {
    fn drop(&mut self) {
        win::close(self.controller.load(Ordering::SeqCst));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::Arc;

    use dsa_host_core::background::{BackgroundApp, BackgroundCommand, MediaSession};
    use windows_sys::core::GUID;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Controls::{IsDlgButtonChecked, BST_CHECKED};
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForWindow, GetSystemMetricsForDpi, SetThreadDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_GUID, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD,
        NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
        DestroyIcon, DestroyMenu, DestroyWindow, DialogBoxIndirectParamW, DispatchMessageW,
        EndDialog, GetCursorPos, GetMessageW, LoadIconW, PostMessageW, PostQuitMessage,
        RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem,
        TrackPopupMenuEx, TranslateMessage, HICON, ICONINFO, IDI_APPLICATION, MF_SEPARATOR,
        MF_STRING, MSG, SM_CXSMICON, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP,
        WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_INITDIALOG, WM_NULL, WM_SETTINGCHANGE,
        WNDCLASSEXW, WS_POPUP,
    };

    use super::{
        app_name, chosen, menu_entries, stop_dialog_template, tooltip, tray_guid, Texts,
        TrayCommand, TrayReport, CANCEL_ID, DO_NOT_ASK_ID, STOP_ID,
    };
    use crate::compositor::log::{note, CompositorLogger};

    /// The message that tells the controller window commands are waiting.
    const WM_COMMANDS: u32 = WM_APP + 1;
    /// The message the notification area sends about an icon.
    const WM_TRAY: u32 = WM_APP + 2;
    /// A left click or Enter on an icon, with version 4 of the notification area's messages.
    const NIN_SELECT: u32 = 0x0400;
    const NIN_KEYSELECT: u32 = 0x0401;

    /// DSA's own notification area icon, in one colour: dark for a light taskbar, white for
    /// a dark one. An app whose icon cannot be read shows it.
    const TRAY_FOR_LIGHT_TASKBAR: &[u8] = include_bytes!("../../resources/icons/tray-light.ico");
    const TRAY_FOR_DARK_TASKBAR: &[u8] = include_bytes!("../../resources/icons/tray-dark.ico");

    struct Icon {
        id: u32,
        app: BackgroundApp,
        icon: HICON,
        /// Whether it is DSA's own icon, drawn for the taskbar's theme, rather than the app's.
        themed: bool,
        /// Whether the icon is known by its GUID, or by the window and ID: Windows refuses
        /// a GUID it has seen with another program's path, as after DSA moved.
        by_guid: bool,
    }

    struct State {
        commands: Receiver<TrayCommand>,
        reports: Sender<TrayReport>,
        controller: HWND,
        icons: HashMap<String, Icon>,
        by_id: HashMap<u32, String>,
        next_id: u32,
        ask_before_stop: bool,
        taskbar_created: u32,
        texts: Texts,
        media_class: Vec<u16>,
        media: HashMap<String, super::media::Controls>,
        decoder: Option<crate::compositor::widgets::png::Decoder>,
        logger: Option<Arc<CompositorLogger>>,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
        STATE.with(|state| state.borrow_mut().as_mut().map(f))
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Tells the controller that commands are waiting.
    pub(super) fn wake(controller: isize) {
        if controller != 0 {
            // SAFETY: posting to a window handle; a stale one fails harmlessly.
            unsafe { PostMessageW(controller as HWND, WM_COMMANDS, 0, 0) };
        }
    }

    /// Asks the controller to remove every icon and end its thread.
    pub(super) fn close(controller: isize) {
        if controller != 0 {
            // SAFETY: as above.
            unsafe { PostMessageW(controller as HWND, WM_CLOSE, 0, 0) };
        }
    }

    pub(super) fn run(
        slot: Arc<AtomicIsize>,
        commands: Receiver<TrayCommand>,
        reports: Sender<TrayReport>,
        ready: Sender<Result<(), String>>,
        logger: Option<Arc<CompositorLogger>>,
    ) {
        // SAFETY: changes this thread's DPI awareness only.
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let _com = crate::start_menu::ComApartment::enter();
        let decoder = match crate::compositor::widgets::png::Decoder::new() {
            Ok(decoder) => Some(decoder),
            Err(why) => {
                note(
                    logger.as_deref(),
                    &format!("background apps show a plain icon: {why}"),
                );
                None
            }
        };
        let class = wide(&format!("DsaWmTray_{}", std::process::id()));
        let media_class = wide(&format!("DsaWmMedia_{}", std::process::id()));
        // SAFETY: the module handle of this process.
        let instance = unsafe { GetModuleHandleW(null()) };
        for (name, proc_) in [
            (&class, controller_proc as WndProc),
            (&media_class, media_proc as WndProc),
        ] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(proc_),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: null_mut(),
                hCursor: null_mut(),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: name.as_ptr(),
                hIconSm: null_mut(),
            };
            // SAFETY: the names outlive the classes, which live as long as the process.
            unsafe { RegisterClassExW(&wc) };
        }
        let taskbar = wide("TaskbarCreated");
        // SAFETY: a NUL-terminated message name.
        let taskbar_created = unsafe { RegisterWindowMessageW(taskbar.as_ptr()) };
        // A hidden top-level window, never shown: the notification area sends it the icons'
        // messages, and a menu needs a window that can come to the front.
        // SAFETY: a window of a class registered above.
        let controller = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if controller.is_null() {
            let _ = ready.send(Err(format!(
                "could not open the tray's window: {}",
                std::io::Error::last_os_error()
            )));
            return;
        }
        // SAFETY: reads the user's language.
        let korean = unsafe { GetUserDefaultUILanguage() } & 0x3ff == 0x12;
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                commands,
                reports,
                controller,
                icons: HashMap::new(),
                by_id: HashMap::new(),
                next_id: 1,
                ask_before_stop: true,
                taskbar_created,
                texts: if korean {
                    Texts::korean()
                } else {
                    Texts::english()
                },
                media_class,
                media: HashMap::new(),
                decoder,
                logger,
            })
        });
        slot.store(controller as isize, Ordering::SeqCst);
        let _ = ready.send(Ok(()));
        // SAFETY: the usual message loop of this thread's windows.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        slot.store(0, Ordering::SeqCst);
        STATE.with(|state| *state.borrow_mut() = None);
    }

    type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

    unsafe extern "system" fn media_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: the default handling of every message.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    unsafe extern "system" fn controller_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_COMMANDS => {
                apply_commands();
                0
            }
            WM_TRAY => {
                let event = (lparam & 0xffff) as u32;
                let id = ((lparam >> 16) & 0xffff) as u32;
                let package = with_state(|s| s.by_id.get(&id).cloned()).flatten();
                if let Some(package) = package {
                    match event {
                        NIN_SELECT | NIN_KEYSELECT => {
                            report(TrayReport::Command(package, BackgroundCommand::Open))
                        }
                        WM_CONTEXTMENU => {
                            let x = (wparam & 0xffff) as u16 as i16 as i32;
                            let y = ((wparam >> 16) & 0xffff) as u16 as i16 as i32;
                            show_menu(hwnd, &package, x, y);
                        }
                        _ => {}
                    }
                }
                0
            }
            WM_CLOSE => {
                let packages: Vec<String> =
                    with_state(|s| s.icons.keys().cloned().collect()).unwrap_or_default();
                for package in packages {
                    remove(&package);
                }
                let media: Vec<String> =
                    with_state(|s| s.media.keys().cloned().collect()).unwrap_or_default();
                for package in media {
                    media_gone(&package);
                }
                // SAFETY: the controller is this thread's.
                unsafe { DestroyWindow(hwnd) };
                0
            }
            WM_SETTINGCHANGE => {
                // The taskbar's theme changed: DSA's own icons are drawn again in its colour.
                let area = setting_name(lparam);
                if area == "ImmersiveColorSet" {
                    let apps: Vec<BackgroundApp> = with_state(|s| {
                        s.icons
                            .values()
                            .filter(|icon| icon.themed)
                            .map(|icon| icon.app.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                    for app in apps {
                        show(app);
                    }
                }
                // SAFETY: the default handling of a message of this window.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                0
            }
            _ => {
                let taskbar_created = with_state(|s| s.taskbar_created).unwrap_or(0);
                if taskbar_created != 0 && msg == taskbar_created {
                    // Explorer restarted, and the notification area with it: every icon is
                    // added again.
                    let apps: Vec<BackgroundApp> = with_state(|s| {
                        s.icons
                            .drain()
                            .map(|(_, icon)| {
                                // SAFETY: an icon this thread made.
                                unsafe { DestroyIcon(icon.icon) };
                                icon.app
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                    with_state(|s| s.by_id.clear());
                    for app in apps {
                        show(app);
                    }
                    return 0;
                }
                // SAFETY: the default handling of every other message.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
        }
    }

    fn report(report: TrayReport) {
        with_state(|s| {
            let _ = s.reports.send(report);
        });
    }

    fn apply_commands() {
        loop {
            let Some(Some(command)) = with_state(|s| s.commands.try_recv().ok()) else {
                return;
            };
            match command {
                TrayCommand::Show(app) => show(app),
                TrayCommand::Remove(package) => remove(&package),
                TrayCommand::Media(session) => media(session),
                TrayCommand::MediaGone(package) => media_gone(&package),
                TrayCommand::AskBeforeStop(ask) => {
                    with_state(|s| s.ask_before_stop = ask);
                }
            }
        }
    }

    /// The setting area a `WM_SETTINGCHANGE` names, or an empty string.
    fn setting_name(lparam: LPARAM) -> String {
        let text = lparam as *const u16;
        if text.is_null() {
            return String::new();
        }
        let mut length = 0;
        // SAFETY: for WM_SETTINGCHANGE the parameter is null or a NUL-terminated string,
        // read up to its NUL and never past 256 units.
        unsafe {
            while length < 256 && *text.add(length) != 0 {
                length += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
        }
    }

    fn copy_text(target: &mut [u16], text: &str) {
        let units: Vec<u16> = text.encode_utf16().take(target.len() - 1).collect();
        target[..units.len()].copy_from_slice(&units);
        target[units.len()] = 0;
    }

    fn icon_data(controller: HWND, icon: &Icon, package: &str) -> NOTIFYICONDATAW {
        // SAFETY: a plain C structure, all zero is its empty value.
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = controller;
        data.uID = icon.id;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = icon.icon;
        if icon.by_guid {
            data.uFlags |= NIF_GUID;
            data.guidItem = GUID::from_u128(tray_guid(package));
        }
        copy_text(&mut data.szTip, &tooltip(&icon.app));
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        data
    }

    /// Shows `app`'s icon, or changes the one shown.
    fn show(app: BackgroundApp) {
        let Some((controller, known)) = with_state(|s| {
            (
                s.controller,
                s.icons
                    .get(&app.package_name)
                    .map(|icon| (icon.id, icon.by_guid)),
            )
        }) else {
            return;
        };
        let (handle, themed) = make_icon(controller, &app.icon_png);
        let package = app.package_name.clone();
        if let Some((id, by_guid)) = known {
            let icon = Icon {
                id,
                app,
                icon: handle,
                themed,
                by_guid,
            };
            let data = icon_data(controller, &icon, &package);
            // SAFETY: a filled structure for an icon of this window.
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
            if let Some(old) = with_state(|s| s.icons.insert(package, icon)).flatten() {
                // SAFETY: an icon this thread made, which the notification area copied.
                unsafe { DestroyIcon(old.icon) };
            }
            return;
        }
        let id = with_state(|s| {
            let id = s.next_id;
            s.next_id = s.next_id.wrapping_add(1).max(1);
            id
        })
        .unwrap_or(1);
        let mut icon = Icon {
            id,
            app,
            icon: handle,
            themed,
            by_guid: true,
        };
        let mut data = icon_data(controller, &icon, &package);
        // SAFETY: filled structures for an icon of this window.
        let mut added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0;
        if !added {
            // An icon of this GUID left over from an earlier run is removed first.
            // SAFETY: as above.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
            added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0;
        }
        if !added {
            // Windows keeps a GUID for the program path it first saw it with; after DSA
            // moved, the icon goes by this window and its ID instead.
            icon.by_guid = false;
            data = icon_data(controller, &icon, &package);
            // SAFETY: as above.
            added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0;
        }
        if !added {
            with_state(|s| {
                note(
                    s.logger.as_deref(),
                    &format!(
                        "the notification area did not take the icon of {package}: {}",
                        std::io::Error::last_os_error()
                    ),
                )
            });
            // SAFETY: an icon this thread made, which nothing else holds.
            unsafe { DestroyIcon(icon.icon) };
            return;
        }
        // SAFETY: as above; asks for version 4 of the icon's messages.
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) };
        with_state(|s| {
            s.by_id.insert(id, package.clone());
            s.icons.insert(package, icon);
        });
    }

    /// Removes the package's icon.
    fn remove(package: &str) {
        let Some((controller, Some(icon))) =
            with_state(|s| (s.controller, s.icons.remove(package)))
        else {
            return;
        };
        with_state(|s| s.by_id.remove(&icon.id));
        let data = icon_data(controller, &icon, package);
        // SAFETY: the structure names an icon of this window.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &data);
            DestroyIcon(icon.icon);
        }
    }

    /// Whether the taskbar uses the light theme (Windows' "default app mode" is separate).
    fn taskbar_is_light() -> bool {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
        };
        let path = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let name = wide("SystemUsesLightTheme");
        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: NUL-terminated strings and a DWORD with its size, all outliving the call.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            )
        };
        status == 0 && value == 1
    }

    /// The PNG image of DSA's own notification area icon for the taskbar's theme: the
    /// smallest of its sizes that is at least `size` pixels, else its largest.
    fn dsa_tray_png(size: u32) -> Option<Vec<u8>> {
        let file = if taskbar_is_light() {
            TRAY_FOR_LIGHT_TASKBAR
        } else {
            TRAY_FOR_DARK_TASKBAR
        };
        let images = dsa_exe_resources::parse_ico(file).ok()?;
        let side = |image: &dsa_exe_resources::IconImage| match image.width {
            0 => 256,
            width => u32::from(width),
        };
        images
            .iter()
            .filter(|image| side(image) >= size)
            .min_by_key(|image| side(image))
            .or_else(|| images.iter().max_by_key(|image| side(image)))
            .map(|image| image.data.clone())
    }

    /// An icon of the notification area's size from the app's PNG, else DSA's own icon for
    /// the taskbar's theme, else the default application icon. The flag says whether it is
    /// DSA's own icon, which changes with the taskbar's theme.
    fn make_icon(controller: HWND, png: &[u8]) -> (HICON, bool) {
        // SAFETY: plain calls; the DPI of a window of this thread.
        let size = unsafe {
            let dpi = GetDpiForWindow(controller).max(96);
            GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16)
        } as u32;
        let decode = |png: &[u8]| {
            if !png.starts_with(b"\x89PNG") {
                return None;
            }
            with_state(|s| {
                s.decoder
                    .as_ref()
                    .map(|decoder| decoder.decode_straight(png, size, size))
            })
            .flatten()
            .and_then(Result::ok)
        };
        let (pixels, themed) = match decode(png) {
            Some(pixels) => (Some(pixels), false),
            None => (dsa_tray_png(size).and_then(|png| decode(&png)), true),
        };
        let fallback = || {
            // SAFETY: a system icon, which is never destroyed.
            (unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }, false)
        };
        let Some(pixels) = pixels else {
            return fallback();
        };
        // SAFETY: a DIB section of the given size, filled from `pixels`, and a mask bitmap,
        // both deleted once the icon has copied them.
        unsafe {
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let mut bits: *mut core::ffi::c_void = null_mut();
            let color =
                CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if color.is_null() || bits.is_null() {
                return fallback();
            }
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
            let mask = CreateBitmap(size as i32, size as i32, 1, 1, null());
            let info = ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            };
            let icon = CreateIconIndirect(&info);
            DeleteObject(color);
            DeleteObject(mask);
            if icon.is_null() {
                fallback()
            } else {
                (icon, themed)
            }
        }
    }

    /// Shows the icon's menu at (`x`, `y`) and acts on what the user picks.
    fn show_menu(hwnd: HWND, package: &str, x: i32, y: i32) {
        let Some(Some((app, texts))) = with_state(|s| {
            s.icons
                .get(package)
                .map(|icon| (icon.app.clone(), s.texts.clone()))
        }) else {
            return;
        };
        let entries = menu_entries(&app, &texts);
        // SAFETY: a menu made, shown and destroyed here, with NUL-terminated texts that
        // outlive the calls.
        let picked = unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            for entry in &entries {
                if entry.id == 0 {
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                } else {
                    let text = wide(&entry.text);
                    AppendMenuW(menu, MF_STRING, entry.id as usize, text.as_ptr());
                }
            }
            SetMenuDefaultItem(menu, super::MENU_OPEN, 0);
            let (x, y) = if x == 0 && y == 0 {
                let mut point = POINT { x: 0, y: 0 };
                GetCursorPos(&mut point);
                (point.x, point.y)
            } else {
                (x, y)
            };
            // The menu closes when the user clicks elsewhere only if its window is in front.
            SetForegroundWindow(hwnd);
            let picked = TrackPopupMenuEx(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                x,
                y,
                hwnd,
                null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            picked as u32
        };
        match chosen(picked) {
            Some(BackgroundCommand::Stop) => {
                if confirm_stop(hwnd, app_name(&app), &texts) {
                    report(TrayReport::Command(
                        package.to_string(),
                        BackgroundCommand::Stop,
                    ));
                }
            }
            Some(command) => report(TrayReport::Command(package.to_string(), command)),
            None => {}
        }
    }

    /// Asks before a stop, unless the user chose not to be asked. True to stop.
    fn confirm_stop(hwnd: HWND, name: &str, texts: &Texts) -> bool {
        if !with_state(|s| s.ask_before_stop).unwrap_or(true) {
            return true;
        }
        let template = stop_dialog_template(
            &texts.stop_question(name),
            texts,
            "Desktop Subsystem for Android",
        );
        // SAFETY: a modal dialog from a template that outlives it.
        let answer = unsafe {
            DialogBoxIndirectParamW(
                GetModuleHandleW(null()),
                template.as_ptr() as *const _,
                hwnd,
                Some(stop_dialog_proc),
                0,
            )
        };
        match answer {
            2 => {
                with_state(|s| s.ask_before_stop = false);
                report(TrayReport::NeverAskBeforeStop);
                true
            }
            1 => true,
            _ => false,
        }
    }

    unsafe extern "system" fn stop_dialog_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> isize {
        match msg {
            WM_INITDIALOG => {
                // SAFETY: brings the question in front of the window that asked it.
                unsafe { SetForegroundWindow(hwnd) };
                1
            }
            WM_COMMAND => {
                let id = (wparam & 0xffff) as u16;
                if id == STOP_ID {
                    // SAFETY: a control of this dialog.
                    let never =
                        unsafe { IsDlgButtonChecked(hwnd, DO_NOT_ASK_ID as i32) } == BST_CHECKED;
                    // SAFETY: ends this modal dialog.
                    unsafe { EndDialog(hwnd, if never { 2 } else { 1 }) };
                    1
                } else if id == CANCEL_ID {
                    // SAFETY: as above.
                    unsafe { EndDialog(hwnd, 0) };
                    1
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// Shows a media session in Windows' media controls, or changes it.
    fn media(session: MediaSession) {
        let package = session.package_name.clone();
        let existing = with_state(|s| s.media.contains_key(&package)).unwrap_or(false);
        if !existing {
            let Some((class, reports, logger)) =
                with_state(|s| (s.media_class.clone(), s.reports.clone(), s.logger.clone()))
            else {
                return;
            };
            match super::media::Controls::open(&class, &package, reports) {
                Ok(controls) => {
                    with_state(|s| s.media.insert(package.clone(), controls));
                }
                Err(why) => {
                    note(
                        logger.as_deref(),
                        &format!("the media keys do not reach {package}: {why}"),
                    );
                    return;
                }
            }
        }
        let failed = with_state(|s| {
            s.media
                .get(&package)
                .map(|controls| controls.update(&session).err())
        })
        .flatten()
        .flatten();
        if let Some(why) = failed {
            with_state(|s| {
                note(
                    s.logger.as_deref(),
                    &format!("the media controls of {package} did not change: {why}"),
                )
            });
        }
    }

    fn media_gone(package: &str) {
        if let Some(Some(controls)) = with_state(|s| s.media.remove(package)) {
            controls.close();
        }
    }
}

/// Windows' media controls for one media session: a hidden window that carries the app's
/// AppUserModelID, and its SystemMediaTransportControls.
#[cfg(windows)]
mod media {
    use std::ptr::{null, null_mut};
    use std::sync::mpsc::Sender;

    use dsa_host_core::background::{MediaCommand, MediaSession, Playback};
    use windows::core::{HSTRING, PWSTR};
    use windows::Foundation::TypedEventHandler;
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
    };
    use windows::Storage::Streams::{IRandomAccessStream, RandomAccessStreamReference};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::{
        PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::System::WinRT::{
        CreateRandomAccessStreamOverStream, ISystemMediaTransportControlsInterop, BSOS_DEFAULT,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreForWindow,
    };
    use windows::Win32::UI::Shell::SHCreateMemStream;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_POPUP};

    use super::TrayReport;

    /// `System.AppUserModel.ID`.
    const APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };

    pub(super) struct Controls {
        hwnd: windows_sys::Win32::Foundation::HWND,
        controls: SystemMediaTransportControls,
        token: i64,
    }

    fn hwnd(handle: windows_sys::Win32::Foundation::HWND) -> windows::Win32::Foundation::HWND {
        windows::Win32::Foundation::HWND(handle)
    }

    impl Controls {
        pub(super) fn open(
            class: &[u16],
            package: &str,
            reports: Sender<TrayReport>,
        ) -> Result<Controls, String> {
            // SAFETY: a hidden window of a class this thread registered, never shown.
            let handle = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    null_mut(),
                    null_mut(),
                    GetModuleHandleW(null()),
                    null(),
                )
            };
            if handle.is_null() {
                return Err(format!(
                    "no window for the media controls: {}",
                    std::io::Error::last_os_error()
                ));
            }
            match Self::attach(handle, package, reports) {
                Ok((controls, token)) => Ok(Controls {
                    hwnd: handle,
                    controls,
                    token,
                }),
                Err(why) => {
                    // SAFETY: the window made above.
                    unsafe { DestroyWindow(handle) };
                    Err(why)
                }
            }
        }

        fn attach(
            handle: windows_sys::Win32::Foundation::HWND,
            package: &str,
            reports: Sender<TrayReport>,
        ) -> Result<(SystemMediaTransportControls, i64), String> {
            let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
            let id: Vec<u16> = crate::start_menu::app_user_model_id(package)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: COM calls on this thread's window; the value borrows `id`, which
            // outlives it, and the store keeps a copy, so it is never cleared.
            unsafe {
                let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd(handle))
                    .map_err(|e| fail("the window's properties", e))?;
                let value = std::mem::ManuallyDrop::new(PROPVARIANT {
                    Anonymous: PROPVARIANT_0 {
                        Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
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
                    .map_err(|e| fail("the window's app ID", e))?;
            }
            let interop = windows::core::factory::<
                SystemMediaTransportControls,
                ISystemMediaTransportControlsInterop,
            >()
            .map_err(|e| fail("Windows has no media controls", e))?;
            // SAFETY: a top-level window of this thread.
            let controls: SystemMediaTransportControls =
                unsafe { interop.GetForWindow(hwnd(handle)) }
                    .map_err(|e| fail("the media controls of the window", e))?;
            let package = package.to_string();
            let token = controls
                .ButtonPressed(&TypedEventHandler::<
                    SystemMediaTransportControls,
                    SystemMediaTransportControlsButtonPressedEventArgs,
                >::new(move |_, args| {
                    let button = args.ok()?.Button()?;
                    let command = if button == SystemMediaTransportControlsButton::Play {
                        Some(MediaCommand::Play)
                    } else if button == SystemMediaTransportControlsButton::Pause {
                        Some(MediaCommand::Pause)
                    } else if button == SystemMediaTransportControlsButton::Next {
                        Some(MediaCommand::Next)
                    } else if button == SystemMediaTransportControlsButton::Previous {
                        Some(MediaCommand::Previous)
                    } else if button == SystemMediaTransportControlsButton::Stop {
                        Some(MediaCommand::Stop)
                    } else {
                        None
                    };
                    if let Some(command) = command {
                        let _ = reports.send(TrayReport::Media(package.clone(), command));
                    }
                    Ok(())
                }))
                .map_err(|e| fail("the media keys", e))?;
            controls
                .SetIsEnabled(true)
                .map_err(|e| fail("turning the media controls on", e))?;
            Ok((controls, token))
        }

        /// Shows the session's state and metadata.
        pub(super) fn update(&self, session: &MediaSession) -> Result<(), String> {
            let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
            let controls = &self.controls;
            controls
                .SetIsPlayEnabled(session.can_play || session.playback != Playback::Playing)
                .map_err(|e| fail("play", e))?;
            controls
                .SetIsPauseEnabled(session.can_pause || session.playback == Playback::Playing)
                .map_err(|e| fail("pause", e))?;
            controls
                .SetIsNextEnabled(session.can_next)
                .map_err(|e| fail("next", e))?;
            controls
                .SetIsPreviousEnabled(session.can_previous)
                .map_err(|e| fail("previous", e))?;
            controls
                .SetIsStopEnabled(true)
                .map_err(|e| fail("stop", e))?;
            controls
                .SetPlaybackStatus(match session.playback {
                    Playback::Playing => MediaPlaybackStatus::Playing,
                    Playback::Paused => MediaPlaybackStatus::Paused,
                    Playback::Stopped => MediaPlaybackStatus::Stopped,
                })
                .map_err(|e| fail("the playback state", e))?;
            let updater = controls
                .DisplayUpdater()
                .map_err(|e| fail("the display", e))?;
            updater
                .SetType(MediaPlaybackType::Music)
                .map_err(|e| fail("the display", e))?;
            let music = updater
                .MusicProperties()
                .map_err(|e| fail("the display", e))?;
            let title = if session.title.is_empty() {
                &session.app_label
            } else {
                &session.title
            };
            music
                .SetTitle(&HSTRING::from(title.as_str()))
                .map_err(|e| fail("the title", e))?;
            music
                .SetArtist(&HSTRING::from(session.artist.as_str()))
                .map_err(|e| fail("the artist", e))?;
            music
                .SetAlbumTitle(&HSTRING::from(session.album.as_str()))
                .map_err(|e| fail("the album", e))?;
            if session.art_png.starts_with(b"\x89PNG") {
                // SAFETY: an in-memory stream over a copy of the art, wrapped for WinRT.
                let art = unsafe {
                    SHCreateMemStream(Some(&session.art_png))
                        .ok_or_else(|| "no memory for the album art".to_string())
                        .and_then(|stream| {
                            let wrapped: windows::core::Result<IRandomAccessStream> =
                                CreateRandomAccessStreamOverStream(&stream, BSOS_DEFAULT);
                            wrapped.map_err(|e| fail("the album art", e))
                        })
                };
                let reference = art.and_then(|stream| {
                    RandomAccessStreamReference::CreateFromStream(&stream)
                        .map_err(|e| fail("the album art", e))
                })?;
                updater
                    .SetThumbnail(&reference)
                    .map_err(|e| fail("the album art", e))?;
            }
            updater.Update().map_err(|e| fail("the display", e))
        }

        /// Takes the session out of the media controls.
        pub(super) fn close(self) {
            let _ = self.controls.RemoveButtonPressed(self.token);
            let _ = self.controls.SetIsEnabled(false);
            let _ = self.controls.DisplayUpdater().and_then(|u| u.ClearAll());
            // SAFETY: the window this session made.
            unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn music(actions: &[&str]) -> BackgroundApp {
        BackgroundApp {
            package_name: "com.example.music".into(),
            app_label: "Music".into(),
            icon_png: Vec::new(),
            title: "Playing".into(),
            text: "Song - Artist".into(),
            actions: actions.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn fr24_each_app_has_its_own_icon_guid_and_keeps_it() {
        let music = tray_guid("com.example.music");
        assert_eq!(music, tray_guid("com.example.music"));
        assert_ne!(music, tray_guid("com.example.vpn"));
        // A version 5 layout: the version nibble and the variant bits.
        let bytes = music.to_be_bytes();
        assert_eq!(bytes[6] >> 4, 5);
        assert_eq!(bytes[8] >> 6, 0b10);
    }

    #[test]
    fn fr24_the_tooltip_is_the_name_title_and_text_cut_to_fit() {
        assert_eq!(tooltip(&music(&[])), "Music\nPlaying\nSong - Artist");
        let mut long = music(&[]);
        long.text = "\u{c74c}".repeat(300);
        let tip = tooltip(&long);
        assert!(tip.encode_utf16().count() <= MAX_TOOLTIP);
        assert!(tip.starts_with("Music\nPlaying\n"));
        let mut unnamed = music(&[]);
        unnamed.app_label = String::new();
        unnamed.title = String::new();
        unnamed.text = String::new();
        assert_eq!(tooltip(&unnamed), "com.example.music");
    }

    #[test]
    fn fr24_the_menu_is_open_the_actions_and_stop() {
        let texts = Texts::english();
        let entries = menu_entries(&music(&["Pause", "Next", "R&B"]), &texts);
        let ids: Vec<u32> = entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![MENU_OPEN, 0, 100, 101, 102, 0, MENU_STOP]);
        assert_eq!(entries[0].text, "Open");
        assert_eq!(entries[4].text, "R&&B");
        assert_eq!(entries[6].text, "Stop");
        let plain = menu_entries(&music(&[]), &texts);
        assert_eq!(
            plain.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![MENU_OPEN, 0, MENU_STOP]
        );
        assert_eq!(chosen(MENU_OPEN), Some(BackgroundCommand::Open));
        assert_eq!(chosen(101), Some(BackgroundCommand::Action(1)));
        assert_eq!(chosen(MENU_STOP), Some(BackgroundCommand::Stop));
        assert_eq!(chosen(0), None);
        assert_eq!(chosen(999), None);
    }

    #[test]
    fn fr24_the_stop_question_names_the_app_in_the_users_language() {
        assert!(Texts::english()
            .stop_question("Music")
            .starts_with("Stop Music?"));
        assert!(Texts::korean().stop_question("Music").starts_with("Music"));
        assert_ne!(Texts::korean().stop, Texts::english().stop);
    }

    #[test]
    fn fr24_the_stop_question_is_a_dialog_with_a_do_not_ask_again_box() {
        let texts = Texts::english();
        let template = stop_dialog_template("Stop Music?", &texts, "DSA");
        let bytes: Vec<u8> = template.iter().flat_map(|d| d.to_le_bytes()).collect();
        // Four controls.
        assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 4);
        let utf16: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let contains = |text: &str| {
            let needle: Vec<u16> = text.encode_utf16().collect();
            utf16.windows(needle.len()).any(|w| w == needle.as_slice())
        };
        assert!(contains("Stop Music?"));
        assert!(contains("Do not ask again"));
        assert!(contains("Cancel"));
        assert!(contains("Segoe UI"));
    }

    #[derive(Default)]
    struct Recorded {
        commands: Vec<TrayCommand>,
        reports: Vec<TrayReport>,
    }

    impl Tray for Arc<std::sync::Mutex<Recorded>> {
        fn command(&mut self, command: TrayCommand) {
            self.lock().unwrap().commands.push(command);
        }
        fn reports(&mut self) -> Vec<TrayReport> {
            std::mem::take(&mut self.lock().unwrap().reports)
        }
    }

    #[test]
    fn fr24_the_tray_follows_the_service_and_passes_a_stop_back() {
        use dsa_host_core::host_api::UserAndroid;
        use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
        use dsa_host_core::session::BackgroundRequest;
        use std::time::Instant;

        let (listener, connector) = dsa_transport::local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let _service = RunningService::start(Box::new(listener), androids.clone());
        let recorded = Arc::new(std::sync::Mutex::new(Recorded::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let syncing = {
            let mut tray = Arc::clone(&recorded);
            let stop = Arc::clone(&stop);
            thread::spawn(move || sync(Box::new(connector), &mut tray, stop, None))
        };
        // The watch is up once the service has made the caller's Android for it.
        let deadline = Instant::now() + Duration::from_secs(10);
        let android = loop {
            if let Some(android) = androids.androids.lock().unwrap().values().next().cloned() {
                break android;
            }
            assert!(Instant::now() < deadline, "the apps were never watched");
            thread::sleep(Duration::from_millis(10));
        };
        thread::sleep(Duration::from_millis(100));
        android.start();
        android.background_event(BackgroundEvent::App(music(&["Pause"])));
        let wait = |what: &str, done: &dyn Fn(&Recorded) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done(&recorded.lock().unwrap()) {
                assert!(Instant::now() < deadline, "{what}");
                thread::sleep(Duration::from_millis(10));
            }
        };
        wait("the app's icon was never shown", &|r| {
            r.commands.contains(&TrayCommand::Show(music(&["Pause"])))
        });
        recorded.lock().unwrap().reports.push(TrayReport::Command(
            "com.example.music".into(),
            BackgroundCommand::Stop,
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        while android.background_requests.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "the stop never reached Android");
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            *android.background_requests.lock().unwrap(),
            vec![BackgroundRequest::App(
                "com.example.music".into(),
                BackgroundCommand::Stop
            )]
        );
        android.background_event(BackgroundEvent::AppRemoved {
            package_name: "com.example.music".into(),
        });
        wait("the app's icon was never removed", &|r| {
            r.commands
                .contains(&TrayCommand::Remove("com.example.music".into()))
        });
        stop.store(true, Ordering::Relaxed);
        syncing.join().unwrap();
    }
}
