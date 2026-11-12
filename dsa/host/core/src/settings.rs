//! The settings a user changes from the settings app, kept by the service.
//!
//! Six of them are the user's own: the GPU their window manager renders on, whether their
//! Android may be frozen when idle, whether their clipboard is shared with it, whether web
//! links open in apps in their Android instead of the host's browser, and developer mode
//! (whether adb reaches their Android) with the loopback port adb connects to and whether that
//! port is attached to their adb server by itself, file sharing
//! (which of their host folders their Android shows in its shared storage), and whether the
//! host's own touch gestures are off while one of their task windows is in front, and whether
//! their Android restarts by itself into an Android image the service downloaded, and whether their
//! Android trusts the root certificates added to the computer. The others are the
//! machine's, shared by every user: whether the service updates DSA itself, whether it takes
//! pre-releases of DSA as well as stable releases, whether it downloads new Android images,
//! and whether DSA's processes write the input trace and the per-stage resize trace to their
//! logs.
//!
//! Clients read and change them through the local API only. The service keeps them in its
//! own folder, which users cannot write to, so the values a client sees are the values the
//! service acts on.
//!
//! ```text
//! <dir>/machine.json          {"program_updates": true, "prerelease_updates": false,
//!                              "image_updates": true, "input_trace": false,
//!                              "resize_trace": false}
//! <dir>/users/<user>.json     {"render_gpu": "display", "freeze": false, "clipboard": true,
//!                              "links_in_android": false, "developer_mode": false,
//!                              "adb_port": 58526, "adb_auto_connect": true,
//!                              "arm_translation": true,
//!                              "file_sharing": true,
//!                              "shared_folders": ["Documents", "Pictures"],
//!                              "advanced_networking": false,
//!                              "touch_gestures_off": true,
//!                              "image_auto_restart": true,
//!                              "host_certificates": true}
//! <dir>/users/<user>.app-windows.json
//!                             the user's window settings for each app
//! <dir>/users/<user>.arm-translation.json
//!                             the ARM translation package the user installed, if any
//! <dir>/users/<user>.arm-translation-boot.json
//!                             the translator the user's latest boot of Android was told to use
//! <dir>/users/<user>.gapps.json
//!                             the build of Google's apps the user installed, if any
//! ```
//!
//! A missing file means the defaults. A user who has never saved settings starts from what
//! the platform finds for them (on Windows, the render GPU in the window manager's own
//! `compositor.json`), so a choice made before the settings app existed is kept.
//!
//! Whether pre-releases are taken starts from the installer that put DSA on the computer
//! ([`FileSettings::with_first_channel`]): a pre-release installer takes pre-releases, a
//! stable one does not. The service records that first value when it starts
//! ([`FileSettings::keep_first_channel`]), so an upgrade, whichever installer it comes from,
//! keeps what the computer had.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::app_windows::AppWindowSettings;
use crate::arm_translation::{Package, Translator};
use crate::file_share;
use crate::gapps::GappsPackage;
use crate::update::program::ReleaseChannel;

/// The longest render GPU value accepted, in characters.
const MAX_RENDER_GPU: usize = 128;

/// The loopback port adb connects to in developer mode unless the user picks another: the one
/// Windows Subsystem for Android used, which adb users and tools already know.
pub const DEFAULT_ADB_PORT: u16 = 58526;

/// The settings as a client sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `display` (the default), `integrated`, `discrete`, or part of a GPU's name.
    pub render_gpu: String,
    /// Whether the user's Android may be frozen when idle. Off by default.
    pub freeze: bool,
    /// Whether the web links apps in the user's Android open go to apps in it rather than to
    /// the host's default browser. Off by default.
    pub links_in_android: bool,
    /// The most memory the user's Android may have, in MiB. 0 (the default) is automatic:
    /// the service's default size.
    pub memory_limit_mb: u32,
    /// Whether text copied on the host can be pasted in the user's Android, and the reverse.
    /// On by default.
    pub clipboard: bool,
    /// Whether stopping an app that runs in the background skips the question that confirms
    /// it. Off by default, so the first stop asks.
    pub stop_without_asking: bool,
    /// Whether the user's Android joins the host's LAN as a device of its own, rather than
    /// sitting behind the NAT network the service makes for its VM. Off by default; it applies
    /// from the next start of Android.
    pub advanced_networking: bool,
    /// Whether the service updates DSA itself.
    pub program_updates: bool,
    /// Whether DSA is updated to pre-releases as well as stable releases (the pre-release
    /// channel). The machine's; it starts from the installer that was used.
    pub prerelease_updates: bool,
    /// Whether the service downloads new Android images.
    pub image_updates: bool,
    /// Whether every DSA process writes the input trace to its log. The machine's; off by
    /// default.
    pub input_trace: bool,
    /// Whether every DSA process writes each stage of each resize to its log. The machine's;
    /// off by default.
    pub resize_trace: bool,
    /// Whether adb reaches the user's Android through the host's loopback address. Off by
    /// default.
    pub developer_mode: bool,
    /// The port on `127.0.0.1` adb connects to in developer mode. [`DEFAULT_ADB_PORT`] by
    /// default.
    pub adb_port: u16,
    /// Whether, while developer mode is on, the service attaches the port to the user's adb
    /// server by itself, so adb lists it without `adb connect`. On by default.
    pub adb_auto_connect: bool,
    /// Whether the user's Android uses an installed ARM translation package, from its next
    /// start. On by default; it does nothing while no package is installed.
    pub arm_translation: bool,
    /// Whether the user's host folders show in their Android's shared storage. On by default.
    pub file_sharing: bool,
    /// The host folders shared while file sharing is on, by name, in the order of
    /// [`file_share::FOLDERS`]. All of them by default.
    pub shared_folders: Vec<String>,
    /// Whether the host's three- and four-finger touch gestures are off while one of the
    /// user's task windows is in front, so apps get every finger. On by default; the user's
    /// window manager applies it.
    pub touch_gestures_off: bool,
    /// Whether the user's Android restarts by itself into an Android image the service
    /// downloaded: at once while no app window is open, else after asking, or once every
    /// window is closed. On by default; off, a downloaded image waits for the next start.
    pub image_auto_restart: bool,
    /// Whether the user's Android trusts, as system certificates, the root certificates a user
    /// or an administrator added to the computer. On by default; it applies at once.
    pub host_certificates: bool,
    /// Whether apps may elevate to root through the host's administrator approval. The
    /// machine's, shared by every user; off by default. It takes effect from the next start
    /// of Android.
    pub root_elevation: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            render_gpu: "display".into(),
            freeze: false,
            links_in_android: false,
            memory_limit_mb: 0,
            clipboard: true,
            stop_without_asking: false,
            advanced_networking: false,
            program_updates: true,
            prerelease_updates: false,
            image_updates: true,
            input_trace: false,
            resize_trace: false,
            developer_mode: false,
            adb_port: DEFAULT_ADB_PORT,
            adb_auto_connect: true,
            arm_translation: true,
            file_sharing: true,
            shared_folders: file_share::all_folders(),
            touch_gestures_off: true,
            image_auto_restart: true,
            host_certificates: true,
            root_elevation: false,
        }
    }
}

/// The machine's share of the settings, which the update loops read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSettings {
    #[serde(default = "yes")]
    pub program_updates: bool,
    /// The release channel: true takes pre-releases too. A file without it reads as the
    /// channel of the installer that was used ([`FileSettings::with_first_channel`]).
    #[serde(default)]
    pub prerelease_updates: bool,
    #[serde(default = "yes")]
    pub image_updates: bool,
    #[serde(default)]
    pub input_trace: bool,
    #[serde(default)]
    pub resize_trace: bool,
    /// Whether apps may elevate to root through the host's administrator approval. Off
    /// unless an administrator turns it on. It takes effect from the next start of Android.
    #[serde(default)]
    pub root_elevation: bool,
}

impl MachineSettings {
    /// The release channel DSA follows.
    pub fn release_channel(&self) -> ReleaseChannel {
        ReleaseChannel::from_prereleases(self.prerelease_updates)
    }
}

impl Default for MachineSettings {
    fn default() -> MachineSettings {
        MachineSettings {
            program_updates: true,
            prerelease_updates: false,
            image_updates: true,
            input_trace: false,
            resize_trace: false,
            root_elevation: false,
        }
    }
}

fn yes() -> bool {
    true
}

fn display() -> String {
    "display".into()
}

fn default_adb_port() -> u16 {
    DEFAULT_ADB_PORT
}

/// The adb port as kept: 0, which a client sends for "the default", becomes
/// [`DEFAULT_ADB_PORT`].
pub fn normalise_adb_port(port: u16) -> u16 {
    if port == 0 {
        DEFAULT_ADB_PORT
    } else {
        port
    }
}

/// One user's share of the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSettings {
    #[serde(default = "display")]
    pub render_gpu: String,
    #[serde(default)]
    pub freeze: bool,
    #[serde(default)]
    pub links_in_android: bool,
    #[serde(default)]
    pub memory_limit_mb: u32,
    #[serde(default = "yes")]
    pub clipboard: bool,
    #[serde(default)]
    pub developer_mode: bool,
    #[serde(default = "default_adb_port")]
    pub adb_port: u16,
    #[serde(default = "yes")]
    pub adb_auto_connect: bool,
    #[serde(default = "yes")]
    pub arm_translation: bool,
    #[serde(default)]
    pub stop_without_asking: bool,
    #[serde(default = "yes")]
    pub file_sharing: bool,
    #[serde(default = "file_share::all_folders")]
    pub shared_folders: Vec<String>,
    #[serde(default)]
    pub advanced_networking: bool,
    #[serde(default = "yes")]
    pub touch_gestures_off: bool,
    #[serde(default = "yes")]
    pub image_auto_restart: bool,
    #[serde(default = "yes")]
    pub host_certificates: bool,
}

impl Default for UserSettings {
    fn default() -> UserSettings {
        UserSettings {
            render_gpu: display(),
            freeze: false,
            links_in_android: false,
            memory_limit_mb: 0,
            clipboard: true,
            developer_mode: false,
            adb_port: DEFAULT_ADB_PORT,
            adb_auto_connect: true,
            arm_translation: true,
            stop_without_asking: false,
            file_sharing: true,
            shared_folders: file_share::all_folders(),
            advanced_networking: false,
            touch_gestures_off: true,
            image_auto_restart: true,
            host_certificates: true,
        }
    }
}

impl UserSettings {
    /// The user's share of `settings`, as kept, with `render_gpu` and `shared_folders`
    /// already checked.
    fn from_settings(
        settings: &Settings,
        render_gpu: String,
        shared_folders: Vec<String>,
    ) -> UserSettings {
        UserSettings {
            render_gpu,
            freeze: settings.freeze,
            links_in_android: settings.links_in_android,
            memory_limit_mb: settings.memory_limit_mb,
            clipboard: settings.clipboard,
            developer_mode: settings.developer_mode,
            adb_port: normalise_adb_port(settings.adb_port),
            adb_auto_connect: settings.adb_auto_connect,
            arm_translation: settings.arm_translation,
            stop_without_asking: settings.stop_without_asking,
            file_sharing: settings.file_sharing,
            shared_folders,
            advanced_networking: settings.advanced_networking,
            touch_gestures_off: settings.touch_gestures_off,
            image_auto_restart: settings.image_auto_restart,
            host_certificates: settings.host_certificates,
        }
    }
}

/// Checks a render GPU value and writes it the way it is kept: the three keywords in lower
/// case, a GPU name as given without the spaces around it.
pub fn normalise_render_gpu(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(
            "the render GPU is empty; use display, integrated, discrete or part of a GPU's name"
                .into(),
        );
    }
    if trimmed.chars().count() > MAX_RENDER_GPU {
        return Err(format!(
            "the render GPU is longer than {MAX_RENDER_GPU} characters; use part of the GPU's name"
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("the render GPU contains a control character".into());
    }
    let lower = trimmed.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "display" | "integrated" | "discrete" => lower,
        _ => trimmed.to_string(),
    })
}

/// The name a user's files in the service's folder start with. The identity becomes a file
/// name, so only the characters SIDs and Unix user IDs use are accepted.
pub(crate) fn user_file_stem(user: &str) -> Result<String, String> {
    if user.is_empty()
        || user.len() > 200
        || !user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':' || c == '_')
    {
        return Err(format!(
            "{user:?} is not a user identity the service keeps settings for"
        ));
    }
    Ok(user.replace(':', "_"))
}

/// Where the service keeps the settings.
pub trait SettingsStore: Send + Sync {
    /// The settings of `user`, an identity as the transport reports it.
    fn get(&self, user: &str) -> Result<Settings, String>;
    /// Replaces the settings of `user` and the machine's with `settings`, and returns them as
    /// they are now. An invalid value changes nothing.
    fn set(&self, user: &str, settings: &Settings) -> Result<Settings, String>;
    /// The machine's settings, for the update loops. The defaults when they cannot be read.
    fn machine(&self) -> MachineSettings;
    /// Replaces the machine's settings alone, as the service does itself (bringing over a
    /// choice an earlier release kept elsewhere), with no user to act for.
    fn set_machine(&self, machine: MachineSettings) -> Result<(), String>;
    /// The ARM translation package `user` has installed, if any.
    fn arm_translation(&self, user: &str) -> Result<Option<Package>, String>;
    /// Records the ARM translation package `user` has installed, or that there is none.
    fn set_arm_translation(&self, user: &str, package: Option<&Package>) -> Result<(), String>;
    /// The ARM translator `user`'s latest boot of Android was told to use, if any.
    fn arm_translation_boot(&self, user: &str) -> Result<Option<Translator>, String> {
        let _ = user;
        Ok(None)
    }
    /// Records the ARM translator a boot of `user`'s Android is told to use, or that it is told
    /// to use none.
    fn set_arm_translation_boot(
        &self,
        user: &str,
        translator: Option<&Translator>,
    ) -> Result<(), String> {
        let _ = (user, translator);
        Ok(())
    }
    /// The window settings of `user` for every app that has any, by package name.
    fn app_windows(&self, user: &str) -> Result<Vec<AppWindowSettings>, String> {
        let _ = user;
        Ok(Vec::new())
    }
    /// Replaces the window settings of `user` for every app.
    fn set_app_windows(&self, user: &str, apps: &[AppWindowSettings]) -> Result<(), String> {
        let _ = (user, apps);
        Err("this service keeps no window settings for apps".into())
    }
    /// The build of Google's apps `user` has installed, if any.
    fn gapps(&self, user: &str) -> Result<Option<GappsPackage>, String> {
        let _ = user;
        Ok(None)
    }
    /// Records the build of Google's apps `user` has installed, or that there is none.
    fn set_gapps(&self, user: &str, package: Option<&GappsPackage>) -> Result<(), String> {
        let _ = (user, package);
        Err("this service keeps no Google apps".into())
    }
    /// The root elevation level of each of `user`'s apps that has one other than off, by
    /// package name. Apps the user has not set are off and are not listed.
    fn root_apps(&self, user: &str) -> Result<Vec<RootApp>, String> {
        let _ = user;
        Ok(Vec::new())
    }
    /// Records the root elevation level of one of `user`'s apps. The package's default level
    /// (off for an app, ask for adb shell) forgets it.
    fn set_root_app(&self, user: &str, package: &str, level: &str) -> Result<(), String> {
        let _ = (user, package, level);
        Err("this service keeps no app root levels".into())
    }
}

/// One app's stored root elevation level: the package and its level ("off", "ask" or
/// "remember"). Kept per user in `<dir>/users/<user>.root-apps.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootApp {
    pub package: String,
    pub level: String,
}

/// Finds the render GPU a user chose before they first saved settings, if any.
pub type PreviousRenderGpu = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The settings in JSON files in the service's own folder.
pub struct FileSettings {
    dir: PathBuf,
    previous_render_gpu: Option<PreviousRenderGpu>,
    first_channel: ReleaseChannel,
    lock: Mutex<()>,
}

/// The key of [`MachineSettings::prerelease_updates`] in `machine.json`.
const PRERELEASE_KEY: &str = "prerelease_updates";

impl FileSettings {
    pub fn new(dir: impl Into<PathBuf>) -> FileSettings {
        FileSettings {
            dir: dir.into(),
            previous_render_gpu: None,
            first_channel: ReleaseChannel::Stable,
            lock: Mutex::new(()),
        }
    }

    /// Starts a computer whose settings record no release channel yet on `channel`, the
    /// channel of the installer that put DSA here. Stable unless this is called.
    pub fn with_first_channel(mut self, channel: ReleaseChannel) -> FileSettings {
        self.first_channel = channel;
        self
    }

    /// Records the release channel in `machine.json` when it records none yet: the first
    /// value, from [`with_first_channel`](Self::with_first_channel). From then on the file
    /// keeps the channel through every upgrade, until a user changes it. Returns the channel
    /// the computer follows.
    pub fn keep_first_channel(&self) -> Result<ReleaseChannel, String> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.machine_file();
        let recorded = read_json::<serde_json::Value>(&path)?
            .is_some_and(|value| value.get(PRERELEASE_KEY).is_some());
        let machine = self.read_machine()?;
        if !recorded {
            write_json(&path, &machine)?;
        }
        Ok(machine.release_channel())
    }

    /// Starts a user who has never saved settings from the render GPU `previous` finds.
    pub fn with_previous_render_gpu(mut self, previous: PreviousRenderGpu) -> FileSettings {
        self.previous_render_gpu = Some(previous);
        self
    }

    fn machine_file(&self) -> PathBuf {
        self.dir.join("machine.json")
    }

    fn user_file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.json", user_file_stem(user)?)))
    }

    fn app_windows_file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.app-windows.json", user_file_stem(user)?)))
    }

    fn arm_translation_file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.arm-translation.json", user_file_stem(user)?)))
    }

    fn gapps_file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.gapps.json", user_file_stem(user)?)))
    }

    fn root_apps_file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.root-apps.json", user_file_stem(user)?)))
    }

    fn arm_translation_boot_file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self.dir.join("users").join(format!(
            "{}.arm-translation-boot.json",
            user_file_stem(user)?
        )))
    }

    fn read_machine(&self) -> Result<MachineSettings, String> {
        let path = self.machine_file();
        let first = MachineSettings {
            prerelease_updates: self.first_channel.takes_prereleases(),
            ..MachineSettings::default()
        };
        let Some(value) = read_json::<serde_json::Value>(&path)? else {
            return Ok(first);
        };
        let recorded = value.get(PRERELEASE_KEY).is_some();
        let mut machine: MachineSettings = serde_json::from_value(value)
            .map_err(|e| format!("the settings in {} are not usable: {e}", path.display()))?;
        if !recorded {
            // Saved before the channel existed: the installer's channel, as on a new install.
            machine.prerelease_updates = first.prerelease_updates;
        }
        Ok(machine)
    }

    fn read_user(&self, user: &str) -> Result<UserSettings, String> {
        match read_json::<UserSettings>(&self.user_file(user)?)? {
            Some(settings) => Ok(settings),
            None => Ok(UserSettings {
                render_gpu: self
                    .previous_render_gpu
                    .as_ref()
                    .and_then(|previous| previous(user))
                    .and_then(|value| normalise_render_gpu(&value).ok())
                    .unwrap_or_else(display),
                ..UserSettings::default()
            }),
        }
    }
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("the settings in {} are not usable: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading the settings {}: {e}", path.display())),
    }
}

/// Writes `value` to `path` through a file beside it, so a reader never sees half a file.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(value).expect("settings serialise");
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text).map_err(|e| format!("writing {}: {e}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("replacing {}: {e}", path.display()))
}

/// A user's settings and the machine's, as a client sees them together.
fn combine(mine: UserSettings, machine: MachineSettings) -> Settings {
    Settings {
        render_gpu: mine.render_gpu,
        freeze: mine.freeze,
        links_in_android: mine.links_in_android,
        memory_limit_mb: mine.memory_limit_mb,
        clipboard: mine.clipboard,
        stop_without_asking: mine.stop_without_asking,
        advanced_networking: mine.advanced_networking,
        program_updates: machine.program_updates,
        prerelease_updates: machine.prerelease_updates,
        image_updates: machine.image_updates,
        input_trace: machine.input_trace,
        resize_trace: machine.resize_trace,
        root_elevation: machine.root_elevation,
        developer_mode: mine.developer_mode,
        adb_port: normalise_adb_port(mine.adb_port),
        adb_auto_connect: mine.adb_auto_connect,
        arm_translation: mine.arm_translation,
        file_sharing: mine.file_sharing,
        // A folder name a later release no longer knows is dropped rather than refused, so
        // a file kept by another release still reads.
        shared_folders: file_share::normalise_folders(&mine.shared_folders)
            .unwrap_or_else(|_| file_share::known_folders(&mine.shared_folders)),
        touch_gestures_off: mine.touch_gestures_off,
        image_auto_restart: mine.image_auto_restart,
        host_certificates: mine.host_certificates,
    }
}

/// The machine's share of `settings`.
fn machine_part(settings: &Settings) -> MachineSettings {
    MachineSettings {
        program_updates: settings.program_updates,
        prerelease_updates: settings.prerelease_updates,
        image_updates: settings.image_updates,
        input_trace: settings.input_trace,
        resize_trace: settings.resize_trace,
        root_elevation: settings.root_elevation,
    }
}

impl SettingsStore for FileSettings {
    fn get(&self, user: &str) -> Result<Settings, String> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mine = self.read_user(user)?;
        let machine = self.read_machine()?;
        Ok(combine(mine, machine))
    }

    fn set(&self, user: &str, settings: &Settings) -> Result<Settings, String> {
        let render_gpu = normalise_render_gpu(&settings.render_gpu)?;
        let shared_folders = file_share::normalise_folders(&settings.shared_folders)?;
        let user_file = self.user_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        write_json(
            &user_file,
            &UserSettings::from_settings(settings, render_gpu.clone(), shared_folders.clone()),
        )?;
        write_json(&self.machine_file(), &machine_part(settings))?;
        Ok(Settings {
            render_gpu,
            adb_port: normalise_adb_port(settings.adb_port),
            shared_folders,
            ..settings.clone()
        })
    }

    fn machine(&self) -> MachineSettings {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read_machine().unwrap_or_else(|why| {
            eprintln!("settings: {why}; using the defaults");
            MachineSettings {
                prerelease_updates: self.first_channel.takes_prereleases(),
                ..MachineSettings::default()
            }
        })
    }

    fn set_machine(&self, machine: MachineSettings) -> Result<(), String> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        write_json(&self.machine_file(), &machine)
    }

    fn arm_translation(&self, user: &str) -> Result<Option<Package>, String> {
        let file = self.arm_translation_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        read_json(&file)
    }

    fn set_arm_translation(&self, user: &str, package: Option<&Package>) -> Result<(), String> {
        let file = self.arm_translation_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match package {
            Some(package) => write_json(&file, package),
            None => match std::fs::remove_file(&file) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("removing {}: {e}", file.display())),
            },
        }
    }

    fn arm_translation_boot(&self, user: &str) -> Result<Option<Translator>, String> {
        let file = self.arm_translation_boot_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        read_json(&file)
    }

    fn set_arm_translation_boot(
        &self,
        user: &str,
        translator: Option<&Translator>,
    ) -> Result<(), String> {
        let file = self.arm_translation_boot_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match translator {
            Some(translator) => write_json(&file, translator),
            None => match std::fs::remove_file(&file) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("removing {}: {e}", file.display())),
            },
        }
    }

    fn app_windows(&self, user: &str) -> Result<Vec<AppWindowSettings>, String> {
        let file = self.app_windows_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        read_json(&file).map(Option::unwrap_or_default)
    }

    fn set_app_windows(&self, user: &str, apps: &[AppWindowSettings]) -> Result<(), String> {
        let file = self.app_windows_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        write_json(&file, &apps)
    }

    fn gapps(&self, user: &str) -> Result<Option<GappsPackage>, String> {
        let file = self.gapps_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        read_json(&file)
    }

    fn set_gapps(&self, user: &str, package: Option<&GappsPackage>) -> Result<(), String> {
        let file = self.gapps_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match package {
            Some(package) => write_json(&file, package),
            None => match std::fs::remove_file(&file) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("removing {}: {e}", file.display())),
            },
        }
    }

    fn root_apps(&self, user: &str) -> Result<Vec<RootApp>, String> {
        let file = self.root_apps_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(read_json::<Vec<RootApp>>(&file)?.unwrap_or_default())
    }

    fn set_root_app(&self, user: &str, package: &str, level: &str) -> Result<(), String> {
        let file = self.root_apps_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut apps = read_json::<Vec<RootApp>>(&file)?.unwrap_or_default();
        apps.retain(|app| app.package != package);
        if level != crate::root_elevation::default_level(package).code() {
            apps.push(RootApp {
                package: package.to_string(),
                level: level.to_string(),
            });
        }
        apps.sort_by(|a, b| a.package.cmp(&b.package));
        if apps.is_empty() {
            match std::fs::remove_file(&file) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("removing {}: {e}", file.display())),
            }
        } else {
            write_json(&file, &apps)
        }
    }
}

/// Settings kept in memory, for tests and for a service that keeps no folder.
#[derive(Default)]
pub struct MemorySettings {
    users: Mutex<std::collections::HashMap<String, UserSettings>>,
    machine: Mutex<MachineSettings>,
    arm_translation: Mutex<std::collections::HashMap<String, Package>>,
    arm_translation_boot: Mutex<std::collections::HashMap<String, Translator>>,
    app_windows: Mutex<std::collections::HashMap<String, Vec<AppWindowSettings>>>,
    gapps: Mutex<std::collections::HashMap<String, GappsPackage>>,
    root_apps: Mutex<std::collections::HashMap<String, Vec<RootApp>>>,
}

impl SettingsStore for MemorySettings {
    fn get(&self, user: &str) -> Result<Settings, String> {
        let mine = self
            .users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned()
            .unwrap_or_default();
        Ok(combine(mine, self.machine()))
    }

    fn set(&self, user: &str, settings: &Settings) -> Result<Settings, String> {
        let render_gpu = normalise_render_gpu(&settings.render_gpu)?;
        let shared_folders = file_share::normalise_folders(&settings.shared_folders)?;
        self.users.lock().unwrap_or_else(|e| e.into_inner()).insert(
            user.to_string(),
            UserSettings::from_settings(settings, render_gpu.clone(), shared_folders.clone()),
        );
        *self.machine.lock().unwrap_or_else(|e| e.into_inner()) = machine_part(settings);
        Ok(Settings {
            render_gpu,
            adb_port: normalise_adb_port(settings.adb_port),
            shared_folders,
            ..settings.clone()
        })
    }

    fn machine(&self) -> MachineSettings {
        *self.machine.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_machine(&self, machine: MachineSettings) -> Result<(), String> {
        *self.machine.lock().unwrap_or_else(|e| e.into_inner()) = machine;
        Ok(())
    }

    fn arm_translation(&self, user: &str) -> Result<Option<Package>, String> {
        Ok(self
            .arm_translation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned())
    }

    fn set_arm_translation(&self, user: &str, package: Option<&Package>) -> Result<(), String> {
        let mut packages = self
            .arm_translation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match package {
            Some(package) => packages.insert(user.to_string(), package.clone()),
            None => packages.remove(user),
        };
        Ok(())
    }

    fn arm_translation_boot(&self, user: &str) -> Result<Option<Translator>, String> {
        Ok(self
            .arm_translation_boot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned())
    }

    fn set_arm_translation_boot(
        &self,
        user: &str,
        translator: Option<&Translator>,
    ) -> Result<(), String> {
        let mut boots = self
            .arm_translation_boot
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match translator {
            Some(translator) => boots.insert(user.to_string(), translator.clone()),
            None => boots.remove(user),
        };
        Ok(())
    }

    fn app_windows(&self, user: &str) -> Result<Vec<AppWindowSettings>, String> {
        Ok(self
            .app_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned()
            .unwrap_or_default())
    }

    fn set_app_windows(&self, user: &str, apps: &[AppWindowSettings]) -> Result<(), String> {
        self.app_windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user.to_string(), apps.to_vec());
        Ok(())
    }

    fn gapps(&self, user: &str) -> Result<Option<GappsPackage>, String> {
        Ok(self
            .gapps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned())
    }

    fn set_gapps(&self, user: &str, package: Option<&GappsPackage>) -> Result<(), String> {
        let mut packages = self.gapps.lock().unwrap_or_else(|e| e.into_inner());
        match package {
            Some(package) => packages.insert(user.to_string(), package.clone()),
            None => packages.remove(user),
        };
        Ok(())
    }

    fn root_apps(&self, user: &str) -> Result<Vec<RootApp>, String> {
        Ok(self
            .root_apps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned()
            .unwrap_or_default())
    }

    fn set_root_app(&self, user: &str, package: &str, level: &str) -> Result<(), String> {
        let mut all = self.root_apps.lock().unwrap_or_else(|e| e.into_inner());
        let apps = all.entry(user.to_string()).or_default();
        apps.retain(|app| app.package != package);
        if level != crate::root_elevation::default_level(package).code() {
            apps.push(RootApp {
                package: package.to_string(),
                level: level.to_string(),
            });
        }
        apps.sort_by(|a, b| a.package.cmp(&b.package));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsa-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn fr17_settings_default_to_the_display_gpu_freeze_off_and_updates_on() {
        let dir = scratch("defaults");
        let store = FileSettings::new(&dir);
        let settings = store.get("S-1-5-21-1-2-3-1001").unwrap();
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.render_gpu, "display");
        assert!(
            !settings.freeze,
            "freeze is off unless the user turns it on"
        );
        assert_eq!(store.machine(), MachineSettings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr29_file_sharing_is_on_with_every_folder_by_default() {
        let dir = scratch("sharing-default");
        let settings = FileSettings::new(&dir).get("S-1-5-21-1-2-3-1001").unwrap();
        assert!(settings.file_sharing);
        assert_eq!(settings.shared_folders, file_share::all_folders());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr29_chosen_folders_are_kept_and_an_unknown_one_is_refused() {
        let dir = scratch("sharing-chosen");
        let store = FileSettings::new(&dir);
        let user = "S-1-5-21-1-2-3-1001";
        let chosen = Settings {
            file_sharing: false,
            shared_folders: vec!["pictures".into(), "Documents".into(), "Pictures".into()],
            ..Settings::default()
        };
        let kept = store.set(user, &chosen).unwrap();
        assert_eq!(kept.shared_folders, ["Documents", "Pictures"]);
        let again = FileSettings::new(&dir).get(user).unwrap();
        assert!(!again.file_sharing);
        assert_eq!(again.shared_folders, ["Documents", "Pictures"]);
        let unknown = Settings {
            shared_folders: vec!["C:\\".into()],
            ..Settings::default()
        };
        assert!(store.set(user, &unknown).is_err());
        assert_eq!(
            store.get(user).unwrap().shared_folders,
            ["Documents", "Pictures"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr17_saved_settings_are_read_back_per_user_and_per_machine() {
        let dir = scratch("saved");
        let store = FileSettings::new(&dir);
        let saved = store
            .set(
                "S-1-5-21-1-2-3-1001",
                &Settings {
                    render_gpu: " Discrete ".into(),
                    freeze: true,
                    links_in_android: true,
                    memory_limit_mb: 0,
                    clipboard: false,
                    stop_without_asking: false,
                    advanced_networking: false,
                    program_updates: false,
                    prerelease_updates: true,
                    image_updates: true,
                    input_trace: true,
                    resize_trace: false,
                    developer_mode: true,
                    adb_port: 5037,
                    adb_auto_connect: false,
                    arm_translation: false,
                    file_sharing: false,
                    shared_folders: vec!["Pictures".into()],
                    touch_gestures_off: false,
                    image_auto_restart: true,
                    host_certificates: true,
                    root_elevation: false,
                },
            )
            .unwrap();
        assert_eq!(saved.render_gpu, "discrete");

        let again = FileSettings::new(&dir);
        let mine = again.get("S-1-5-21-1-2-3-1001").unwrap();
        assert_eq!(mine.render_gpu, "discrete");
        assert!(mine.freeze);
        assert!(!mine.clipboard);
        assert!(!mine.program_updates);
        // Another user has their own GPU and freeze, and shares the machine's updates.
        let theirs = again.get("S-1-5-21-1-2-3-1002").unwrap();
        assert_eq!(theirs.render_gpu, "display");
        assert!(!theirs.freeze);
        assert!(theirs.clipboard);
        assert!(!theirs.program_updates);
        assert!(!again.machine().program_updates);
        assert!(theirs.prerelease_updates, "the channel is the machine's");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr17_an_invalid_render_gpu_changes_nothing() {
        let dir = scratch("invalid");
        let store = FileSettings::new(&dir);
        for bad in ["", "   ", "a\nb", &"x".repeat(MAX_RENDER_GPU + 1)] {
            let error = store
                .set(
                    "uid:1000",
                    &Settings {
                        render_gpu: bad.to_string(),
                        program_updates: false,
                        ..Settings::default()
                    },
                )
                .unwrap_err();
            assert!(error.contains("render GPU"), "{error}");
        }
        assert!(store.machine().program_updates);
        assert!(!dir.exists(), "nothing was written");
    }

    #[test]
    fn fr9_links_open_on_the_host_by_default_and_the_choice_is_kept() {
        let dir = scratch("links");
        let store = FileSettings::new(&dir);
        assert!(!store.get("uid:1000").unwrap().links_in_android);
        // A file saved before the setting existed keeps links on the host.
        std::fs::create_dir_all(dir.join("users")).unwrap();
        std::fs::write(
            dir.join("users").join("uid_1001.json"),
            r#"{"render_gpu": "display", "freeze": true}"#,
        )
        .unwrap();
        assert!(!store.get("uid:1001").unwrap().links_in_android);
        store
            .set(
                "uid:1000",
                &Settings {
                    links_in_android: true,
                    memory_limit_mb: 0,
                    ..Settings::default()
                },
            )
            .unwrap();
        let again = FileSettings::new(&dir);
        assert!(again.get("uid:1000").unwrap().links_in_android);
        assert!(!again.get("uid:1001").unwrap().links_in_android);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr13_clipboard_sharing_is_on_by_default_and_kept_when_turned_off() {
        let dir = scratch("clipboard");
        let store = FileSettings::new(&dir);
        assert!(store.get("uid:1000").unwrap().clipboard);
        // A file saved before the setting existed keeps it on.
        std::fs::create_dir_all(dir.join("users")).unwrap();
        std::fs::write(
            dir.join("users").join("uid_1001.json"),
            r#"{"render_gpu": "display", "freeze": true}"#,
        )
        .unwrap();
        assert!(store.get("uid:1001").unwrap().clipboard);
        store
            .set(
                "uid:1000",
                &Settings {
                    clipboard: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert!(!FileSettings::new(&dir).get("uid:1000").unwrap().clipboard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr24_stopping_asks_by_default_and_do_not_ask_again_is_kept() {
        let dir = scratch("stop");
        let store = FileSettings::new(&dir);
        assert!(!store.get("uid:1000").unwrap().stop_without_asking);
        // A file saved before the setting existed asks.
        std::fs::create_dir_all(dir.join("users")).unwrap();
        std::fs::write(
            dir.join("users").join("uid_1001.json"),
            r#"{"render_gpu": "display", "clipboard": false}"#,
        )
        .unwrap();
        assert!(!store.get("uid:1001").unwrap().stop_without_asking);
        store
            .set(
                "uid:1000",
                &Settings {
                    stop_without_asking: true,
                    ..Settings::default()
                },
            )
            .unwrap();
        let again = FileSettings::new(&dir);
        assert!(again.get("uid:1000").unwrap().stop_without_asking);
        assert!(!again.get("uid:1001").unwrap().stop_without_asking);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr28_advanced_networking_is_off_by_default_and_kept_per_user() {
        let dir = scratch("network");
        let store = FileSettings::new(&dir);
        assert!(!store.get("uid:1000").unwrap().advanced_networking);
        // A file saved before the setting existed stays on NAT.
        std::fs::create_dir_all(dir.join("users")).unwrap();
        std::fs::write(
            dir.join("users").join("uid_1001.json"),
            r#"{"render_gpu": "display"}"#,
        )
        .unwrap();
        assert!(!store.get("uid:1001").unwrap().advanced_networking);
        store
            .set(
                "uid:1000",
                &Settings {
                    advanced_networking: true,
                    ..Settings::default()
                },
            )
            .unwrap();
        let again = FileSettings::new(&dir);
        assert!(again.get("uid:1000").unwrap().advanced_networking);
        assert!(!again.get("uid:1001").unwrap().advanced_networking);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr19_trace_switches_are_machine_settings_off_by_default() {
        let dir = scratch("traces");
        let store = FileSettings::new(&dir);
        let settings = store.get("uid:1000").unwrap();
        assert!(!settings.input_trace && !settings.resize_trace);
        // A file saved before the switches existed keeps them off.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("machine.json"), r#"{"program_updates": true}"#).unwrap();
        assert!(!store.machine().input_trace);
        store
            .set(
                "uid:1000",
                &Settings {
                    input_trace: true,
                    resize_trace: true,
                    ..Settings::default()
                },
            )
            .unwrap();
        let again = FileSettings::new(&dir);
        assert!(again.machine().input_trace && again.machine().resize_trace);
        // The machine's: another user sees them on too.
        assert!(again.get("uid:1001").unwrap().input_trace);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr17_a_gpu_name_keeps_its_case() {
        assert_eq!(normalise_render_gpu(" RTX 4050 ").unwrap(), "RTX 4050");
        assert_eq!(normalise_render_gpu("INTEGRATED").unwrap(), "integrated");
    }

    #[test]
    fn fr17_a_user_without_settings_keeps_the_gpu_chosen_before() {
        let dir = scratch("previous");
        let store = FileSettings::new(&dir).with_previous_render_gpu(Box::new(|user| {
            (user == "S-1-5-21-1-2-3-1001").then(|| "RTX".to_string())
        }));
        assert_eq!(store.get("S-1-5-21-1-2-3-1001").unwrap().render_gpu, "RTX");
        assert_eq!(
            store.get("S-1-5-21-1-2-3-1002").unwrap().render_gpu,
            "display"
        );
        // Once saved, the saved value wins.
        store
            .set(
                "S-1-5-21-1-2-3-1001",
                &Settings {
                    render_gpu: "display".into(),
                    ..Settings::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.get("S-1-5-21-1-2-3-1001").unwrap().render_gpu,
            "display"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr20_developer_mode_is_off_by_default_on_port_58526_and_kept_per_user() {
        let dir = scratch("developer-mode");
        let store = FileSettings::new(&dir);
        let fresh = store.get("uid:1000").unwrap();
        assert!(!fresh.developer_mode);
        assert_eq!(fresh.adb_port, 58526);
        let saved = store
            .set(
                "uid:1000",
                &Settings {
                    developer_mode: true,
                    adb_port: 0,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert!(saved.developer_mode);
        assert_eq!(saved.adb_port, 58526, "0 means the default port");
        store
            .set(
                "uid:1000",
                &Settings {
                    developer_mode: true,
                    adb_port: 5037,
                    ..Settings::default()
                },
            )
            .unwrap();
        let again = FileSettings::new(&dir);
        assert!(again.get("uid:1000").unwrap().developer_mode);
        assert_eq!(again.get("uid:1000").unwrap().adb_port, 5037);
        assert!(
            !again.get("uid:1001").unwrap().developer_mode,
            "the user's own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr20_adb_auto_connect_is_on_by_default_and_kept_per_user() {
        let dir = scratch("adb-auto-connect");
        let store = FileSettings::new(&dir);
        assert!(store.get("uid:1000").unwrap().adb_auto_connect);
        let saved = store
            .set(
                "uid:1000",
                &Settings {
                    adb_auto_connect: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert!(!saved.adb_auto_connect);
        let again = FileSettings::new(&dir);
        assert!(!again.get("uid:1000").unwrap().adb_auto_connect);
        assert!(
            again.get("uid:1001").unwrap().adb_auto_connect,
            "the user's own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr14_image_auto_restart_is_on_by_default_and_kept_per_user() {
        let dir = scratch("image-auto-restart");
        let store = FileSettings::new(&dir);
        assert!(store.get("uid:1000").unwrap().image_auto_restart);
        let saved = store
            .set(
                "uid:1000",
                &Settings {
                    image_auto_restart: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert!(!saved.image_auto_restart);
        let again = FileSettings::new(&dir);
        assert!(!again.get("uid:1000").unwrap().image_auto_restart);
        assert!(
            again.get("uid:1001").unwrap().image_auto_restart,
            "the user's own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr28_host_certificates_are_trusted_by_default_and_kept_per_user() {
        let dir = scratch("host-certificates");
        let store = FileSettings::new(&dir);
        assert!(store.get("uid:1000").unwrap().host_certificates);
        let saved = store
            .set(
                "uid:1000",
                &Settings {
                    host_certificates: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert!(!saved.host_certificates);
        let again = FileSettings::new(&dir);
        assert!(!again.get("uid:1000").unwrap().host_certificates);
        assert!(
            again.get("uid:1001").unwrap().host_certificates,
            "the user's own"
        );
        // A file kept by a release without the setting reads as on.
        std::fs::write(dir.join("users").join("uid_1002.json"), "{}").unwrap();
        assert!(again.get("uid:1002").unwrap().host_certificates);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr5_touch_gestures_off_is_on_by_default_and_kept_per_user() {
        let dir = scratch("touch-gestures");
        let store = FileSettings::new(&dir);
        assert!(store.get("uid:1000").unwrap().touch_gestures_off);
        let saved = store
            .set(
                "uid:1000",
                &Settings {
                    touch_gestures_off: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert!(!saved.touch_gestures_off);
        let again = FileSettings::new(&dir);
        assert!(!again.get("uid:1000").unwrap().touch_gestures_off);
        assert!(
            again.get("uid:1001").unwrap().touch_gestures_off,
            "the user's own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr5_a_settings_file_from_before_touch_gestures_reads_it_as_on() {
        let kept: UserSettings = serde_json::from_str(r#"{"clipboard": false}"#).unwrap();
        assert!(!kept.clipboard);
        assert!(kept.touch_gestures_off);
    }

    #[test]
    fn fr20_a_settings_file_from_before_auto_connect_reads_it_as_on() {
        let kept: UserSettings =
            serde_json::from_str(r#"{"developer_mode": true, "adb_port": 58526}"#).unwrap();
        assert!(kept.developer_mode);
        assert!(kept.adb_auto_connect);
    }

    #[test]
    fn fr25_arm_translation_is_on_by_default_and_its_package_is_kept_per_user() {
        let dir = scratch("arm-translation");
        let store = FileSettings::new(&dir);
        assert!(store.get("uid:1000").unwrap().arm_translation);
        assert_eq!(store.arm_translation("uid:1000").unwrap(), None);
        let package = Package {
            name: "Translator".into(),
            version: "1".into(),
            library: "libtranslator.so".into(),
            abis: vec!["arm64-v8a".into()],
            license: "Apache-2.0".into(),
        };
        store
            .set_arm_translation("uid:1000", Some(&package))
            .unwrap();
        let again = FileSettings::new(&dir);
        assert_eq!(again.arm_translation("uid:1000").unwrap(), Some(package));
        assert_eq!(again.arm_translation("uid:1001").unwrap(), None);
        again.set_arm_translation("uid:1000", None).unwrap();
        assert_eq!(again.arm_translation("uid:1000").unwrap(), None);
        // Removing what is not there is not an error.
        again.set_arm_translation("uid:1000", None).unwrap();
        let off = again
            .set(
                "uid:1000",
                &Settings {
                    arm_translation: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        assert!(!off.arm_translation);
        assert!(
            !FileSettings::new(&dir)
                .get("uid:1000")
                .unwrap()
                .arm_translation
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr25_the_translator_a_boot_was_told_to_use_is_kept_per_user() {
        use crate::arm_translation::Source;
        let dir = scratch("arm-translation-boot");
        let store = FileSettings::new(&dir);
        assert_eq!(store.arm_translation_boot("uid:1000").unwrap(), None);
        let booted = Translator {
            source: Source::Default,
            package: Package {
                name: "Digitalis".into(),
                version: "17-abefbe2b1600".into(),
                library: "libberberis_arm64.so".into(),
                abis: vec!["arm64-v8a".into()],
                license: "Apache-2.0".into(),
            },
        };
        store
            .set_arm_translation_boot("uid:1000", Some(&booted))
            .unwrap();
        let again = FileSettings::new(&dir);
        assert_eq!(
            again.arm_translation_boot("uid:1000").unwrap(),
            Some(booted.clone())
        );
        assert_eq!(again.arm_translation_boot("uid:1001").unwrap(), None);
        again.set_arm_translation_boot("uid:1000", None).unwrap();
        assert_eq!(again.arm_translation_boot("uid:1000").unwrap(), None);
        again.set_arm_translation_boot("uid:1000", None).unwrap();
        let memory = MemorySettings::default();
        memory
            .set_arm_translation_boot("uid:1000", Some(&booted))
            .unwrap();
        assert_eq!(
            memory.arm_translation_boot("uid:1000").unwrap(),
            Some(booted)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr32_the_google_apps_a_user_installed_are_kept_per_user() {
        let dir = scratch("gapps");
        let store = FileSettings::new(&dir);
        assert_eq!(store.gapps("uid:1000").unwrap(), None);
        let package = GappsPackage {
            variant: "core".into(),
            version: "20261001".into(),
            android_version: "16.0".into(),
            sdk: 36,
            arch: "x86_64".into(),
            file: "LiteGapps-core-x86_64-16.0-20261001-official.zip".into(),
            source: "https://example.com/build.zip".into(),
            md5: "ea9945a6d63849fa9e51782edfe613a4".into(),
            image_format: crate::gapps::IMAGE_FORMAT,
        };
        store.set_gapps("uid:1000", Some(&package)).unwrap();
        let again = FileSettings::new(&dir);
        assert_eq!(again.gapps("uid:1000").unwrap(), Some(package));
        assert_eq!(again.gapps("uid:1001").unwrap(), None);
        again.set_gapps("uid:1000", None).unwrap();
        assert_eq!(again.gapps("uid:1000").unwrap(), None);
        // Removing what is not there is not an error.
        again.set_gapps("uid:1000", None).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr17_a_user_identity_never_leaves_the_settings_folder() {
        let dir = scratch("identity");
        let store = FileSettings::new(&dir);
        for bad in ["..\\..\\x", "../x", "a/b", ""] {
            assert!(store.get(bad).is_err(), "{bad:?}");
        }
        assert!(store.get("uid:501").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr17_a_broken_settings_file_is_reported() {
        let dir = scratch("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("machine.json"), "{").unwrap();
        let store = FileSettings::new(&dir);
        assert!(store.get("uid:1000").unwrap_err().contains("not usable"));
        assert_eq!(store.machine(), MachineSettings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr31_a_run_mode_kept_by_an_earlier_release_is_ignored_and_dropped() {
        let dir = scratch("run-mode-gone");
        std::fs::create_dir_all(dir.join("users")).unwrap();
        let file = dir.join("users").join("uid_1000.json");
        std::fs::write(
            &file,
            r#"{"render_gpu": "discrete", "run_mode": "as-needed", "stop_after_secs": 300, "keep_for_notifications": true}"#,
        )
        .unwrap();
        let store = FileSettings::new(&dir);
        let mine = store.get("uid:1000").unwrap();
        assert_eq!(
            mine.render_gpu, "discrete",
            "the rest of the file still reads"
        );
        store.set("uid:1000", &mine).unwrap();
        let kept = std::fs::read_to_string(&file).unwrap();
        for gone in ["run_mode", "stop_after_secs", "keep_for_notifications"] {
            assert!(!kept.contains(gone), "{gone} is dropped: {kept}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn fr15_a_first_install_takes_the_channel_of_its_installer() {
        for (installer, prereleases) in [
            (ReleaseChannel::Prerelease, true),
            (ReleaseChannel::Stable, false),
        ] {
            let dir = scratch(&format!("first-channel-{}", installer.as_str()));
            let store = FileSettings::new(&dir).with_first_channel(installer);
            assert_eq!(
                store.get("uid:1000").unwrap().prerelease_updates,
                prereleases
            );
            assert_eq!(store.keep_first_channel().unwrap(), installer);
            // Recorded: the next installation's channel no longer decides.
            let other = match installer {
                ReleaseChannel::Stable => ReleaseChannel::Prerelease,
                ReleaseChannel::Prerelease => ReleaseChannel::Stable,
            };
            let upgraded = FileSettings::new(&dir).with_first_channel(other);
            assert_eq!(upgraded.keep_first_channel().unwrap(), installer);
            assert_eq!(upgraded.machine().prerelease_updates, prereleases);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn fr15_an_upgrade_keeps_the_channel_the_user_chose() {
        let dir = scratch("chosen-channel");
        let store = FileSettings::new(&dir).with_first_channel(ReleaseChannel::Prerelease);
        store.keep_first_channel().unwrap();
        store
            .set(
                "uid:1000",
                &Settings {
                    prerelease_updates: false,
                    ..Settings::default()
                },
            )
            .unwrap();
        // A pre-release installer upgrades DSA: the stable channel the user chose stays.
        let upgraded = FileSettings::new(&dir).with_first_channel(ReleaseChannel::Prerelease);
        assert_eq!(
            upgraded.keep_first_channel().unwrap(),
            ReleaseChannel::Stable
        );
        assert!(!upgraded.get("uid:1001").unwrap().prerelease_updates);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr15_settings_saved_before_channels_existed_take_the_installers_channel() {
        let dir = scratch("old-machine-file");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("machine.json"),
            r#"{"program_updates": false, "image_updates": true}"#,
        )
        .unwrap();
        let store = FileSettings::new(&dir).with_first_channel(ReleaseChannel::Prerelease);
        assert!(store.machine().prerelease_updates);
        assert_eq!(
            store.keep_first_channel().unwrap(),
            ReleaseChannel::Prerelease
        );
        let text = std::fs::read_to_string(dir.join("machine.json")).unwrap();
        assert!(text.contains("\"prerelease_updates\": true"), "{text}");
        assert!(!store.machine().program_updates, "the other settings stay");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
