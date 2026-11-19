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

use crate::apk_info::axml;
use crate::apk_info::zip::Archive;
use crate::ui_language::UiLanguage;

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

/// Whether an app with `libraries` runs only through ARM translation: it has arm64-v8a
/// libraries and no x86_64 ones.
pub fn needs_translation(libraries: &NativeLibraries) -> bool {
    libraries.has(ARM64) && !libraries.has(X86_64)
}

/// The ABI the package installer is told to install an app with `libraries` for, as
/// [`choose`] says, or why the app is not installed at all. `translation` says whether the
/// running Android translates ARM code (`None` when it cannot be asked); it is asked only when
/// the answer depends on it.
///
/// An app whose 64-bit code is for arm64-v8a only is installed as arm64-v8a while translation
/// is on, and refused with [`TranslationOff`] while it is off: Android would refuse it too, in
/// words that do not say translation is the way out. When translation cannot be asked, the app
/// is left to Android.
pub fn install_abi(
    libraries: &NativeLibraries,
    translation: impl FnOnce() -> Option<bool>,
) -> Result<Option<&'static str>, TranslationOff> {
    if needs_translation(libraries) {
        return match translation() {
            Some(true) => Ok(Some(ARM64)),
            Some(false) => Err(TranslationOff),
            None => Ok(None),
        };
    }
    Ok(choose(libraries, || translation() == Some(true)))
}

/// Why an app is not installed: its 64-bit native code is for arm64-v8a only, which the guest
/// runs through ARM translation, and the running Android does not translate ARM code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TranslationOff;

impl TranslationOff {
    /// A stable code for programs, such as the install window, that say it in the user's
    /// language.
    pub fn code(self) -> &'static str {
        "translation-off"
    }
}

impl fmt::Display for TranslationOff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "the app's native code is for arm64-v8a only, which DSA's Android runs through ARM \
             translation, and ARM translation is not on in the running Android. Turn it on \
             under ARM apps, restart Android and install the app again",
        )
    }
}

/// Whether `abilist`, the guest's `ro.product.cpu.abilist`, holds arm64-v8a, which it does
/// only while a translator is in place.
pub fn translates_arm64(abilist: &str) -> bool {
    abilist.split(',').any(|abi| abi.trim() == ARM64)
}

/// The 32-bit ABIs. The guest is 64-bit only: it has no 32-bit zygote and no 32-bit system
/// libraries, so it runs none of their code.
pub const ABIS_32_BIT: [&str; 3] = ["x86", "armeabi-v7a", "armeabi"];

/// Whether `abi` is a 32-bit ABI.
pub fn is_32_bit(abi: &str) -> bool {
    ABIS_32_BIT.contains(&abi)
}

/// The 64-bit ABI of the same processor family as the 32-bit ABI `abi`: where a device has no
/// 32-bit ABI, an app's code for this one is what Android installs instead.
fn wide_abi(abi: &str) -> Option<&'static str> {
    match abi {
        "x86" => Some(X86_64),
        "armeabi-v7a" | "armeabi" => Some(ARM64),
        _ => None,
    }
}

/// Whether the native libraries an installed app's APKs hold include code for a 32-bit ABI.
pub fn has_32_bit_code(libraries: &NativeLibraries) -> bool {
    libraries.abis().any(is_32_bit)
}

/// What Chromium says when a build made to run as 32-bit code starts as 64-bit code, which
/// for it holds only WebView: "Starting in 64-bit mode requires the 64-bit native library".
pub const CHROMIUM_32_BIT_BUILD: &str = "64-bit native library";

/// Whether an app that ended for want of native code (`detail`, the line of its crash record
/// that says so) is a build made to run as 32-bit code, whose 64-bit build has to be installed
/// instead: its APKs on the device hold 32-bit code, running it as ARM is no way out (it has
/// no ARM code, or runs as ARM already), and either its 64-bit code leaves out libraries its
/// 32-bit code has or the app says it is such a build.
pub fn needs_64_bit_build(libraries: &NativeLibraries, switch: RunAsArm, detail: &str) -> bool {
    let no_way_out = switch.on || switch.unavailable == Some(RunAsArmUnavailable::NoArmLibraries);
    let partial = libraries
        .abis()
        .any(|abi| wide_abi(abi).is_some_and(|wide| !libraries.covers(wide, abi)));
    has_32_bit_code(libraries) && no_way_out && (partial || detail.contains(CHROMIUM_32_BIT_BUILD))
}

/// The framework's resource ID of `android:use32bitAbi`, which [`NativeCode`] reads.
const USE_32BIT_ABI: u32 = 0x0101_0515;

/// The largest manifest [`NativeCode`] reads.
const MAX_MANIFEST: usize = 4 * 1024 * 1024;

/// What an app's APKs hold of native code, read before Android sees them, to tell an app the
/// guest cannot run at all ([`NativeCode::wont_run`]).
///
/// Unlike [`NativeLibraries`], it leaves empty files out of the code. Chromium's split builds
/// put an empty `libplaceholder.so` under an ABI whose code is elsewhere, only so that Android
/// installs the app for that ABI: the 32-bit builds of Chrome hold one under `arm64-v8a` or
/// `x86_64`, and all their own code under the 32-bit ABI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeCode {
    /// The libraries that are not empty, by ABI.
    code: NativeLibraries,
    /// Every ABI with a file under `lib/<abi>/`, empty or not.
    folders: BTreeSet<String>,
    /// The manifest asks Android to run the app's 32-bit code where a device has both
    /// (`android:use32bitAbi`).
    prefers_32_bit: bool,
    /// The app uses a static shared library (`<uses-static-library>`), which can hold its
    /// native code for it.
    shared_library: bool,
}

impl NativeCode {
    pub fn new() -> NativeCode {
        NativeCode::default()
    }

    /// Adds the archive entry `entry` of `size` bytes, when it is a native library.
    pub fn add_entry(&mut self, entry: &str, size: u64) {
        let Some(rest) = entry.strip_prefix("lib/") else {
            return;
        };
        let Some((abi, name)) = rest.split_once('/') else {
            return;
        };
        if !valid_abi(abi) || !valid_library(name) {
            return;
        }
        if self.folders.len() < MAX_ABIS || self.folders.contains(abi) {
            self.folders.insert(abi.to_string());
        }
        if size > 0 {
            self.code.add(abi, name);
        }
    }

    /// Takes what the binary manifest `document` says of the app's native code. A split's
    /// manifest has no `application` element and changes nothing.
    pub fn add_manifest(&mut self, document: &axml::Document) {
        let flag = |element: &axml::Element, id: u32, name: &str| {
            element.integer(id, name).is_some_and(|value| value != 0)
        };
        for element in &document.elements {
            if element.depth == 1 && element.name == "application" {
                self.prefers_32_bit |= flag(element, USE_32BIT_ABI, "use32bitAbi");
            }
            if element.depth == 2 && element.name == "uses-static-library" {
                self.shared_library = true;
            }
        }
    }

    /// The native code of the APK `archive` is, with its manifest when it can be read; Android
    /// judges a manifest that cannot.
    pub fn of_archive<R: Read + Seek>(archive: &mut Archive<R>) -> NativeCode {
        let mut code = NativeCode::new();
        for entry in &archive.entries {
            code.add_entry(&entry.name, entry.size());
        }
        if let Some(entry) = archive.entry("AndroidManifest.xml").cloned() {
            if let Ok(document) = archive
                .read(&entry, MAX_MANIFEST)
                .and_then(|data| axml::parse(&data))
            {
                code.add_manifest(&document);
            }
        }
        code
    }

    /// The native code of the APK `reader` holds.
    pub fn read<R: Read + Seek>(reader: R) -> Result<NativeCode, String> {
        let mut archive = Archive::open(reader)?;
        Ok(NativeCode::of_archive(&mut archive))
    }

    /// The native code of an APK and its splits together.
    pub fn merged<'a>(apks: impl IntoIterator<Item = &'a NativeCode>) -> NativeCode {
        let mut all = NativeCode::new();
        for apk in apks {
            all.code = NativeLibraries::merged([&all.code, &apk.code]);
            for abi in &apk.folders {
                if all.folders.len() < MAX_ABIS || all.folders.contains(abi) {
                    all.folders.insert(abi.clone());
                }
            }
            all.prefers_32_bit |= apk.prefers_32_bit;
            all.shared_library |= apk.shared_library;
        }
        all
    }

    /// Why the guest cannot run this app's native code at all, or `None` when it may. Code for
    /// arm64-v8a counts as code the guest runs, since ARM translation can be turned on; whether
    /// it is on decides only the ABI the app is installed for ([`choose`]).
    pub fn wont_run(&self) -> Option<WontRun> {
        if self.code.is_empty() {
            return None;
        }
        let runs = self.code.has(X86_64) || self.code.has(ARM64);
        if !runs {
            // An app may keep its 64-bit code in a static shared library it uses, with an
            // empty file under the 64-bit ABI so that Android installs it for that ABI, as the
            // Trichrome builds of Chrome and WebView do. Unless it asks to run as 32-bit code,
            // its 64-bit code is in that library.
            let declares_64_bit = self.folders.contains(X86_64) || self.folders.contains(ARM64);
            if self.shared_library && declares_64_bit && !self.prefers_32_bit {
                return None;
            }
            let abis: Vec<String> = self.code.abis().map(str::to_string).collect();
            return Some(if abis.iter().all(|abi| is_32_bit(abi)) {
                WontRun::ThirtyTwoBitOnly { abis }
            } else {
                WontRun::OtherAbis { abis }
            });
        }
        if self.prefers_32_bit {
            // A build made to run as 32-bit code whose 64-bit code is only a part of the app,
            // as Chrome's 32-bit builds carry only WebView in their 64-bit library.
            for abi in self.code.abis() {
                if let Some(wide) = wide_abi(abi) {
                    if self.folders.contains(wide) && !self.code.covers(wide, abi) {
                        return Some(WontRun::ThirtyTwoBitBuild {
                            abi: abi.to_string(),
                            wide,
                        });
                    }
                }
            }
        }
        None
    }
}

/// Why the guest cannot run an app's native code, found before Android sees the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WontRun {
    /// All of its native code is for 32-bit ABIs (`abis`).
    ThirtyTwoBitOnly { abis: Vec<String> },
    /// Its native code is for ABIs the guest does not run (`abis`), such as riscv64.
    OtherAbis { abis: Vec<String> },
    /// It asks Android to run its 32-bit code for `abi`, and its code for the 64-bit ABI
    /// `wide` leaves out libraries the 32-bit code has, so on a 64-bit-only Android it would
    /// start without them.
    ThirtyTwoBitBuild { abi: String, wide: &'static str },
}

impl WontRun {
    /// A stable code for programs, such as the install window, that say it in the user's
    /// language.
    pub fn code(&self) -> &'static str {
        "unsupported-abi"
    }
}

impl fmt::Display for WontRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const RUNS: &str = "DSA's Android runs 64-bit code only: x86_64, and arm64-v8a through \
                            ARM translation";
        match self {
            WontRun::ThirtyTwoBitOnly { abis } => write!(
                f,
                "the app's native code is 32-bit only ({}), and {RUNS}. Get the app's x86_64 or \
                 arm64-v8a build",
                abis.join(", ")
            ),
            WontRun::OtherAbis { abis } => write!(
                f,
                "the app's native code is for {} only, and {RUNS}. Get the app's x86_64 or \
                 arm64-v8a build",
                abis.join(", ")
            ),
            WontRun::ThirtyTwoBitBuild { abi, wide } => write!(
                f,
                "this is a build of the app made to run as 32-bit code ({abi}); its {wide} code \
                 is only part of the app, and {RUNS}. Get the app's 64-bit build"
            ),
        }
    }
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
    /// The app ended for want of native code, its APKs hold code for a 32-bit ABI, and it
    /// cannot run as ARM: it is a build the guest does not run, and its 64-bit build has to
    /// be installed instead. A click removes it.
    Needs64Bit,
}

impl Notice {
    fn word(self) -> &'static str {
        match self {
            Notice::Offer => "offer",
            Notice::Done => "done",
            Notice::NeedsErase => "needs-erase",
            Notice::Failed => "failed",
            Notice::Needs64Bit => "needs-64-bit",
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
            Notice::Needs64Bit,
        ]
        .into_iter()
        .find(|notice| notice.word() == word)?;
        crate::apk::valid_package_name(package).then_some((notice, package))
    }

    /// The title and text, in `lang`. `label` is the app's name.
    pub fn text(self, label: &str, lang: UiLanguage) -> (String, String) {
        let words = notice_words(lang);
        let (title, text) = match self {
            Notice::Offer => words.offer,
            Notice::Done => words.done,
            Notice::NeedsErase => words.needs_erase,
            Notice::Failed => words.failed,
            Notice::Needs64Bit => words.needs_64_bit,
        };
        (title.replace("{label}", label), text.to_string())
    }

    /// The notification of this notice about the installed app `app`, in English; a host that
    /// shows it in the user's language takes [`Notice::text`] of its key instead.
    pub fn notification(self, app: &crate::tasks::AppInfo) -> crate::notifications::Notification {
        let label = if app.label.trim().is_empty() {
            &app.package_name
        } else {
            &app.label
        };
        let (title, text) = self.text(label, UiLanguage::English);
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
            buttons: Vec::new(),
        }
    }
}

/// The words of the notices about running an app as ARM in one language. Each notice is its
/// title, in which `{label}` stands for the app's name, and its text.
struct NoticeWords {
    offer: (&'static str, &'static str),
    done: (&'static str, &'static str),
    needs_erase: (&'static str, &'static str),
    failed: (&'static str, &'static str),
    needs_64_bit: (&'static str, &'static str),
}

fn notice_words(lang: UiLanguage) -> NoticeWords {
    match lang {
        UiLanguage::English => NoticeWords {
            offer: (
                "{label} could not start",
                "It needs ARM code that its x86_64 version leaves out. Click to install it again \
                 to run as ARM; its data stays.",
            ),
            done: ("{label} now runs as ARM", "Click to open it."),
            needs_erase: (
                "{label} was not installed again",
                "Android cannot change how it runs and keep its data. To do it anyway, turn on \
                 Run as ARM for it in DSA Settings, Apps; that erases its data.",
            ),
            failed: (
                "{label} was not installed again",
                "Android did not install it again to run as ARM. DSA Settings, Apps can try \
                 again.",
            ),
            needs_64_bit: (
                "{label} cannot run in DSA",
                "This build runs 32-bit code, which DSA does not run. Uninstall it, then install \
                 its x86_64 build with DSA's APK install. For Chrome: TrichromeLibrary first, \
                 then Chrome, with a version code ending in 38.",
            ),
        },
        UiLanguage::Korean => NoticeWords {
            offer: (
                "{label}을(를) 시작하지 못했습니다",
                "x86_64판에 없는 ARM 코드가 필요합니다. 누르면 ARM으로 실행하도록 다시 \
                 설치합니다. 데이터는 그대로 남습니다.",
            ),
            done: ("{label}이(가) 이제 ARM으로 실행됩니다", "누르면 엽니다."),
            needs_erase: (
                "{label}을(를) 다시 설치하지 않았습니다",
                "Android가 데이터를 지킨 채 실행 방식을 바꿀 수 없습니다. 그래도 바꾸려면 DSA \
                 설정의 앱 화면에서 \"ARM으로 실행\"을 켜십시오. 데이터가 지워집니다.",
            ),
            failed: (
                "{label}을(를) 다시 설치하지 않았습니다",
                "Android가 ARM으로 실행하도록 다시 설치하지 않았습니다. DSA 설정의 앱 \
                 화면에서 다시 해 볼 수 있습니다.",
            ),
            needs_64_bit: (
                "{label}을(를) DSA에서 실행할 수 없습니다",
                "이 빌드는 32비트 코드를 쓰는데, DSA는 이를 실행하지 않습니다. 앱을 지우고 \
                 DSA의 APK 설치로 x86_64판을 설치하십시오. Chrome은 TrichromeLibrary를 먼저, \
                 그다음 Chrome을 설치하며, 버전 코드가 38로 끝나야 합니다.",
            ),
        },
        UiLanguage::Japanese => NoticeWords {
            offer: (
                "{label} を起動できませんでした",
                "x86_64 版に含まれていない ARM コードが必要です。クリックすると、ARM で実行\
                 するように再インストールします。データはそのまま残ります。",
            ),
            done: (
                "{label} は ARM で実行されるようになりました",
                "クリックすると開きます。",
            ),
            needs_erase: (
                "{label} は再インストールされませんでした",
                "Android は、データを保持したまま実行方法を変更できません。それでも変更するには、\
                 DSA の設定の [アプリ] で [ARM で実行] をオンにしてください。データは消去されます。",
            ),
            failed: (
                "{label} は再インストールされませんでした",
                "Android は ARM で実行するための再インストールを行いませんでした。DSA の設定の \
                 [アプリ] からもう一度お試しください。",
            ),
            needs_64_bit: (
                "{label} は DSA で実行できません",
                "このビルドは DSA が実行しない 32 ビット コードを使っています。アプリを\
                 アンインストールし、DSA の APK インストールで x86_64 ビルドをインストール\
                 してください。Chrome は TrichromeLibrary、次に Chrome の順で、バージョン \
                 コードが 38 で終わるものです。",
            ),
        },
        UiLanguage::ChineseSimplified => NoticeWords {
            offer: (
                "{label} 无法启动",
                "它需要 x86_64 版本中没有的 ARM 代码。点击即可重新安装，以 ARM 方式运行；\
                 数据会保留。",
            ),
            done: ("{label} 现在以 ARM 方式运行", "点击即可打开。"),
            needs_erase: (
                "{label} 未重新安装",
                "Android 无法在保留数据的同时更改其运行方式。如果仍要更改，请在 DSA 设置的\
                 “应用”中为它打开“以 ARM 方式运行”；这会清除其数据。",
            ),
            failed: (
                "{label} 未重新安装",
                "Android 未能重新安装它以 ARM 方式运行。可以在 DSA 设置的“应用”中重试。",
            ),
            needs_64_bit: (
                "{label} 无法在 DSA 中运行",
                "此版本使用 DSA 不运行的 32 位代码。请卸载它，再用 DSA 的 APK 安装功能安装其 \
                 x86_64 版本。Chrome 请先安装 TrichromeLibrary，再安装 Chrome，版本代码以 38 结尾。",
            ),
        },
        UiLanguage::ChineseTraditional => NoticeWords {
            offer: (
                "{label} 無法啟動",
                "它需要 x86_64 版本中沒有的 ARM 程式碼。按一下即可重新安裝，以 ARM 方式執行；\
                 資料會保留。",
            ),
            done: ("{label} 現在以 ARM 方式執行", "按一下即可開啟。"),
            needs_erase: (
                "{label} 未重新安裝",
                "Android 無法在保留資料的情況下變更其執行方式。若仍要變更，請在 DSA 設定的\
                 「應用程式」中為它開啟「以 ARM 方式執行」；這會清除其資料。",
            ),
            failed: (
                "{label} 未重新安裝",
                "Android 未能重新安裝它以 ARM 方式執行。您可以在 DSA 設定的「應用程式」中再試\
                 一次。",
            ),
            needs_64_bit: (
                "{label} 無法在 DSA 中執行",
                "此組建使用 DSA 不執行的 32 位元程式碼。請解除安裝它，再用 DSA 的 APK 安裝功能\
                 安裝其 x86_64 組建。Chrome 請先安裝 TrichromeLibrary，再安裝 Chrome，版本代碼\
                 以 38 結尾。",
            ),
        },
        UiLanguage::Spanish => NoticeWords {
            offer: (
                "No se pudo iniciar {label}",
                "Necesita código ARM que su versión x86_64 no incluye. Haga clic para volver a \
                 instalarla y ejecutarla como ARM; sus datos se conservan.",
            ),
            done: (
                "{label} ahora se ejecuta como ARM",
                "Haga clic para abrirla.",
            ),
            needs_erase: (
                "{label} no se volvió a instalar",
                "Android no puede cambiar la forma en que se ejecuta y conservar sus datos. Para \
                 hacerlo de todos modos, active Ejecutar como ARM para esta aplicación en \
                 Configuración de DSA, Aplicaciones; eso borra sus datos.",
            ),
            failed: (
                "{label} no se volvió a instalar",
                "Android no la volvió a instalar para ejecutarla como ARM. Puede volver a \
                 intentarlo en Configuración de DSA, Aplicaciones.",
            ),
            needs_64_bit: (
                "{label} no se puede ejecutar en DSA",
                "Esta compilación usa código de 32 bits, que DSA no ejecuta. Desinstálela e \
                 instale su compilación x86_64 con la instalación de APK de DSA. En Chrome: \
                 primero TrichromeLibrary y luego Chrome, con un código de versión que termine \
                 en 38.",
            ),
        },
        UiLanguage::French => NoticeWords {
            offer: (
                "Impossible de démarrer {label}",
                "L'application a besoin de code ARM que sa version x86_64 ne contient pas. \
                 Cliquez pour la réinstaller afin de l'exécuter en ARM ; ses données sont \
                 conservées.",
            ),
            done: (
                "{label} s'exécute maintenant en ARM",
                "Cliquez pour l'ouvrir.",
            ),
            needs_erase: (
                "{label} n'a pas été réinstallée",
                "Android ne peut pas changer son mode d'exécution tout en conservant ses \
                 données. Pour le faire quand même, activez Exécuter en ARM pour cette \
                 application dans Paramètres DSA, Applications ; cela efface ses données.",
            ),
            failed: (
                "{label} n'a pas été réinstallée",
                "Android ne l'a pas réinstallée pour l'exécuter en ARM. Vous pouvez réessayer \
                 dans Paramètres DSA, Applications.",
            ),
            needs_64_bit: (
                "{label} ne peut pas s'exécuter dans DSA",
                "Cette version utilise du code 32 bits, que DSA n'exécute pas. Désinstallez-la, \
                 puis installez sa version x86_64 avec l'installation d'APK de DSA. Pour \
                 Chrome : TrichromeLibrary d'abord, puis Chrome, avec un code de version qui \
                 finit par 38.",
            ),
        },
        UiLanguage::German => NoticeWords {
            offer: (
                "{label} konnte nicht gestartet werden",
                "Die App benötigt ARM-Code, der in ihrer x86_64-Version fehlt. Klicken Sie, um \
                 sie für die Ausführung als ARM neu zu installieren; ihre Daten bleiben \
                 erhalten.",
            ),
            done: (
                "{label} wird jetzt als ARM ausgeführt",
                "Klicken Sie, um die App zu öffnen.",
            ),
            needs_erase: (
                "{label} wurde nicht neu installiert",
                "Android kann die Ausführungsart nicht ändern und dabei die Daten behalten. Um \
                 es trotzdem zu tun, aktivieren Sie in den DSA-Einstellungen unter Apps die \
                 Option Als ARM ausführen; dabei werden die Daten gelöscht.",
            ),
            failed: (
                "{label} wurde nicht neu installiert",
                "Android hat die App nicht für die Ausführung als ARM neu installiert. In den \
                 DSA-Einstellungen unter Apps können Sie es erneut versuchen.",
            ),
            needs_64_bit: (
                "{label} kann in DSA nicht ausgeführt werden",
                "Dieser Build nutzt 32-Bit-Code, den DSA nicht ausführt. Deinstallieren Sie die \
                 App und installieren Sie ihren x86_64-Build mit der APK-Installation von DSA. \
                 Bei Chrome zuerst TrichromeLibrary, dann Chrome, mit einem Versionscode, der \
                 auf 38 endet.",
            ),
        },
        UiLanguage::PortugueseBrazil => NoticeWords {
            offer: (
                "Não foi possível iniciar {label}",
                "O app precisa de código ARM que a versão x86_64 dele não inclui. Clique para \
                 reinstalá-lo e executá-lo como ARM; os dados dele são mantidos.",
            ),
            done: ("{label} agora é executado como ARM", "Clique para abrir."),
            needs_erase: (
                "{label} não foi reinstalado",
                "O Android não pode mudar a forma como o app é executado e manter os dados \
                 dele. Para fazer isso mesmo assim, ative Executar como ARM para ele em \
                 Configurações do DSA, Apps; isso apaga os dados dele.",
            ),
            failed: (
                "{label} não foi reinstalado",
                "O Android não reinstalou o app para executá-lo como ARM. Você pode tentar \
                 novamente em Configurações do DSA, Apps.",
            ),
            needs_64_bit: (
                "{label} não pode ser executado no DSA",
                "Esta versão usa código de 32 bits, que o DSA não executa. Desinstale o app e \
                 instale a versão x86_64 dele com a instalação de APK do DSA. No Chrome: \
                 primeiro o TrichromeLibrary e depois o Chrome, com código de versão \
                 terminado em 38.",
            ),
        },
        UiLanguage::Russian => NoticeWords {
            offer: (
                "Не удалось запустить {label}",
                "Приложению нужен код ARM, которого нет в его версии x86_64. Нажмите, чтобы \
                 переустановить его для запуска как ARM; данные сохранятся.",
            ),
            done: (
                "Приложение {label} теперь работает как ARM",
                "Нажмите, чтобы открыть.",
            ),
            needs_erase: (
                "Приложение {label} не переустановлено",
                "Android не может изменить способ запуска приложения с сохранением его данных. \
                 Чтобы сделать это всё равно, включите для него «Запускать как ARM» в разделе \
                 «Приложения» параметров DSA; при этом его данные будут удалены.",
            ),
            failed: (
                "Приложение {label} не переустановлено",
                "Android не переустановил приложение для запуска как ARM. Попробовать снова \
                 можно в разделе «Приложения» параметров DSA.",
            ),
            needs_64_bit: (
                "Приложение {label} нельзя запустить в DSA",
                "Эта сборка использует 32-разрядный код, который DSA не выполняет. Удалите \
                 приложение и установите его сборку x86_64 через установку APK в DSA. Для \
                 Chrome: сначала TrichromeLibrary, затем Chrome, код версии должен \
                 заканчиваться на 38.",
            ),
        },
        UiLanguage::Italian => NoticeWords {
            offer: (
                "Impossibile avviare {label}",
                "L'app richiede codice ARM che la sua versione x86_64 non include. Fai clic per \
                 reinstallarla ed eseguirla come ARM; i suoi dati restano.",
            ),
            done: (
                "L'app {label} ora viene eseguita come ARM",
                "Fai clic per aprirla.",
            ),
            needs_erase: (
                "L'app {label} non è stata reinstallata",
                "Android non può cambiare il modo in cui viene eseguita mantenendone i dati. \
                 Per farlo comunque, attiva Esegui come ARM per l'app in Impostazioni DSA, App; \
                 i suoi dati verranno cancellati.",
            ),
            failed: (
                "L'app {label} non è stata reinstallata",
                "Android non l'ha reinstallata per eseguirla come ARM. Puoi riprovare da \
                 Impostazioni DSA, App.",
            ),
            needs_64_bit: (
                "Impossibile eseguire {label} in DSA",
                "Questa build usa codice a 32 bit, che DSA non esegue. Disinstalla l'app e \
                 installa la sua build x86_64 con l'installazione APK di DSA. Per Chrome: \
                 prima TrichromeLibrary, poi Chrome, con un codice di versione che termina \
                 con 38.",
            ),
        },
        UiLanguage::Vietnamese => NoticeWords {
            offer: (
                "Không thể khởi động {label}",
                "Ứng dụng cần mã ARM mà phiên bản x86_64 không có. Bấm để cài đặt lại ứng dụng \
                 để chạy dưới dạng ARM; dữ liệu của ứng dụng vẫn được giữ.",
            ),
            done: ("{label} hiện chạy dưới dạng ARM", "Bấm để mở."),
            needs_erase: (
                "Chưa cài đặt lại {label}",
                "Android không thể thay đổi cách ứng dụng chạy mà vẫn giữ dữ liệu. Nếu vẫn muốn \
                 thay đổi, hãy bật Chạy dưới dạng ARM cho ứng dụng trong Cài đặt DSA, Ứng \
                 dụng; thao tác này sẽ xóa dữ liệu của ứng dụng.",
            ),
            failed: (
                "Chưa cài đặt lại {label}",
                "Android chưa cài đặt lại ứng dụng để chạy dưới dạng ARM. Bạn có thể thử lại \
                 trong Cài đặt DSA, Ứng dụng.",
            ),
            needs_64_bit: (
                "Không thể chạy {label} trong DSA",
                "Bản dựng này dùng mã 32 bit, mà DSA không chạy. Hãy gỡ cài đặt ứng dụng rồi \
                 cài bản dựng x86_64 của nó bằng tính năng cài APK của DSA. Với Chrome: cài \
                 TrichromeLibrary trước, rồi đến Chrome, mã phiên bản kết thúc bằng 38.",
            ),
        },
        UiLanguage::Indonesian => NoticeWords {
            offer: (
                "{label} tidak dapat dimulai",
                "Aplikasi ini memerlukan kode ARM yang tidak ada di versi x86_64-nya. Klik untuk \
                 menginstalnya ulang agar berjalan sebagai ARM; datanya tetap ada.",
            ),
            done: ("{label} kini berjalan sebagai ARM", "Klik untuk membukanya."),
            needs_erase: (
                "{label} tidak diinstal ulang",
                "Android tidak dapat mengubah cara aplikasi ini berjalan sambil mempertahankan \
                 datanya. Untuk tetap melakukannya, aktifkan Jalankan sebagai ARM untuk \
                 aplikasi ini di Pengaturan DSA, Aplikasi; tindakan ini menghapus datanya.",
            ),
            failed: (
                "{label} tidak diinstal ulang",
                "Android tidak menginstalnya ulang agar berjalan sebagai ARM. Anda dapat \
                 mencobanya lagi di Pengaturan DSA, Aplikasi.",
            ),
            needs_64_bit: (
                "{label} tidak dapat berjalan di DSA",
                "Build ini memakai kode 32-bit, yang tidak dijalankan DSA. Copot pemasangannya \
                 lalu instal build x86_64-nya dengan instalasi APK DSA. Untuk Chrome: \
                 TrichromeLibrary dahulu, lalu Chrome, dengan kode versi berakhiran 38.",
            ),
        },
        UiLanguage::Turkish => NoticeWords {
            offer: (
                "{label} başlatılamadı",
                "x86_64 sürümünde bulunmayan ARM kodu gerekiyor. ARM olarak çalışacak şekilde \
                 yeniden yüklemek için tıklayın; verileri korunur.",
            ),
            done: ("{label} artık ARM olarak çalışıyor", "Açmak için tıklayın."),
            needs_erase: (
                "{label} yeniden yüklenmedi",
                "Android, verilerini koruyarak uygulamanın çalışma biçimini değiştiremez. Yine \
                 de değiştirmek için DSA Ayarları, Uygulamalar bölümünde bu uygulama için ARM \
                 olarak çalıştır seçeneğini açın; bu işlem verilerini siler.",
            ),
            failed: (
                "{label} yeniden yüklenmedi",
                "Android, uygulamayı ARM olarak çalışacak şekilde yeniden yüklemedi. DSA \
                 Ayarları, Uygulamalar bölümünden yeniden deneyebilirsiniz.",
            ),
            needs_64_bit: (
                "{label} DSA'da çalıştırılamıyor",
                "Bu derleme, DSA'nın çalıştırmadığı 32 bit kod kullanıyor. Uygulamayı kaldırın, \
                 sonra x86_64 derlemesini DSA'nın APK yüklemesiyle yükleyin. Chrome için önce \
                 TrichromeLibrary, sonra Chrome; sürüm kodu 38 ile bitmelidir.",
            ),
        },
    }
}

/// `notification` in the user's language. The host's own notifications about running an app
/// as ARM come from the service in English, with a key that says which they are; a host shows
/// them in `lang`. So do its alerts about updates
/// ([`UpdateAlert`](crate::update::alerts::UpdateAlert)), which carry only why an update failed,
/// in their text, and get their words here in any interface language. Every other
/// notification is an app's own and stays as it came.
pub fn in_user_language(
    mut notification: crate::notifications::Notification,
    lang: UiLanguage,
) -> crate::notifications::Notification {
    if let Some((notice, _)) = Notice::parse(&notification.key) {
        let (title, text) = notice.text(&notification.app_label, lang);
        notification.title = title;
        notification.text = text;
    } else if let Some(alert) = crate::update::alerts::UpdateAlert::of_notification(&notification) {
        let (title, text) = alert.text(lang);
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
    fn fr25_an_app_with_x86_64_and_arm64_code_is_installed_as_x86_64() {
        for translation in [Some(true), Some(false), None] {
            assert_eq!(
                install_abi(&apk(BOTH), || translation),
                Ok(Some(X86_64)),
                "{translation:?}"
            );
        }
        // Partial x86_64 code runs as ARM only while translation is on.
        assert_eq!(
            install_abi(&apk(X86_PARTIAL), || Some(true)),
            Ok(Some(ARM64))
        );
        assert_eq!(
            install_abi(&apk(X86_PARTIAL), || Some(false)),
            Ok(Some(X86_64))
        );
    }

    #[test]
    fn fr25_an_app_with_arm64_code_only_is_installed_as_arm64_while_translation_is_on() {
        assert!(needs_translation(&apk(ARM_ONLY)));
        assert_eq!(install_abi(&apk(ARM_ONLY), || Some(true)), Ok(Some(ARM64)));
        // Android judges it when the running Android cannot be asked.
        assert_eq!(install_abi(&apk(ARM_ONLY), || None), Ok(None));
    }

    #[test]
    fn fr25_an_app_with_arm64_code_only_is_refused_with_its_reason_while_translation_is_off() {
        let refused = install_abi(&apk(ARM_ONLY), || Some(false)).unwrap_err();
        assert_eq!(refused.code(), "translation-off");
        let text = refused.to_string();
        assert!(text.contains("arm64-v8a only"), "{text}");
        assert!(text.contains("ARM translation is not on"), "{text}");
        assert!(text.contains("Turn it on under ARM apps"), "{text}");
        // ARM code beside 32-bit code needs translation as much.
        let mut with_32_bit = apk(ARM_ONLY);
        with_32_bit.add("armeabi-v7a", "libmain.so");
        assert_eq!(
            install_abi(&with_32_bit, || Some(false)),
            Err(TranslationOff)
        );
    }

    #[test]
    fn fr25_an_app_with_32_bit_code_only_is_still_refused() {
        // The install refuses it before Android sees it, whatever translation does.
        for abis in [
            &["lib/armeabi-v7a/libmain.so"][..],
            &["lib/x86/libmain.so"][..],
        ] {
            let mut code = NativeCode::new();
            for entry in abis {
                code.add_entry(entry, 1);
            }
            assert!(
                matches!(code.wont_run(), Some(WontRun::ThirtyTwoBitOnly { .. })),
                "{abis:?}"
            );
            // Neither 64-bit ABI is ever chosen for it.
            let libraries = apk(abis);
            for translation in [Some(true), Some(false), None] {
                assert_eq!(install_abi(&libraries, || translation), Ok(None));
            }
        }
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
            Notice::Needs64Bit,
        ] {
            let key = notice.key("com.example.game");
            assert!(key.starts_with(crate::notifications::HOST_KEY_PREFIX));
            assert_eq!(Notice::parse(&key), Some((notice, "com.example.game")));
            let (english, _) = notice.text("Game", UiLanguage::English);
            let (korean, _) = notice.text("Game", UiLanguage::Korean);
            assert!(english.contains("Game") && korean.contains("Game"));
        }
        assert_eq!(Notice::parse("dsa:run-as-arm:offer:not a package"), None);
        assert_eq!(Notice::parse("dsa:run-as-arm:other:com.example.game"), None);
        assert_eq!(Notice::parse("0|com.example.game|1|null|10100"), None);
    }

    /// Every notice about running an app as ARM reads in every interface language: a title
    /// that names the app and a text, none empty and none with an em dash or a placeholder
    /// left in. A notification shown in a language with no translation reads in English.
    #[test]
    fn fr17_the_run_as_arm_notices_read_in_every_language() {
        let notices = [
            Notice::Offer,
            Notice::Done,
            Notice::NeedsErase,
            Notice::Failed,
            Notice::Needs64Bit,
        ];
        for lang in UiLanguage::ALL {
            for notice in notices {
                let (title, text) = notice.text("Game", lang);
                assert!(title.contains("Game"), "{lang:?} {notice:?}: {title}");
                assert!(!text.trim().is_empty(), "{lang:?} {notice:?}");
                for words in [&title, &text] {
                    assert!(
                        !words.contains(char::from_u32(0x2014).unwrap()),
                        "{lang:?}: {words}"
                    );
                    assert!(
                        crate::ui_language::placeholders(words).is_empty(),
                        "{lang:?}: {words}"
                    );
                }
            }
        }
        let app = crate::tasks::AppInfo {
            package_name: "com.example.game".into(),
            label: "Game".into(),
            ..Default::default()
        };
        let english = Notice::Offer.notification(&app);
        let shown = in_user_language(english.clone(), UiLanguage::from_tag_or_english("nl"));
        assert_eq!((shown.title, shown.text), (english.title, english.text));
        let shown = in_user_language(
            Notice::Offer.notification(&app),
            UiLanguage::from_tag_or_english("de-AT"),
        );
        assert_eq!(shown.title, "Game konnte nicht gestartet werden");
    }

    /// The native code of an APK of `files` (each with content, or empty), whose manifest asks
    /// to run as 32-bit code when `prefers_32_bit`.
    fn code(files: &[(&str, bool)], prefers_32_bit: bool) -> NativeCode {
        use crate::apk_info::axml::TYPE_INT_BOOLEAN;
        use crate::apk_info::fixture::{document, Attr, El};
        let application = if prefers_32_bit {
            vec![Attr::Typed(
                "use32bitAbi",
                USE_32BIT_ABI,
                TYPE_INT_BOOLEAN,
                u32::MAX,
            )]
        } else {
            Vec::new()
        };
        let manifest = document(&[
            El(
                "manifest",
                vec![Attr::Str("package", 0, "com.example.game")],
                false,
            ),
            El("application", application, false),
        ]);
        let mut entries: Vec<(&str, &[u8])> = vec![("AndroidManifest.xml", &manifest[..])];
        for (name, content) in files {
            entries.push((*name, if *content { &b"\x7fELF"[..] } else { &b""[..] }));
        }
        NativeCode::read(Cursor::new(archive(&entries, &[]))).expect("an APK")
    }

    #[test]
    fn fr11_an_app_without_64_bit_code_is_told_apart() {
        // Code the guest runs, or no code at all.
        assert_eq!(
            code(&[("lib/x86_64/libmain.so", true)], false).wont_run(),
            None
        );
        assert_eq!(
            code(&[("lib/arm64-v8a/libmain.so", true)], false).wont_run(),
            None
        );
        assert_eq!(code(&[], false).wont_run(), None);
        assert_eq!(
            code(&[("lib/x86/libplaceholder.so", false)], false).wont_run(),
            None
        );
        // 32-bit code only, with or without an empty 64-bit placeholder.
        let x86 = code(
            &[
                ("lib/x86/libmain.so", true),
                ("lib/armeabi-v7a/libmain.so", true),
            ],
            false,
        );
        assert_eq!(
            x86.wont_run(),
            Some(WontRun::ThirtyTwoBitOnly {
                abis: vec!["armeabi-v7a".into(), "x86".into()]
            })
        );
        let placeholder = code(
            &[
                ("lib/armeabi-v7a/libmain.so", true),
                ("lib/arm64-v8a/libplaceholder.so", false),
            ],
            true,
        );
        assert!(matches!(
            placeholder.wont_run(),
            Some(WontRun::ThirtyTwoBitOnly { .. })
        ));
        // Code for ABIs the guest does not run at all.
        assert_eq!(
            code(&[("lib/riscv64/libmain.so", true)], false).wont_run(),
            Some(WontRun::OtherAbis {
                abis: vec!["riscv64".into()]
            })
        );
        // A build made to run as 32-bit code whose 64-bit code is only a part of it.
        let partial = code(
            &[
                ("lib/x86/libmonochrome.so", true),
                ("lib/x86/libchrome.so", true),
                ("lib/x86_64/libmonochrome.so", true),
            ],
            true,
        );
        assert_eq!(
            partial.wont_run(),
            Some(WontRun::ThirtyTwoBitBuild {
                abi: "x86".into(),
                wide: X86_64
            })
        );
        let text = partial.wont_run().unwrap().to_string();
        assert!(
            text.contains("32-bit code (x86)") && text.contains("64-bit build"),
            "{text}"
        );
        // The same files without use32bitAbi run as x86_64, whose code Android picks.
        let wide = code(
            &[
                ("lib/x86/libmonochrome.so", true),
                ("lib/x86/libchrome.so", true),
                ("lib/x86_64/libmonochrome.so", true),
            ],
            false,
        );
        assert_eq!(wide.wont_run(), None);
        // A split's code counts with its base's manifest.
        let base = code(&[], true);
        let split = code(&[("lib/x86/libmain.so", true)], false);
        assert!(NativeCode::merged([&base, &split]).wont_run().is_some());
        let split64 = code(&[("lib/x86_64/libmain.so", true)], false);
        assert_eq!(
            NativeCode::merged([&base, &split, &split64]).wont_run(),
            None
        );
    }

    #[test]
    fn fr25_a_32_bit_build_that_ended_for_want_of_code_is_told_to_get_its_64_bit_build() {
        fn libraries(sets: &[(&str, &[&str])]) -> NativeLibraries {
            let mut all = NativeLibraries::new();
            for (abi, names) in sets {
                for name in names.iter() {
                    all.add(abi, name);
                }
            }
            all
        }
        let no_arm = RunAsArm {
            on: false,
            unavailable: Some(RunAsArmUnavailable::NoArmLibraries),
        };
        let chromium = "java.lang.RuntimeException: Starting in 64-bit mode requires the 64-bit \
                        native library.";
        // Chrome's 32-bit build: the same library name under both ABIs, and Chromium says it.
        let monochrome = libraries(&[
            ("x86", &["libmonochrome.so"][..]),
            (X86_64, &["libmonochrome.so"][..]),
        ]);
        assert!(needs_64_bit_build(&monochrome, no_arm, chromium));
        assert!(!needs_64_bit_build(
            &monochrome,
            no_arm,
            "dlopen failed: libfoo.so"
        ));
        // A 64-bit ABI that leaves out a 32-bit library says so itself.
        let partial = libraries(&[
            ("x86", &["libmain.so", "libextra.so"][..]),
            (X86_64, &["libmain.so"][..]),
        ]);
        assert!(needs_64_bit_build(
            &partial,
            no_arm,
            "dlopen failed: libextra.so"
        ));
        // An app with 64-bit code only, or one ARM translation can still help, is not.
        let wide = libraries(&[(X86_64, &["libmain.so"][..])]);
        assert!(!needs_64_bit_build(&wide, no_arm, chromium));
        let off = RunAsArm {
            on: false,
            unavailable: Some(RunAsArmUnavailable::TranslationOff),
        };
        assert!(!needs_64_bit_build(&partial, off, "dlopen failed"));
        // A 32-bit ARM app that runs as arm64 already has no way out either.
        let arm = libraries(&[
            ("armeabi-v7a", &["libmain.so", "libgame.so"][..]),
            (ARM64, &["libmain.so"][..]),
        ]);
        let on = RunAsArm {
            on: true,
            unavailable: Some(RunAsArmUnavailable::ArmOnly),
        };
        assert!(needs_64_bit_build(&arm, on, "dlopen failed: libgame.so"));
        let (title, text) = Notice::Needs64Bit.text("Chrome", UiLanguage::English);
        assert!(
            title.contains("Chrome") && text.contains("x86_64 build"),
            "{text}"
        );
        let (title, text) = Notice::Needs64Bit.text("Chrome", UiLanguage::Korean);
        assert!(
            title.contains("Chrome") && text.contains("x86_64판"),
            "{text}"
        );
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
