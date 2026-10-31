//! The settings a user changes from the settings app, kept by the service.
//!
//! Two of them are the user's own: the GPU their window manager renders on and whether their
//! Android may be frozen when idle. Two are the machine's, shared by every user: whether the
//! service updates DSA itself and whether it downloads new Android images.
//!
//! Clients read and change them through the local API only. The service keeps them in its
//! own folder, which users cannot write to, so the values a client sees are the values the
//! service acts on.
//!
//! ```text
//! <dir>/machine.json          {"program_updates": true, "image_updates": true}
//! <dir>/users/<user>.json     {"render_gpu": "display", "freeze": false}
//! ```
//!
//! A missing file means the defaults. A user who has never saved settings starts from what
//! the platform finds for them (on Windows, the render GPU in the window manager's own
//! `compositor.json`), so a choice made before the settings app existed is kept.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The longest render GPU value accepted, in characters.
const MAX_RENDER_GPU: usize = 128;

/// The settings as a client sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `display` (the default), `integrated`, `discrete`, or part of a GPU's name.
    pub render_gpu: String,
    /// Whether the user's Android may be frozen when idle. Off by default.
    pub freeze: bool,
    /// Whether the service updates DSA itself.
    pub program_updates: bool,
    /// Whether the service downloads new Android images.
    pub image_updates: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            render_gpu: "display".into(),
            freeze: false,
            program_updates: true,
            image_updates: true,
        }
    }
}

/// The machine's share of the settings, which the update loops read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSettings {
    #[serde(default = "yes")]
    pub program_updates: bool,
    #[serde(default = "yes")]
    pub image_updates: bool,
}

impl Default for MachineSettings {
    fn default() -> MachineSettings {
        MachineSettings {
            program_updates: true,
            image_updates: true,
        }
    }
}

fn yes() -> bool {
    true
}

fn display() -> String {
    "display".into()
}

/// One user's share of the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSettings {
    #[serde(default = "display")]
    pub render_gpu: String,
    #[serde(default)]
    pub freeze: bool,
}

impl Default for UserSettings {
    fn default() -> UserSettings {
        UserSettings {
            render_gpu: display(),
            freeze: false,
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

/// Where the service keeps the settings.
pub trait SettingsStore: Send + Sync {
    /// The settings of `user`, an identity as the transport reports it.
    fn get(&self, user: &str) -> Result<Settings, String>;
    /// Replaces the settings of `user` and the machine's with `settings`, and returns them as
    /// they are now. An invalid value changes nothing.
    fn set(&self, user: &str, settings: &Settings) -> Result<Settings, String>;
    /// The machine's settings, for the update loops. The defaults when they cannot be read.
    fn machine(&self) -> MachineSettings;
}

/// Finds the render GPU a user chose before they first saved settings, if any.
pub type PreviousRenderGpu = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The settings in JSON files in the service's own folder.
pub struct FileSettings {
    dir: PathBuf,
    previous_render_gpu: Option<PreviousRenderGpu>,
    lock: Mutex<()>,
}

impl FileSettings {
    pub fn new(dir: impl Into<PathBuf>) -> FileSettings {
        FileSettings {
            dir: dir.into(),
            previous_render_gpu: None,
            lock: Mutex::new(()),
        }
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
        // The identity becomes a file name, so only the characters SIDs and Unix user IDs
        // use are accepted.
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
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.json", user.replace(':', "_"))))
    }

    fn read_machine(&self) -> Result<MachineSettings, String> {
        read_json(&self.machine_file()).map(Option::unwrap_or_default)
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
                freeze: false,
            }),
        }
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("the settings in {} are not usable: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading the settings {}: {e}", path.display())),
    }
}

/// Writes `value` to `path` through a file beside it, so a reader never sees half a file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(value).expect("settings serialise");
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text).map_err(|e| format!("writing {}: {e}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("replacing {}: {e}", path.display()))
}

impl SettingsStore for FileSettings {
    fn get(&self, user: &str) -> Result<Settings, String> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mine = self.read_user(user)?;
        let machine = self.read_machine()?;
        Ok(Settings {
            render_gpu: mine.render_gpu,
            freeze: mine.freeze,
            program_updates: machine.program_updates,
            image_updates: machine.image_updates,
        })
    }

    fn set(&self, user: &str, settings: &Settings) -> Result<Settings, String> {
        let render_gpu = normalise_render_gpu(&settings.render_gpu)?;
        let user_file = self.user_file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        write_json(
            &user_file,
            &UserSettings {
                render_gpu: render_gpu.clone(),
                freeze: settings.freeze,
            },
        )?;
        write_json(
            &self.machine_file(),
            &MachineSettings {
                program_updates: settings.program_updates,
                image_updates: settings.image_updates,
            },
        )?;
        Ok(Settings {
            render_gpu,
            ..settings.clone()
        })
    }

    fn machine(&self) -> MachineSettings {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read_machine().unwrap_or_else(|why| {
            eprintln!("settings: {why}; using the defaults");
            MachineSettings::default()
        })
    }
}

/// Settings kept in memory, for tests and for a service that keeps no folder.
#[derive(Default)]
pub struct MemorySettings {
    users: Mutex<std::collections::HashMap<String, UserSettings>>,
    machine: Mutex<MachineSettings>,
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
        let machine = self.machine();
        Ok(Settings {
            render_gpu: mine.render_gpu,
            freeze: mine.freeze,
            program_updates: machine.program_updates,
            image_updates: machine.image_updates,
        })
    }

    fn set(&self, user: &str, settings: &Settings) -> Result<Settings, String> {
        let render_gpu = normalise_render_gpu(&settings.render_gpu)?;
        self.users.lock().unwrap_or_else(|e| e.into_inner()).insert(
            user.to_string(),
            UserSettings {
                render_gpu: render_gpu.clone(),
                freeze: settings.freeze,
            },
        );
        *self.machine.lock().unwrap_or_else(|e| e.into_inner()) = MachineSettings {
            program_updates: settings.program_updates,
            image_updates: settings.image_updates,
        };
        Ok(Settings {
            render_gpu,
            ..settings.clone()
        })
    }

    fn machine(&self) -> MachineSettings {
        *self.machine.lock().unwrap_or_else(|e| e.into_inner())
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
    fn fr17_saved_settings_are_read_back_per_user_and_per_machine() {
        let dir = scratch("saved");
        let store = FileSettings::new(&dir);
        let saved = store
            .set(
                "S-1-5-21-1-2-3-1001",
                &Settings {
                    render_gpu: " Discrete ".into(),
                    freeze: true,
                    program_updates: false,
                    image_updates: true,
                },
            )
            .unwrap();
        assert_eq!(saved.render_gpu, "discrete");

        let again = FileSettings::new(&dir);
        let mine = again.get("S-1-5-21-1-2-3-1001").unwrap();
        assert_eq!(mine.render_gpu, "discrete");
        assert!(mine.freeze);
        assert!(!mine.program_updates);
        // Another user has their own GPU and freeze, and shares the machine's updates.
        let theirs = again.get("S-1-5-21-1-2-3-1002").unwrap();
        assert_eq!(theirs.render_gpu, "display");
        assert!(!theirs.freeze);
        assert!(!theirs.program_updates);
        assert!(!again.machine().program_updates);
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
}
