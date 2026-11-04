//! An APK's resource table (`resources.arsc`): what the resource IDs its manifest names
//! stand for, in each configuration (language, screen density and so on).
//!
//! Only what the install window needs is read: plain values (strings, colours, file paths,
//! references), with the language and the density of the configuration each is for. Styles
//! and other complex values are skipped.

use std::collections::HashMap;

use super::axml::{chunk, u16_at, u32_at, StringPool, Value, STRING_POOL, TYPE_STRING};

pub(crate) const TABLE: u16 = 0x0002;
pub(crate) const TABLE_PACKAGE: u16 = 0x0200;
pub(crate) const TABLE_TYPE: u16 = 0x0201;

const ENTRY_COMPLEX: u16 = 0x0001;
const ENTRY_COMPACT: u16 = 0x0008;
const TYPE_SPARSE: u8 = 0x01;
const TYPE_OFFSET16: u8 = 0x02;

/// The configuration a value is for: the parts the install window chooses by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Config {
    /// The language as two ASCII letters, or zeros for the default.
    pub language: [u8; 2],
    pub country: [u8; 2],
    /// The screen density in dots per inch; 0 for any, 0xFFFE for none, 0xFFFF for any
    /// density (a vector drawable).
    pub density: u16,
}

impl Config {
    pub fn is_default_language(&self) -> bool {
        self.language == [0, 0]
    }
}

/// A resource table: every plain value of every package, by resource ID.
#[derive(Default)]
pub struct ResourceTable {
    strings: StringPool,
    values: HashMap<u32, Vec<(Config, Value)>>,
}

impl ResourceTable {
    pub fn parse(data: &[u8]) -> Result<ResourceTable, String> {
        let (kind, header, size) = chunk(data, 0)?;
        if kind != TABLE {
            return Err("resources.arsc is not a resource table".into());
        }
        let mut table = ResourceTable::default();
        let mut at = header;
        while at + 8 <= size {
            let (kind, _, chunk_size) = chunk(data, at)?;
            match kind {
                STRING_POOL => table.strings = StringPool::read(data, at)?,
                TABLE_PACKAGE => table.package(&data[at..at + chunk_size])?,
                _ => {}
            }
            at += chunk_size;
        }
        Ok(table)
    }

    fn package(&mut self, data: &[u8]) -> Result<(), String> {
        let (_, header, size) = chunk(data, 0)?;
        let id = u32_at(data, 8)? & 0xff;
        let mut at = header;
        while at + 8 <= size {
            let (kind, _, chunk_size) = chunk(data, at)?;
            if kind == TABLE_TYPE {
                self.kind(id, &data[at..at + chunk_size])?;
            }
            at += chunk_size;
        }
        Ok(())
    }

    fn kind(&mut self, package: u32, data: &[u8]) -> Result<(), String> {
        let (_, header, size) = chunk(data, 0)?;
        let type_id = u32::from(*data.get(8).ok_or("a resource type is damaged")?);
        let flags = *data.get(9).ok_or("a resource type is damaged")?;
        let count = u32_at(data, 12)? as usize;
        let entries_start = u32_at(data, 16)? as usize;
        // ResTable_config: its size, mcc, mnc, language, country, orientation, touchscreen,
        // density, and more that is not used here.
        let config = Config {
            language: [*data.get(28).unwrap_or(&0), *data.get(29).unwrap_or(&0)],
            country: [*data.get(30).unwrap_or(&0), *data.get(31).unwrap_or(&0)],
            density: u16_at(data, 34).unwrap_or(0),
        };
        if count > size {
            return Err("a resource type is damaged".into());
        }
        let mut offsets = Vec::with_capacity(count);
        if flags & TYPE_SPARSE != 0 {
            for i in 0..count {
                let index = u32::from(u16_at(data, header + i * 4)?);
                let offset = usize::from(u16_at(data, header + i * 4 + 2)?) * 4;
                offsets.push((index, offset));
            }
        } else if flags & TYPE_OFFSET16 != 0 {
            for i in 0..count {
                let offset = u16_at(data, header + i * 2)?;
                if offset != 0xFFFF {
                    offsets.push((i as u32, usize::from(offset) * 4));
                }
            }
        } else {
            for i in 0..count {
                let offset = u32_at(data, header + i * 4)?;
                if offset != u32::MAX {
                    offsets.push((i as u32, offset as usize));
                }
            }
        }
        for (index, offset) in offsets {
            let at = entries_start + offset;
            let entry_size = u16_at(data, at)?;
            let entry_flags = u16_at(data, at + 2)?;
            let value = if entry_flags & ENTRY_COMPACT != 0 {
                // A compact entry keeps its key in the first field, the value's type in the
                // high byte of its flags and the value's data in place of the key.
                Value {
                    kind: (entry_flags >> 8) as u8,
                    data: u32_at(data, at + 4)?,
                }
            } else if entry_flags & ENTRY_COMPLEX != 0 {
                continue;
            } else {
                let value_at = at + usize::from(entry_size);
                Value {
                    kind: *data
                        .get(value_at + 3)
                        .ok_or("a resource entry is damaged")?,
                    data: u32_at(data, value_at + 4)?,
                }
            };
            let id = (package << 24) | (type_id << 16) | (index & 0xffff);
            self.values.entry(id).or_default().push((config, value));
        }
        Ok(())
    }

    /// Every value of the resource `id`, with the configuration each is for.
    pub fn values(&self, id: u32) -> &[(Config, Value)] {
        self.values.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// A string of the table's string pool.
    pub fn string(&self, index: u32) -> Option<&str> {
        self.strings.get(index)
    }

    /// The resource's text in the default language, else in English, else in any: what a
    /// device set to a language the app does not offer shows. References are followed.
    pub fn text(&self, id: u32) -> Option<String> {
        self.text_at(id, 0)
    }

    fn text_at(&self, id: u32, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        let values = self.values(id);
        let chosen = values
            .iter()
            .find(|(config, _)| config.is_default_language())
            .or_else(|| values.iter().find(|(config, _)| &config.language == b"en"))
            .or_else(|| values.first())?;
        match chosen.1.kind {
            TYPE_STRING => self.string(chosen.1.data).map(str::to_string),
            super::axml::TYPE_REFERENCE => self.text_at(chosen.1.data, depth + 1),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk_info::fixture::{table, Res};

    #[test]
    fn fr11_an_apps_label_is_read_in_the_default_language() {
        let data = table(
            &["Notizen", "Notes"],
            &[
                Res {
                    type_id: 2,
                    entry: 1,
                    language: "de",
                    density: 0,
                    value: Value {
                        kind: TYPE_STRING,
                        data: 0,
                    },
                },
                Res {
                    type_id: 2,
                    entry: 1,
                    language: "",
                    density: 0,
                    value: Value {
                        kind: TYPE_STRING,
                        data: 1,
                    },
                },
            ],
        );
        let table = ResourceTable::parse(&data).unwrap();
        assert_eq!(table.text(0x7f020001).as_deref(), Some("Notes"));
        assert_eq!(table.values(0x7f020001).len(), 2);
        assert_eq!(table.text(0x7f020002), None);
    }

    #[test]
    fn fr11_a_damaged_resource_table_is_refused_without_panicking() {
        let data = table(
            &["Notes"],
            &[Res {
                type_id: 2,
                entry: 0,
                language: "",
                density: 0,
                value: Value {
                    kind: TYPE_STRING,
                    data: 0,
                },
            }],
        );
        for cut in 0..data.len() {
            let _ = ResourceTable::parse(&data[..cut]);
        }
    }
}
