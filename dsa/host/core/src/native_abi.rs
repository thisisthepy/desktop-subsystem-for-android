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
            (Notice::Needs64Bit, false) => (
                format!("{label} cannot run in DSA"),
                "This build of it runs as 32-bit code, and DSA's Android runs 64-bit code only. \
                 Uninstall it and install its x86_64 build instead (or its arm64-v8a build, \
                 with ARM translation on)."
                    .into(),
            ),
            (Notice::Needs64Bit, true) => (
                format!("{label}을(를) DSA에서 실행할 수 없습니다"),
                "이 빌드는 32비트 코드로 실행되는데, DSA의 Android는 64비트 코드만 \
                 실행합니다. 이 앱을 지우고 x86_64판을 대신 설치하십시오(ARM 번역이 켜져 \
                 있으면 arm64-v8a판도 됩니다)."
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
            Notice::Needs64Bit,
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
        let (title, text) = Notice::Needs64Bit.text("Chrome", false);
        assert!(
            title.contains("Chrome") && text.contains("x86_64 build"),
            "{text}"
        );
        let (title, text) = Notice::Needs64Bit.text("Chrome", true);
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
