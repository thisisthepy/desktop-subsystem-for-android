//! The notifications of the user's Android, as Windows toast notifications.
//!
//! The window manager watches the notifications of the user's Android over the local API
//! and shows each one as a toast. A toast carries the AppUserModelID of the app that posted
//! it ([`app_user_model_id`]), the one its Start menu shortcut and its windows carry, so
//! Windows shows it under the app's own name and icon, and groups it with the app's taskbar
//! button. Each toast's tag comes from the notification's key ([`tag_of`]), so a new version
//! of a notification replaces its toast, and a notification removed in Android removes its
//! toast.
//!
//! A click on a toast opens the notification in Android. DSA is an unpackaged desktop
//! program, and Windows tells such a program of a click on its toast in one of two ways: a COM
//! server it registers and names in the shortcut (`ToastActivatorCLSID`), or a protocol the
//! toast launches. The protocol is used ([`PROTOCOL`]): Windows starts `dsa-wm.exe
//! --notification <uri>` ([`activate`]), which passes the click to the service and ends. It
//! needs no COM server running, so a click reaches Android even when the window manager has
//! restarted since the toast was shown, and it works for an app with no Start menu shortcut.
//! The click goes to the service, which wakes Android if it dozes before the app opens.
//!
//! A toast the user dismisses (`UserCanceled`) is dismissed in Android too; the service
//! leaves a notification its app made one the user cannot dismiss.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::notifications::{Notification, NotificationAction, NotificationEvent};
use dsa_transport::Connector;
use sha2::{Digest, Sha256};

use crate::compositor::log::{note, CompositorLogger};
pub use crate::start_menu::app_user_model_id;

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm notifications";

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long to wait before connecting again after the service could not be reached.
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch waits for the service before it looks for dismissals again.
const POLL: Duration = Duration::from_millis(100);

/// The protocol a toast launches when it is clicked.
pub const PROTOCOL: &str = "dsa-notification";

/// The argument that hands `dsa-wm` the URI of a clicked toast.
pub const ARGUMENT: &str = "--notification";

/// The group every toast of Android is in; its tag tells it apart.
pub const GROUP: &str = "android";

/// The tag of the toast of the notification with `key`. Windows takes tags of at most 64
/// characters, and Android's keys are longer, so the tag is a hash of the key.
pub fn tag_of(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Escapes text for an XML attribute or element.
fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 has no other control characters.
            c if c.is_control() && !matches!(c, '\n' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

/// Percent-encodes `text` for a URI, keeping only unreserved characters.
fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The URI a click on the toast of the notification with `key` launches.
pub fn activation_uri(key: &str) -> String {
    format!("{PROTOCOL}:open?key={}", percent_encode(key))
}

/// What a launched URI asks for, or `None` for a URI that is not one of ours or is damaged.
pub fn parse_activation(uri: &str) -> Option<NotificationAction> {
    let rest = uri.trim().trim_matches('"').strip_prefix(PROTOCOL)?;
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim_start_matches('/');
    let (verb, query) = rest.split_once('?')?;
    let key = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("key="))
        .and_then(percent_decode)?;
    if !dsa_host_core::notifications::valid_key(&key) {
        return None;
    }
    match verb.trim_end_matches('/') {
        "open" => Some(NotificationAction::Open { key }),
        _ => None,
    }
}

/// The toast's XML for `notification`, with its icon at `icon` when it has one. A
/// notification of low importance or below makes no sound, as in Android.
pub fn toast_xml(notification: &Notification, icon: Option<&Path>) -> String {
    let mut lines = String::new();
    let title = if notification.title.trim().is_empty() {
        &notification.app_label
    } else {
        &notification.title
    };
    lines.push_str(&format!("<text>{}</text>", xml(title)));
    if !notification.text.trim().is_empty() {
        lines.push_str(&format!("<text>{}</text>", xml(&notification.text)));
    }
    if let Some(icon) = icon {
        lines.push_str(&format!(
            "<image placement=\"appLogoOverride\" src=\"{}\"/>",
            xml(&file_uri(icon))
        ));
    }
    let audio = if quiet(notification) {
        "<audio silent=\"true\"/>"
    } else {
        ""
    };
    format!(
        "<toast launch=\"{}\" activationType=\"protocol\"><visual><binding template=\"ToastGeneric\">{lines}</binding></visual>{audio}</toast>",
        xml(&activation_uri(&notification.key))
    )
}

/// Whether the notification goes to the notification centre without popping up or making a
/// sound: one of low importance or below, as Android shows it.
pub fn quiet(notification: &Notification) -> bool {
    (0..=2).contains(&notification.importance) && !notification.high_priority
}

/// A `file:///` URI for a local path.
fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let encoded: String = text
        .split('/')
        .map(|part| {
            if part.ends_with(':') {
                part.to_string()
            } else {
                percent_encode(part)
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    format!("file:///{}", encoded.trim_start_matches('/'))
}

/// Where the toasts' icons go: `%LOCALAPPDATA%\DSA\notifications`.
pub fn default_icon_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("DSA").join("notifications"))
}

/// One toast to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// The AppUserModelID it is shown under, and the app's name for it.
    pub app_id: String,
    pub app_label: String,
    /// Its tag, which a new toast with the same tag replaces.
    pub tag: String,
    pub xml: String,
    /// Whether it goes to the notification centre without popping up.
    pub quiet: bool,
    /// The key of its notification.
    pub key: String,
}

/// Shows and removes toasts. The window manager's is Windows' notification platform
/// ([`WindowsToasts`]); a test's records what it is asked.
pub trait Toasts {
    /// Shows the toast, replacing the one with its tag. A toast the user dismisses is
    /// reported on `dismissed` with its notification's key.
    fn show(&mut self, toast: &Toast, dismissed: Sender<String>) -> Result<(), String>;
    /// Removes the toast `tag` of `app_id`.
    fn remove(&mut self, app_id: &str, tag: &str) -> Result<(), String>;
}

/// The toasts shown, by notification key.
struct Shown {
    app_id: String,
    tag: String,
    icon: Option<PathBuf>,
}

/// Shows the notifications of the user's Android until `stop`, reconnecting to the service
/// whenever the connection ends. Dismissals are passed back through new connections made
/// with `connector`.
pub fn sync(
    mut connector: Box<dyn Connector>,
    toasts: &mut dyn Toasts,
    icon_dir: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let (dismissed_tx, dismissed) = mpsc::channel();
    let mut shown: HashMap<String, Shown> = HashMap::new();
    let mut last_error = String::new();
    if let Some(dir) = &icon_dir {
        // Icons of an earlier run's toasts; those toasts no longer act on anything.
        let _ = std::fs::remove_dir_all(dir);
    }
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_notifications());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    note(
                        logger.as_deref(),
                        &format!("notifications cannot reach the service: {error}"),
                    );
                    last_error = error;
                }
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        while !stop.load(Ordering::Relaxed) {
            dismissals(&mut *connector, &dismissed, logger.as_deref());
            match stream.next(POLL) {
                Ok(Some(event)) => apply(
                    event,
                    toasts,
                    &mut shown,
                    icon_dir.as_deref(),
                    &dismissed_tx,
                    logger.as_deref(),
                ),
                Ok(None) => {}
                Err(error) => {
                    note(
                        logger.as_deref(),
                        &format!("the notifications' connection to the service ended: {error}"),
                    );
                    break;
                }
            }
        }
    }
}

/// Passes the dismissals the user made to the service.
fn dismissals(
    connector: &mut dyn Connector,
    dismissed: &Receiver<String>,
    logger: Option<&CompositorLogger>,
) {
    while let Ok(key) = dismissed.try_recv() {
        let result = HostApiClient::connect(connector, BUILD, ANSWER).and_then(|mut client| {
            client.act_on_notification(&NotificationAction::Dismiss { key })
        });
        if let Err(error) = result {
            note(
                logger,
                &format!("a dismissed notification did not reach Android: {error}"),
            );
        }
    }
}

/// Shows or removes a toast for one change to the notifications.
fn apply(
    event: NotificationEvent,
    toasts: &mut dyn Toasts,
    shown: &mut HashMap<String, Shown>,
    icon_dir: Option<&Path>,
    dismissed: &Sender<String>,
    logger: Option<&CompositorLogger>,
) {
    match event {
        NotificationEvent::Posted(notification) => {
            let app_id = app_user_model_id(&notification.package_name);
            let tag = tag_of(&notification.key);
            let icon = icon_dir.and_then(|dir| write_icon(dir, &tag, &notification.icon_png));
            let xml = toast_xml(&notification, icon.as_deref());
            let label = if notification.app_label.trim().is_empty() {
                notification.package_name.clone()
            } else {
                notification.app_label.clone()
            };
            let toast = Toast {
                app_id: app_id.clone(),
                app_label: label,
                tag: tag.clone(),
                xml,
                quiet: quiet(&notification),
                key: notification.key.clone(),
            };
            if let Err(why) = toasts.show(&toast, dismissed.clone()) {
                note(
                    logger,
                    &format!(
                        "a notification of {} was not shown: {why}",
                        notification.package_name
                    ),
                );
            }
            if let Some(old) = shown.insert(notification.key, Shown { app_id, tag, icon }) {
                if let Some(old_icon) = old.icon {
                    if !shown.values().any(|s| s.icon.as_ref() == Some(&old_icon)) {
                        let _ = std::fs::remove_file(old_icon);
                    }
                }
            }
        }
        NotificationEvent::Removed { key } => {
            let Some(gone) = shown.remove(&key) else {
                return;
            };
            if let Err(why) = toasts.remove(&gone.app_id, &gone.tag) {
                note(
                    logger,
                    &format!("a removed notification's toast stayed: {why}"),
                );
            }
            if let Some(icon) = gone.icon {
                let _ = std::fs::remove_file(icon);
            }
        }
    }
}

/// Writes a toast's icon, which Windows reads from a file. `None` without one or when it
/// cannot be written; the toast then shows the app's icon.
fn write_icon(dir: &Path, tag: &str, png: &[u8]) -> Option<PathBuf> {
    if png.is_empty() || !png.starts_with(b"\x89PNG") {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{tag}.png"));
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// Handles `dsa-wm --notification <uri>`, which Windows runs when a toast is clicked: passes
/// the click to the service through `connector`. Returns what went wrong, if anything.
pub fn activate(uri: &str, connector: &mut dyn Connector) -> Result<(), String> {
    let action =
        parse_activation(uri).ok_or_else(|| format!("{uri:?} is not a DSA notification"))?;
    // The app's window comes from the window manager, which may bring it to the front: the
    // click gave this process that right, which it passes on.
    #[cfg(windows)]
    // SAFETY: a plain call with no pointers.
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
    let mut client = HostApiClient::connect(connector, "dsa-wm notification click", ANSWER)
        .map_err(|e| e.to_string())?;
    client
        .act_on_notification(&action)
        .map_err(|e| e.to_string())
}

/// Registers the protocol a click on a toast launches, for the current user, to run
/// `program` (this `dsa-wm.exe`). Done each time the window manager starts, so it follows the
/// program when DSA moves.
pub fn register_protocol(program: &Path) -> Result<(), String> {
    let base = format!("Software\\Classes\\{PROTOCOL}");
    registry::set(&base, "", "URL:Desktop Subsystem for Android notification")?;
    registry::set(&base, "URL Protocol", "")?;
    registry::set(
        &format!("{base}\\shell\\open\\command"),
        "",
        &format!("\"{}\" {ARGUMENT} \"%1\"", program.display()),
    )
}

/// The current user's registry, as far as the toasts need it.
mod registry {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Sets the string `name` (empty for the key's default) of `HKCU\<path>` to `value`.
    pub fn set(path: &str, name: &str, value: &str) -> Result<(), String> {
        let path_w = wide(path);
        let name_w = wide(name);
        let value_w = wide(value);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: NUL-terminated strings that outlive the calls, and a key closed once.
        unsafe {
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                path_w.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            );
            if status != ERROR_SUCCESS {
                return Err(format!("could not open HKCU\\{path} (error {status})"));
            }
            let status = RegSetValueExW(
                key,
                name_w.as_ptr(),
                0,
                REG_SZ,
                value_w.as_ptr() as *const u8,
                (value_w.len() * 2) as u32,
            );
            RegCloseKey(key);
            if status != ERROR_SUCCESS {
                return Err(format!("could not set HKCU\\{path} (error {status})"));
            }
        }
        Ok(())
    }
}

/// Toasts through Windows' notification platform.
///
/// An unpackaged program's toast shows only for an AppUserModelID Windows knows: one a Start
/// menu shortcut carries, which every app a launcher lists has, or one registered under
/// `HKCU\Software\Classes\AppUserModelId`. The second is written for each app that posts a
/// notification, with the app's name, so an app with no shortcut still gets its toasts.
pub struct WindowsToasts {
    /// The toasts shown, kept so Windows can call their dismissal handlers.
    live: HashMap<(String, String), windows::UI::Notifications::ToastNotification>,
    registered: std::collections::HashSet<String>,
}

impl WindowsToasts {
    pub fn new() -> WindowsToasts {
        WindowsToasts {
            live: HashMap::new(),
            registered: std::collections::HashSet::new(),
        }
    }
}

impl Default for WindowsToasts {
    fn default() -> Self {
        WindowsToasts::new()
    }
}

impl Toasts for WindowsToasts {
    fn show(&mut self, wanted: &Toast, dismissed: Sender<String>) -> Result<(), String> {
        use windows::core::HSTRING;
        use windows::Data::Xml::Dom::XmlDocument;
        use windows::Foundation::TypedEventHandler;
        use windows::UI::Notifications::{
            ToastDismissalReason, ToastDismissedEventArgs, ToastNotification,
            ToastNotificationManager,
        };

        let Toast {
            app_id,
            app_label,
            tag,
            xml,
            quiet,
            key,
        } = wanted;
        if self.registered.insert(app_id.to_string()) {
            let path = format!("Software\\Classes\\AppUserModelId\\{app_id}");
            if let Err(why) = registry::set(&path, "DisplayName", app_label) {
                self.registered.remove(app_id.as_str());
                return Err(why);
            }
        }
        let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
        let document = XmlDocument::new().map_err(|e| fail("making the toast", e))?;
        document
            .LoadXml(&HSTRING::from(xml.as_str()))
            .map_err(|e| fail("reading the toast", e))?;
        let toast = ToastNotification::CreateToastNotification(&document)
            .map_err(|e| fail("making the toast", e))?;
        toast
            .SetTag(&HSTRING::from(tag.as_str()))
            .map_err(|e| fail("tagging the toast", e))?;
        toast
            .SetGroup(&HSTRING::from(GROUP))
            .map_err(|e| fail("tagging the toast", e))?;
        if *quiet {
            toast
                .SetSuppressPopup(true)
                .map_err(|e| fail("quieting the toast", e))?;
        }
        let key = key.clone();
        toast
            .Dismissed(&TypedEventHandler::<
                ToastNotification,
                ToastDismissedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    if args.Reason()? == ToastDismissalReason::UserCanceled {
                        let _ = dismissed.send(key.clone());
                    }
                }
                Ok(())
            }))
            .map_err(|e| fail("following the toast", e))?;
        let notifier =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id.as_str()))
                .map_err(|e| fail("reaching the notification centre", e))?;
        notifier
            .Show(&toast)
            .map_err(|e| fail("showing the toast", e))?;
        self.live.insert((app_id.clone(), tag.clone()), toast);
        Ok(())
    }

    fn remove(&mut self, app_id: &str, tag: &str) -> Result<(), String> {
        use windows::core::HSTRING;
        use windows::UI::Notifications::ToastNotificationManager;
        self.live.remove(&(app_id.to_string(), tag.to_string()));
        ToastNotificationManager::History()
            .and_then(|history| {
                history.RemoveGroupedTagWithId(
                    &HSTRING::from(tag),
                    &HSTRING::from(GROUP),
                    &HSTRING::from(app_id),
                )
            })
            .map_err(|e| format!("removing the toast: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(key: &str) -> Notification {
        Notification {
            key: key.into(),
            package_name: "com.example.chat".into(),
            app_label: "Chat".into(),
            title: "Ann & <Bob>".into(),
            text: "see you \"at\" noon".into(),
            icon_png: b"\x89PNG....".to_vec(),
            importance: 4,
            high_priority: true,
            clearable: true,
            when_millis: 0,
        }
    }

    const KEY: &str = "0|com.example.chat|1|null|10100";

    #[test]
    fn fr8_a_toast_carries_the_title_body_icon_and_click() {
        let xml = toast_xml(&chat(KEY), Some(Path::new("C:\\Users\\Ann Lee\\a.png")));
        assert!(xml.contains("<text>Ann &amp; &lt;Bob&gt;</text>"), "{xml}");
        assert!(
            xml.contains("<text>see you &quot;at&quot; noon</text>"),
            "{xml}"
        );
        assert!(
            xml.contains("src=\"file:///C:/Users/Ann%20Lee/a.png\""),
            "{xml}"
        );
        assert!(xml.contains("activationType=\"protocol\""));
        assert!(xml.contains(&format!("launch=\"{}\"", xml_escape_uri(KEY))));
        assert!(!xml.contains("silent"));
    }

    fn xml_escape_uri(key: &str) -> String {
        xml(&activation_uri(key))
    }

    #[test]
    fn fr8_a_low_importance_notification_is_quiet_and_a_titleless_one_shows_the_app() {
        let low = Notification {
            importance: 2,
            high_priority: false,
            title: String::new(),
            ..chat(KEY)
        };
        assert!(quiet(&low));
        let xml = toast_xml(&low, None);
        assert!(xml.contains("<audio silent=\"true\"/>"));
        assert!(xml.contains("<text>Chat</text>"));
        assert!(!xml.contains("<image"));
        assert!(!quiet(&chat(KEY)));
    }

    #[test]
    fn fr8_a_click_uri_names_the_notification_it_opens() {
        let uri = activation_uri(KEY);
        assert!(uri.starts_with("dsa-notification:open?key="));
        assert!(!uri.contains('|'));
        assert_eq!(
            parse_activation(&uri),
            Some(NotificationAction::Open { key: KEY.into() })
        );
        // As Windows passes it on the command line, quoted.
        assert_eq!(
            parse_activation(&format!("\"{uri}\"")),
            Some(NotificationAction::Open { key: KEY.into() })
        );
        let korean = "0|com.example|1|\u{c548}\u{b155}|10100";
        assert_eq!(
            parse_activation(&activation_uri(korean)),
            Some(NotificationAction::Open { key: korean.into() })
        );
    }

    #[test]
    fn nfr5_a_click_uri_that_is_not_ours_or_damaged_is_refused() {
        for uri in [
            "",
            "https://example.com/?key=a",
            "dsa-notification:open",
            "dsa-notification:open?key=",
            "dsa-notification:open?key=%zz",
            "dsa-notification:open?key=%e",
            "dsa-notification:delete?key=a",
            "dsa-notification:open?key=%FF%FE",
        ] {
            assert_eq!(parse_activation(uri), None, "{uri}");
        }
    }

    #[test]
    fn fr8_a_tag_is_short_and_follows_the_key() {
        let tag = tag_of(KEY);
        assert_eq!(tag.len(), 32);
        assert_eq!(tag, tag_of(KEY));
        assert_ne!(tag, tag_of("0|com.example.chat|2|null|10100"));
    }

    #[derive(Default)]
    struct Recorded {
        shown: Vec<(String, String, bool)>,
        removed: Vec<(String, String)>,
        dismiss: Vec<(String, Sender<String>)>,
    }

    impl Toasts for Recorded {
        fn show(&mut self, toast: &Toast, dismissed: Sender<String>) -> Result<(), String> {
            self.shown
                .push((toast.app_id.clone(), toast.tag.clone(), toast.quiet));
            self.dismiss.push((toast.key.clone(), dismissed));
            Ok(())
        }
        fn remove(&mut self, app_id: &str, tag: &str) -> Result<(), String> {
            self.removed.push((app_id.into(), tag.into()));
            Ok(())
        }
    }

    #[test]
    fn fr8_posting_updating_and_removing_show_replace_and_remove_the_toast() {
        let dir = std::env::temp_dir().join(format!("dsa-toasts-{}", std::process::id()));
        let mut toasts = Recorded::default();
        let mut shown = HashMap::new();
        let (tx, _rx) = mpsc::channel();
        apply(
            NotificationEvent::Posted(chat(KEY)),
            &mut toasts,
            &mut shown,
            Some(&dir),
            &tx,
            None,
        );
        apply(
            NotificationEvent::Posted(chat(KEY)),
            &mut toasts,
            &mut shown,
            Some(&dir),
            &tx,
            None,
        );
        let id = app_user_model_id("com.example.chat");
        assert_eq!(toasts.shown, vec![(id.clone(), tag_of(KEY), false); 2]);
        assert!(dir.join(format!("{}.png", tag_of(KEY))).exists());
        apply(
            NotificationEvent::Removed { key: KEY.into() },
            &mut toasts,
            &mut shown,
            Some(&dir),
            &tx,
            None,
        );
        assert_eq!(toasts.removed, vec![(id, tag_of(KEY))]);
        assert!(!dir.join(format!("{}.png", tag_of(KEY))).exists());
        // A removal of one never shown does nothing.
        apply(
            NotificationEvent::Removed {
                key: "other".into(),
            },
            &mut toasts,
            &mut shown,
            Some(&dir),
            &tx,
            None,
        );
        assert_eq!(toasts.removed.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nfr5_an_icon_that_is_not_a_png_is_not_written() {
        let dir = std::env::temp_dir().join(format!("dsa-toast-icons-{}", std::process::id()));
        assert_eq!(write_icon(&dir, "t", b"GIF89a"), None);
        assert_eq!(write_icon(&dir, "t", b""), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
