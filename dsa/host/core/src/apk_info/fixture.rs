//! Writers of APKs and of their parts, shaped as aapt2 and apksigner make them, for the
//! tests of the reader here and of the programs that use it.

use super::arsc::{TABLE, TABLE_PACKAGE, TABLE_TYPE};
use super::attr;
use super::axml::{
    Value, STRING_POOL, TYPE_INT_DEC, TYPE_REFERENCE, TYPE_STRING, XML, XML_END_ELEMENT,
    XML_RESOURCE_MAP, XML_START_ELEMENT,
};
use super::signing::BLOCK_MAGIC;

/// A string pool of UTF-8 strings, each shorter than 128 bytes.
pub fn string_pool(strings: &[&str]) -> Vec<u8> {
    let mut offsets = Vec::new();
    let mut body = Vec::new();
    for s in strings {
        offsets.extend_from_slice(&(body.len() as u32).to_le_bytes());
        let chars = s.chars().count();
        let bytes = s.len();
        assert!(chars < 128 && bytes < 128);
        body.push(chars as u8);
        body.push(bytes as u8);
        body.extend_from_slice(s.as_bytes());
        body.push(0);
    }
    while body.len() % 4 != 0 {
        body.push(0);
    }
    let header = 28usize;
    let strings_start = header + offsets.len();
    let mut out = Vec::new();
    out.extend_from_slice(&STRING_POOL.to_le_bytes());
    out.extend_from_slice(&(header as u16).to_le_bytes());
    out.extend_from_slice(&((strings_start + body.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(1u32 << 8).to_le_bytes());
    out.extend_from_slice(&(strings_start as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&offsets);
    out.extend_from_slice(&body);
    out
}

/// An attribute to write: its name, its framework resource ID (0 for none), and its
/// value: a string, or a typed value.
pub enum Attr<'a> {
    Str(&'a str, u32, &'a str),
    Typed(&'a str, u32, u8, u32),
}

/// An element: its name, its attributes, and whether it closes before the next one.
pub struct El<'a>(pub &'a str, pub Vec<Attr<'a>>, pub bool);

/// Writes a document of `elements`; an element that does not close contains the ones
/// after it, until the end.
pub fn document(elements: &[El<'_>]) -> Vec<u8> {
    // Attribute names with resource IDs come first, in the order of the resource map.
    let mut strings: Vec<String> = Vec::new();
    let mut ids: Vec<u32> = Vec::new();
    for El(_, attributes, _) in elements {
        for attribute in attributes {
            let (name, id) = match attribute {
                Attr::Str(name, id, _) | Attr::Typed(name, id, _, _) => (*name, *id),
            };
            if id != 0 && !strings.iter().any(|s| s == name) {
                strings.push(name.to_string());
                ids.push(id);
            }
        }
    }
    let index = |s: &str, strings: &mut Vec<String>| -> u32 {
        if let Some(i) = strings.iter().position(|x| x == s) {
            return i as u32;
        }
        strings.push(s.to_string());
        (strings.len() - 1) as u32
    };
    let mut body = Vec::new();
    let mut open = 0;
    for El(name, attributes, closes) in elements {
        let name_index = index(name, &mut strings);
        let mut attrs = Vec::new();
        for attribute in attributes {
            let (attr_name, raw, kind, data) = match attribute {
                Attr::Str(n, _, v) => {
                    let v = index(v, &mut strings);
                    (*n, v, TYPE_STRING, v)
                }
                Attr::Typed(n, _, kind, data) => (*n, u32::MAX, *kind, *data),
            };
            let attr_name = index(attr_name, &mut strings);
            attrs.extend_from_slice(&u32::MAX.to_le_bytes());
            attrs.extend_from_slice(&attr_name.to_le_bytes());
            attrs.extend_from_slice(&raw.to_le_bytes());
            attrs.extend_from_slice(&8u16.to_le_bytes());
            attrs.push(0);
            attrs.push(kind);
            attrs.extend_from_slice(&data.to_le_bytes());
        }
        let size = 16 + 20 + attrs.len();
        body.extend_from_slice(&XML_START_ELEMENT.to_le_bytes());
        body.extend_from_slice(&16u16.to_le_bytes());
        body.extend_from_slice(&(size as u32).to_le_bytes());
        body.extend_from_slice(&[0; 8]);
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(&name_index.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&(attributes.len() as u16).to_le_bytes());
        body.extend_from_slice(&[0; 6]);
        body.extend_from_slice(&attrs);
        if *closes {
            end(&mut body, name_index);
        } else {
            open += 1;
        }
    }
    for _ in 0..open {
        end(&mut body, 0);
    }
    let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
    let pool = string_pool(&refs);
    let mut map = Vec::new();
    map.extend_from_slice(&XML_RESOURCE_MAP.to_le_bytes());
    map.extend_from_slice(&8u16.to_le_bytes());
    map.extend_from_slice(&((8 + ids.len() * 4) as u32).to_le_bytes());
    for id in &ids {
        map.extend_from_slice(&id.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(&XML.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    out.extend_from_slice(&((8 + pool.len() + map.len() + body.len()) as u32).to_le_bytes());
    out.extend_from_slice(&pool);
    out.extend_from_slice(&map);
    out.extend_from_slice(&body);
    out
}

fn end(body: &mut Vec<u8>, name: u32) {
    body.extend_from_slice(&XML_END_ELEMENT.to_le_bytes());
    body.extend_from_slice(&16u16.to_le_bytes());
    body.extend_from_slice(&24u32.to_le_bytes());
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&u32::MAX.to_le_bytes());
    body.extend_from_slice(&name.to_le_bytes());
}

/// One value to write: its type ID, its entry index, its language, its density and its
/// value.
pub struct Res<'a> {
    pub type_id: u8,
    pub entry: u16,
    pub language: &'a str,
    pub density: u16,
    pub value: Value,
}

/// Writes a table of package 0x7f holding `values`, with `strings` as its string pool.
pub fn table(strings: &[&str], values: &[Res<'_>]) -> Vec<u8> {
    let mut types = Vec::new();
    for res in values {
        // One type chunk for each value keeps the writer simple; aapt2 groups them.
        let config_size = 64usize;
        let header = 20 + config_size;
        let count = usize::from(res.entry) + 1;
        let entries_start = header + count * 4;
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&TABLE_TYPE.to_le_bytes());
        chunk.extend_from_slice(&(header as u16).to_le_bytes());
        chunk.extend_from_slice(&((entries_start + 16) as u32).to_le_bytes());
        chunk.push(res.type_id);
        chunk.push(0);
        chunk.extend_from_slice(&0u16.to_le_bytes());
        chunk.extend_from_slice(&(count as u32).to_le_bytes());
        chunk.extend_from_slice(&(entries_start as u32).to_le_bytes());
        let mut config = vec![0u8; config_size];
        config[0..4].copy_from_slice(&(config_size as u32).to_le_bytes());
        if res.language.len() == 2 {
            config[8..10].copy_from_slice(res.language.as_bytes());
        }
        config[14..16].copy_from_slice(&res.density.to_le_bytes());
        chunk.extend_from_slice(&config);
        for i in 0..count {
            let offset = if i == usize::from(res.entry) {
                0
            } else {
                u32::MAX
            };
            chunk.extend_from_slice(&offset.to_le_bytes());
        }
        chunk.extend_from_slice(&8u16.to_le_bytes());
        chunk.extend_from_slice(&0u16.to_le_bytes());
        chunk.extend_from_slice(&0u32.to_le_bytes());
        chunk.extend_from_slice(&8u16.to_le_bytes());
        chunk.push(0);
        chunk.push(res.value.kind);
        chunk.extend_from_slice(&res.value.data.to_le_bytes());
        types.extend_from_slice(&chunk);
    }
    let package_header = 288usize;
    let mut package = Vec::new();
    package.extend_from_slice(&TABLE_PACKAGE.to_le_bytes());
    package.extend_from_slice(&(package_header as u16).to_le_bytes());
    package.extend_from_slice(&((package_header + types.len()) as u32).to_le_bytes());
    package.extend_from_slice(&0x7fu32.to_le_bytes());
    package.resize(package_header, 0);
    package.extend_from_slice(&types);
    let pool = string_pool(strings);
    let mut out = Vec::new();
    out.extend_from_slice(&TABLE.to_le_bytes());
    out.extend_from_slice(&12u16.to_le_bytes());
    out.extend_from_slice(&((12 + pool.len() + package.len()) as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&pool);
    out.extend_from_slice(&package);
    out
}

/// Builds a ZIP archive of stored entries, with `before_directory` between the last
/// entry and the central directory (where an APK keeps its signing block).
pub fn archive(entries: &[(&str, &[u8])], before_directory: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&0u32.to_le_bytes()); // crc, unchecked
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        directory.extend_from_slice(b"PK\x01\x02");
        directory.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&0u32.to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    out.extend_from_slice(before_directory);
    let directory_offset = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Each of `parts` after its length, as the signing block nests its parts.
pub fn prefixed(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(&(part.len() as u32).to_le_bytes());
        out.extend_from_slice(part);
    }
    out
}

/// A v2 scheme value with one signer holding `certificate`.
pub fn v2_value(certificate: &[u8]) -> Vec<u8> {
    let signed_data = [
        prefixed(&[&[]]),
        prefixed(&[&prefixed(&[certificate])]),
        prefixed(&[&[]]),
    ]
    .concat();
    let signer = prefixed(&[&signed_data, &prefixed(&[]), b"key"]);
    prefixed(&[&prefixed(&[&signer])])
}

/// A signing block holding `pairs`, as it sits before the central directory.
pub fn signing_block(pairs: &[(u32, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, value) in pairs {
        body.extend_from_slice(&((value.len() + 4) as u64).to_le_bytes());
        body.extend_from_slice(&id.to_le_bytes());
        body.extend_from_slice(value);
    }
    let size = (body.len() + 24) as u64;
    let mut out = Vec::new();
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(BLOCK_MAGIC);
    out
}

/// The xxxhdpi icon of [`notes_apk`].
pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\nxxxhdpi";

/// An APK as aapt2 and apksigner would make it: a manifest, a resource table with the
/// label in two languages and the icon in two densities, and a v2 signing block.
pub fn notes_apk() -> Vec<u8> {
    let manifest = document(&[
        El(
            "manifest",
            vec![
                Attr::Typed("versionCode", attr::VERSION_CODE, TYPE_INT_DEC, 42),
                Attr::Str("versionName", attr::VERSION_NAME, "4.2"),
                Attr::Str("package", 0, "com.example.notes"),
            ],
            false,
        ),
        El(
            "uses-sdk",
            vec![
                Attr::Typed("minSdkVersion", attr::MIN_SDK_VERSION, TYPE_INT_DEC, 26),
                Attr::Typed(
                    "targetSdkVersion",
                    attr::TARGET_SDK_VERSION,
                    TYPE_INT_DEC,
                    34,
                ),
            ],
            true,
        ),
        El(
            "uses-permission",
            vec![Attr::Str("name", attr::NAME, "android.permission.CAMERA")],
            true,
        ),
        El(
            "uses-permission",
            vec![Attr::Str("name", attr::NAME, "android.permission.INTERNET")],
            true,
        ),
        El(
            "application",
            vec![
                Attr::Typed("label", attr::LABEL, TYPE_REFERENCE, 0x7f02_0000),
                Attr::Typed("icon", attr::ICON, TYPE_REFERENCE, 0x7f01_0000),
            ],
            false,
        ),
        El(
            "uses-permission",
            vec![Attr::Str("name", attr::NAME, "not.a.child.of.manifest")],
            true,
        ),
    ]);
    let resources = table(
        &[
            "Notizen",
            "Notes",
            "res/mipmap-mdpi/ic_launcher.png",
            "res/mipmap-xxxhdpi/ic_launcher.png",
        ],
        &[
            Res {
                type_id: 2,
                entry: 0,
                language: "de",
                density: 0,
                value: Value {
                    kind: TYPE_STRING,
                    data: 0,
                },
            },
            Res {
                type_id: 2,
                entry: 0,
                language: "",
                density: 0,
                value: Value {
                    kind: TYPE_STRING,
                    data: 1,
                },
            },
            Res {
                type_id: 1,
                entry: 0,
                language: "",
                density: 160,
                value: Value {
                    kind: TYPE_STRING,
                    data: 2,
                },
            },
            Res {
                type_id: 1,
                entry: 0,
                language: "",
                density: 640,
                value: Value {
                    kind: TYPE_STRING,
                    data: 3,
                },
            },
        ],
    );
    let block = signing_block(&[(0x7109_871a, &v2_value(b"\x30\x03\x02\x01\x07")[..])]);
    archive(
        &[
            ("AndroidManifest.xml", &manifest[..]),
            ("resources.arsc", &resources[..]),
            (
                "res/mipmap-mdpi/ic_launcher.png",
                &b"\x89PNG\r\n\x1a\nmdpi"[..],
            ),
            ("res/mipmap-xxxhdpi/ic_launcher.png", PNG),
            ("lib/x86_64/libnotes.so", &b"elf"[..]),
            ("lib/arm64-v8a/libnotes.so", &b"elf"[..]),
        ],
        &block,
    )
}

/// An APK whose launcher icon is an adaptive icon, as most apps made since Android 8 have:
/// the background a colour resource, the foreground a vector drawable with one path in a
/// group moved 10 units right.
pub fn adaptive_vector_apk() -> Vec<u8> {
    let manifest = document(&[
        El(
            "manifest",
            vec![
                Attr::Typed("versionCode", attr::VERSION_CODE, TYPE_INT_DEC, 1),
                Attr::Str("package", 0, "com.example.vector"),
            ],
            false,
        ),
        El(
            "application",
            vec![Attr::Typed("icon", attr::ICON, TYPE_REFERENCE, 0x7f01_0000)],
            true,
        ),
    ]);
    let adaptive = document(&[
        El("adaptive-icon", vec![], false),
        El(
            "background",
            vec![Attr::Typed(
                "drawable",
                attr::DRAWABLE,
                TYPE_REFERENCE,
                0x7f04_0000,
            )],
            true,
        ),
        El(
            "foreground",
            vec![Attr::Typed(
                "drawable",
                attr::DRAWABLE,
                TYPE_REFERENCE,
                0x7f03_0000,
            )],
            true,
        ),
    ]);
    let float = |value: f32| value.to_bits();
    let foreground = document(&[
        El(
            "vector",
            vec![
                Attr::Typed("viewportWidth", 0x0101_0402, 0x04, float(108.0)),
                Attr::Typed("viewportHeight", 0x0101_0403, 0x04, float(108.0)),
            ],
            false,
        ),
        El("group", vec![Attr::Str("translateX", 0, "10")], false),
        El(
            "path",
            vec![
                Attr::Str("pathData", 0x0101_0405, "M0,0L10,0L10,10z"),
                Attr::Typed("fillColor", 0x0101_0404, 0x1c, 0xffdb_4437),
            ],
            true,
        ),
    ]);
    let resources = table(
        &[
            "res/mipmap-anydpi-v26/ic_launcher.xml",
            "res/drawable/ic_launcher_foreground.xml",
        ],
        &[
            Res {
                type_id: 1,
                entry: 0,
                language: "",
                density: 0xffff,
                value: Value {
                    kind: TYPE_STRING,
                    data: 0,
                },
            },
            Res {
                type_id: 3,
                entry: 0,
                language: "",
                density: 0,
                value: Value {
                    kind: TYPE_STRING,
                    data: 1,
                },
            },
            Res {
                type_id: 4,
                entry: 0,
                language: "",
                density: 0,
                value: Value {
                    kind: 0x1c,
                    data: 0xff42_85f4,
                },
            },
        ],
    );
    archive(
        &[
            ("AndroidManifest.xml", &manifest[..]),
            ("resources.arsc", &resources[..]),
            ("res/mipmap-anydpi-v26/ic_launcher.xml", &adaptive[..]),
            ("res/drawable/ic_launcher_foreground.xml", &foreground[..]),
        ],
        b"",
    )
}

/// A split APK of [`notes_apk`]'s app (com.example.notes, version 42), signed with the same
/// certificate: the split `split`, holding the native libraries `libraries` (such as
/// `lib/x86_64/libnotes.so`).
pub fn notes_split(split: &str, libraries: &[&str]) -> Vec<u8> {
    let manifest = document(&[El(
        "manifest",
        vec![
            Attr::Typed("versionCode", attr::VERSION_CODE, TYPE_INT_DEC, 42),
            Attr::Str("package", 0, "com.example.notes"),
            Attr::Str("split", 0, split),
        ],
        false,
    )]);
    let mut entries: Vec<(&str, &[u8])> = vec![("AndroidManifest.xml", &manifest[..])];
    for library in libraries {
        entries.push((*library, &b"\x7fELF"[..]));
    }
    let block = signing_block(&[(0x7109_871a, &v2_value(b"\x30\x03\x02\x01\x07")[..])]);
    archive(&entries, &block)
}
