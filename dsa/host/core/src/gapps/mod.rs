//! Google Play for a user who asks for it: LiteGapps' Core build of Google's apps, which the
//! service downloads, turns into a system image and hands to the user's Android.
//!
//! DSA ships no Google app. On the user's request the service reads the list of builds
//! LiteGapps publishes on its file host for the Android version and architecture the running
//! Android reports ([`listing`]), downloads the newest and checks it against the MD5 digest the
//! host publishes, then makes an erofs image of the build's apps, permission lists, system
//! configuration and framework files ([`package`]). Every path in the build is checked first:
//! one that leaves the build (`..`, an absolute path) refuses the whole build, and so does a
//! build for another architecture or Android version. The image goes to the user's Android,
//! which keeps it on the user's disk (`/data/vendor/dsa/gapps/system.img`), and the build's
//! description goes with the user's settings. While one is recorded, each boot's bootconfig
//! carries [`BOOTCONFIG_ON`], and the guest then lays the image over its `/product` and
//! `/system_ext` before its package manager starts. Every change applies from the next start of
//! Android, because the package manager reads the system apps and their permissions once.
//!
//! ```text
//! image root
//!   product/{app,priv-app,etc,framework}/...      over /product
//!   system_ext/{app,priv-app,etc,framework}/...   over /system_ext
//! ```

pub mod listing;
pub mod md5;
pub mod package;
pub mod tar;
pub mod xz;

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::settings::{user_file_stem, SettingsStore};
use crate::update::store::Fetch;

/// The bootconfig line that lays the Google apps over the system for one boot.
pub const BOOTCONFIG_ON: &str = "androidboot.dsa.gapps = \"on\"\n";
/// LiteGapps' name for the build DSA installs: Google Play services, the Play Store, the
/// services framework, and the account sync adapters.
pub const VARIANT: &str = "core";
/// The API levels of the Android versions DSA runs, which LiteGapps builds for.
pub const SDKS: [u32; 2] = [36, 37];
/// Google's terms, which Google's apps follow.
pub const TERMS_URL: &str = "https://policies.google.com/terms";
/// Google's page where a user registers a device Google has not certified, as DSA's Android
/// is, so that Google's apps let them sign in.
pub const REGISTRATION_URL: &str = "https://www.google.com/android/uncertified/";

/// The build of Google's apps a user installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GappsPackage {
    /// LiteGapps' name for the build: [`VARIANT`].
    pub variant: String,
    /// The build's date, yyyymmdd.
    pub version: String,
    /// The Android version the build is for, as LiteGapps names it ("16.0").
    pub android_version: String,
    /// The Android API level the build is for.
    pub sdk: u32,
    /// The architecture the build is for, as LiteGapps names it ([`Target::arch`]).
    pub arch: String,
    /// The file downloaded.
    pub file: String,
    /// Where it came from.
    pub source: String,
    /// The MD5 digest the file host published for it.
    pub md5: String,
}

/// What a build must be for: the Android the user runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// LiteGapps' name for the architecture: `x86_64` or `arm64`.
    pub arch: &'static str,
    pub sdk: u32,
}

impl Target {
    /// The build a guest with API level `sdk` and primary ABI `abi` needs, or why there is none.
    pub fn for_guest(sdk: u32, abi: &str) -> Result<Target, String> {
        let arch = match abi {
            "x86_64" => "x86_64",
            "arm64-v8a" => "arm64",
            "" => return Err("Android did not say which processor it runs on".into()),
            other => {
                return Err(format!(
                    "Android runs on {other}, which DSA installs Google Play for only on \
                     x86_64 and arm64-v8a"
                ))
            }
        };
        if !SDKS.contains(&sdk) {
            return Err(format!(
                "Android here has API level {sdk}; DSA installs Google Play for API levels {} \
                 and {}",
                SDKS[0], SDKS[1]
            ));
        }
        Ok(Target { arch, sdk })
    }
}

/// What a boot's bootconfig carries for the Google apps: [`BOOTCONFIG_ON`] while a build is
/// installed, nothing otherwise.
pub fn bootconfig(installed: Option<&GappsPackage>) -> &'static str {
    if installed.is_some() {
        BOOTCONFIG_ON
    } else {
        ""
    }
}

/// The bootconfig for `user`'s next boot, from what their settings record. A record that cannot
/// be read is logged and counts as none, so Android still starts.
pub fn bootconfig_for(store: &dyn SettingsStore, user: &str) -> String {
    let package = store.gapps(user).unwrap_or_else(|why| {
        eprintln!("gapps: {why}; starting {user}'s Android without Google's apps");
        None
    });
    bootconfig(package.as_ref()).to_string()
}

/// Where the Google apps stand in the running guest, as it says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestGapps {
    /// The image is on the user's disk.
    pub installed: bool,
    /// This boot runs with the image laid over the system.
    pub active: bool,
    /// The guest's API level.
    pub sdk: u32,
    /// The guest's primary ABI.
    pub abi: String,
    /// The ABIs the guest installs apps for, in the order it prefers them.
    pub abilist: String,
    /// The Google Services Framework Android ID, in decimal; empty until there is one.
    pub device_id: String,
}

/// Whether what the user has installed differs from what the running guest booted with, so
/// that it applies only once Android starts again.
pub fn restart_needed(installed: Option<&GappsPackage>, guest: Option<&GuestGapps>) -> bool {
    guest.is_some_and(|guest| guest.active != installed.is_some())
}

/// What an install is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Reading LiteGapps' list of builds.
    Listing,
    /// Downloading the build.
    Downloading,
    /// Checking the build and making its image.
    Building,
    /// Sending the image to Android.
    Sending,
}

/// News of an install: what it is doing, and how far it has got (`done` of `total` bytes;
/// both 0 when there is nothing to count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub stage: Stage,
    pub done: u64,
    pub total: u64,
}

/// How often news of bytes is given: every this many bytes, and at the end.
const NEWS_STEP: u64 = 4 << 20;

/// Where the service gets Google Play from, and where it makes it ready.
pub struct Source {
    fetch: Arc<dyn Fetch + Send + Sync>,
    /// LiteGapps' project on its file host ([`listing::PROJECT`]).
    project: String,
    /// The folder downloads and images are made in, a folder of each user's in it; the service
    /// owns it, and nothing stays there once an install ends.
    work_dir: PathBuf,
    /// Held for the length of one install, so two do not share the folder or the bandwidth.
    turn: Mutex<()>,
}

/// A build downloaded, checked and made into an image for a user's Android. The image is
/// removed when this is dropped.
pub struct Prepared {
    pub package: GappsPackage,
    pub image: PathBuf,
    pub size: u64,
    pub sha256: Vec<u8>,
    pub built: package::Built,
}

impl Drop for Prepared {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.image);
    }
}

impl Source {
    /// Downloads through `fetch` from LiteGapps' project, and makes builds ready in `work_dir`.
    pub fn new(fetch: Arc<dyn Fetch + Send + Sync>, work_dir: impl Into<PathBuf>) -> Source {
        Source {
            fetch,
            project: listing::PROJECT.to_string(),
            work_dir: work_dir.into(),
            turn: Mutex::new(()),
        }
    }

    /// Reads the builds from `project` instead, a mirror with the same layout.
    pub fn with_project(mut self, project: &str) -> Source {
        self.project = project.trim_end_matches('/').to_string();
        self
    }

    /// Finds the newest build for `target`, downloads and checks it, and makes its image for
    /// `user`, telling `progress` how it goes. The download is removed once the image is made.
    pub fn prepare(
        &self,
        user: &str,
        target: &Target,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Prepared, String> {
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.work_dir.join(user_file_stem(user)?);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("{} cannot be made: {e}", dir.display()))?;
        progress(Progress {
            stage: Stage::Listing,
            done: 0,
            total: 0,
        });
        let build = listing::newest(&*self.fetch, &self.project, target)?;
        let total = build.size;
        let zip = dir.join("gapps-download.zip");
        let mut told = 0u64;
        progress(Progress {
            stage: Stage::Downloading,
            done: 0,
            total,
        });
        let package = listing::download(
            &*self.fetch,
            &self.project,
            target,
            &build,
            &zip,
            &mut |done| {
                if done >= told + NEWS_STEP || done == total {
                    told = done;
                    progress(Progress {
                        stage: Stage::Downloading,
                        done,
                        total,
                    });
                }
            },
        )?;
        progress(Progress {
            stage: Stage::Building,
            done: 0,
            total: 0,
        });
        let image = dir.join("gapps-system.img");
        let built = package::build_image(&zip, target, &image);
        let _ = std::fs::remove_file(&zip);
        let built = built?;
        let measured = std::fs::metadata(&image)
            .map_err(|e| format!("{} cannot be read: {e}", image.display()))
            .and_then(|metadata| {
                crate::arm_translation::file_sha256(&image).map(|sha| (metadata.len(), sha))
            });
        match measured {
            Ok((size, sha256)) => Ok(Prepared {
                package,
                image,
                size,
                sha256,
                built,
            }),
            Err(why) => {
                let _ = std::fs::remove_file(&image);
                Err(why)
            }
        }
    }
}

/// A reader that says how many bytes have been read, every [`NEWS_STEP`] bytes and at the end.
pub struct Reporting<'a> {
    inner: &'a mut dyn Read,
    done: u64,
    told: u64,
    report: &'a mut dyn FnMut(u64),
}

impl<'a> Reporting<'a> {
    pub fn new(inner: &'a mut dyn Read, report: &'a mut dyn FnMut(u64)) -> Reporting<'a> {
        Reporting {
            inner,
            done: 0,
            told: 0,
            report,
        }
    }
}

impl Read for Reporting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.done += read as u64;
        if self.done >= self.told + NEWS_STEP || (read == 0 && self.done > self.told) {
            self.told = self.done;
            (self.report)(self.done);
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package() -> GappsPackage {
        GappsPackage {
            variant: "core".into(),
            version: "20261001".into(),
            android_version: "16.0".into(),
            sdk: 36,
            arch: "x86_64".into(),
            file: "LiteGapps-core-x86_64-16.0-20261001-official.zip".into(),
            source: "https://example.com/LiteGapps-core-x86_64-16.0-20261001-official.zip".into(),
            md5: "ea9945a6d63849fa9e51782edfe613a4".into(),
        }
    }

    #[test]
    fn fr32_the_bootconfig_lays_the_google_apps_over_only_while_they_are_installed() {
        assert_eq!(bootconfig(None), "");
        assert_eq!(
            bootconfig(Some(&package())),
            "androidboot.dsa.gapps = \"on\"\n"
        );
    }

    #[test]
    fn fr32_a_build_is_chosen_for_the_guests_architecture_and_android_version() {
        assert_eq!(
            Target::for_guest(36, "x86_64"),
            Ok(Target {
                arch: "x86_64",
                sdk: 36
            })
        );
        assert_eq!(
            Target::for_guest(37, "arm64-v8a"),
            Ok(Target {
                arch: "arm64",
                sdk: 37
            })
        );
        assert!(Target::for_guest(35, "x86_64").is_err());
        assert!(Target::for_guest(36, "armeabi-v7a").is_err());
        assert!(Target::for_guest(36, "").is_err());
    }

    #[test]
    fn fr32_a_restart_is_needed_when_the_guest_booted_otherwise() {
        let installed = package();
        let active = GuestGapps {
            installed: true,
            active: true,
            ..GuestGapps::default()
        };
        let inactive = GuestGapps::default();
        assert!(!restart_needed(Some(&installed), None), "nothing known");
        assert!(
            restart_needed(Some(&installed), Some(&inactive)),
            "installed"
        );
        assert!(!restart_needed(Some(&installed), Some(&active)));
        assert!(restart_needed(None, Some(&active)), "removed");
        assert!(!restart_needed(None, Some(&inactive)));
    }
}
