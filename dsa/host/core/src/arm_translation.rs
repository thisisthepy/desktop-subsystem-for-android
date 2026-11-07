//! ARM app translation: a translation package (a native bridge with its ARM64 libraries), and
//! what the user's Android is told at boot.
//!
//! Most apps ship ARM native code only. The guest is x86_64, so such an app installs and runs
//! only with a translator, which plugs into Android as its native bridge. A translator comes as
//! a package: a folder holding
//!
//! ```text
//! arm-translation.json   {"name": "...", "version": "...", "library": "libberberis_arm64.so",
//!                         "abis": ["arm64-v8a"], "license": "Apache-2.0"}
//! system.img             an erofs image whose system/lib64, system/bin and system/etc are
//!                        laid over the guest's, with system/lib64/libdsa_native_bridge.so
//!                        being (or linking to) the bridge library
//! ```
//!
//! There are three, and the user chooses ([`Source`]):
//!
//! - the image's default package: an x86_64 image carries one (Digitalis, an open source
//!   translator) in its system partition, which its `image.json` describes under
//!   `arm_translation` ([`default_package`]). An arm64 image runs ARM apps itself and has none;
//! - a package the user installs: the host checks the description and the image's erofs
//!   signature, sends the image to the user's Android, which keeps it on the user's disk, and
//!   keeps the description with the user's settings;
//! - Google's translator, which [`google`] makes into such a package on the user's request:
//!   the user's computer downloads it from Google, and DSA never ships it.
//!
//! The package a user installed (theirs or Google's) is used in place of the default one, and
//! removing it brings the default one back. While the user's `arm-translation` setting is on,
//! each boot's bootconfig carries [`BOOTCONFIG_ON`] for an installed package or
//! [`BOOTCONFIG_DEFAULT`] for the image's own: the guest then imports the bridge and ABI
//! properties and mounts that package's image before its zygote starts. Without either the
//! guest is exactly the image as made, so an x86_64 app sees no difference. The ABIs Android
//! installs apps for are fixed at boot, so every change applies from the next start of Android;
//! the service records what each boot was told ([`bootconfig_for`]), so it can say when what the
//! user chose differs from what Android runs.

pub mod google;

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::SettingsStore;

/// The package's description, in its folder.
pub const MANIFEST: &str = "arm-translation.json";
/// The package's system image, in its folder.
pub const IMAGE: &str = "system.img";
/// The bootconfig line that turns translation on for one boot, with the package the user
/// installed.
pub const BOOTCONFIG_ON: &str = "androidboot.dsa.arm_translation = \"on\"\n";
/// The bootconfig line that turns translation on for one boot, with the image's default
/// package.
pub const BOOTCONFIG_DEFAULT: &str = "androidboot.dsa.arm_translation = \"default\"\n";
/// Where an image that has a default package carries its system image, as `image.json` says.
pub const DEFAULT_SYSTEM_IMAGE: &str = "/system/etc/dsa/arm-translation/default.img";
/// The only ABI a package may translate: the guest has no 32-bit zygote.
pub const ARM64: &str = "arm64-v8a";
/// The largest image taken.
pub const MAX_IMAGE_BYTES: u64 = 2 << 30;

/// Where erofs keeps its superblock, and the signature it starts with.
const EROFS_SUPERBLOCK: usize = 1024;
const EROFS_MAGIC: u32 = 0xE0F5_E1E2;

/// What a package says of itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// The translator's native bridge library, a file name in the image's `system/lib64`.
    pub library: String,
    /// The ABIs it translates.
    pub abis: Vec<String>,
    /// Its licence, as an SPDX expression.
    #[serde(default)]
    pub license: String,
}

/// A text field of the description: printable, without spaces at either end, not too long.
fn check_text(what: &str, value: &str, max: usize) -> Result<(), String> {
    if value.is_empty() || value.chars().count() > max {
        return Err(format!(
            "the package's {what} must have from 1 to {max} characters"
        ));
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(format!(
            "the package's {what} {value:?} has spaces at an end or a control character"
        ));
    }
    Ok(())
}

impl Package {
    /// Checks the description, and says what is wrong with it.
    pub fn check(&self) -> Result<(), String> {
        check_text("name", &self.name, 64)?;
        check_text("version", &self.version, 64)?;
        if !self.license.is_empty() {
            check_text("license", &self.license, 128)?;
        }
        let library_ok = self.library.len() <= 128
            && self.library.ends_with(".so")
            && self.library.len() > 3
            && self
                .library
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !library_ok {
            return Err(format!(
                "the bridge library {:?} is not a library file name (letters, digits, '.', \
                 '_' and '-', ending in .so)",
                self.library
            ));
        }
        if self.abis.is_empty() {
            return Err(format!(
                "the package names no ABI; it must translate {ARM64}"
            ));
        }
        if let Some(other) = self.abis.iter().find(|abi| abi.as_str() != ARM64) {
            return Err(format!(
                "the package translates {other}, but Android here runs only 64-bit apps, so \
                 only {ARM64} is taken"
            ));
        }
        Ok(())
    }
}

/// Whether `header`, the start of an image, is an erofs file system.
pub fn is_erofs(header: &[u8]) -> bool {
    header
        .get(EROFS_SUPERBLOCK..EROFS_SUPERBLOCK + 4)
        .map(|magic| u32::from_le_bytes([magic[0], magic[1], magic[2], magic[3]]) == EROFS_MAGIC)
        .unwrap_or(false)
}

/// A package folder, checked.
#[derive(Debug, Clone)]
pub struct PackageDir {
    pub package: Package,
    pub image: PathBuf,
    pub size: u64,
}

/// Reads and checks the package in `dir`: its description, and that its image is erofs and
/// not too large.
pub fn open_package(dir: &Path) -> Result<PackageDir, String> {
    let manifest = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&manifest).map_err(|e| {
        format!(
            "{} is not an ARM translation package: {} cannot be read ({e})",
            dir.display(),
            manifest.display()
        )
    })?;
    let package: Package = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a usable description: {e}", manifest.display()))?;
    package.check()?;
    let image = dir.join(IMAGE);
    let mut file = std::fs::File::open(&image)
        .map_err(|e| format!("the package's {} cannot be read: {e}", image.display()))?;
    let size = file
        .metadata()
        .map_err(|e| format!("the package's {} cannot be read: {e}", image.display()))?
        .len();
    if size == 0 || size > MAX_IMAGE_BYTES {
        return Err(format!(
            "the package's {IMAGE} is {size} bytes; it must have from 1 byte to {} MiB",
            MAX_IMAGE_BYTES >> 20
        ));
    }
    let mut header = Vec::with_capacity(EROFS_SUPERBLOCK + 4);
    let _ = file
        .by_ref()
        .take((EROFS_SUPERBLOCK + 4) as u64)
        .read_to_end(&mut header);
    if !is_erofs(&header) {
        return Err(format!(
            "the package's {IMAGE} is not an erofs image; make the package with \
             dsa/guest/arm-translation/make-package.py"
        ));
    }
    Ok(PackageDir {
        package,
        image,
        size,
    })
}

/// The SHA-256 digest of the file at `path`, which the guest checks the image it receives
/// against.
pub fn file_sha256(path: &Path) -> Result<Vec<u8>, String> {
    use sha2::Digest;
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(hasher.finalize().to_vec()),
            Ok(read) => hasher.update(&buffer[..read]),
            Err(e) => return Err(format!("{} cannot be read: {e}", path.display())),
        }
    }
}

/// Where a translator comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The image's own default package.
    Default,
    /// A package the user installed.
    User,
    /// Google's translator, which the user fetched and installed.
    Google,
}

impl Source {
    /// The word the local API and `dsa arm-translation --json` use.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::User => "user",
            Source::Google => "google",
        }
    }

    /// The source a word names; `None` for any other word, the empty one included.
    pub fn parse(word: &str) -> Option<Source> {
        match word {
            "default" => Some(Source::Default),
            "user" => Some(Source::User),
            "google" => Some(Source::Google),
            _ => None,
        }
    }

    /// Where an installed package comes from: Google's translator when it is what
    /// [`google`] makes, the user's own otherwise.
    pub fn of_installed(package: &Package) -> Source {
        if package.name == google::NAME && package.library == google::LIBRARY {
            Source::Google
        } else {
            Source::User
        }
    }
}

/// A translator Android uses: where it comes from, and its description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Translator {
    pub source: Source,
    pub package: Package,
}

/// The translator Android uses with these settings: the package the user installed while
/// there is one, else the image's default package; none while the user's setting is off.
pub fn choose(
    installed: Option<&Package>,
    default: Option<&Package>,
    enabled: bool,
) -> Option<Translator> {
    if !enabled {
        return None;
    }
    match (installed, default) {
        (Some(package), _) => Some(Translator {
            source: Source::of_installed(package),
            package: package.clone(),
        }),
        (None, Some(package)) => Some(Translator {
            source: Source::Default,
            package: package.clone(),
        }),
        (None, None) => None,
    }
}

/// What a boot's bootconfig carries for ARM translation: [`BOOTCONFIG_ON`] for a package the
/// user installed, [`BOOTCONFIG_DEFAULT`] for the image's own, nothing without a translator.
pub fn bootconfig(chosen: Option<&Translator>) -> &'static str {
    match chosen.map(|translator| translator.source) {
        Some(Source::User | Source::Google) => BOOTCONFIG_ON,
        Some(Source::Default) => BOOTCONFIG_DEFAULT,
        None => "",
    }
}

/// The default package an image's `image.json` describes. Only an x86_64 image has one, and
/// only one whose system partition carries it at [`DEFAULT_SYSTEM_IMAGE`]; a description the
/// host would refuse from a user counts as none.
pub fn default_from_manifest(manifest: &serde_json::Value) -> Option<Package> {
    let arch = manifest
        .get("arch")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("x86_64");
    if arch != "x86_64" {
        return None;
    }
    let entry = manifest.get("arm_translation")?;
    if entry
        .get("system_image")
        .and_then(serde_json::Value::as_str)
        != Some(DEFAULT_SYSTEM_IMAGE)
    {
        return None;
    }
    let package: Package = serde_json::from_value(serde_json::json!({
        "name": entry.get("name")?,
        "version": entry.get("version")?,
        "library": entry.get("library")?,
        "abis": entry.get("abis")?,
        "license": entry.get("license").cloned().unwrap_or_else(|| "".into()),
    }))
    .ok()?;
    package.check().ok().map(|()| package)
}

/// The default package of the image in `dir`, from its `image.json`.
pub fn default_package(dir: &Path) -> Option<Package> {
    let text = std::fs::read_to_string(dir.join("image.json")).ok()?;
    default_from_manifest(&serde_json::from_str(&text).ok()?)
}

/// The bootconfig for `user`'s next boot of the image in `image_dir`, from what their settings
/// record: the translator [`choose`] picks. It records that choice as what this boot was told,
/// and logs it. A record that cannot be read counts as none, so Android still starts.
pub fn bootconfig_for(store: &dyn SettingsStore, user: &str, image_dir: Option<&Path>) -> String {
    let installed = store.arm_translation(user).unwrap_or_else(|why| {
        eprintln!("arm translation: {user}'s package is not known ({why}); booting without it");
        None
    });
    let enabled = store.get(user).map(|s| s.arm_translation).unwrap_or(true);
    let default = image_dir.and_then(default_package);
    let chosen = choose(installed.as_ref(), default.as_ref(), enabled);
    match &chosen {
        Some(translator) => eprintln!(
            "arm translation: {user}'s Android starts with {} {} ({})",
            translator.package.name,
            translator.package.version,
            match translator.source {
                Source::Default => "the image's default translator",
                Source::User => "the package they installed",
                Source::Google => "Google's translator, which they installed",
            }
        ),
        None if !enabled && (installed.is_some() || default.is_some()) => {
            eprintln!("arm translation: {user}'s Android starts without it (turned off)")
        }
        None => {}
    }
    if let Err(why) = store.set_arm_translation_boot(user, chosen.as_ref()) {
        eprintln!("arm translation: what {user}'s Android starts with is not recorded: {why}");
    }
    bootconfig(chosen.as_ref()).to_string()
}

/// Where ARM translation stands in the running guest, as its control daemon says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestArmTranslation {
    /// A package image the user installed is on the user's disk.
    pub installed: bool,
    /// This boot runs with a package (the user's or the image's default) laid over the system.
    pub active: bool,
    /// The ABIs the guest's package manager installs apps for.
    pub abilist: String,
}

/// Whether the translator the user's settings choose (`wanted`) differs from what the running
/// guest booted with, so that it applies only once Android starts again. `booted` is what the
/// service told that boot to use; whether the guest has a translator laid over its system at
/// all comes from the guest itself.
pub fn restart_needed(
    wanted: Option<&Translator>,
    booted: Option<&Translator>,
    guest: Option<&GuestArmTranslation>,
) -> bool {
    let Some(guest) = guest else {
        return false;
    };
    if guest.active != wanted.is_some() {
        return true;
    }
    // Both run a translator: a different one (a package installed or removed while Android
    // ran) applies from the next start. A boot the service did not record says nothing more.
    guest.active && booted.is_some() && booted != wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> Package {
        Package {
            name: "Digitalis".into(),
            version: "16.0.1".into(),
            library: "libberberis_arm64.so".into(),
            abis: vec!["arm64-v8a".into()],
            license: "Apache-2.0".into(),
        }
    }

    fn erofs(size: usize) -> Vec<u8> {
        let mut image = vec![0u8; size];
        image[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
        image
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-arm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fr25_a_package_must_name_a_library_file_and_translate_arm64_only() {
        assert_eq!(package().check(), Ok(()));
        for library in [
            "",
            "libbridge",
            "../lib/libx.so",
            "lib x.so",
            "/system/lib64/l.so",
        ] {
            let bad = Package {
                library: library.into(),
                ..package()
            };
            assert!(bad.check().is_err(), "{library:?} is taken");
        }
        for abis in [
            vec![],
            vec!["armeabi-v7a".to_string()],
            vec!["x86_64".to_string()],
        ] {
            let bad = Package {
                abis: abis.clone(),
                ..package()
            };
            assert!(bad.check().is_err(), "{abis:?} is taken");
        }
        let blank = Package {
            name: " ".into(),
            ..package()
        };
        assert!(blank.check().is_err());
    }

    #[test]
    fn fr25_a_package_folder_needs_its_description_and_an_erofs_image() {
        let dir = scratch("folder");
        assert!(open_package(&dir).unwrap_err().contains(MANIFEST));
        std::fs::write(
            dir.join(MANIFEST),
            serde_json::to_string(&package()).unwrap(),
        )
        .unwrap();
        assert!(open_package(&dir).is_err(), "no image");
        std::fs::write(dir.join(IMAGE), vec![7u8; 4096]).unwrap();
        assert!(open_package(&dir).unwrap_err().contains("erofs"));
        std::fs::write(dir.join(IMAGE), erofs(4096)).unwrap();
        let opened = open_package(&dir).unwrap();
        assert_eq!(opened.package, package());
        assert_eq!(opened.size, 4096);
        assert_eq!(opened.image, dir.join(IMAGE));
        let wrong = Package {
            abis: vec!["armeabi-v7a".into()],
            ..package()
        };
        std::fs::write(dir.join(MANIFEST), serde_json::to_string(&wrong).unwrap()).unwrap();
        assert!(open_package(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn default_package_of_image() -> Package {
        Package {
            name: "Digitalis".into(),
            version: "17-abefbe2b1600".into(),
            library: "libberberis_arm64.so".into(),
            abis: vec!["arm64-v8a".into()],
            license: "Apache-2.0".into(),
        }
    }

    fn google_package() -> Package {
        Package {
            name: google::NAME.into(),
            version: "16 (BE2A.250530.026.F3)".into(),
            library: google::LIBRARY.into(),
            abis: vec!["arm64-v8a".into()],
            license: google::LICENSE.into(),
        }
    }

    /// image.json of an image made with a default package, as make-image.py writes it.
    fn manifest(arch: &str) -> serde_json::Value {
        serde_json::json!({
            "version": "17.0.0",
            "arch": arch,
            "files": {},
            "arm_translation": {
                "file": "arm-translation.zip",
                "size": 4096,
                "sha256": "00",
                "name": "Digitalis",
                "version": "17-abefbe2b1600",
                "library": "libberberis_arm64.so",
                "abis": ["arm64-v8a"],
                "license": "Apache-2.0",
                "system_image": "/system/etc/dsa/arm-translation/default.img",
                "system_image_sha256": "00",
            },
        })
    }

    #[test]
    fn fr25_the_bootconfig_turns_translation_on_only_with_a_package_and_the_setting() {
        let installed = package();
        let default = default_package_of_image();
        let config =
            |installed, default, enabled| bootconfig(choose(installed, default, enabled).as_ref());
        assert_eq!(config(None, None, true), "");
        assert_eq!(config(Some(&installed), None, false), "");
        assert_eq!(
            config(Some(&installed), None, true),
            "androidboot.dsa.arm_translation = \"on\"\n"
        );
        // The image's default package, from the first start, while the user has none.
        assert_eq!(
            config(None, Some(&default), true),
            "androidboot.dsa.arm_translation = \"default\"\n"
        );
        assert_eq!(config(None, Some(&default), false), "", "turned off");
        // A package the user installed, theirs or Google's, is used in its place.
        assert_eq!(
            config(Some(&installed), Some(&default), true),
            BOOTCONFIG_ON
        );
        assert_eq!(
            config(Some(&google_package()), Some(&default), true),
            BOOTCONFIG_ON
        );
        assert_eq!(config(Some(&installed), Some(&default), false), "");
    }

    #[test]
    fn fr25_the_default_package_is_used_until_the_user_installs_one_and_again_once_it_is_removed() {
        let default = default_package_of_image();
        let first = choose(None, Some(&default), true).unwrap();
        assert_eq!(first.source, Source::Default);
        assert_eq!(first.package, default);
        let user = choose(Some(&package()), Some(&default), true).unwrap();
        assert_eq!((user.source, user.package), (Source::User, package()));
        let google = choose(Some(&google_package()), Some(&default), true).unwrap();
        assert_eq!(google.source, Source::Google);
        // Removing the installed package brings the default back.
        assert_eq!(choose(None, Some(&default), true), Some(first));
        // Without a default package (an arm64 image), nothing is used until one is installed.
        assert_eq!(choose(None, None, true), None);
        for source in [Source::Default, Source::User, Source::Google] {
            assert_eq!(Source::parse(source.as_str()), Some(source));
        }
        assert_eq!(Source::parse(""), None);
    }

    #[test]
    fn fr25_only_an_x86_64_image_that_carries_its_default_package_has_one() {
        assert_eq!(
            default_from_manifest(&manifest("x86_64")),
            Some(default_package_of_image())
        );
        // An arm64 image runs ARM apps itself.
        assert_eq!(default_from_manifest(&manifest("arm64")), None);
        let mut none = manifest("x86_64");
        none.as_object_mut().unwrap().remove("arm_translation");
        assert_eq!(default_from_manifest(&none), None);
        // A release whose zip is the only copy: the system partition does not have it.
        let mut zip_only = manifest("x86_64");
        zip_only["arm_translation"]
            .as_object_mut()
            .unwrap()
            .remove("system_image");
        assert_eq!(default_from_manifest(&zip_only), None);
        let mut refused = manifest("x86_64");
        refused["arm_translation"]["abis"] = serde_json::json!(["armeabi-v7a"]);
        assert_eq!(default_from_manifest(&refused), None);
        let dir = scratch("image-json");
        assert_eq!(default_package(&dir), None);
        std::fs::write(dir.join("image.json"), manifest("x86_64").to_string()).unwrap();
        assert_eq!(default_package(&dir), Some(default_package_of_image()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr25_each_boot_records_the_translator_it_was_told_to_use() {
        use crate::settings::{MemorySettings, Settings};
        let dir = scratch("boot-image");
        std::fs::write(dir.join("image.json"), manifest("x86_64").to_string()).unwrap();
        let store = MemorySettings::default();
        let user = "uid:1000";
        assert_eq!(bootconfig_for(&store, user, Some(&dir)), BOOTCONFIG_DEFAULT);
        assert_eq!(
            store.arm_translation_boot(user).unwrap().map(|t| t.source),
            Some(Source::Default)
        );
        store.set_arm_translation(user, Some(&package())).unwrap();
        assert_eq!(bootconfig_for(&store, user, Some(&dir)), BOOTCONFIG_ON);
        assert_eq!(
            store.arm_translation_boot(user).unwrap(),
            Some(Translator {
                source: Source::User,
                package: package()
            })
        );
        store
            .set(
                user,
                &Settings {
                    arm_translation: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert_eq!(bootconfig_for(&store, user, Some(&dir)), "");
        assert_eq!(store.arm_translation_boot(user).unwrap(), None);
        // No image known: only an installed package can be used.
        store.set(user, &Settings::default()).unwrap();
        store.set_arm_translation(user, None).unwrap();
        assert_eq!(bootconfig_for(&store, user, None), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr25_a_restart_is_needed_when_the_guest_booted_otherwise() {
        let user = choose(Some(&package()), None, true);
        let default = choose(None, Some(&default_package_of_image()), true);
        let google = choose(Some(&google_package()), None, true);
        let active = GuestArmTranslation {
            installed: true,
            active: true,
            abilist: "x86_64,arm64-v8a".into(),
        };
        let inactive = GuestArmTranslation::default();
        assert!(!restart_needed(user.as_ref(), None, None), "nothing known");
        assert!(restart_needed(user.as_ref(), None, Some(&inactive)));
        assert!(!restart_needed(user.as_ref(), user.as_ref(), Some(&active)));
        assert!(
            restart_needed(None, user.as_ref(), Some(&active)),
            "removed"
        );
        assert!(
            restart_needed(None, default.as_ref(), Some(&active)),
            "turned off"
        );
        assert!(!restart_needed(None, None, Some(&inactive)));
        // Switching between translators while Android runs.
        assert!(restart_needed(
            google.as_ref(),
            default.as_ref(),
            Some(&active)
        ));
        assert!(restart_needed(
            default.as_ref(),
            user.as_ref(),
            Some(&active)
        ));
        assert!(restart_needed(
            user.as_ref(),
            google.as_ref(),
            Some(&active)
        ));
        assert!(!restart_needed(
            default.as_ref(),
            default.as_ref(),
            Some(&active)
        ));
        // A boot the service did not record: only whether a translator runs is known.
        assert!(!restart_needed(default.as_ref(), None, Some(&active)));
    }
}
