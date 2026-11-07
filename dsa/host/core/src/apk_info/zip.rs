//! Reading the entries of an APK, which is a ZIP archive.
//!
//! Only what an APK needs is read: the central directory (with its ZIP64 form, for an APK
//! above 4 GiB of entries), stored and deflated entries, and the bytes just before the
//! central directory, where the APK Signature Scheme puts its signing block.

use std::io::{Read, Seek, SeekFrom};

use super::inflate::inflate;

/// One file in the archive.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    flags: u16,
    method: u16,
    compressed_size: u64,
    size: u64,
    header_offset: u64,
}

impl Entry {
    /// The entry's length once uncompressed.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Whether the entry is encrypted (bit 0 of its general purpose flags), so that it
    /// cannot be read without a key.
    pub fn encrypted(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// An opened archive: its entries, and where its central directory starts.
pub struct Archive<R> {
    reader: R,
    pub entries: Vec<Entry>,
    /// Where the central directory starts, which is where the signing block ends.
    pub central_directory_offset: u64,
}

/// The most entries an archive may have; real APKs have a few thousand.
const MAX_ENTRIES: usize = 200_000;

fn u16_at(data: &[u8], at: usize) -> Result<u16, String> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| "the archive is cut short".to_string())
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, String> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "the archive is cut short".to_string())
}

fn u64_at(data: &[u8], at: usize) -> Result<u64, String> {
    data.get(at..at + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| "the archive is cut short".to_string())
}

fn io(error: std::io::Error) -> String {
    format!("the file cannot be read: {error}")
}

impl<R: Read + Seek> Archive<R> {
    /// Opens the archive `reader` holds, reading its central directory.
    pub fn open(mut reader: R) -> Result<Archive<R>, String> {
        let length = reader.seek(SeekFrom::End(0)).map_err(io)?;
        // The end of central directory record is 22 bytes, followed by a comment of at most
        // 65535 bytes.
        let tail_length = length.min(22 + 65_535);
        let tail = read_at(&mut reader, length - tail_length, tail_length as usize)?;
        let end = (0..tail.len().saturating_sub(21))
            .rev()
            .find(|&at| tail[at..].starts_with(b"PK\x05\x06"))
            .ok_or("the file is not a ZIP archive, so it is not an APK")?;
        let mut count = u64::from(u16_at(&tail, end + 10)?);
        let mut directory_size = u64::from(u32_at(&tail, end + 12)?);
        let mut directory_offset = u64::from(u32_at(&tail, end + 16)?);
        let end_offset = length - tail_length + end as u64;
        if count == 0xFFFF || directory_size == 0xFFFF_FFFF || directory_offset == 0xFFFF_FFFF {
            // ZIP64: a locator just before the record points at the ZIP64 record.
            if end_offset >= 20 {
                let locator = read_at(&mut reader, end_offset - 20, 20)?;
                if locator.starts_with(b"PK\x06\x07") {
                    let record_offset = u64_at(&locator, 8)?;
                    let record = read_at(&mut reader, record_offset, 56)?;
                    if !record.starts_with(b"PK\x06\x06") {
                        return Err("the archive's ZIP64 record is damaged".into());
                    }
                    count = u64_at(&record, 32)?;
                    directory_size = u64_at(&record, 40)?;
                    directory_offset = u64_at(&record, 48)?;
                }
            }
        }
        if count as usize > MAX_ENTRIES || directory_offset + directory_size > length {
            return Err("the archive's directory is damaged".into());
        }
        let directory = read_at(&mut reader, directory_offset, directory_size as usize)?;
        let mut entries = Vec::with_capacity(count as usize);
        let mut at = 0;
        for _ in 0..count {
            if !directory
                .get(at..)
                .is_some_and(|rest| rest.starts_with(b"PK\x01\x02"))
            {
                return Err("the archive's directory is damaged".into());
            }
            let flags = u16_at(&directory, at + 8)?;
            let method = u16_at(&directory, at + 10)?;
            let mut compressed_size = u64::from(u32_at(&directory, at + 20)?);
            let mut size = u64::from(u32_at(&directory, at + 24)?);
            let name_length = usize::from(u16_at(&directory, at + 28)?);
            let extra_length = usize::from(u16_at(&directory, at + 30)?);
            let comment_length = usize::from(u16_at(&directory, at + 32)?);
            let mut header_offset = u64::from(u32_at(&directory, at + 42)?);
            let name = directory
                .get(at + 46..at + 46 + name_length)
                .ok_or("the archive's directory is damaged")?;
            let name = String::from_utf8_lossy(name).into_owned();
            let extra = directory
                .get(at + 46 + name_length..at + 46 + name_length + extra_length)
                .ok_or("the archive's directory is damaged")?;
            // The ZIP64 extra field holds, in this order, whichever sizes were too large.
            let mut field = 0;
            while field + 4 <= extra.len() {
                let id = u16_at(extra, field)?;
                let field_length = usize::from(u16_at(extra, field + 2)?);
                if id == 1 {
                    let mut value = field + 4;
                    for slot in [&mut size, &mut compressed_size, &mut header_offset] {
                        if *slot == 0xFFFF_FFFF && value + 8 <= field + 4 + field_length {
                            *slot = u64_at(extra, value)?;
                            value += 8;
                        }
                    }
                }
                field += 4 + field_length;
            }
            entries.push(Entry {
                name,
                flags,
                method,
                compressed_size,
                size,
                header_offset,
            });
            at += 46 + name_length + extra_length + comment_length;
        }
        Ok(Archive {
            reader,
            entries,
            central_directory_offset: directory_offset,
        })
    }

    /// The entry called `name`, if the archive has one.
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// The content of `entry`, uncompressed, refusing an entry larger than `limit` bytes.
    pub fn read(&mut self, entry: &Entry, limit: usize) -> Result<Vec<u8>, String> {
        if entry.size > limit as u64 {
            return Err(format!(
                "{} is {} bytes, more than the {limit} read",
                entry.name, entry.size
            ));
        }
        let header = read_at(&mut self.reader, entry.header_offset, 30)?;
        if !header.starts_with(b"PK\x03\x04") {
            return Err(format!("the archive's entry {} is damaged", entry.name));
        }
        let skip = 30 + u64::from(u16_at(&header, 26)?) + u64::from(u16_at(&header, 28)?);
        let compressed_limit = (limit as u64).saturating_mul(2).max(1024);
        if entry.compressed_size > compressed_limit {
            return Err(format!("the archive's entry {} is damaged", entry.name));
        }
        let data = read_at(
            &mut self.reader,
            entry.header_offset + skip,
            entry.compressed_size as usize,
        )?;
        match entry.method {
            0 => Ok(data),
            8 => inflate(&data, limit).map_err(|why| format!("{}: {why}", entry.name)),
            other => Err(format!(
                "{} is compressed in a way APKs do not use (method {other})",
                entry.name
            )),
        }
    }

    /// Where the stored or compressed bytes of `entry` are in the file, for an entry too
    /// large to read into memory: its compression method, the offset and the length.
    pub fn data_span(&mut self, entry: &Entry) -> Result<(u16, u64, u64), String> {
        let header = read_at(&mut self.reader, entry.header_offset, 30)?;
        if !header.starts_with(b"PK\x03\x04") {
            return Err(format!("the archive's entry {} is damaged", entry.name));
        }
        let skip = 30 + u64::from(u16_at(&header, 26)?) + u64::from(u16_at(&header, 28)?);
        let offset = entry.header_offset + skip;
        Ok((entry.method, offset, entry.compressed_size))
    }

    /// The file the archive is read from.
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// `length` bytes from `offset` in the file, as the signing block needs.
    pub fn read_raw(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, String> {
        read_at(&mut self.reader, offset, length)
    }
}

fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    reader.seek(SeekFrom::Start(offset)).map_err(io)?;
    let mut data = vec![0u8; length];
    reader.read_exact(&mut data).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            "the archive is cut short".to_string()
        } else {
            io(e)
        }
    })?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apk_info::fixture::archive;
    use std::io::Cursor;

    #[test]
    fn fr11_the_entries_of_an_apk_are_read() {
        let data = archive(
            &[
                ("AndroidManifest.xml", &b"manifest"[..]),
                ("res/a.png", &b"png"[..]),
            ],
            b"",
        );
        let mut zip = Archive::open(Cursor::new(data)).unwrap();
        assert_eq!(zip.entries.len(), 2);
        let entry = zip.entry("res/a.png").unwrap().clone();
        assert_eq!(zip.read(&entry, 100).unwrap(), b"png");
        assert!(zip.read(&entry, 2).is_err());
    }

    #[test]
    fn fr11_a_file_that_is_not_a_zip_archive_is_refused() {
        assert!(Archive::open(Cursor::new(b"not an archive at all, really".to_vec())).is_err());
        assert!(Archive::open(Cursor::new(Vec::new())).is_err());
    }
}
