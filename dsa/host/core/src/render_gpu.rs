//! Which GPU the user wants the host renderer on: the setting's words, the same on every host.
//!
//! The setting is `display` (the GPU that drives the primary monitor), `integrated`,
//! `discrete`, or part of a GPU's name (`"RTX"`, `"Arc"`), matched ignoring case. What each
//! host asks its renderer for is its own: only a host that knows its displays can tell which
//! GPU drives the primary monitor.

use std::fmt;

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

#[cfg(test)]
mod tests {
    use super::RenderGpu;

    #[test]
    fn pr3_the_render_gpu_setting_reads_three_keywords_and_otherwise_a_name() {
        assert_eq!(RenderGpu::parse("Display"), Ok(RenderGpu::Display));
        assert_eq!(RenderGpu::parse(" INTEGRATED "), Ok(RenderGpu::Integrated));
        assert_eq!(RenderGpu::parse("discrete"), Ok(RenderGpu::Discrete));
        assert_eq!(
            RenderGpu::parse("  RTX 4070 "),
            Ok(RenderGpu::Named("RTX 4070".into()))
        );
        assert!(RenderGpu::parse("   ").is_err());
    }

    #[test]
    fn pr3_the_render_gpu_setting_is_kept_as_a_keyword_or_the_name_as_given() {
        assert_eq!(RenderGpu::Display.value(), "display");
        assert_eq!(RenderGpu::Named("Arc".into()).value(), "Arc");
        assert_eq!(RenderGpu::Named("Arc".into()).to_string(), "\"Arc\"");
    }

    #[test]
    fn pr3_the_render_gpu_setting_is_read_from_json_text() {
        let gpu: RenderGpu = serde_json::from_str("\"discrete\"").unwrap();
        assert_eq!(gpu, RenderGpu::Discrete);
        assert!(serde_json::from_str::<RenderGpu>("\"\"").is_err());
    }
}
