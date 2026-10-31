//! Which GPU the host renderer runs on.
//!
//! By default the renderer runs on the GPU that drives the primary monitor, so a frame never
//! crosses from one GPU to another on its way to the screen and a laptop's discrete GPU stays
//! asleep. The renderer uses one GPU, so a task window on a monitor that another GPU drives
//! is still rendered by the primary monitor's GPU, and Windows moves its frames across.
//!
//! The user chooses another GPU in the settings app, which keeps the choice in the service's
//! settings; the window manager asks the service for it when it starts. Before the settings
//! app, the choice was made in `%LOCALAPPDATA%\DSA\compositor.json`, and that file still
//! counts: the service starts a user who has never saved settings from it, and the window
//! manager reads it when it cannot reach the service.
//!
//! ```json
//! { "render_gpu": "discrete" }
//! ```
//!
//! `render_gpu` is `display` (the default), `integrated`, `discrete`, or part of a GPU's name
//! (`"RTX"`, `"Arc"`), matched ignoring case. The renderer chooses when it starts, so a new
//! value takes effect when `dsa-wm` starts again. When no GPU matches, the renderer
//! ranks the GPUs itself (a discrete GPU first) and the window manager's log says so.
//!
//! Everything here is plain data, so it is tested on every platform; the Windows calls that
//! find the displays and name the adapters are in `compositor::display_gpu`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

/// The setting: which GPU the user wants the renderer on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RenderGpu {
    /// The GPU that drives the primary monitor.
    #[default]
    Display,
    /// The first integrated GPU.
    Integrated,
    /// The first discrete GPU.
    Discrete,
    /// The first GPU whose name contains this text, ignoring case.
    Named(String),
}

impl RenderGpu {
    /// Reads a `render_gpu` value. The three keywords ignore case; anything else that is not
    /// blank is part of a GPU's name.
    pub fn parse(value: &str) -> Result<RenderGpu, String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(
                "render_gpu is empty; use display, integrated, discrete or part of a GPU's name"
                    .into(),
            );
        }
        Ok(match trimmed.to_ascii_lowercase().as_str() {
            "display" => RenderGpu::Display,
            "integrated" => RenderGpu::Integrated,
            "discrete" => RenderGpu::Discrete,
            _ => RenderGpu::Named(trimmed.to_string()),
        })
    }
}

impl RenderGpu {
    /// The value as the settings keep it: a keyword, or the part of a GPU's name as given.
    pub fn value(&self) -> String {
        match self {
            RenderGpu::Display => "display".into(),
            RenderGpu::Integrated => "integrated".into(),
            RenderGpu::Discrete => "discrete".into(),
            RenderGpu::Named(name) => name.clone(),
        }
    }
}

/// The render GPU a user chose in their `compositor.json`, below their local application
/// data folder, before the settings app kept it. `None` when the file is missing or unusable.
pub fn previous_render_gpu(local_app_data: &Path) -> Option<String> {
    let path = default_settings_file(Some(local_app_data));
    if !path.exists() {
        return None;
    }
    match CompositorSettings::load(&path) {
        (settings, None) => Some(settings.render_gpu.value()),
        (_, Some(_)) => None,
    }
}

impl fmt::Display for RenderGpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderGpu::Display => f.write_str("display"),
            RenderGpu::Integrated => f.write_str("integrated"),
            RenderGpu::Discrete => f.write_str("discrete"),
            RenderGpu::Named(name) => write!(f, "{name:?}"),
        }
    }
}

impl<'de> Deserialize<'de> for RenderGpu {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        RenderGpu::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// The window manager's per-user settings file, `%LOCALAPPDATA%\DSA\compositor.json`. Unknown
/// keys are an error, so a misspelt setting is reported rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositorSettings {
    /// The GPU the renderer runs on.
    #[serde(default)]
    pub render_gpu: RenderGpu,
}

impl CompositorSettings {
    /// Parses the settings file's contents.
    pub fn parse(json: &str) -> Result<CompositorSettings, String> {
        serde_json::from_str(json).map_err(|e| format!("compositor.json is not usable: {e}"))
    }

    /// Reads the settings at `path`. A missing file means the defaults. A file that cannot
    /// be read or parsed also gives the defaults, with the reason to log.
    pub fn load(path: &Path) -> (CompositorSettings, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match CompositorSettings::parse(&text) {
                Ok(settings) => (settings, None),
                Err(why) => (
                    CompositorSettings::default(),
                    Some(format!("{why} ({}); using the defaults", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (CompositorSettings::default(), None)
            }
            Err(e) => (
                CompositorSettings::default(),
                Some(format!(
                    "could not read {}: {e}; using the defaults",
                    path.display()
                )),
            ),
        }
    }
}

/// The render GPU the window manager uses: the one the service's settings name, or, when the
/// service cannot be asked, the one in `compositor.json` (`file` reads it). Returns the
/// setting and the lines to log about where it came from.
pub fn render_gpu_setting(
    service: Result<String, String>,
    file: impl FnOnce() -> (CompositorSettings, Option<String>),
) -> (RenderGpu, Vec<String>) {
    match service.and_then(|value| RenderGpu::parse(&value)) {
        Ok(setting) => (
            setting,
            vec!["render GPU setting from the DSA service's settings".into()],
        ),
        Err(why) => {
            let (settings, problem) = file();
            let mut notes = vec![format!(
                "the DSA service's settings could not be read ({why}); using compositor.json"
            )];
            notes.extend(problem);
            (settings.render_gpu, notes)
        }
    }
}

/// Where the window manager's settings are: `compositor.json` in `%LOCALAPPDATA%\DSA`, beside
/// the `logs` folder.
pub fn default_settings_file(local_app_data: Option<&Path>) -> PathBuf {
    match local_app_data {
        Some(dir) => dir.join("DSA").join("compositor.json"),
        None => PathBuf::from("compositor.json"),
    }
}

/// An adapter's locally unique ID, 8 bytes as a Windows `LUID` lays them out (`LowPart`
/// then `HighPart`, little endian), which is also how Vulkan reports `deviceLUID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Luid(pub [u8; 8]);

impl Luid {
    /// From a Windows `LUID`'s two halves.
    pub fn from_parts(low: u32, high: i32) -> Luid {
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&low.to_le_bytes());
        bytes[4..].copy_from_slice(&high.to_le_bytes());
        Luid(bytes)
    }

    /// The `LowPart` half.
    pub fn low(&self) -> u32 {
        u32::from_le_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }

    /// The `HighPart` half.
    pub fn high(&self) -> i32 {
        i32::from_le_bytes([self.0[4], self.0[5], self.0[6], self.0[7]])
    }
}

impl fmt::Display for Luid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}:{:08x}", self.high() as u32, self.low())
    }
}

/// One active display source: the desktop area one adapter shows, and where it is on the
/// virtual desktop. The primary monitor's area starts at (0, 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySource {
    pub adapter: Luid,
    pub x: i32,
    pub y: i32,
}

/// An adapter's name, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterName {
    pub luid: Luid,
    pub name: String,
}

/// What the renderer is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuRequest {
    /// The GPU with this LUID.
    Luid(Luid),
    /// The first integrated GPU.
    Integrated,
    /// The first discrete GPU.
    Discrete,
    /// The first GPU whose name contains this text, ignoring case.
    Named(String),
    /// Nothing: the renderer ranks the GPUs itself.
    Ranking,
}

/// Turns the setting into a request, given the active display sources. `display` asks for
/// the adapter of the source at (0, 0), the primary monitor; with no such source (no
/// display, or a remote session whose display no GPU drives) it asks for nothing.
pub fn choose(setting: &RenderGpu, sources: &[DisplaySource]) -> GpuRequest {
    match setting {
        RenderGpu::Display => sources
            .iter()
            .find(|source| source.x == 0 && source.y == 0)
            .map(|primary| GpuRequest::Luid(primary.adapter))
            .unwrap_or(GpuRequest::Ranking),
        RenderGpu::Integrated => GpuRequest::Integrated,
        RenderGpu::Discrete => GpuRequest::Discrete,
        RenderGpu::Named(name) => GpuRequest::Named(name.clone()),
    }
}

fn adapter_name(adapters: &[AdapterName], luid: Luid) -> String {
    adapters
        .iter()
        .find(|adapter| adapter.luid == luid)
        .map(|adapter| format!("{} (LUID {luid})", adapter.name))
        .unwrap_or_else(|| format!("the adapter with LUID {luid}"))
}

/// The log line that says what the renderer was asked for and why.
pub fn describe_request(
    setting: &RenderGpu,
    request: &GpuRequest,
    adapters: &[AdapterName],
) -> String {
    match request {
        GpuRequest::Luid(luid) => format!(
            "render GPU setting {setting}: asking the renderer for {}, which drives the \
             primary monitor; task windows on other monitors are rendered there too",
            adapter_name(adapters, *luid)
        ),
        GpuRequest::Ranking => format!(
            "render GPU setting {setting}: no GPU drives the primary monitor, so the renderer \
             ranks the GPUs itself"
        ),
        GpuRequest::Integrated => {
            format!("render GPU setting {setting}: asking the renderer for an integrated GPU")
        }
        GpuRequest::Discrete => {
            format!("render GPU setting {setting}: asking the renderer for a discrete GPU")
        }
        GpuRequest::Named(name) => format!(
            "render GPU setting {setting}: asking the renderer for a GPU whose name contains \
             {name:?}"
        ),
    }
}

/// What the renderer reports about the GPU it runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererGpu {
    /// The Vulkan device name.
    pub name: String,
    pub luid: Option<Luid>,
    /// `Some(true)` when it is the GPU asked for, `Some(false)` when none matched and the
    /// renderer ranked them, `None` when nothing was asked.
    pub matched: Option<bool>,
}

/// The log line that names the GPU the renderer runs on. It starts with "rendering on".
pub fn describe_outcome(request: &GpuRequest, gpu: &RendererGpu) -> String {
    let device = match gpu.luid {
        Some(luid) => format!("{} (LUID {luid})", gpu.name),
        None => gpu.name.clone(),
    };
    match (request, gpu.matched) {
        (GpuRequest::Ranking, _) | (_, None) => {
            format!("rendering on {device}, which the renderer ranked first")
        }
        (_, Some(true)) => format!("rendering on {device}, the GPU asked for"),
        (GpuRequest::Luid(luid), Some(false)) => format!(
            "rendering on {device}, which the renderer ranked first: no Vulkan GPU has the \
             primary monitor's adapter LUID {luid}, so frames cross GPUs"
        ),
        (_, Some(false)) => format!(
            "rendering on {device}, which the renderer ranked first: no Vulkan GPU matches \
             the render GPU setting"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTEL: Luid = Luid([0x8a, 0x4e, 0x01, 0, 0, 0, 0, 0]);
    const NVIDIA: Luid = Luid([0x2c, 0x51, 0x01, 0, 0, 0, 0, 0]);

    fn laptop() -> Vec<DisplaySource> {
        // The built-in panel is primary and on the integrated GPU; an external monitor to
        // its right is on the discrete one.
        vec![
            DisplaySource {
                adapter: NVIDIA,
                x: 1920,
                y: 0,
            },
            DisplaySource {
                adapter: INTEL,
                x: 0,
                y: 0,
            },
        ]
    }

    #[test]
    fn pr3_render_gpu_defaults_to_the_primary_monitors_adapter() {
        let settings = CompositorSettings::parse("{}").unwrap();
        assert_eq!(settings.render_gpu, RenderGpu::Display);
        assert_eq!(
            choose(&settings.render_gpu, &laptop()),
            GpuRequest::Luid(INTEL)
        );
    }

    #[test]
    fn pr3_render_gpu_follows_the_primary_monitor_when_it_moves() {
        let sources = vec![
            DisplaySource {
                adapter: INTEL,
                x: -1920,
                y: 0,
            },
            DisplaySource {
                adapter: NVIDIA,
                x: 0,
                y: 0,
            },
        ];
        assert_eq!(
            choose(&RenderGpu::Display, &sources),
            GpuRequest::Luid(NVIDIA)
        );
    }

    #[test]
    fn pr3_render_gpu_falls_back_to_the_ranking_without_a_primary_monitor() {
        assert_eq!(choose(&RenderGpu::Display, &[]), GpuRequest::Ranking);
        let off_origin = [DisplaySource {
            adapter: INTEL,
            x: 10,
            y: 0,
        }];
        assert_eq!(
            choose(&RenderGpu::Display, &off_origin),
            GpuRequest::Ranking
        );
    }

    #[test]
    fn pr3_render_gpu_setting_chooses_integrated_discrete_or_a_name() {
        let pick = |json: &str| {
            let settings = CompositorSettings::parse(json).unwrap();
            choose(&settings.render_gpu, &laptop())
        };
        assert_eq!(
            pick(r#"{"render_gpu": "display"}"#),
            GpuRequest::Luid(INTEL)
        );
        assert_eq!(
            pick(r#"{"render_gpu": "Integrated"}"#),
            GpuRequest::Integrated
        );
        assert_eq!(pick(r#"{"render_gpu": "DISCRETE"}"#), GpuRequest::Discrete);
        assert_eq!(
            pick(r#"{"render_gpu": " RTX 4050 "}"#),
            GpuRequest::Named("RTX 4050".into())
        );
    }

    #[test]
    fn pr3_bad_render_gpu_setting_is_rejected_and_the_default_used() {
        assert!(CompositorSettings::parse(r#"{"render_gpu": "  "}"#).is_err());
        assert!(CompositorSettings::parse(r#"{"render_gpu": 1}"#).is_err());
        assert!(CompositorSettings::parse(r#"{"render_gpus": "discrete"}"#).is_err());

        let dir = std::env::temp_dir().join(format!("dsa-render-gpu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("compositor.json");
        std::fs::write(&bad, r#"{"render_gpu": ""}"#).unwrap();
        let (settings, why) = CompositorSettings::load(&bad);
        assert_eq!(settings.render_gpu, RenderGpu::Display);
        assert!(why.unwrap().contains("using the defaults"));

        let (settings, why) = CompositorSettings::load(&dir.join("missing.json"));
        assert_eq!(settings, CompositorSettings::default());
        assert!(why.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr17_the_window_manager_takes_the_render_gpu_from_the_service() {
        let file_read = std::cell::Cell::new(false);
        let file = || {
            file_read.set(true);
            (
                CompositorSettings {
                    render_gpu: RenderGpu::Integrated,
                },
                None,
            )
        };
        let (setting, notes) = render_gpu_setting(Ok("discrete".into()), file);
        assert_eq!(setting, RenderGpu::Discrete);
        assert!(notes[0].contains("service"));
        assert!(
            !file_read.get(),
            "the file is not read when the service answers"
        );
    }

    #[test]
    fn fr17_without_the_service_the_window_manager_reads_compositor_json() {
        let (setting, notes) = render_gpu_setting(Err("the pipe is not there".into()), || {
            (
                CompositorSettings {
                    render_gpu: RenderGpu::Named("Arc".into()),
                },
                Some("compositor.json is not usable".into()),
            )
        });
        assert_eq!(setting, RenderGpu::Named("Arc".into()));
        assert!(notes[0].contains("the pipe is not there"));
        assert_eq!(notes[1], "compositor.json is not usable");
    }

    #[test]
    fn fr17_a_render_gpu_chosen_in_compositor_json_is_kept() {
        let dir = std::env::temp_dir().join(format!("dsa-previous-gpu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(previous_render_gpu(&dir), None, "no file, nothing chosen");
        std::fs::create_dir_all(dir.join("DSA")).unwrap();
        std::fs::write(
            dir.join("DSA").join("compositor.json"),
            r#"{"render_gpu": " RTX 4050 "}"#,
        )
        .unwrap();
        assert_eq!(previous_render_gpu(&dir).as_deref(), Some("RTX 4050"));
        std::fs::write(
            dir.join("DSA").join("compositor.json"),
            r#"{"render_gpu": "Discrete"}"#,
        )
        .unwrap();
        assert_eq!(previous_render_gpu(&dir).as_deref(), Some("discrete"));
        std::fs::write(dir.join("DSA").join("compositor.json"), "{").unwrap();
        assert_eq!(
            previous_render_gpu(&dir),
            None,
            "an unusable file chose nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pr3_render_gpu_settings_live_beside_the_compositor_logs() {
        let path = default_settings_file(Some(Path::new("C:/Users/u/AppData/Local")));
        assert!(path.ends_with("DSA/compositor.json"));
    }

    #[test]
    fn pr3_luid_reads_like_a_windows_luid() {
        let luid = Luid::from_parts(0x0001_4e8a, 0);
        assert_eq!(luid, INTEL);
        assert_eq!(luid.low(), 0x0001_4e8a);
        assert_eq!(luid.high(), 0);
        assert_eq!(luid.to_string(), "00000000:00014e8a");
    }

    #[test]
    fn pr3_the_log_names_the_rendering_gpu_and_why() {
        let adapters = [
            AdapterName {
                luid: INTEL,
                name: "Intel(R) Arc(TM) Graphics".into(),
            },
            AdapterName {
                luid: NVIDIA,
                name: "NVIDIA GeForce RTX 4050 Laptop GPU".into(),
            },
        ];
        let request = choose(&RenderGpu::Display, &laptop());
        let asked = describe_request(&RenderGpu::Display, &request, &adapters);
        assert!(asked.contains("Intel(R) Arc(TM) Graphics (LUID 00000000:00014e8a)"));
        assert!(asked.contains("other monitors"));

        let on = |matched| RendererGpu {
            name: "Intel(R) Arc(TM) Graphics".into(),
            luid: Some(INTEL),
            matched,
        };
        assert_eq!(
            describe_outcome(&request, &on(Some(true))),
            "rendering on Intel(R) Arc(TM) Graphics (LUID 00000000:00014e8a), the GPU asked for"
        );
        let fell_back = describe_outcome(&request, &on(Some(false)));
        assert!(fell_back.starts_with("rendering on "));
        assert!(fell_back.contains("ranked first"));
        assert!(fell_back.contains("00000000:00014e8a"));
        assert!(describe_outcome(&GpuRequest::Ranking, &on(None)).contains("ranked first"));
        assert!(describe_outcome(&GpuRequest::Discrete, &on(Some(false)))
            .contains("no Vulkan GPU matches the render GPU setting"));
    }
}
