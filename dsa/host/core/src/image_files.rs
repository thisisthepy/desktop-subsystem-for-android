//! What a VM boots from an image folder: the kernel, the initrd, the kernel command line and
//! the disks, read from the folder's `image.json` and the files beside it.
//!
//! Every host reads an image the same way. What differs is data a host passes in
//! ([`ImageRules`]): the kernel names it boots, the disk formats it can attach and in which
//! order it prefers them, the guest architecture it runs, and the kernel and command line
//! its own configuration supplies.

use std::path::{Path, PathBuf};

/// The architecture of the guest. A VM runs a guest of its host's own architecture only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestArch {
    Arm64,
    X86_64,
}

impl GuestArch {
    /// The architecture this host was built for.
    pub const HOST: GuestArch = if cfg!(target_arch = "aarch64") {
        GuestArch::Arm64
    } else {
        GuestArch::X86_64
    };

    /// The name of the kernel an image ships for this architecture.
    pub fn kernel_name(self) -> &'static str {
        match self {
            GuestArch::Arm64 => "Image",
            GuestArch::X86_64 => "bzImage",
        }
    }

    /// The name this architecture has in an image's `image.json` and in messages.
    pub fn name(self) -> &'static str {
        match self {
            GuestArch::Arm64 => "arm64",
            GuestArch::X86_64 => "x86_64",
        }
    }

    /// The architecture an image's `image.json` names.
    pub fn of_manifest(arch: &str) -> Option<GuestArch> {
        match arch {
            "arm64" | "aarch64" => Some(GuestArch::Arm64),
            "x86_64" => Some(GuestArch::X86_64),
            _ => None,
        }
    }
}

/// What a VM boots from one image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFiles {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub kernel_cmdline: String,
    /// The read-only system disk of an image of two disks; `None` for an image of one.
    pub system_disk: Option<PathBuf>,
    /// The template a user's disk is made from: `data` of an image of two disks, `disk` of one.
    /// `None` for an older image that has only a system disk.
    pub disk_template: Option<PathBuf>,
}

/// What a host decides about reading an image.
#[derive(Debug, Clone, Copy)]
pub struct ImageRules<'a> {
    /// The kernel file names the host boots, in order of preference.
    pub kernel_names: &'a [&'a str],
    /// The disk formats (file extensions) the host attaches, most preferred first. The last
    /// is the published format, which names a disk that is not there yet.
    pub disk_extensions: &'a [&'a str],
    /// The guest architecture the host runs. An image whose `image.json` names another is
    /// refused before anything boots.
    pub arch: Option<GuestArch>,
    /// The kernel to boot when the image carries none of its own.
    pub fallback_kernel: Option<&'a Path>,
    /// Words that follow the image's own kernel command line.
    pub extra_cmdline: &'a str,
}

/// [`ImageRules`] that a host keeps for its whole run.
#[derive(Debug, Clone)]
pub struct ImageRulesOwned {
    pub kernel_names: Vec<String>,
    pub disk_extensions: Vec<String>,
    pub arch: Option<GuestArch>,
    pub fallback_kernel: Option<PathBuf>,
    pub extra_cmdline: String,
}

impl ImageRulesOwned {
    /// Reads the image in `dir` by these rules ([`read_image`]).
    pub fn read(&self, dir: &Path) -> Result<ImageFiles, String> {
        let kernel_names: Vec<&str> = self.kernel_names.iter().map(String::as_str).collect();
        let disk_extensions: Vec<&str> = self.disk_extensions.iter().map(String::as_str).collect();
        read_image(
            dir,
            &ImageRules {
                kernel_names: &kernel_names,
                disk_extensions: &disk_extensions,
                arch: self.arch,
                fallback_kernel: self.fallback_kernel.as_deref(),
                extra_cmdline: &self.extra_cmdline,
            },
        )
    }
}

/// Reads the image in `dir`.
///
/// An image made from the published builds carries `image.json`, `initrd.img` and either
/// `system` and `data` disks or one `disk`; its command line comes from `image.json`. A disk
/// the host can attach directly wins over the published one beside it. A folder without
/// `image.json` is an older image: its disks are found by name, and one with none has a
/// `system` disk that is attached beside a blank data disk.
pub fn read_image(dir: &Path, rules: &ImageRules<'_>) -> Result<ImageFiles, String> {
    let manifest = match std::fs::read_to_string(dir.join("image.json")) {
        Ok(text) => Some(
            serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|e| format!("the image.json in {} is not valid: {e}", dir.display()))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("reading the image.json in {}: {e}", dir.display())),
    };
    if let (Some(host), Some(image_arch)) = (
        rules.arch,
        manifest.as_ref().and_then(|m| m["arch"].as_str()),
    ) {
        if GuestArch::of_manifest(image_arch) != Some(host) {
            return Err(format!(
                "the image in {} is for {image_arch}, and this computer runs {} guests only",
                dir.display(),
                host.name()
            ));
        }
    }
    let kernel = rules
        .kernel_names
        .iter()
        .map(|name| dir.join(name))
        .find(|own| own.exists())
        .or_else(|| rules.fallback_kernel.map(Path::to_path_buf))
        .ok_or_else(|| {
            format!(
                "the image in {} has no kernel (looked for {})",
                dir.display(),
                rules.kernel_names.join(" or ")
            )
        })?;
    let image_cmdline = manifest
        .as_ref()
        .and_then(|m| m["kernel_cmdline"].as_str())
        .unwrap_or_default();
    let kernel_cmdline = format!("{image_cmdline} {}", rules.extra_cmdline)
        .trim()
        .to_string();
    let initrd = Some(dir.join("initrd.img")).filter(|path| manifest.is_some() || path.exists());
    let published = rules.disk_extensions.last().copied().unwrap_or("vhdx");
    // A disk is the first of its formats that is in the folder, else the first the image
    // lists, which is not extracted yet.
    let find = |stem: &str| -> Option<PathBuf> {
        let names: Vec<String> = rules
            .disk_extensions
            .iter()
            .map(|ext| format!("{stem}.{ext}"))
            .collect();
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
            .or_else(|| {
                names
                    .iter()
                    .find(|name| {
                        manifest
                            .as_ref()
                            .is_some_and(|m| m["files"].get(name.as_str()).is_some())
                    })
                    .map(|name| dir.join(name))
            })
    };
    // An image with an `image.json` names its disks even when they are not there yet.
    let named = |stem: &str| {
        manifest
            .is_some()
            .then(|| dir.join(format!("{stem}.{published}")))
    };
    let (system_disk, disk_template) = match find("system") {
        Some(system) => (Some(system), find("data").or_else(|| named("data"))),
        None => match find("disk").or_else(|| named("disk")) {
            Some(disk) => (None, Some(disk)),
            None => (Some(dir.join(format!("system.{published}"))), None),
        },
    };
    Ok(ImageFiles {
        kernel,
        initrd,
        kernel_cmdline,
        system_disk,
        disk_template,
    })
}
