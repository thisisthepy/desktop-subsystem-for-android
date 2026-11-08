//! Which ABI an app's native code is installed for: x86_64 first, arm64-v8a only when it has
//! to be.
//!
//! Translated ARM code is slower and takes more memory than code made for the guest, so an app
//! that has x86_64 code runs it. The guest's ABI list keeps x86_64 first even while a
//! translator is in place, which is enough for an APK whose x86_64 libraries are complete. An
//! APK whose x86_64 folder leaves out some of the libraries its arm64-v8a folder has would be
//! installed as x86_64 by that rule, and would end the moment it loads a library that is not
//! there. So before the host installs an APK, it compares the library names under `lib/` by
//! ABI ([`NativeLibraries`]), across the APK and its splits together, and has the package
//! installer take arm64-v8a ([`choose`]) when x86_64 misses some and translation is on.
//!
//! The same comparison decides whether the "Run as ARM" switch of an installed app can be
//! changed ([`run_as_arm`]): the app's APKs on the device must hold its arm64 libraries, and
//! translation must be on.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Read, Seek};

use dsa_protocol::control;
use dsa_protocol::host_api::types;

use crate::apk_info::zip::Archive;

/// The guest's own ABI.
pub const X86_64: &str = "x86_64";
/// The ABI a translator runs.
pub const ARM64: &str = "arm64-v8a";

/// The most ABIs kept of one app. Android knows six.
pub const MAX_ABIS: usize = 8;
/// The most library names kept of one ABI. Real apps have a few dozen.
pub const MAX_LIBRARIES: usize = 2048;
/// The longest library file name kept, in bytes.
pub const MAX_NAME_BYTES: usize = 255;
/// The longest ABI name kept, in bytes.
const MAX_ABI_BYTES: usize = 32;
/// How long the service waits for Android to install an app again with another ABI: it
/// copies and verifies every APK of the app, which for a large game takes minutes.
pub const REINSTALL_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// The native libraries of an app, by ABI: the file names under `lib/<abi>/` of its APK, or of
/// its APK and its splits together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeLibraries {
    by_abi: BTreeMap<String, BTreeSet<String>>,
}

/// Whether `abi` looks like an ABI name: lowercase letters, digits, `_` and `-`.
fn valid_abi(abi: &str) -> bool {
    !abi.is_empty()
        && abi.len() <= MAX_ABI_BYTES
        && abi
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

/// `abi` as the host keeps an ABI name the guest sent: itself when it looks like one, else
/// empty.
pub fn checked_abi(abi: &str) -> String {
    if valid_abi(abi) {
        abi.to_string()
    } else {
        String::new()
    }
}

/// Whether `name` is a library file name: a `.so` file, without a folder or a control
/// character.
fn valid_library(name: &str) -> bool {
    name.len() > 3
        && name.len() <= MAX_NAME_BYTES
        && name.ends_with(".so")
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

impl NativeLibraries {
    pub fn new() -> NativeLibraries {
        NativeLibraries::default()
    }

    /// Adds `name` as a library of `abi`. A name or an ABI that is not one, or one past the
    /// limits, is left out.
    pub fn add(&mut self, abi: &str, name: &str) {
        if !valid_abi(abi) || !valid_library(name) {
            return;
        }
        if !self.by_abi.contains_key(abi) && self.by_abi.len() >= MAX_ABIS {
            return;
        }
        let names = self.by_abi.entry(abi.to_string()).or_default();
        if names.len() < MAX_LIBRARIES {
            names.insert(name.to_string());
        }
    }

    /// Adds the archive entry `entry` when it is a native library: `lib/<abi>/<name>.so`.
    pub fn add_entry(&mut self, entry: &str) {
        let Some(rest) = entry.strip_prefix("lib/") else {
            return;
        };
        if let Some((abi, name)) = rest.split_once('/') {
            self.add(abi, name);
        }
    }

    /// The native libraries of the APK `reader` holds.
    pub fn read<R: Read + Seek>(reader: R) -> Result<NativeLibraries, String> {
        let archive = Archive::open(reader)?;
        let mut libraries = NativeLibraries::new();
        for entry in &archive.entries {
            libraries.add_entry(&entry.name);
        }
        Ok(libraries)
    }

    /// The native libraries of an APK and its splits together.
    pub fn merged<'a>(apks: impl IntoIterator<Item = &'a NativeLibraries>) -> NativeLibraries {
        let mut all = NativeLibraries::new();
        for apk in apks {
            for (abi, names) in &apk.by_abi {
                for name in names {
                    all.add(abi, name);
                }
            }
        }
        all
    }

    /// The library names of `abi`, if the app has any.
    pub fn names(&self, abi: &str) -> Option<&BTreeSet<String>> {
        self.by_abi.get(abi).filter(|names| !names.is_empty())
    }

    /// Whether the app has native libraries for `abi`.
    pub fn has(&self, abi: &str) -> bool {
        self.names(abi).is_some()
    }

    /// Whether the app has no native library at all.
    pub fn is_empty(&self) -> bool {
        self.by_abi.values().all(BTreeSet::is_empty)
    }

    /// The ABIs the app has native libraries for, in name order.
    pub fn abis(&self) -> impl Iterator<Item = &str> {
        self.by_abi
            .iter()
            .filter(|(_, names)| !names.is_empty())
            .map(|(abi, _)| abi.as_str())
    }

    /// Whether every library of `abi` has one of the same name in `other`.
    pub fn covers(&self, other: &str, abi: &str) -> bool {
        match (self.names(other), self.names(abi)) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(have), Some(need)) => need.is_subset(have),
        }
    }

    pub fn from_control(libraries: &[control::NativeLibraries]) -> NativeLibraries {
        let mut all = NativeLibraries::new();
        for one in libraries {
            for name in &one.names {
                all.add(&one.abi, name);
            }
        }
        all
    }

    pub fn from_wire(libraries: &[types::NativeLibraries]) -> NativeLibraries {
        let mut all = NativeLibraries::new();
        for one in libraries {
            for name in &one.names {
                all.add(&one.abi, name);
            }
        }
        all
    }

    pub fn to_wire(&self) -> Vec<types::NativeLibraries> {
        self.by_abi
            .iter()
            .filter(|(_, names)| !names.is_empty())
            .map(|(abi, names)| types::NativeLibraries {
                abi: abi.clone(),
                names: names.iter().cloned().collect(),
            })
            .collect()
    }
}

/// The ABI the package installer is told to install an app with `libraries` for, or `None`
/// to leave it to the package manager. `translation` says whether the guest's ABI list holds
/// arm64-v8a this boot; it is asked only when the answer depends on it.
///
/// - An app with x86_64 libraries that include every arm64-v8a one is installed as x86_64.
/// - An app whose x86_64 libraries leave out some arm64-v8a ones, or that has only arm64-v8a
///   libraries, is installed as arm64-v8a while translation is on. With translation off it is
///   never arm64-v8a: the partial x86_64 code is installed, or Android says the app does not
///   run here.
/// - An app without native libraries, or with none for either ABI, is left to Android.
pub fn choose(
    libraries: &NativeLibraries,
    translation: impl FnOnce() -> bool,
) -> Option<&'static str> {
    let x86 = libraries.has(X86_64);
    if !libraries.has(ARM64) {
        return x86.then_some(X86_64);
    }
    if libraries.covers(X86_64, ARM64) {
        return Some(X86_64);
    }
    if translation() {
        Some(ARM64)
    } else {
        x86.then_some(X86_64)
    }
}

/// Whether `abilist`, the guest's `ro.product.cpu.abilist`, holds arm64-v8a, which it does
/// only while a translator is in place.
pub fn translates_arm64(abilist: &str) -> bool {
    abilist.split(',').any(|abi| abi.trim() == ARM64)
}

/// Why the "Run as ARM" switch of an app cannot be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAsArmUnavailable {
    /// The app is part of the Android image, which is not installed again.
    SystemApp,
    /// No APK of the app on the device holds its arm64-v8a libraries.
    NoArmLibraries,
    /// ARM translation is not on in the running Android.
    TranslationOff,
    /// The app runs as ARM and has no complete x86_64 code to go back to.
    ArmOnly,
}

impl RunAsArmUnavailable {
    /// A stable name for programs, such as the settings apps.
    pub fn code(self) -> &'static str {
        match self {
            RunAsArmUnavailable::SystemApp => "system-app",
            RunAsArmUnavailable::NoArmLibraries => "no-arm-libraries",
            RunAsArmUnavailable::TranslationOff => "translation-off",
            RunAsArmUnavailable::ArmOnly => "arm-only",
        }
    }

    /// What it means, as a sentence.
    pub fn describe(self) -> &'static str {
        match self {
            RunAsArmUnavailable::SystemApp => "The app is part of Android",
            RunAsArmUnavailable::NoArmLibraries => {
                "No APK of the app on the device holds its ARM code"
            }
            RunAsArmUnavailable::TranslationOff => {
                "ARM translation is not on; turn it on under ARM apps and restart Android"
            }
            RunAsArmUnavailable::ArmOnly => "The app has only ARM code, so it runs as ARM",
        }
    }
}

/// The "Run as ARM" switch of an installed app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunAsArm {
    /// The app's native code runs as arm64-v8a now.
    pub on: bool,
    /// Why the switch cannot be changed; `None` when it can.
    pub unavailable: Option<RunAsArmUnavailable>,
}

impl RunAsArm {
    pub fn available(&self) -> bool {
        self.unavailable.is_none()
    }
}

/// The switch of an app that runs as `primary_abi` with `libraries` on the device, while
/// translation is on (`translation`) or not.
pub fn run_as_arm(
    system_app: bool,
    primary_abi: &str,
    libraries: &NativeLibraries,
    translation: bool,
) -> RunAsArm {
    let on = primary_abi == ARM64;
    let unavailable = if system_app {
        Some(RunAsArmUnavailable::SystemApp)
    } else if !libraries.has(ARM64) {
        Some(RunAsArmUnavailable::NoArmLibraries)
    } else if !translation {
        Some(RunAsArmUnavailable::TranslationOff)
    } else if on && !libraries.covers(X86_64, ARM64) {
        Some(RunAsArmUnavailable::ArmOnly)
    } else {
        None
    };
    RunAsArm { on, unavailable }
}

/// How installing an app again with another ABI went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAsArmStatus {
    Done,
    /// Android cannot change the ABI and keep the app's data; nothing changed.
    DataWouldBeErased,
    NotInstalled,
    SystemApp,
    /// The app's APKs on the device hold no native libraries for that ABI.
    NoLibraries,
    /// ARM translation is not on in the running Android.
    TranslationOff,
    Other,
}

impl RunAsArmStatus {
    /// A stable name for programs, such as the settings apps.
    pub fn code(self) -> &'static str {
        match self {
            RunAsArmStatus::Done => "done",
            RunAsArmStatus::DataWouldBeErased => "data-would-be-erased",
            RunAsArmStatus::NotInstalled => "not-installed",
            RunAsArmStatus::SystemApp => "system-app",
            RunAsArmStatus::NoLibraries => "no-libraries",
            RunAsArmStatus::TranslationOff => "translation-off",
            RunAsArmStatus::Other => "other",
        }
    }

    /// What the status means, as a sentence.
    pub fn describe(self) -> &'static str {
        match self {
            RunAsArmStatus::Done => "The app is installed again",
            RunAsArmStatus::DataWouldBeErased => {
                "Android cannot change how the app runs and keep its data; installing it again \
                 erases its data"
            }
            RunAsArmStatus::NotInstalled => "No app of this package is installed in Android",
            RunAsArmStatus::SystemApp => "The app is part of Android and is not installed again",
            RunAsArmStatus::NoLibraries => "The app's APKs on the device hold no code for that ABI",
            RunAsArmStatus::TranslationOff => {
                "ARM translation is not on; turn it on under ARM apps and restart Android"
            }
            RunAsArmStatus::Other => "Android did not install the app again",
        }
    }

    /// The status a control message carries. An unknown or missing one is `Other`.
    pub fn from_control(status: i32) -> RunAsArmStatus {
        match control::ReinstallStatus::try_from(status) {
            Ok(control::ReinstallStatus::Success) => RunAsArmStatus::Done,
            Ok(control::ReinstallStatus::DataWouldBeErased) => RunAsArmStatus::DataWouldBeErased,
            Ok(control::ReinstallStatus::NotInstalled) => RunAsArmStatus::NotInstalled,
            Ok(control::ReinstallStatus::SystemApp) => RunAsArmStatus::SystemApp,
            Ok(control::ReinstallStatus::NoLibraries) => RunAsArmStatus::NoLibraries,
            Ok(control::ReinstallStatus::Other)
            | Ok(control::ReinstallStatus::Unspecified)
            | Err(_) => RunAsArmStatus::Other,
        }
    }

    pub fn to_wire(self) -> types::RunAsArmStatus {
        match self {
            RunAsArmStatus::Done => types::RunAsArmStatus::Success,
            RunAsArmStatus::DataWouldBeErased => types::RunAsArmStatus::DataWouldBeErased,
            RunAsArmStatus::NotInstalled => types::RunAsArmStatus::NotInstalled,
            RunAsArmStatus::SystemApp => types::RunAsArmStatus::SystemApp,
            RunAsArmStatus::NoLibraries => types::RunAsArmStatus::NoLibraries,
            RunAsArmStatus::TranslationOff => types::RunAsArmStatus::TranslationOff,
            RunAsArmStatus::Other => types::RunAsArmStatus::Other,
        }
    }

    /// The status the local API carries. An unknown or missing one is `Other`.
    pub fn from_wire(status: i32) -> RunAsArmStatus {
        match types::RunAsArmStatus::try_from(status) {
            Ok(types::RunAsArmStatus::Success) => RunAsArmStatus::Done,
            Ok(types::RunAsArmStatus::DataWouldBeErased) => RunAsArmStatus::DataWouldBeErased,
            Ok(types::RunAsArmStatus::NotInstalled) => RunAsArmStatus::NotInstalled,
            Ok(types::RunAsArmStatus::SystemApp) => RunAsArmStatus::SystemApp,
            Ok(types::RunAsArmStatus::NoLibraries) => RunAsArmStatus::NoLibraries,
            Ok(types::RunAsArmStatus::TranslationOff) => RunAsArmStatus::TranslationOff,
            Ok(types::RunAsArmStatus::Other) | Ok(types::RunAsArmStatus::Unspecified) | Err(_) => {
                RunAsArmStatus::Other
            }
        }
    }
}

/// The longest message from Android that is passed on.
const MAX_MESSAGE: usize = 512;

/// How a change of the "Run as ARM" switch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAsArmOutcome {
    pub package_name: String,
    pub status: RunAsArmStatus,
    /// Android's own words, for the log and a bug report.
    pub message: String,
    /// Whether the app's native code runs as arm64-v8a now.
    pub on: bool,
}

impl RunAsArmOutcome {
    pub fn done(&self) -> bool {
        self.status == RunAsArmStatus::Done
    }

    pub fn from_control(result: &control::ReinstallResult) -> RunAsArmOutcome {
        RunAsArmOutcome {
            package_name: result.package_name.clone(),
            status: RunAsArmStatus::from_control(result.status),
            message: result.message.chars().take(MAX_MESSAGE).collect(),
            on: result.primary_abi == ARM64,
        }
    }

    pub fn to_wire(&self) -> types::RunAsArmResult {
        types::RunAsArmResult {
            package_name: self.package_name.clone(),
            status: self.status.to_wire() as i32,
            message: self.message.clone(),
            on: self.on,
        }
    }

    pub fn from_wire(result: types::RunAsArmResult) -> RunAsArmOutcome {
        RunAsArmOutcome {
            package_name: result.package_name,
            status: RunAsArmStatus::from_wire(result.status),
            message: result.message.chars().take(MAX_MESSAGE).collect(),
            on: result.on,
        }
    }
}

impl fmt::Display for RunAsArmOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.status.describe())?;
        if !self.done() && !self.message.trim().is_empty() {
            write!(f, " ({})", self.message.trim())?;
        }
        Ok(())
    }
}

/// What the keys of the notifications about running an app as ARM start with.
pub const NOTICE_PREFIX: &str = "dsa:run-as-arm:";

/// A notification of the host's own about running an app as ARM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The app ended because it could not load a native library, and installing it again as
    /// arm64-v8a may fix that. A click does it.
    Offer,
    /// The app is installed again and runs as ARM. A click opens it.
    Done,
    /// Android cannot change how the app runs and keep its data, so nothing changed.
    NeedsErase,
    /// The app was not installed again.
    Failed,
}

impl Notice {
    fn word(self) -> &'static str {
        match self {
            Notice::Offer => "offer",
            Notice::Done => "done",
            Notice::NeedsErase => "needs-erase",
            Notice::Failed => "failed",
        }
    }

    /// The notice that tells how a reinstall the user asked for from an offer ended.
    pub fn of_outcome(outcome: &RunAsArmOutcome) -> Notice {
        match outcome.status {
            RunAsArmStatus::Done => Notice::Done,
            RunAsArmStatus::DataWouldBeErased => Notice::NeedsErase,
            _ => Notice::Failed,
        }
    }

    /// The key of this notice about `package`.
    pub fn key(self, package: &str) -> String {
        format!("{NOTICE_PREFIX}{}:{package}", self.word())
    }

    /// The notice and the package a key names, or `None` for a key that is not one of these.
    pub fn parse(key: &str) -> Option<(Notice, &str)> {
        let rest = key.strip_prefix(NOTICE_PREFIX)?;
        let (word, package) = rest.split_once(':')?;
        let notice = [
            Notice::Offer,
            Notice::Done,
            Notice::NeedsErase,
            Notice::Failed,
        ]
        .into_iter()
        .find(|notice| notice.word() == word)?;
        crate::apk::valid_package_name(package).then_some((notice, package))
    }

    /// The title and text, in Korean when `korean`, else in English. `label` is the app's name.
    pub fn text(self, label: &str, korean: bool) -> (String, String) {
        match (self, korean) {
            (Notice::Offer, false) => (
                format!("{label} could not start"),
                "It needs ARM code that its x86_64 version leaves out. Click to install it again \
                 to run as ARM; its data stays."
                    .into(),
            ),
            (Notice::Offer, true) => (
                format!("{label}을(를) 시작하지 못했습니다"),
                "x86_64판에 없는 ARM 코드가 필요합니다. 누르면 ARM으로 실행하도록 다시 \
                 설치합니다. 데이터는 그대로 남습니다."
                    .into(),
            ),
            (Notice::Done, false) => (
                format!("{label} now runs as ARM"),
                "Click to open it.".into(),
            ),
            (Notice::Done, true) => (
                format!("{label}이(가) 이제 ARM으로 실행됩니다"),
                "누르면 엽니다.".into(),
            ),
            (Notice::NeedsErase, false) => (
                format!("{label} was not installed again"),
                "Android cannot change how it runs and keep its data. To do it anyway, turn on \
                 Run as ARM for it in DSA Settings, Apps; that erases its data."
                    .into(),
            ),
            (Notice::NeedsErase, true) => (
                format!("{label}을(를) 다시 설치하지 않았습니다"),
                "Android가 데이터를 지킨 채 실행 방식을 바꿀 수 없습니다. 그래도 바꾸려면 DSA \
                 설정의 앱 화면에서 \"ARM으로 실행\"을 켜십시오. 데이터가 지워집니다."
                    .into(),
            ),
            (Notice::Failed, false) => (
                format!("{label} was not installed again"),
                "Android did not install it again to run as ARM. DSA Settings, Apps can try \
                 again."
                    .into(),
            ),
            (Notice::Failed, true) => (
                format!("{label}을(를) 다시 설치하지 않았습니다"),
                "Android가 ARM으로 실행하도록 다시 설치하지 않았습니다. DSA 설정의 앱 \
                 화면에서 다시 해 볼 수 있습니다."
                    .into(),
            ),
        }
    }

    /// The notification of this notice about the installed app `app`, in English; a host that
    /// shows it in the user's language takes [`Notice::text`] of its key instead.
    pub fn notification(self, app: &crate::tasks::AppInfo) -> crate::notifications::Notification {
        let label = if app.label.trim().is_empty() {
            &app.package_name
        } else {
            &app.label
        };
        let (title, text) = self.text(label, false);
        crate::notifications::Notification {
            key: self.key(&app.package_name),
            package_name: app.package_name.clone(),
            app_label: label.clone(),
            title,
            text,
            icon_png: if app.icon_png.len() <= crate::notifications::MAX_ICON_BYTES {
                app.icon_png.clone()
            } else {
                Vec::new()
            },
            importance: 4,
            high_priority: false,
            clearable: true,
            when_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_millis() as i64)
                .unwrap_or(0),
        }
    }
}

/// `notification` in the user's language. The host's own notifications about running an app
/// as ARM come from the service in English, with a key that says which they are; a host shows
/// them in Korean when `korean`. So do its alerts about updates
/// ([`UpdateAlert`](crate::update::alerts::UpdateAlert)), which carry only why an update failed,
/// in their text, and get their words here in either language. Every other notification is an
/// app's own and stays as it came.
pub fn in_user_language(
    mut notification: crate::notifications::Notification,
    korean: bool,
) -> crate::notifications::Notification {
    if let Some((notice, _)) = Notice::parse(&notification.key) {
        let (title, text) = notice.text(&notification.app_label, korean);
        notification.title = title;
        notification.text = text;
    } else if let Some(alert) = crate::update::alerts::UpdateAlert::of_notification(&notification) {
        let (title, text) = alert.text(korean);
        notification.title = title;
        notification.text = text;
    }
    notification
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk_info::fixture::archive;
    use std::io::Cursor;

    fn apk(entries: &[&str]) -> NativeLibraries {
        let files: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|name| (*name, &b"\x7fELF"[..]))
            .collect();
        NativeLibraries::read(Cursor::new(archive(&files, &[]))).expect("an APK")
    }

    const BOTH: &[&str] = &[
        "AndroidManifest.xml",
        "lib/x86_64/libmain.so",
        "lib/x86_64/libgame.so",
        "lib/arm64-v8a/libmain.so",
        "lib/arm64-v8a/libgame.so",
    ];
    const X86_PARTIAL: &[&str] = &[
        "AndroidManifest.xml",
        "lib/x86_64/libmain.so",
        "lib/arm64-v8a/libmain.so",
        "lib/arm64-v8a/libunity.so",
    ];
    const ARM_ONLY: &[&str] = &["AndroidManifest.xml", "lib/arm64-v8a/libmain.so"];
    const NO_LIBRARIES: &[&str] = &["AndroidManifest.xml", "classes.dex", "res/raw/lib.so"];

    #[test]
    fn fr25_the_abi_of_each_fake_apk_is_x86_64_first() {
        let on = || true;
        assert_eq!(choose(&apk(BOTH), on), Some(X86_64));
        assert_eq!(choose(&apk(X86_PARTIAL), on), Some(ARM64));
        assert_eq!(choose(&apk(ARM_ONLY), on), Some(ARM64));
        assert_eq!(choose(&apk(NO_LIBRARIES), on), None);
    }

    #[test]
    fn fr25_a_split_apk_is_compared_with_its_splits_together() {
        // The base has the app's code, each split the libraries of one ABI.
        let base = apk(&["AndroidManifest.xml", "classes.dex"]);
        let x86 = apk(&["lib/x86_64/libmain.so"]);
        let arm = apk(&["lib/arm64-v8a/libmain.so", "lib/arm64-v8a/libextra.so"]);
        let all = NativeLibraries::merged([&base, &x86, &arm]);
        assert_eq!(all.abis().collect::<Vec<_>>(), vec![ARM64, X86_64]);
        assert_eq!(choose(&all, || true), Some(ARM64));
        // Once the x86_64 split has every library, the app stays x86_64.
        let x86_whole = apk(&["lib/x86_64/libmain.so", "lib/x86_64/libextra.so"]);
        let whole = NativeLibraries::merged([&base, &x86_whole, &arm]);
        assert_eq!(choose(&whole, || true), Some(X86_64));
        // Only the base: nothing to choose.
        assert_eq!(choose(&NativeLibraries::merged([&base]), || true), None);
    }

    #[test]
    fn fr25_with_translation_off_arm64_is_never_chosen() {
        let off = || false;
        assert_eq!(choose(&apk(BOTH), off), Some(X86_64));
        assert_eq!(choose(&apk(X86_PARTIAL), off), Some(X86_64));
        assert_eq!(choose(&apk(ARM_ONLY), off), None);
        assert_eq!(choose(&apk(NO_LIBRARIES), off), None);
    }

    #[test]
    fn fr25_translation_is_asked_only_when_the_choice_depends_on_it() {
        let asked = std::cell::Cell::new(false);
        let ask = || {
            asked.set(true);
            true
        };
        assert_eq!(choose(&apk(BOTH), ask), Some(X86_64));
        assert!(!asked.get());
    }

    #[test]
    fn fr25_names_that_are_no_libraries_are_left_out() {
        let mut libraries = NativeLibraries::new();
        libraries.add_entry("lib/arm64-v8a/libok.so");
        libraries.add_entry("lib/arm64-v8a/sub/libnested.so");
        libraries.add_entry("lib/arm64-v8a/readme.txt");
        libraries.add_entry("lib/ARM64/libupper.so");
        libraries.add_entry("assets/lib/x86_64/libasset.so");
        assert_eq!(libraries.abis().collect::<Vec<_>>(), vec![ARM64]);
        assert_eq!(libraries.names(ARM64).unwrap().len(), 1);
        let wire = NativeLibraries::from_wire(&libraries.to_wire());
        assert_eq!(wire, libraries);
    }

    #[test]
    fn fr25_the_abi_list_says_whether_arm64_is_translated() {
        assert!(translates_arm64("x86_64,arm64-v8a"));
        assert!(!translates_arm64("x86_64"));
        assert!(!translates_arm64(""));
    }

    #[test]
    fn fr25_the_run_as_arm_switch_says_why_it_cannot_be_changed() {
        let both = apk(BOTH);
        let partial = apk(X86_PARTIAL);
        let arm = apk(ARM_ONLY);
        let none = apk(NO_LIBRARIES);
        let switch = run_as_arm(false, X86_64, &both, true);
        assert!(!switch.on && switch.available());
        assert_eq!(
            run_as_arm(true, X86_64, &both, true).unavailable,
            Some(RunAsArmUnavailable::SystemApp)
        );
        assert_eq!(
            run_as_arm(false, "", &none, true).unavailable,
            Some(RunAsArmUnavailable::NoArmLibraries)
        );
        assert_eq!(
            run_as_arm(false, X86_64, &both, false).unavailable,
            Some(RunAsArmUnavailable::TranslationOff)
        );
        assert_eq!(
            run_as_arm(false, ARM64, &arm, true),
            RunAsArm {
                on: true,
                unavailable: Some(RunAsArmUnavailable::ArmOnly),
            }
        );
        // An app installed as ARM because its x86_64 code was partial cannot go back either.
        assert_eq!(
            run_as_arm(false, ARM64, &partial, true).unavailable,
            Some(RunAsArmUnavailable::ArmOnly)
        );
        let back = run_as_arm(false, ARM64, &both, true);
        assert!(back.on && back.available());
    }

    #[test]
    fn fr25_the_keys_of_the_run_as_arm_notices_name_their_app() {
        for notice in [
            Notice::Offer,
            Notice::Done,
            Notice::NeedsErase,
            Notice::Failed,
        ] {
            let key = notice.key("com.example.game");
            assert!(key.starts_with(crate::notifications::HOST_KEY_PREFIX));
            assert_eq!(Notice::parse(&key), Some((notice, "com.example.game")));
            let (english, _) = notice.text("Game", false);
            let (korean, _) = notice.text("Game", true);
            assert!(english.contains("Game") && korean.contains("Game"));
        }
        assert_eq!(Notice::parse("dsa:run-as-arm:offer:not a package"), None);
        assert_eq!(Notice::parse("dsa:run-as-arm:other:com.example.game"), None);
        assert_eq!(Notice::parse("0|com.example.game|1|null|10100"), None);
    }

    #[test]
    fn fr25_every_run_as_arm_status_survives_the_local_api() {
        for status in [
            RunAsArmStatus::Done,
            RunAsArmStatus::DataWouldBeErased,
            RunAsArmStatus::NotInstalled,
            RunAsArmStatus::SystemApp,
            RunAsArmStatus::NoLibraries,
            RunAsArmStatus::TranslationOff,
            RunAsArmStatus::Other,
        ] {
            assert_eq!(RunAsArmStatus::from_wire(status.to_wire() as i32), status);
        }
        assert_eq!(
            RunAsArmStatus::from_control(control::ReinstallStatus::DataWouldBeErased as i32),
            RunAsArmStatus::DataWouldBeErased
        );
        assert_eq!(RunAsArmStatus::from_control(99), RunAsArmStatus::Other);
    }
}
