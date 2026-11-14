//! How DSA itself is updated on Linux: by the system's package manager, never by DSA.
//!
//! The .deb and the .rpm register DSA's signed package repository when they are installed,
//! so a new version arrives through `apt`, `dnf`, `zypper` or the desktop's software center
//! like any other package. The service therefore carries no updater of its own. `dsa update`
//! is answered with where updates come from instead of an error, so the user learns what to
//! run rather than that something is missing.
//!
//! The repository comes in two release channels, as the program lists do on Windows: the
//! apt suites `stable` and `prerelease`, and the dnf and zypper repositories under `rpm/`
//! and `rpm-prerelease/`. The pre-release ones carry the stable packages too, so the package
//! manager installs the higher version. A package writes the entry of its own channel when
//! it is first installed and keeps the channel an entry already has when it is upgraded.
//! When the machine's `prerelease-updates` setting changes, the service points the entries
//! that are there at the chosen channel ([`ChannelFollowingSettings`]). An entry the user
//! removed is not written again.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dsa_host_core::app_windows::AppWindowSettings;
use dsa_host_core::arm_translation::{Package, Translator};
use dsa_host_core::gapps::GappsPackage;
use dsa_host_core::host_api::{ProgramUpdate, UpdateProgram};
use dsa_host_core::settings::{MachineSettings, RootApp, Settings, SettingsStore};
use dsa_host_core::update::program::ReleaseChannel;

/// What `dsa update` and the settings say about updating DSA on Linux.
pub const PACKAGE_MANAGER_UPDATES: &str =
    "DSA is updated by the system's package manager from DSA's package repository: \
     use the software center, or run `sudo apt update && sudo apt upgrade` (Debian, Ubuntu), \
     `sudo dnf upgrade` (Fedora) or `sudo zypper update` (openSUSE)";

/// Answers `dsa update` by pointing at the package manager. It never downloads or installs.
#[derive(Debug, Default, Clone, Copy)]
pub struct PackageManagerUpdates;

impl UpdateProgram for PackageManagerUpdates {
    fn update_now(&self) -> Result<ProgramUpdate, String> {
        Ok(ProgramUpdate {
            installing: None,
            detail: PACKAGE_MANAGER_UPDATES.to_string(),
        })
    }
}

/// Points the text of one repository entry at a channel's repository, or gives `None` when the
/// text is not an entry DSA's package wrote.
type RewriteEntry = fn(&str, ReleaseChannel) -> Option<String>;

/// The repository entries DSA's packages write when they are installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryEntries {
    /// apt's entry, in the deb822 format.
    pub apt: PathBuf,
    /// dnf's entry.
    pub dnf: PathBuf,
    /// zypper's entry, which the .rpm writes where zypper is.
    pub zypper: PathBuf,
}

impl RepositoryEntries {
    /// Where the .deb and the .rpm write them.
    pub fn system() -> RepositoryEntries {
        RepositoryEntries::in_root(Path::new("/"))
    }

    /// The same files under `root`, for tests.
    pub fn in_root(root: &Path) -> RepositoryEntries {
        RepositoryEntries {
            apt: root.join("etc/apt/sources.list.d/dsa.sources"),
            dnf: root.join("etc/yum.repos.d/dsa.repo"),
            zypper: root.join("etc/zypp/repos.d/dsa.repo"),
        }
    }

    /// Every entry, with the rewrite that points it at a channel.
    fn all(&self) -> [(&Path, RewriteEntry); 3] {
        [
            (self.apt.as_path(), apt_entry_for as RewriteEntry),
            (self.dnf.as_path(), rpm_entry_for),
            (self.zypper.as_path(), rpm_entry_for),
        ]
    }

    /// The channel the entries point at: that of the first entry there that names one. `None`
    /// when the package wrote none (or the user removed them).
    pub fn channel(&self) -> Option<ReleaseChannel> {
        self.all().into_iter().find_map(|(path, _)| {
            let text = std::fs::read_to_string(path).ok()?;
            entry_channel(&text)
        })
    }

    /// Points every entry that is there at `channel`, and returns the entries it changed.
    /// An entry that is not there stays away: the package wrote none, or the user removed it.
    pub fn follow(&self, channel: ReleaseChannel) -> Result<Vec<PathBuf>, String> {
        let mut changed = Vec::new();
        for (path, rewrite) in self.all() {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("reading {}: {e}", path.display())),
            };
            let Some(new) = rewrite(&text, channel) else {
                return Err(format!(
                    "{} is not the entry DSA's package wrote, so it is left as it is",
                    path.display()
                ));
            };
            if new != text {
                // Written in place, so the file keeps its owner, mode and security label.
                std::fs::write(path, new.as_bytes()).map_err(|e| {
                    format!(
                        "pointing {} at the {channel} repository: {e}",
                        path.display()
                    )
                })?;
                changed.push(path.to_path_buf());
            }
        }
        Ok(changed)
    }
}

/// The channel an apt or rpm entry points at.
fn entry_channel(text: &str) -> Option<ReleaseChannel> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(suites) = line.strip_prefix("Suites:") {
            return ReleaseChannel::parse(suites.trim()).ok();
        }
        if let Some(url) = line.strip_prefix("baseurl=") {
            return if url.contains("/rpm-prerelease/") {
                Some(ReleaseChannel::Prerelease)
            } else if url.contains("/rpm/") {
                Some(ReleaseChannel::Stable)
            } else {
                None
            };
        }
    }
    None
}

/// The apt entry `text` pointing at `channel`'s suite, or `None` when it names no suite.
pub fn apt_entry_for(text: &str, channel: ReleaseChannel) -> Option<String> {
    let mut found = false;
    let lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|line| {
            let body = line.trim_end_matches(['\n', '\r']);
            if body.trim_start().starts_with("Suites:") {
                found = true;
                format!("Suites: {}{}", channel.as_str(), &line[body.len()..])
            } else {
                line.to_string()
            }
        })
        .collect();
    found.then(|| lines.concat())
}

/// The dnf or zypper entry `text` pointing at `channel`'s repository, or `None` when its
/// `baseurl` is not one of DSA's.
pub fn rpm_entry_for(text: &str, channel: ReleaseChannel) -> Option<String> {
    let folder = match channel {
        ReleaseChannel::Stable => "/rpm/",
        ReleaseChannel::Prerelease => "/rpm-prerelease/",
    };
    let mut found = false;
    let lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|line| {
            let Some(url) = line.strip_prefix("baseurl=") else {
                return line.to_string();
            };
            let at = url
                .rfind("/rpm-prerelease/")
                .map(|at| (at, "/rpm-prerelease/".len()))
                .or_else(|| url.rfind("/rpm/").map(|at| (at, "/rpm/".len())));
            match at {
                Some((at, len)) => {
                    found = true;
                    format!("baseurl={}{folder}{}", &url[..at], &url[at + len..])
                }
                None => line.to_string(),
            }
        })
        .collect();
    found.then(|| lines.concat())
}

/// The service's settings, with the package repository entries following the machine's
/// release channel: a change of `prerelease-updates` points them at the chosen channel's
/// repository before it is kept, and is refused, changing nothing, when they cannot be
/// written.
pub struct ChannelFollowingSettings {
    inner: Arc<dyn SettingsStore>,
    entries: RepositoryEntries,
}

impl ChannelFollowingSettings {
    pub fn new(inner: Arc<dyn SettingsStore>, entries: RepositoryEntries) -> Self {
        ChannelFollowingSettings { inner, entries }
    }

    /// Points the entries at the channel the settings keep, as the service does when it
    /// starts. Returns the entries it changed.
    pub fn bring_entries_in_line(&self) -> Result<Vec<PathBuf>, String> {
        self.entries.follow(self.inner.machine().release_channel())
    }

    /// Keeps `prereleases` with `keep`, pointing the entries at its channel first and back
    /// again when keeping it fails.
    fn changing<T>(
        &self,
        prereleases: bool,
        keep: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let before = self.inner.machine().prerelease_updates;
        if prereleases == before {
            return keep();
        }
        let wanted = ReleaseChannel::from_prereleases(prereleases);
        self.entries.follow(wanted).map_err(|why| {
            format!("the {wanted} channel was not chosen, because DSA's package repository entries could not follow it: {why}")
        })?;
        keep().inspect_err(|_| {
            let _ = self
                .entries
                .follow(ReleaseChannel::from_prereleases(before));
        })
    }
}

impl SettingsStore for ChannelFollowingSettings {
    fn get(&self, user: &str) -> Result<Settings, String> {
        self.inner.get(user)
    }
    fn set(&self, user: &str, settings: &Settings) -> Result<Settings, String> {
        self.changing(settings.prerelease_updates, || {
            self.inner.set(user, settings)
        })
    }
    fn machine(&self) -> MachineSettings {
        self.inner.machine()
    }
    fn set_machine(&self, machine: MachineSettings) -> Result<(), String> {
        self.changing(machine.prerelease_updates, || {
            self.inner.set_machine(machine)
        })
    }
    fn arm_translation(&self, user: &str) -> Result<Option<Package>, String> {
        self.inner.arm_translation(user)
    }
    fn set_arm_translation(&self, user: &str, package: Option<&Package>) -> Result<(), String> {
        self.inner.set_arm_translation(user, package)
    }
    fn arm_translation_boot(&self, user: &str) -> Result<Option<Translator>, String> {
        self.inner.arm_translation_boot(user)
    }
    fn set_arm_translation_boot(
        &self,
        user: &str,
        translator: Option<&Translator>,
    ) -> Result<(), String> {
        self.inner.set_arm_translation_boot(user, translator)
    }
    fn app_windows(&self, user: &str) -> Result<Vec<AppWindowSettings>, String> {
        self.inner.app_windows(user)
    }
    fn set_app_windows(&self, user: &str, apps: &[AppWindowSettings]) -> Result<(), String> {
        self.inner.set_app_windows(user, apps)
    }
    fn gapps(&self, user: &str) -> Result<Option<GappsPackage>, String> {
        self.inner.gapps(user)
    }
    fn set_gapps(&self, user: &str, package: Option<&GappsPackage>) -> Result<(), String> {
        self.inner.set_gapps(user, package)
    }
    fn widevine(
        &self,
        user: &str,
    ) -> Result<Option<dsa_host_core::widevine::WidevinePackage>, String> {
        self.inner.widevine(user)
    }
    fn set_widevine(
        &self,
        user: &str,
        package: Option<&dsa_host_core::widevine::WidevinePackage>,
    ) -> Result<(), String> {
        self.inner.set_widevine(user, package)
    }
    fn root_apps(&self, user: &str) -> Result<Vec<RootApp>, String> {
        self.inner.root_apps(user)
    }
    fn set_root_app(&self, user: &str, package: &str, level: &str) -> Result<(), String> {
        self.inner.set_root_app(user, package, level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr15_linux_update_points_at_the_package_manager_and_installs_nothing() {
        let answer = PackageManagerUpdates.update_now().unwrap();
        assert_eq!(answer.installing, None);
        assert!(answer.detail.contains("package manager"));
        assert!(answer.detail.contains("apt"));
        assert!(answer.detail.contains("dnf"));
    }
    const APT_STABLE: &str = "Types: deb\nURIs: https://example.invalid/dsa/apt\nSuites: stable\nComponents: main\nArchitectures: amd64\nSigned-By: /usr/share/keyrings/dsa-archive-keyring.gpg\n";
    const DNF_STABLE: &str = "[dsa]\nname=Desktop Subsystem for Android\nbaseurl=https://example.invalid/dsa/rpm/$basearch\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=1\ngpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-dsa\n";

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dsa-repo-entries-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn fr15_linux_entries_point_at_the_chosen_channels_repository() {
        let apt = apt_entry_for(APT_STABLE, ReleaseChannel::Prerelease).unwrap();
        assert!(apt.contains("\nSuites: prerelease\n"), "{apt}");
        assert!(apt.contains("Signed-By: /usr/share/keyrings/dsa-archive-keyring.gpg"));
        assert_eq!(
            apt_entry_for(&apt, ReleaseChannel::Stable).unwrap(),
            APT_STABLE
        );
        let dnf = rpm_entry_for(DNF_STABLE, ReleaseChannel::Prerelease).unwrap();
        assert!(
            dnf.contains("baseurl=https://example.invalid/dsa/rpm-prerelease/$basearch\n"),
            "{dnf}"
        );
        assert_eq!(
            rpm_entry_for(&dnf, ReleaseChannel::Stable).unwrap(),
            DNF_STABLE
        );
        assert_eq!(entry_channel(&apt), Some(ReleaseChannel::Prerelease));
        assert_eq!(entry_channel(DNF_STABLE), Some(ReleaseChannel::Stable));
        assert_eq!(apt_entry_for("Types: deb\n", ReleaseChannel::Stable), None);
        assert_eq!(
            rpm_entry_for("[other]\nbaseurl=https://x/y\n", ReleaseChannel::Stable),
            None
        );
    }

    #[test]
    fn fr15_linux_changing_the_channel_switches_the_entries_the_package_wrote() {
        use dsa_host_core::settings::MemorySettings;
        let root = scratch("switch");
        let entries = RepositoryEntries::in_root(&root);
        write(&entries.apt, APT_STABLE);
        write(&entries.dnf, DNF_STABLE);
        assert_eq!(entries.channel(), Some(ReleaseChannel::Stable));
        let store =
            ChannelFollowingSettings::new(Arc::new(MemorySettings::default()), entries.clone());
        let prereleases = Settings {
            prerelease_updates: true,
            ..Settings::default()
        };
        store.set("uid:1000", &prereleases).unwrap();
        assert!(store.machine().prerelease_updates);
        assert_eq!(entries.channel(), Some(ReleaseChannel::Prerelease));
        let dnf = std::fs::read_to_string(&entries.dnf).unwrap();
        assert!(dnf.contains("/rpm-prerelease/"), "{dnf}");
        assert!(
            !entries.zypper.exists(),
            "an entry that was not there is not written"
        );

        // Back to stable.
        store.set("uid:1000", &Settings::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&entries.apt).unwrap(), APT_STABLE);
        assert_eq!(std::fs::read_to_string(&entries.dnf).unwrap(), DNF_STABLE);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fr15_linux_a_channel_the_entries_cannot_follow_is_not_kept() {
        use dsa_host_core::settings::MemorySettings;
        let root = scratch("refused");
        let entries = RepositoryEntries::in_root(&root);
        // Not an entry DSA's package wrote: it names no suite.
        write(
            &entries.apt,
            "Types: deb\nURIs: https://example.invalid/apt\n",
        );
        let store =
            ChannelFollowingSettings::new(Arc::new(MemorySettings::default()), entries.clone());
        let why = store
            .set(
                "uid:1000",
                &Settings {
                    prerelease_updates: true,
                    ..Settings::default()
                },
            )
            .unwrap_err();
        assert!(why.contains("could not follow"), "{why}");
        assert!(!store.machine().prerelease_updates, "nothing changed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fr15_linux_the_first_channel_is_the_one_the_package_wrote() {
        let root = scratch("first");
        let entries = RepositoryEntries::in_root(&root);
        assert_eq!(entries.channel(), None);
        write(
            &entries.dnf,
            &rpm_entry_for(DNF_STABLE, ReleaseChannel::Prerelease).unwrap(),
        );
        assert_eq!(entries.channel(), Some(ReleaseChannel::Prerelease));
        let _ = std::fs::remove_dir_all(&root);
    }
}
