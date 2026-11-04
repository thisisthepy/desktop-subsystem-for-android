//! Android's binary XML, in which an APK stores its manifest and its compiled XML resources,
//! and the string pool that it and the resource table share.
//!
//! The format is the one `aapt2` writes and the framework's `ResXMLTree` reads: a tree of
//! chunks, each with a type, a header size and a total size, all little-endian.

/// The chunk types read here.
pub const STRING_POOL: u16 = 0x0001;
pub const XML: u16 = 0x0003;
pub(crate) const XML_START_ELEMENT: u16 = 0x0102;
pub(crate) const XML_END_ELEMENT: u16 = 0x0103;
pub(crate) const XML_RESOURCE_MAP: u16 = 0x0180;

/// The value types of a typed value (Res_value).
pub const TYPE_REFERENCE: u8 = 0x01;
pub const TYPE_STRING: u8 = 0x03;
pub const TYPE_INT_DEC: u8 = 0x10;
pub const TYPE_INT_HEX: u8 = 0x11;
pub const TYPE_INT_BOOLEAN: u8 = 0x12;
pub const TYPE_FIRST_COLOR: u8 = 0x1c;
pub const TYPE_LAST_COLOR: u8 = 0x1f;

pub fn u16_at(data: &[u8], at: usize) -> Result<u16, String> {
    data.get(at..at.wrapping_add(2))
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "the binary XML is cut short".to_string())
}

pub fn u32_at(data: &[u8], at: usize) -> Result<u32, String> {
    data.get(at..at.wrapping_add(4))
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "the binary XML is cut short".to_string())
}

/// A chunk's type, header size and total size, checked to fit in `data` from `at`.
pub fn chunk(data: &[u8], at: usize) -> Result<(u16, usize, usize), String> {
    let kind = u16_at(data, at)?;
    let header = usize::from(u16_at(data, at + 2)?);
    let size = u32_at(data, at + 4)? as usize;
    if header < 8 || size < header || at.checked_add(size).is_none_or(|end| end > data.len()) {
        return Err(format!("a chunk of type 0x{kind:04x} is damaged"));
    }
    Ok((kind, header, size))
}

/// A string pool: the strings a binary XML or a resource table refers to by index.
#[derive(Debug, Default)]
pub struct StringPool {
    strings: Vec<String>,
}

impl StringPool {
    /// Reads the string pool chunk at `data[at..]`.
    pub fn read(data: &[u8], at: usize) -> Result<StringPool, String> {
        let (kind, header, size) = chunk(data, at)?;
        if kind != STRING_POOL {
            return Err("expected a string pool".into());
        }
        let pool = &data[at..at + size];
        let count = u32_at(pool, 8)? as usize;
        let flags = u32_at(pool, 16)?;
        let strings_start = u32_at(pool, 20)? as usize;
        let utf8 = flags & (1 << 8) != 0;
        if count > pool.len() / 4 {
            return Err("a string pool is damaged".into());
        }
        let mut strings = Vec::with_capacity(count);
        for index in 0..count {
            let offset = u32_at(pool, header + index * 4)? as usize;
            let start = strings_start
                .checked_add(offset)
                .ok_or("a string pool is damaged")?;
            strings.push(if utf8 {
                utf8_string(pool, start)
            } else {
                utf16_string(pool, start)
            }?);
        }
        Ok(StringPool { strings })
    }

    pub fn get(&self, index: u32) -> Option<&str> {
        self.strings.get(index as usize).map(String::as_str)
    }
}

fn utf8_length(data: &[u8], at: &mut usize) -> Result<usize, String> {
    let first = *data.get(*at).ok_or("a string pool is cut short")?;
    *at += 1;
    if first & 0x80 == 0 {
        return Ok(usize::from(first));
    }
    let second = *data.get(*at).ok_or("a string pool is cut short")?;
    *at += 1;
    Ok((usize::from(first & 0x7f) << 8) | usize::from(second))
}

fn utf8_string(pool: &[u8], start: usize) -> Result<String, String> {
    let mut at = start;
    let _characters = utf8_length(pool, &mut at)?;
    let bytes = utf8_length(pool, &mut at)?;
    let data = pool
        .get(at..at + bytes)
        .ok_or("a string pool is cut short")?;
    Ok(String::from_utf8_lossy(data).into_owned())
}

fn utf16_string(pool: &[u8], start: usize) -> Result<String, String> {
    let mut at = start;
    let first = u16_at(pool, at)?;
    at += 2;
    let length = if first & 0x8000 != 0 {
        let second = u16_at(pool, at)?;
        at += 2;
        (usize::from(first & 0x7fff) << 16) | usize::from(second)
    } else {
        usize::from(first)
    };
    let units = (0..length)
        .map(|i| u16_at(pool, at + i * 2))
        .collect::<Result<Vec<u16>, String>>()?;
    Ok(String::from_utf16_lossy(&units))
}

/// An attribute's typed value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value {
    pub kind: u8,
    pub data: u32,
}

/// One attribute of an element.
#[derive(Debug, Clone)]
pub struct Attribute {
    /// The attribute's name, as the string pool holds it; obfuscated APKs may strip it.
    pub name: String,
    /// The framework's resource ID of the attribute (such as `android:versionCode`), or 0.
    pub resource_id: u32,
    /// The raw string, when the value is one.
    pub raw: Option<String>,
    pub value: Value,
}

/// A start tag: its name, its depth (0 for the root) and its attributes.
#[derive(Debug, Clone)]
pub struct Element {
    pub name: String,
    pub depth: usize,
    pub attributes: Vec<Attribute>,
}

impl Element {
    /// The attribute with this framework resource ID, else this name.
    pub fn attribute(&self, resource_id: u32, name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|a| a.resource_id == resource_id && resource_id != 0)
            .or_else(|| {
                self.attributes
                    .iter()
                    .find(|a| a.name == name && (a.resource_id == 0 || resource_id == 0))
            })
    }

    /// The attribute as a string: its raw string, else its string value.
    pub fn string(&self, resource_id: u32, name: &str, pool: &StringPool) -> Option<String> {
        let attribute = self.attribute(resource_id, name)?;
        if let Some(raw) = &attribute.raw {
            return Some(raw.clone());
        }
        (attribute.value.kind == TYPE_STRING)
            .then(|| pool.get(attribute.value.data).map(str::to_string))
            .flatten()
    }

    /// The attribute as an integer, when it is one.
    pub fn integer(&self, resource_id: u32, name: &str) -> Option<u32> {
        let attribute = self.attribute(resource_id, name)?;
        matches!(
            attribute.value.kind,
            TYPE_INT_DEC | TYPE_INT_HEX | TYPE_INT_BOOLEAN
        )
        .then_some(attribute.value.data)
        .or_else(|| attribute.raw.as_deref()?.trim().parse().ok())
    }
}

/// A binary XML document: its string pool and its start tags, in document order.
pub struct Document {
    pub pool: StringPool,
    pub elements: Vec<Element>,
}

/// The most start tags read from one document.
const MAX_ELEMENTS: usize = 100_000;

/// Reads a binary XML document.
pub fn parse(data: &[u8]) -> Result<Document, String> {
    let (kind, header, size) = chunk(data, 0)?;
    if kind != XML {
        return Err("the file is not Android binary XML".into());
    }
    let mut pool = StringPool::default();
    let mut resource_ids: Vec<u32> = Vec::new();
    let mut elements = Vec::new();
    let mut depth = 0usize;
    let mut at = header;
    while at + 8 <= size {
        let (kind, chunk_header, chunk_size) = chunk(data, at)?;
        match kind {
            STRING_POOL => pool = StringPool::read(data, at)?,
            XML_RESOURCE_MAP => {
                resource_ids = (0..(chunk_size - chunk_header) / 4)
                    .map(|i| u32_at(data, at + chunk_header + i * 4))
                    .collect::<Result<_, _>>()?;
            }
            XML_START_ELEMENT => {
                if elements.len() >= MAX_ELEMENTS {
                    return Err("the binary XML has too many elements".into());
                }
                let ext = at + chunk_header;
                let name = u32_at(data, ext + 4)?;
                let attribute_start = usize::from(u16_at(data, ext + 8)?);
                let attribute_size = usize::from(u16_at(data, ext + 10)?);
                let attribute_count = usize::from(u16_at(data, ext + 12)?);
                let mut attributes = Vec::with_capacity(attribute_count);
                for i in 0..attribute_count {
                    let a = ext + attribute_start + i * attribute_size;
                    if a + 20 > at + chunk_size {
                        return Err("an element of the binary XML is damaged".into());
                    }
                    let name_index = u32_at(data, a + 4)?;
                    let raw = u32_at(data, a + 8)?;
                    attributes.push(Attribute {
                        name: pool.get(name_index).unwrap_or_default().to_string(),
                        resource_id: resource_ids.get(name_index as usize).copied().unwrap_or(0),
                        raw: (raw != u32::MAX)
                            .then(|| pool.get(raw).map(str::to_string))
                            .flatten(),
                        value: Value {
                            kind: data[a + 15],
                            data: u32_at(data, a + 16)?,
                        },
                    });
                }
                elements.push(Element {
                    name: pool.get(name).unwrap_or_default().to_string(),
                    depth,
                    attributes,
                });
                depth += 1;
            }
            XML_END_ELEMENT => depth = depth.saturating_sub(1),
            _ => {}
        }
        at += chunk_size;
    }
    Ok(Document { pool, elements })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk_info::fixture::{document, Attr, El};

    #[test]
    fn fr11_a_manifests_elements_and_attributes_are_read() {
        let data = document(&[
            El(
                "manifest",
                vec![
                    Attr::Str("package", 0, "com.example.notes"),
                    Attr::Typed("versionCode", 0x0101021b, TYPE_INT_DEC, 42),
                ],
                false,
            ),
            El(
                "uses-permission",
                vec![Attr::Str("name", 0x01010003, "android.permission.CAMERA")],
                true,
            ),
        ]);
        let document = parse(&data).unwrap();
        assert_eq!(document.elements.len(), 2);
        let manifest = &document.elements[0];
        assert_eq!(manifest.depth, 0);
        assert_eq!(
            manifest.string(0, "package", &document.pool).as_deref(),
            Some("com.example.notes")
        );
        assert_eq!(manifest.integer(0x0101021b, "versionCode"), Some(42));
        let permission = &document.elements[1];
        assert_eq!(permission.depth, 1);
        assert_eq!(
            permission
                .string(0x01010003, "name", &document.pool)
                .as_deref(),
            Some("android.permission.CAMERA")
        );
    }

    #[test]
    fn fr11_damaged_binary_xml_is_refused_without_panicking() {
        let data = document(&[El("manifest", vec![], true)]);
        for cut in 0..data.len() {
            let _ = parse(&data[..cut]);
        }
        assert!(parse(b"<manifest/>").is_err());
    }
}
