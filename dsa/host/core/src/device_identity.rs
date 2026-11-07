//! The device identity a user's Android reports to its apps: the manufacturer, brand, model,
//! device and product name, the form factor, and the build fingerprint.
//!
//! By default the manufacturer, brand and model are the computer's, as its firmware says them
//! ([`crate::computer_identity`]), and the chip is its CPU; the device and product name, the
//! form factor and the fingerprint's build are DSA's own, which the image carries. A user may
//! change any of the values below; the service keeps the changes per user and hands the
//! identity to the guest at every boot, because Android reads these properties once, before
//! its property service makes them read only.
//!
//! The values travel as a property file in a small cpio archive appended to the boot's initrd,
//! at `/system/etc/ramdisk/build.prop`. Android's first stage init copies that file for the
//! second stage, and the image's product `build.prop` ends with `import` lines that take the
//! identity properties from it, so they win over every partition's own. Init itself reads the
//! file, so no SELinux rule is needed. The file goes with every boot, since the computer's
//! values are not in the image.
//!
//! ```text
//! <dir>/users/<user>.device.json   {"model": "My tablet"}    (a missing field: the default)
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dsa_protocol::host_api::types;
use serde::{Deserialize, Serialize};

use crate::computer_identity::{ComputerIdentity, ComputerInfo, Derived, Origin};
use crate::settings::{read_json, user_file_stem, write_json};

/// DSA's own manufacturer, brand and model, for a computer whose firmware does not say.
pub const DEFAULT_MANUFACTURER: &str = "thisisthepy";
pub const DEFAULT_BRAND: &str = "DSA";
pub const DEFAULT_MODEL: &str = "Desktop Subsystem for Android";
/// The device and product name of an x86_64 image.
pub const DEFAULT_DEVICE: &str = "dsa_x86_64";
pub const DEFAULT_NAME: &str = "dsa_x86_64";
/// The device and product name of an arm64 image.
pub const DEFAULT_DEVICE_ARM64: &str = "dsa_arm64";
/// Large-screen layouts without keeping phone apps out: apps pick their layout from the
/// window's size, not from this value.
pub const DEFAULT_CHARACTERISTICS: &str = "tablet";
/// The image is a user build, so its fingerprints say so.
pub const DEFAULT_BUILD_TYPE: &str = "user";
pub const DEFAULT_BUILD_TAGS: &str = "release-keys";

/// The partitions whose own `ro.product.<partition>.*` values and fingerprints the identity
/// sets, so that every one of them says the same.
pub const PARTITIONS: [&str; 8] = [
    "system",
    "system_ext",
    "vendor",
    "product",
    "odm",
    "vendor_dlkm",
    "odm_dlkm",
    "system_dlkm",
];

/// Where in the initrd the identity's property file goes. First stage init copies this one
/// file for the second stage; no other file of the ramdisk survives to it.
pub const RAMDISK_PROP_PATH: &str = "system/etc/ramdisk/build.prop";

const MAX_TEXT: usize = 64;
const MAX_NAME: usize = 32;
const MAX_FINGERPRINT: usize = 256;

/// One value a user can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Manufacturer,
    Brand,
    Model,
    Device,
    Name,
    Characteristics,
    /// The whole fingerprint string. When set, it is the fingerprint, whatever its parts say.
    Fingerprint,
    FingerprintRelease,
    FingerprintId,
    FingerprintIncremental,
    FingerprintType,
    FingerprintTags,
}

impl Field {
    pub const ALL: [Field; 12] = [
        Field::Manufacturer,
        Field::Brand,
        Field::Model,
        Field::Device,
        Field::Name,
        Field::Characteristics,
        Field::Fingerprint,
        Field::FingerprintRelease,
        Field::FingerprintId,
        Field::FingerprintIncremental,
        Field::FingerprintType,
        Field::FingerprintTags,
    ];

    /// The name `dsa settings set device.<name>` takes.
    pub fn key(self) -> &'static str {
        match self {
            Field::Manufacturer => "manufacturer",
            Field::Brand => "brand",
            Field::Model => "model",
            Field::Device => "device",
            Field::Name => "name",
            Field::Characteristics => "characteristics",
            Field::Fingerprint => "fingerprint",
            Field::FingerprintRelease => "fingerprint.release",
            Field::FingerprintId => "fingerprint.id",
            Field::FingerprintIncremental => "fingerprint.incremental",
            Field::FingerprintType => "fingerprint.type",
            Field::FingerprintTags => "fingerprint.tags",
        }
    }

    pub fn parse(key: &str) -> Result<Field, String> {
        Field::ALL
            .into_iter()
            .find(|field| field.key() == key)
            .ok_or_else(|| {
                format!(
                    "there is no device field {key:?}; the fields are {}",
                    Field::ALL.map(Field::key).join(", ")
                )
            })
    }

    /// The default, for the fields whose default is the same on every computer and image.
    /// The manufacturer, brand and model follow the computer, and the device and product
    /// name the image's architecture, so a value a user typed for them is kept as typed.
    pub fn fixed_default(self) -> Option<&'static str> {
        match self {
            Field::Characteristics => Some(DEFAULT_CHARACTERISTICS),
            Field::FingerprintType => Some(DEFAULT_BUILD_TYPE),
            _ => None,
        }
    }

    /// Whether `value` may be this field's value, and if not, why.
    pub fn check(self, value: &str) -> Result<(), String> {
        match self {
            Field::Manufacturer => printable("the manufacturer", value),
            Field::Model => printable("the model", value),
            Field::Brand => name_like("the brand", value),
            Field::Device => name_like("the device", value),
            Field::Name => name_like("the product name", value),
            Field::Characteristics => characteristics(value),
            Field::Fingerprint => fingerprint(value),
            Field::FingerprintRelease => part("the fingerprint's release", value),
            Field::FingerprintId => part("the fingerprint's build ID", value),
            Field::FingerprintIncremental => part("the fingerprint's incremental version", value),
            Field::FingerprintType => build_type(value),
            Field::FingerprintTags => tags(value),
        }
    }
}

fn printable(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_TEXT {
        return Err(format!("{what} is 1 to {MAX_TEXT} characters long"));
    }
    if !value.chars().all(|c| (' '..='~').contains(&c)) {
        return Err(format!(
            "{what} may hold only letters, digits, spaces and the printable ASCII signs"
        ));
    }
    if value.starts_with(' ') || value.ends_with(' ') {
        return Err(format!("{what} may not start or end with a space"));
    }
    Ok(())
}

fn name_like(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_NAME {
        return Err(format!("{what} is 1 to {MAX_NAME} characters long"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        || !value.starts_with(|c: char| c.is_ascii_alphanumeric())
    {
        return Err(format!(
            "{what} may hold only letters, digits, '.', '_' and '-', and starts with a letter \
             or a digit"
        ));
    }
    Ok(())
}

fn characteristics(value: &str) -> Result<(), String> {
    let word = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    if value.len() > MAX_TEXT || !value.split(',').all(word) {
        return Err(format!(
            "the form factor is words of lower-case letters, digits and '_', separated by \
             commas, at most {MAX_TEXT} characters in all (such as tablet or default)"
        ));
    }
    Ok(())
}

fn part(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_TEXT
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "{what} is 1 to {MAX_TEXT} letters, digits, '.', '_' or '-'"
        ));
    }
    Ok(())
}

fn build_type(value: &str) -> Result<(), String> {
    match value {
        "user" | "userdebug" | "eng" => Ok(()),
        _ => Err(format!(
            "the fingerprint's build type is user, userdebug or eng, not {value:?}"
        )),
    }
}

fn tags(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_TEXT
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ','))
    {
        return Err(format!(
            "the fingerprint's tags are 1 to {MAX_TEXT} letters, digits, '.', '_', '-' or ',' \
             (such as release-keys)"
        ));
    }
    Ok(())
}

/// The parts of a fingerprint, `brand/name/device:release/id/incremental:type/tags`.
pub fn split_fingerprint(value: &str) -> Option<[&str; 8]> {
    let sections: Vec<&str> = value.split(':').collect();
    let &[product, build, kind] = sections.as_slice() else {
        return None;
    };
    let product: Vec<&str> = product.split('/').collect();
    let build: Vec<&str> = build.split('/').collect();
    let kind: Vec<&str> = kind.split('/').collect();
    let (&[brand, name, device], &[release, id, incremental], &[build_type, tags]) =
        (product.as_slice(), build.as_slice(), kind.as_slice())
    else {
        return None;
    };
    Some([
        brand,
        name,
        device,
        release,
        id,
        incremental,
        build_type,
        tags,
    ])
}

fn fingerprint(value: &str) -> Result<(), String> {
    let format = "a fingerprint reads brand/name/device:release/id/incremental:type/tags";
    if value.len() > MAX_FINGERPRINT {
        return Err(format!(
            "the fingerprint is at most {MAX_FINGERPRINT} characters long"
        ));
    }
    let Some([brand, name, device, release, id, incremental, kind, tag]) = split_fingerprint(value)
    else {
        return Err(format!("{format}, and {value:?} does not"));
    };
    let fix = |result: Result<(), String>| result.map_err(|why| format!("{format}: {why}"));
    fix(name_like("the brand", brand))?;
    fix(name_like("the product name", name))?;
    fix(name_like("the device", device))?;
    fix(part("the release", release))?;
    fix(part("the build ID", id))?;
    fix(part("the incremental version", incremental))?;
    fix(build_type(kind))?;
    fix(tags(tag))
}

/// What the image's own build says, from its `image.json`: the parts of a fingerprint a user
/// does not have to choose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageBuild {
    pub release: String,
    pub id: String,
    pub incremental: String,
    pub tags: String,
    /// The guest architecture, `x86_64` or `arm64`; empty when the image does not say, and
    /// it is then the host's own.
    pub arch: String,
}

impl ImageBuild {
    /// Whether the image is an arm64 one.
    pub fn is_arm64(&self) -> bool {
        match self.arch.as_str() {
            "" => cfg!(target_arch = "aarch64"),
            arch => arch == "arm64",
        }
    }

    /// The device and product name the image has.
    pub fn default_device(&self) -> &'static str {
        if self.is_arm64() {
            DEFAULT_DEVICE_ARM64
        } else {
            DEFAULT_DEVICE
        }
    }
    /// The image's build from its `image.json`; `None` for an image made before it said.
    pub fn from_manifest(manifest: &serde_json::Value) -> Option<ImageBuild> {
        let identity = manifest.get("identity")?;
        let text = |key: &str| -> Option<String> {
            identity
                .get(key)?
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Some(ImageBuild {
            release: text("release")?,
            id: text("id")?,
            incremental: text("incremental")?,
            tags: text("tags").unwrap_or_else(|| DEFAULT_BUILD_TAGS.into()),
            arch: manifest
                .get("arch")
                .and_then(|a| a.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// The build of the image in `dir`, read from its `image.json`.
    pub fn from_image_dir(dir: &Path) -> Option<ImageBuild> {
        let text = std::fs::read_to_string(dir.join("image.json")).ok()?;
        ImageBuild::from_manifest(&serde_json::from_str(&text).ok()?)
    }
}

/// A user's device identity. An empty value is the default: the computer's or DSA's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceIdentity {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub manufacturer: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub brand: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub device: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub characteristics: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fingerprint: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fingerprint_release: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fingerprint_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fingerprint_incremental: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fingerprint_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub fingerprint_tags: String,
}

impl DeviceIdentity {
    pub fn get(&self, field: Field) -> &str {
        match field {
            Field::Manufacturer => &self.manufacturer,
            Field::Brand => &self.brand,
            Field::Model => &self.model,
            Field::Device => &self.device,
            Field::Name => &self.name,
            Field::Characteristics => &self.characteristics,
            Field::Fingerprint => &self.fingerprint,
            Field::FingerprintRelease => &self.fingerprint_release,
            Field::FingerprintId => &self.fingerprint_id,
            Field::FingerprintIncremental => &self.fingerprint_incremental,
            Field::FingerprintType => &self.fingerprint_type,
            Field::FingerprintTags => &self.fingerprint_tags,
        }
    }

    fn slot(&mut self, field: Field) -> &mut String {
        match field {
            Field::Manufacturer => &mut self.manufacturer,
            Field::Brand => &mut self.brand,
            Field::Model => &mut self.model,
            Field::Device => &mut self.device,
            Field::Name => &mut self.name,
            Field::Characteristics => &mut self.characteristics,
            Field::Fingerprint => &mut self.fingerprint,
            Field::FingerprintRelease => &mut self.fingerprint_release,
            Field::FingerprintId => &mut self.fingerprint_id,
            Field::FingerprintIncremental => &mut self.fingerprint_incremental,
            Field::FingerprintType => &mut self.fingerprint_type,
            Field::FingerprintTags => &mut self.fingerprint_tags,
        }
    }

    /// Sets `field` to `value`, or to the default when `value` is empty or is the default.
    /// A value that is not allowed changes nothing.
    pub fn set(&mut self, field: Field, value: &str) -> Result<(), String> {
        if value.is_empty() || field.fixed_default() == Some(value) {
            self.slot(field).clear();
            return Ok(());
        }
        field.check(value)?;
        *self.slot(field) = value.to_string();
        Ok(())
    }

    /// Puts `field` back to the default.
    pub fn reset(&mut self, field: Field) {
        self.slot(field).clear();
    }

    /// The same identity with every value that equals its default kept as the default.
    pub fn normalised(&self) -> DeviceIdentity {
        let mut out = self.clone();
        for field in Field::ALL {
            if field.fixed_default() == Some(out.get(field)) {
                out.reset(field);
            }
        }
        out
    }

    /// Whether every value is allowed, and if not, the first that is not and why.
    pub fn check(&self) -> Result<(), String> {
        for field in Field::ALL {
            let value = self.get(field);
            if !value.is_empty() {
                field.check(value)?;
            }
        }
        Ok(())
    }

    /// Whether the user changed nothing.
    pub fn is_default(&self) -> bool {
        Field::ALL
            .into_iter()
            .all(|field| self.get(field).is_empty())
    }

    /// Every value as Android will report it: the user's where they chose one, else the
    /// default. The manufacturer, brand and model default to the computer's (`computer`, which
    /// holds DSA's own where the computer said nothing usable), the device and product name to
    /// the image's architecture, and the rest to DSA's values and the image's build. The
    /// fingerprint's release, build ID and incremental version stay empty when neither the
    /// user nor `build` gives them, and so does the composed fingerprint.
    pub fn effective(
        &self,
        build: Option<&ImageBuild>,
        computer: &ComputerIdentity,
    ) -> DeviceIdentity {
        let build = build.cloned().unwrap_or_default();
        let device = build.default_device();
        fn or(value: &str, default: &str) -> String {
            if value.is_empty() {
                default.to_string()
            } else {
                value.to_string()
            }
        }
        let image_tags = if build.tags.is_empty() {
            DEFAULT_BUILD_TAGS
        } else {
            build.tags.as_str()
        };
        let mut out = DeviceIdentity {
            manufacturer: or(&self.manufacturer, &computer.manufacturer.value),
            brand: or(&self.brand, &computer.brand.value),
            model: or(&self.model, &computer.model.value),
            device: or(&self.device, device),
            name: or(&self.name, device),
            characteristics: or(&self.characteristics, DEFAULT_CHARACTERISTICS),
            fingerprint: String::new(),
            fingerprint_release: or(&self.fingerprint_release, &build.release),
            fingerprint_id: or(&self.fingerprint_id, &build.id),
            fingerprint_incremental: or(&self.fingerprint_incremental, &build.incremental),
            fingerprint_type: or(&self.fingerprint_type, DEFAULT_BUILD_TYPE),
            fingerprint_tags: or(&self.fingerprint_tags, image_tags),
        };
        out.fingerprint = if self.fingerprint.is_empty() {
            out.composed_fingerprint().unwrap_or_default()
        } else {
            self.fingerprint.clone()
        };
        out
    }

    /// The fingerprint these values make, when every part of it is known.
    pub fn composed_fingerprint(&self) -> Option<String> {
        let known = [
            &self.fingerprint_release,
            &self.fingerprint_id,
            &self.fingerprint_incremental,
        ];
        if known.iter().any(|part| part.is_empty()) {
            return None;
        }
        Some(format!(
            "{}/{}/{}:{}/{}/{}:{}/{}",
            self.brand,
            self.name,
            self.device,
            self.fingerprint_release,
            self.fingerprint_id,
            self.fingerprint_incremental,
            self.fingerprint_type,
            self.fingerprint_tags
        ))
    }

    /// The property file that gives Android this identity on `computer`. Every boot gets one,
    /// as the computer's values are not in the image. Besides the values a user may change,
    /// it names the model name (`ro.product.marketname`) and the chip (`ro.soc.manufacturer`,
    /// `ro.soc.model`) when the computer says them.
    pub fn boot_properties(
        &self,
        build: Option<&ImageBuild>,
        computer: &ComputerIdentity,
    ) -> String {
        let e = self.effective(build, computer);
        let mut lines = vec![
            "# The device identity: the computer's, with what was chosen in DSA's settings"
                .to_string(),
        ];
        for (field, value) in [
            ("brand", &e.brand),
            ("manufacturer", &e.manufacturer),
            ("model", &e.model),
            ("device", &e.device),
            ("name", &e.name),
        ] {
            lines.push(format!("ro.product.{field}={value}"));
            for partition in PARTITIONS {
                lines.push(format!("ro.product.{partition}.{field}={value}"));
            }
        }
        lines.push(format!("ro.build.characteristics={}", e.characteristics));
        lines.push(format!("ro.build.product={}", e.device));
        lines.push(format!("ro.build.flavor={}-{}", e.name, e.fingerprint_type));
        // Each partition's fingerprint is always the composed one, so it keeps the image's own
        // tag in its incremental version: Android takes a new image for an upgrade by it.
        if let Some(composed) = e.composed_fingerprint() {
            for partition in PARTITIONS {
                lines.push(format!("ro.{partition}.build.fingerprint={composed}"));
            }
            lines.push(format!(
                "ro.build.description={}-{} {} {} {} {}",
                e.name,
                e.fingerprint_type,
                e.fingerprint_release,
                e.fingerprint_id,
                e.fingerprint_incremental,
                e.fingerprint_tags
            ));
        }
        if !e.fingerprint.is_empty() {
            lines.push(format!("ro.build.fingerprint={}", e.fingerprint));
        }
        lines.push(format!(
            "ro.product.marketname={}",
            computer.model_name.value
        ));
        for (key, value) in [
            ("ro.soc.manufacturer", &computer.soc_manufacturer.value),
            ("ro.soc.model", &computer.soc_model.value),
        ] {
            if !value.is_empty() {
                lines.push(format!("{key}={value}"));
            }
        }
        lines.join("\n") + "\n"
    }
}

/// Values Android reports that follow the computer and that a user does not choose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComputerValues {
    /// The name the product is sold under, such as `Galaxy Book4 Ultra`.
    pub model_name: String,
    /// Android's device name: the computer's name, else the model.
    pub device_name: String,
    /// The chip's maker and model; empty when the computer does not say, and Android keeps
    /// its own.
    pub soc_manufacturer: String,
    pub soc_model: String,
}

/// The keys of [`ComputerValues`], as the local API and `dsa settings device --json` name
/// them.
pub const COMPUTER_KEYS: [&str; 4] = ["model_name", "device_name", "soc_manufacturer", "soc_model"];

/// A user's identity as the settings show it: what they chose, what Android reports from its
/// next start, the defaults, and which defaults are the computer's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityReport {
    pub chosen: DeviceIdentity,
    pub effective: DeviceIdentity,
    /// Every value as Android reports it when the user chooses nothing.
    pub defaults: DeviceIdentity,
    /// What follows the computer, as Android reports it.
    pub computer: ComputerValues,
    /// The keys (a [`Field::key`] or one of [`COMPUTER_KEYS`]) whose default is the
    /// computer's; every other default is DSA's.
    pub from_computer: Vec<String>,
}

impl IdentityReport {
    /// The report for `chosen` on `computer`, for an image of `build`.
    pub fn new(
        chosen: &DeviceIdentity,
        build: Option<&ImageBuild>,
        computer: &ComputerIdentity,
    ) -> IdentityReport {
        let effective = chosen.effective(build, computer);
        let device_name = if computer.device_name.value.is_empty() {
            effective.model.clone()
        } else {
            computer.device_name.value.clone()
        };
        let from = |key: &str, derived: &Derived| {
            (derived.origin == Origin::Computer).then(|| key.to_string())
        };
        let from_computer = [
            from("manufacturer", &computer.manufacturer),
            from("brand", &computer.brand),
            from("model", &computer.model),
            from("model_name", &computer.model_name),
            from("device_name", &computer.device_name),
            from("soc_manufacturer", &computer.soc_manufacturer),
            from("soc_model", &computer.soc_model),
        ]
        .into_iter()
        .flatten()
        .collect();
        IdentityReport {
            chosen: chosen.clone(),
            effective,
            defaults: DeviceIdentity::default().effective(build, computer),
            computer: ComputerValues {
                model_name: computer.model_name.value.clone(),
                device_name,
                soc_manufacturer: computer.soc_manufacturer.value.clone(),
                soc_model: computer.soc_model.value.clone(),
            },
            from_computer,
        }
    }

    /// Where the default of `key` came from: `computer` or `dsa`.
    pub fn origin(&self, key: &str) -> &'static str {
        if self.from_computer.iter().any(|k| k == key) {
            Origin::Computer.key()
        } else {
            Origin::Dsa.key()
        }
    }

    /// The value of one of [`COMPUTER_KEYS`].
    pub fn computer_value(&self, key: &str) -> &str {
        match key {
            "model_name" => &self.computer.model_name,
            "device_name" => &self.computer.device_name,
            "soc_manufacturer" => &self.computer.soc_manufacturer,
            "soc_model" => &self.computer.soc_model,
            _ => "",
        }
    }
}

/// Reads what the computer says it is; each host has its own.
pub type ComputerSource = Arc<dyn Fn() -> ComputerInfo + Send + Sync>;

/// A user's identity at each boot of their Android: the host builds the boot's property file
/// from it, and tells the agent the device name it found.
pub struct BootIdentity {
    store: Arc<dyn DeviceIdentityStore>,
    user: String,
    computer: ComputerSource,
    /// The device name the last boot was given.
    device_name: Mutex<Option<String>>,
}

impl BootIdentity {
    pub fn new(
        store: Arc<dyn DeviceIdentityStore>,
        user: impl Into<String>,
        computer: ComputerSource,
    ) -> Arc<BootIdentity> {
        Arc::new(BootIdentity {
            store,
            user: user.into(),
            computer,
            device_name: Mutex::new(None),
        })
    }

    /// Reads the computer and the user's choices now, and gives the property file for this
    /// boot. A store that cannot be read leaves the computer's values standing.
    pub fn boot_properties(&self, build: Option<&ImageBuild>) -> String {
        let chosen = self.store.get(&self.user).unwrap_or_else(|why| {
            eprintln!("device identity: {why}; Android starts with the computer's identity");
            DeviceIdentity::default()
        });
        let computer = (self.computer)().identity();
        let report = IdentityReport::new(&chosen, build, &computer);
        eprintln!(
            "device identity: {} {} ({}), named {:?}",
            report.effective.manufacturer,
            report.effective.model,
            report.computer.model_name,
            report.computer.device_name
        );
        *self.device_name.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(report.computer.device_name.clone());
        chosen.boot_properties(build, &computer)
    }

    /// The device name the last boot was given, which the agent makes the guest's.
    pub fn device_name(&self) -> Option<String> {
        self.device_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|name| !name.is_empty())
    }
}

/// `report` as the local API carries it.
pub fn report_to_wire(report: &IdentityReport) -> types::DeviceIdentityReport {
    let mut effective = to_wire(&report.effective);
    effective.model_name = report.computer.model_name.clone();
    effective.device_name = report.computer.device_name.clone();
    effective.soc_manufacturer = report.computer.soc_manufacturer.clone();
    effective.soc_model = report.computer.soc_model.clone();
    types::DeviceIdentityReport {
        chosen: Some(to_wire(&report.chosen)),
        effective: Some(effective.clone()),
        defaults: Some(types::DeviceIdentity {
            model_name: effective.model_name.clone(),
            device_name: effective.device_name.clone(),
            soc_manufacturer: effective.soc_manufacturer.clone(),
            soc_model: effective.soc_model.clone(),
            ..to_wire(&report.defaults)
        }),
        from_computer: report.from_computer.clone(),
    }
}

/// A report the local API carried.
pub fn report_from_wire(report: types::DeviceIdentityReport) -> IdentityReport {
    let effective = report.effective.unwrap_or_default();
    IdentityReport {
        chosen: from_wire(report.chosen.unwrap_or_default()),
        computer: ComputerValues {
            model_name: effective.model_name.clone(),
            device_name: effective.device_name.clone(),
            soc_manufacturer: effective.soc_manufacturer.clone(),
            soc_model: effective.soc_model.clone(),
        },
        effective: from_wire(effective),
        defaults: from_wire(report.defaults.unwrap_or_default()),
        from_computer: report.from_computer,
    }
}

/// `identity` as the local API carries it.
pub fn to_wire(identity: &DeviceIdentity) -> types::DeviceIdentity {
    types::DeviceIdentity {
        manufacturer: identity.manufacturer.clone(),
        brand: identity.brand.clone(),
        model: identity.model.clone(),
        device: identity.device.clone(),
        name: identity.name.clone(),
        characteristics: identity.characteristics.clone(),
        fingerprint: identity.fingerprint.clone(),
        fingerprint_release: identity.fingerprint_release.clone(),
        fingerprint_id: identity.fingerprint_id.clone(),
        fingerprint_incremental: identity.fingerprint_incremental.clone(),
        fingerprint_type: identity.fingerprint_type.clone(),
        fingerprint_tags: identity.fingerprint_tags.clone(),
        ..Default::default()
    }
}

/// An identity the local API carried.
pub fn from_wire(identity: types::DeviceIdentity) -> DeviceIdentity {
    DeviceIdentity {
        manufacturer: identity.manufacturer,
        brand: identity.brand,
        model: identity.model,
        device: identity.device,
        name: identity.name,
        characteristics: identity.characteristics,
        fingerprint: identity.fingerprint,
        fingerprint_release: identity.fingerprint_release,
        fingerprint_id: identity.fingerprint_id,
        fingerprint_incremental: identity.fingerprint_incremental,
        fingerprint_type: identity.fingerprint_type,
        fingerprint_tags: identity.fingerprint_tags,
    }
}

/// A newc cpio archive holding `properties` at [`RAMDISK_PROP_PATH`], with the folders above
/// it, for appending to an initrd.
pub fn ramdisk_overlay(properties: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut inode = 1;
    for dir in ["system", "system/etc", "system/etc/ramdisk"] {
        cpio_entry(&mut out, inode, dir, 0o040755, &[]);
        inode += 1;
    }
    cpio_entry(
        &mut out,
        inode,
        RAMDISK_PROP_PATH,
        0o100644,
        properties.as_bytes(),
    );
    cpio_entry(&mut out, 0, "TRAILER!!!", 0, &[]);
    out
}

fn pad4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn cpio_entry(out: &mut Vec<u8>, inode: u32, name: &str, mode: u32, data: &[u8]) {
    let links = if mode & 0o040000 != 0 { 2 } else { 1 };
    let fields = [
        inode,
        mode,
        0, // uid
        0, // gid
        links,
        0, // mtime
        data.len() as u32,
        0, // dev major
        0, // dev minor
        0, // rdev major
        0, // rdev minor
        name.len() as u32 + 1,
        0, // check
    ];
    out.extend_from_slice(b"070701");
    for field in fields {
        out.extend_from_slice(format!("{field:08X}").as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    pad4(out);
    out.extend_from_slice(data);
    pad4(out);
}

/// `initrd` followed by `overlay`, which starts on a four-byte boundary as the kernel needs
/// for an uncompressed archive after a compressed one.
pub fn append_overlay(initrd: &[u8], overlay: &[u8]) -> Vec<u8> {
    let mut out = initrd.to_vec();
    pad4(&mut out);
    out.extend_from_slice(overlay);
    out
}

/// Where the service keeps each user's device identity.
pub trait DeviceIdentityStore: Send + Sync {
    fn get(&self, user: &str) -> Result<DeviceIdentity, String>;
    /// Replaces the identity of `user`. A value that is not allowed changes nothing.
    fn set(&self, user: &str, identity: &DeviceIdentity) -> Result<DeviceIdentity, String>;
}

/// Each user's identity in a JSON file in the service's settings folder.
pub struct FileDeviceIdentities {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl FileDeviceIdentities {
    /// `dir` is the settings folder; the files go in its `users` folder.
    pub fn new(dir: impl Into<PathBuf>) -> FileDeviceIdentities {
        FileDeviceIdentities {
            dir: dir.into(),
            lock: Mutex::new(()),
        }
    }

    fn file(&self, user: &str) -> Result<PathBuf, String> {
        Ok(self
            .dir
            .join("users")
            .join(format!("{}.device.json", user_file_stem(user)?)))
    }
}

impl DeviceIdentityStore for FileDeviceIdentities {
    fn get(&self, user: &str) -> Result<DeviceIdentity, String> {
        let file = self.file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(read_json(&file)?.unwrap_or_default())
    }

    fn set(&self, user: &str, identity: &DeviceIdentity) -> Result<DeviceIdentity, String> {
        identity.check()?;
        let identity = identity.normalised();
        let file = self.file(user)?;
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if identity.is_default() {
            match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("removing {}: {e}", file.display())),
            }
        } else {
            write_json(&file, &identity)?;
        }
        Ok(identity)
    }
}

/// Identities kept in memory, for tests and for a service that keeps no folder.
#[derive(Default)]
pub struct MemoryDeviceIdentities {
    users: Mutex<std::collections::HashMap<String, DeviceIdentity>>,
}

impl DeviceIdentityStore for MemoryDeviceIdentities {
    fn get(&self, user: &str) -> Result<DeviceIdentity, String> {
        Ok(self
            .users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .cloned()
            .unwrap_or_default())
    }

    fn set(&self, user: &str, identity: &DeviceIdentity) -> Result<DeviceIdentity, String> {
        identity.check()?;
        let identity = identity.normalised();
        self.users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(user.to_string(), identity.clone());
        Ok(identity)
    }
}
