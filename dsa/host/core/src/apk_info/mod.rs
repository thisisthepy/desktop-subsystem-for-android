//! What an APK on the host says about itself, read from the file before Android sees it.
//!
//! The install window shows this before the user decides, and before Android is started:
//! the app's name and icon, its package, its version, the Android versions it is made for,
//! the permissions it asks for and the certificate it is signed with. Everything comes from
//! the file: the binary manifest (`AndroidManifest.xml`), the resource table
//! (`resources.arsc`) for the name and the icon, and the APK's signing block or JAR
//! signature for the signer. Nothing here runs code from the APK, and a damaged file is
//! refused with a reason rather than read on.

pub mod arsc;
pub mod axml;
#[cfg(any(test, feature = "conformance"))]
pub mod fixture;
pub mod inflate;
pub mod permissions;
pub mod signing;
pub mod vector;
pub mod zip;

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use arsc::ResourceTable;
use axml::{Element, Value, TYPE_FIRST_COLOR, TYPE_LAST_COLOR, TYPE_REFERENCE, TYPE_STRING};
pub use permissions::Permission;
pub use signing::Signer;
pub use vector::{Vector, VectorPath};
use zip::Archive;

/// The framework's resource IDs of the manifest attributes read here.
pub(crate) mod attr {
    pub const LABEL: u32 = 0x0101_0001;
    pub const ICON: u32 = 0x0101_0002;
    pub const NAME: u32 = 0x0101_0003;
    pub const MIN_SDK_VERSION: u32 = 0x0101_020c;
    pub const VERSION_CODE: u32 = 0x0101_021b;
    pub const VERSION_NAME: u32 = 0x0101_021c;
    pub const TARGET_SDK_VERSION: u32 = 0x0101_0270;
    pub const DRAWABLE: u32 = 0x0101_0199;
    pub const VERSION_CODE_MAJOR: u32 = 0x0101_0576;
}

/// The largest manifest read.
const MAX_MANIFEST: usize = 4 * 1024 * 1024;
/// The largest resource table read.
const MAX_RESOURCES: usize = 64 * 1024 * 1024;
/// The largest icon image read.
pub const MAX_ICON: usize = 4 * 1024 * 1024;

/// One layer of an adaptive icon.
#[derive(Debug, Clone, PartialEq)]
pub enum Layer {
    /// A PNG, WebP or JPEG image.
    Image(Vec<u8>),
    /// A solid colour, as 0xAARRGGBB.
    Color(u32),
    /// A vector drawable, drawn by the host.
    Vector(Vector),
    /// A drawable the host does not draw, such as one whose colours come from the theme.
    Unknown,
}

/// The app's launcher icon.
#[derive(Debug, Clone, PartialEq)]
pub enum Icon {
    /// A PNG, WebP or JPEG image.
    Image(Vec<u8>),
    /// An adaptive icon: a background and a foreground of 108 dp, of which the middle 72 dp
    /// show, masked to the launcher's shape.
    Adaptive {
        background: Layer,
        foreground: Layer,
    },
    /// A vector drawable, drawn whole in its bounds.
    Vector(Vector),
}

/// What an APK says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ApkInfo {
    pub package: String,
    /// The app's name as its launcher shows it in the default language; None when it has
    /// none.
    pub label: Option<String>,
    /// The full version code (versionCodeMajor in the high 32 bits).
    pub version_code: i64,
    pub version_name: Option<String>,
    /// The lowest API level the app runs on.
    pub min_sdk: u32,
    /// The API level the app is made for.
    pub target_sdk: u32,
    pub permissions: Vec<Permission>,
    pub icon: Option<Icon>,
    pub signer: Option<Signer>,
    /// The processor types the app's native code is built for; empty when it has none.
    pub abis: Vec<String>,
    /// The file's size in bytes.
    pub size: u64,
}

impl ApkInfo {
    /// Reads the APK at `path`.
    pub fn read(path: &Path) -> Result<ApkInfo, String> {
        let file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let size = file
            .metadata()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?
            .len();
        ApkInfo::read_from(BufReader::new(file), size)
    }

    /// Reads the APK `reader` holds, of `size` bytes.
    pub fn read_from<R: Read + Seek>(reader: R, size: u64) -> Result<ApkInfo, String> {
        let mut archive = Archive::open(reader)?;
        let manifest_entry = archive
            .entry("AndroidManifest.xml")
            .cloned()
            .ok_or("the file has no AndroidManifest.xml, so it is not an APK")?;
        let manifest = archive.read(&manifest_entry, MAX_MANIFEST)?;
        let manifest =
            axml::parse(&manifest).map_err(|why| format!("AndroidManifest.xml: {why}"))?;
        let table = match archive.entry("resources.arsc").cloned() {
            Some(entry) => {
                let data = archive.read(&entry, MAX_RESOURCES)?;
                ResourceTable::parse(&data).map_err(|why| format!("resources.arsc: {why}"))?
            }
            None => ResourceTable::default(),
        };
        let root = manifest
            .elements
            .first()
            .filter(|element| element.name == "manifest")
            .ok_or("AndroidManifest.xml has no manifest element")?;
        let package = root
            .string(0, "package", &manifest.pool)
            .filter(|package| crate::apk::valid_package_name(package))
            .ok_or("AndroidManifest.xml names no valid package")?;
        let version_code = i64::from(root.integer(attr::VERSION_CODE, "versionCode").unwrap_or(0))
            | (i64::from(
                root.integer(attr::VERSION_CODE_MAJOR, "versionCodeMajor")
                    .unwrap_or(0),
            ) << 32);
        let version_name = text(
            root,
            attr::VERSION_NAME,
            "versionName",
            &manifest.pool,
            &table,
        );
        let children = || manifest.elements.iter().filter(|e| e.depth == 1);
        let uses_sdk = children().find(|e| e.name == "uses-sdk");
        let min_sdk = uses_sdk
            .and_then(|e| e.integer(attr::MIN_SDK_VERSION, "minSdkVersion"))
            .unwrap_or(1);
        let target_sdk = uses_sdk
            .and_then(|e| e.integer(attr::TARGET_SDK_VERSION, "targetSdkVersion"))
            .unwrap_or(min_sdk);
        let mut permissions: Vec<Permission> = Vec::new();
        for element in children().filter(|e| {
            matches!(
                e.name.as_str(),
                "uses-permission" | "uses-permission-sdk-23" | "uses-permission-sdk-m"
            )
        }) {
            if let Some(name) = element.string(attr::NAME, "name", &manifest.pool) {
                if !permissions.iter().any(|p| p.name == name) {
                    permissions.push(permissions::describe(&name));
                }
            }
        }
        let application = children().find(|e| e.name == "application");
        let label = application
            .and_then(|e| text(e, attr::LABEL, "label", &manifest.pool, &table))
            .filter(|label| !label.trim().is_empty());
        let icon = application
            .and_then(|e| e.attribute(attr::ICON, "icon"))
            .filter(|a| a.value.kind == TYPE_REFERENCE)
            .and_then(|a| icon(&mut archive, &table, a.value.data));
        let mut abis: Vec<String> = Vec::new();
        for entry in &archive.entries {
            if let Some(rest) = entry.name.strip_prefix("lib/") {
                if let Some((abi, file)) = rest.split_once('/') {
                    if !file.is_empty() && !abis.iter().any(|a| a == abi) {
                        abis.push(abi.to_string());
                    }
                }
            }
        }
        abis.sort();
        let signer = signing::signer(&mut archive);
        Ok(ApkInfo {
            package,
            label,
            version_code,
            version_name,
            min_sdk,
            target_sdk,
            permissions,
            icon,
            signer,
            abis,
            size,
        })
    }

    /// The certificates an update must be signed by one of: the current signer, then the
    /// earlier ones its proof of rotation names.
    pub fn signers(&self) -> Vec<Vec<u8>> {
        match &self.signer {
            Some(signer) => std::iter::once(signer.sha256())
                .chain(signer.past_signers.iter().cloned())
                .collect(),
            None => Vec::new(),
        }
    }
}

/// An attribute's text: a literal string, or a string resource in the default language.
fn text(
    element: &Element,
    id: u32,
    name: &str,
    pool: &axml::StringPool,
    table: &ResourceTable,
) -> Option<String> {
    let attribute = element.attribute(id, name)?;
    if attribute.value.kind == TYPE_REFERENCE {
        return table.text(attribute.value.data);
    }
    element.string(id, name, pool)
}

fn is_image(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.ends_with(".png") || path.ends_with(".webp") || path.ends_with(".jpg")
}

/// How well a density suits an icon shown at up to 256 pixels: the highest up to xxxhdpi
/// (640 dpi) is best.
fn density_rank(density: u16) -> i32 {
    match density {
        0 | 0xFFFE => 160,
        0xFFFF => 0,
        d if d <= 640 => i32::from(d),
        d => 1280 - i32::from(d),
    }
}

/// The file paths of the resource `id` in each density, references followed.
fn files(table: &ResourceTable, id: u32, depth: usize) -> Vec<(u16, String)> {
    if depth > 8 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (config, value) in table.values(id) {
        match value.kind {
            TYPE_STRING => {
                if let Some(path) = table.string(value.data) {
                    out.push((config.density, path.to_string()));
                }
            }
            TYPE_REFERENCE => out.extend(files(table, value.data, depth + 1)),
            _ => {}
        }
    }
    out
}

fn best(mut candidates: Vec<(u16, String)>) -> Option<String> {
    candidates.sort_by_key(|(density, _)| -density_rank(*density));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn read_file<R: Read + Seek>(
    archive: &mut Archive<R>,
    path: &str,
    limit: usize,
) -> Option<Vec<u8>> {
    let entry = archive.entry(path)?.clone();
    archive.read(&entry, limit).ok()
}

/// The XML files of the resource `id`, best density first, references followed.
fn xml_files(table: &ResourceTable, id: u32) -> Vec<String> {
    let mut candidates: Vec<_> = files(table, id, 0)
        .into_iter()
        .filter(|(_, path)| path.to_ascii_lowercase().ends_with(".xml"))
        .collect();
    candidates.sort_by_key(|(density, _)| -density_rank(*density));
    candidates.into_iter().map(|(_, path)| path).collect()
}

fn read_xml<R: Read + Seek>(archive: &mut Archive<R>, path: &str) -> Option<axml::Document> {
    axml::parse(&read_file(archive, path, MAX_MANIFEST)?).ok()
}

/// The icon the resource `id` stands for: its best image, else its adaptive icon's layers,
/// else the vector drawable it is.
fn icon<R: Read + Seek>(archive: &mut Archive<R>, table: &ResourceTable, id: u32) -> Option<Icon> {
    let images: Vec<_> = files(table, id, 0)
        .into_iter()
        .filter(|(_, path)| is_image(path))
        .collect();
    if let Some(path) = best(images) {
        if let Some(image) = read_file(archive, &path, MAX_ICON) {
            return Some(Icon::Image(image));
        }
    }
    for xml in xml_files(table, id) {
        let Some(document) = read_xml(archive, &xml) else {
            continue;
        };
        match document.elements.first().map(|e| e.name.as_str()) {
            Some("adaptive-icon") => {
                let mut layer = |name: &str| -> Layer {
                    let Some(element) = document.elements.iter().find(|e| e.name == name) else {
                        return Layer::Unknown;
                    };
                    match element
                        .attribute(attr::DRAWABLE, "drawable")
                        .map(|a| a.value)
                    {
                        Some(Value { kind, data })
                            if (TYPE_FIRST_COLOR..=TYPE_LAST_COLOR).contains(&kind) =>
                        {
                            Layer::Color(color(kind, data))
                        }
                        Some(Value {
                            kind: TYPE_REFERENCE,
                            data,
                        }) => layer_of(archive, table, data, 0),
                        _ => Layer::Unknown,
                    }
                };
                let background = layer("background");
                let foreground = layer("foreground");
                return Some(Icon::Adaptive {
                    background,
                    foreground,
                });
            }
            Some(_) => {
                if let Layer::Vector(vector) = drawable(archive, table, &document, 0) {
                    return Some(Icon::Vector(vector));
                }
            }
            None => {}
        }
    }
    None
}

/// An adaptive icon's layer from the resource `id`: a colour, its best image, or the
/// drawable XML it is made of (a vector drawable, an inset, a bitmap or a shape).
fn layer_of<R: Read + Seek>(
    archive: &mut Archive<R>,
    table: &ResourceTable,
    id: u32,
    depth: usize,
) -> Layer {
    let mut id = id;
    for _ in 0..8 {
        let values = table.values(id);
        let Some((_, value)) = values
            .iter()
            .find(|(config, _)| config.is_default_language())
            .or(values.first())
        else {
            return Layer::Unknown;
        };
        match value.kind {
            kind if (TYPE_FIRST_COLOR..=TYPE_LAST_COLOR).contains(&kind) => {
                return Layer::Color(color(kind, value.data))
            }
            TYPE_REFERENCE => id = value.data,
            TYPE_STRING => {
                let images: Vec<_> = files(table, id, 0)
                    .into_iter()
                    .filter(|(_, path)| is_image(path))
                    .collect();
                if let Some(image) =
                    best(images).and_then(|path| read_file(archive, &path, MAX_ICON))
                {
                    return Layer::Image(image);
                }
                for xml in xml_files(table, id) {
                    if let Some(document) = read_xml(archive, &xml) {
                        let layer = drawable(archive, table, &document, depth);
                        if layer != Layer::Unknown {
                            return layer;
                        }
                    }
                }
                return Layer::Unknown;
            }
            _ => return Layer::Unknown,
        }
    }
    Layer::Unknown
}

/// A drawable XML file as a layer: a `<vector>`, an `<inset>` around a drawable, a
/// `<bitmap>` of an image, or a `<shape>` filled with one colour.
fn drawable<R: Read + Seek>(
    archive: &mut Archive<R>,
    table: &ResourceTable,
    document: &axml::Document,
    depth: usize,
) -> Layer {
    if depth > 4 {
        return Layer::Unknown;
    }
    let Some(root) = document.elements.first() else {
        return Layer::Unknown;
    };
    let reference = |element: &Element, name: &str| {
        let id = if name == "drawable" {
            attr::DRAWABLE
        } else {
            0
        };
        vector::find(element, id, name).map(|a| a.value)
    };
    match root.name.as_str() {
        "vector" => {
            let mut open = |path: &str| read_xml(archive, path);
            vector::parse(table, &mut open, document).map_or(Layer::Unknown, Layer::Vector)
        }
        "inset" => {
            let inset = ["inset", "insetLeft"]
                .iter()
                .find_map(|name| vector::find(root, 0, name))
                .and_then(|a| vector::number(table, a))
                .unwrap_or(0.0);
            // A dimension inset is in dp of the 108 dp layer; a fraction is of its side.
            let inset = if inset >= 1.0 { inset / 108.0 } else { inset };
            let inner = match reference(root, "drawable") {
                Some(Value {
                    kind: TYPE_REFERENCE,
                    data,
                }) => layer_of(archive, table, data, depth + 1),
                Some(Value { kind, data })
                    if (TYPE_FIRST_COLOR..=TYPE_LAST_COLOR).contains(&kind) =>
                {
                    Layer::Color(color(kind, data))
                }
                _ => Layer::Unknown,
            };
            match inner {
                Layer::Vector(mut vector) => {
                    vector.inset = (vector.inset + inset).clamp(0.0, 0.49);
                    Layer::Vector(vector)
                }
                other => other,
            }
        }
        "bitmap" => match reference(root, "src") {
            Some(Value {
                kind: TYPE_REFERENCE,
                data,
            }) => layer_of(archive, table, data, depth + 1),
            _ => Layer::Unknown,
        },
        "shape" => document
            .elements
            .iter()
            .find(|e| e.name == "solid")
            .and_then(|solid| reference(solid, "color"))
            .and_then(|value| match value {
                Value { kind, data } if (TYPE_FIRST_COLOR..=TYPE_LAST_COLOR).contains(&kind) => {
                    Some(Layer::Color(color(kind, data)))
                }
                Value {
                    kind: TYPE_REFERENCE,
                    data,
                } => match layer_of(archive, table, data, depth + 1) {
                    Layer::Color(argb) => Some(Layer::Color(argb)),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or(Layer::Unknown),
        _ => Layer::Unknown,
    }
}

/// A colour value as 0xAARRGGBB, whichever of the four colour forms it is in.
fn color(kind: u8, data: u32) -> u32 {
    let expand = |nibble: u32| (nibble & 0xf) * 0x11;
    match kind {
        // #rgb and #argb keep four bits a channel.
        0x1f => 0xff00_0000 | (expand(data >> 8) << 16) | (expand(data >> 4) << 8) | expand(data),
        0x1e => {
            (expand(data >> 12) << 24)
                | (expand(data >> 8) << 16)
                | (expand(data >> 4) << 8)
                | expand(data)
        }
        0x1d => 0xff00_0000 | (data & 0x00ff_ffff),
        _ => data,
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{adaptive_vector_apk, archive, notes_apk, PNG};
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fr11_the_install_window_reads_an_apks_name_icon_version_and_permissions() {
        let data = notes_apk();
        let size = data.len() as u64;
        let info = ApkInfo::read_from(Cursor::new(data), size).unwrap();
        assert_eq!(info.package, "com.example.notes");
        assert_eq!(info.label.as_deref(), Some("Notes"));
        assert_eq!(info.version_code, 42);
        assert_eq!(info.version_name.as_deref(), Some("4.2"));
        assert_eq!((info.min_sdk, info.target_sdk), (26, 34));
        assert_eq!(
            info.permissions
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["android.permission.CAMERA", "android.permission.INTERNET"]
        );
        assert_eq!(info.icon, Some(Icon::Image(PNG.to_vec())));
        assert_eq!(info.abis, ["arm64-v8a", "x86_64"]);
        assert_eq!(info.size, size);
        let signer = info.signer.as_ref().expect("signed");
        assert_eq!(signer.certificate, b"\x30\x03\x02\x01\x07");
        assert_eq!(info.signers(), vec![signer.sha256()]);
    }

    #[test]
    fn fr11_an_adaptive_icon_with_a_vector_foreground_reads_both_layers() {
        // The foreground was read as a layer the host does not draw, so the tile showed the
        // background alone: a plain rounded square.
        let data = adaptive_vector_apk();
        let size = data.len() as u64;
        let info = ApkInfo::read_from(Cursor::new(data), size).unwrap();
        let Some(Icon::Adaptive {
            background,
            foreground,
        }) = info.icon.clone()
        else {
            panic!("not an adaptive icon: {:?}", info.icon);
        };
        assert_eq!(background, Layer::Color(0xff42_85f4));
        let Layer::Vector(vector) = foreground else {
            panic!("the foreground is not a vector drawable: {foreground:?}");
        };
        assert_eq!(
            (vector.width, vector.height, vector.inset),
            (108.0, 108.0, 0.0)
        );
        assert_eq!(
            vector.paths,
            vec![VectorPath {
                data: "M0,0L10,0L10,10z".into(),
                fill: Some(0xffdb_4437),
                stroke: None,
                stroke_width: 0.0,
                even_odd: false,
                transform: [1.0, 0.0, 0.0, 1.0, 10.0, 0.0],
            }]
        );
    }

    #[test]
    fn fr11_a_file_that_is_not_an_apk_is_refused_with_its_reason() {
        let not_apk = archive(&[("readme.txt", &b"hello"[..])], b"");
        let why = ApkInfo::read_from(Cursor::new(not_apk), 5).unwrap_err();
        assert!(why.contains("AndroidManifest.xml"), "{why}");
        let why = ApkInfo::read_from(Cursor::new(b"hello".to_vec()), 5).unwrap_err();
        assert!(why.contains("not a ZIP archive"), "{why}");
    }

    #[test]
    fn fr11_a_damaged_apk_is_refused_without_panicking() {
        let data = notes_apk();
        for cut in (0..data.len()).step_by(7) {
            let mut damaged = data.clone();
            damaged[cut] ^= 0x5a;
            let _ = ApkInfo::read_from(Cursor::new(damaged), data.len() as u64);
        }
    }

    #[test]
    fn fr11_colour_values_read_in_all_four_forms() {
        assert_eq!(color(0x1c, 0x80ff_0000), 0x80ff_0000);
        assert_eq!(color(0x1d, 0x00ff_0000), 0xffff_0000);
        assert_eq!(color(0x1e, 0x8f00), 0x88ff_0000);
        assert_eq!(color(0x1f, 0xf00), 0xffff_0000);
    }
}
