//! Reading a tar archive as it is decompressed, for LiteGapps' `files.tar.xz`.
//!
//! The archive is pushed in through [`Write`], as the xz decoder produces it, and each regular
//! file goes on to a [`TarSink`] as its data arrives, so no file is held in memory. POSIX
//! (ustar and pax) and GNU long names are read. A link, a device or a pipe is refused: the
//! image is made of directories and regular files only, and a link could point anywhere.

use std::io::{self, Write};

const BLOCK: usize = 512;
/// The longest name a long name or pax record may give.
const MAX_NAME: u64 = 4096;
/// The largest pax header taken.
const MAX_PAX: u64 = 64 << 10;

/// Where the files of an archive go.
pub trait TarSink {
    /// A regular file starts: `path` as the archive names it; `size` bytes follow in
    /// [`data`](Self::data) calls, then [`end`](Self::end).
    fn begin(&mut self, path: &str, size: u64) -> Result<(), String>;
    fn data(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn end(&mut self) -> Result<(), String>;
}

enum Body {
    File,
    LongName(Vec<u8>),
    Pax(Vec<u8>),
    Skip,
}

enum State {
    Header,
    Body {
        left: u64,
        padding: u64,
        body: Body,
    },
    Padding(u64),
    /// The two empty blocks that end an archive have been read.
    Ended,
}

/// A tar archive being read into a [`TarSink`].
pub struct Tar<'a> {
    sink: &'a mut dyn TarSink,
    header: Vec<u8>,
    state: State,
    empty_blocks: u32,
    long_name: Option<String>,
    pax_path: Option<String>,
    /// The first error, which every later write repeats.
    failed: Option<String>,
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// A numeric field: octal digits (padded with spaces or NULs), or GNU's base-256 form.
fn number(field: &[u8]) -> Result<u64, String> {
    if field.first().is_some_and(|&b| b & 0x80 != 0) {
        let mut value = u64::from(field[0] & 0x7F);
        for &byte in &field[1..] {
            value = value
                .checked_mul(256)
                .and_then(|v| v.checked_add(u64::from(byte)))
                .ok_or("a tar header holds a number that is too large")?;
        }
        return Ok(value);
    }
    let digits = text(field);
    let digits = digits.trim_matches(|c: char| c == ' ' || c == '\0');
    if digits.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(digits, 8).map_err(|_| format!("a tar header holds {digits:?} as a number"))
}

/// The `path` record of a pax header, if it has one.
fn pax_path(records: &[u8]) -> Result<Option<String>, String> {
    let mut rest = records;
    let mut path = None;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or("a pax header is damaged")?;
        let length: usize = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|digits| digits.parse().ok())
            .filter(|&length| length > space + 1 && length <= rest.len())
            .ok_or("a pax header is damaged")?;
        let record = &rest[space + 1..length];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(String::from_utf8_lossy(value).into_owned());
        }
        rest = &rest[length..];
    }
    Ok(path)
}

impl<'a> Tar<'a> {
    pub fn new(sink: &'a mut dyn TarSink) -> Tar<'a> {
        Tar {
            sink,
            header: Vec::with_capacity(BLOCK),
            state: State::Header,
            empty_blocks: 0,
            long_name: None,
            pax_path: None,
            failed: None,
        }
    }

    /// Says whether the whole archive arrived: it must end between entries.
    pub fn finish(self) -> Result<(), String> {
        if let Some(why) = self.failed {
            return Err(why);
        }
        match self.state {
            State::Ended => Ok(()),
            State::Header if self.header.is_empty() && self.empty_blocks > 0 => Ok(()),
            _ => Err("the tar archive ends in the middle of a file".into()),
        }
    }

    fn read_header(&mut self) -> Result<(), String> {
        let header = std::mem::take(&mut self.header);
        if header.iter().all(|&b| b == 0) {
            self.empty_blocks += 1;
            if self.empty_blocks == 2 {
                self.state = State::Ended;
            }
            return Ok(());
        }
        self.empty_blocks = 0;
        let stored = number(&header[148..156])?;
        let sum: u64 = header
            .iter()
            .enumerate()
            .map(|(at, &b)| {
                if (148..156).contains(&at) {
                    u64::from(b' ')
                } else {
                    u64::from(b)
                }
            })
            .sum();
        if stored != sum {
            return Err("the data is not a tar archive (a header's checksum is wrong)".into());
        }
        let mut name = text(&header[0..100]);
        if &header[257..262] == b"ustar" {
            let prefix = text(&header[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        let size = number(&header[124..136])?;
        let kind = header[156];
        let body = match kind {
            b'0' | 0 | b'7' => {
                let path = self
                    .pax_path
                    .take()
                    .or_else(|| self.long_name.take())
                    .unwrap_or(name);
                self.long_name = None;
                if path.ends_with('/') {
                    // A directory, as archives before ustar wrote one.
                    Body::Skip
                } else {
                    self.sink.begin(&path, size)?;
                    Body::File
                }
            }
            b'5' => {
                self.pax_path = None;
                self.long_name = None;
                Body::Skip
            }
            b'L' if size <= MAX_NAME => Body::LongName(Vec::new()),
            b'x' if size <= MAX_PAX => Body::Pax(Vec::new()),
            b'g' => Body::Skip,
            b'1' | b'2' => {
                return Err(format!(
                    "the archive holds a link ({name}); only files and directories are taken"
                ))
            }
            b'3' | b'4' | b'6' => {
                return Err(format!(
                    "the archive holds a device or a pipe ({name}); only files and directories \
                     are taken"
                ))
            }
            b'L' | b'x' => return Err("a tar header names a path that is too long".into()),
            _ => Body::Skip,
        };
        let padding = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
        self.state = State::Body {
            left: size,
            padding,
            body,
        };
        self.after_body()
    }

    /// Moves on once a body has had all its bytes.
    fn after_body(&mut self) -> Result<(), String> {
        let State::Body {
            left: 0, padding, ..
        } = self.state
        else {
            return Ok(());
        };
        let State::Body { body, .. } = std::mem::replace(&mut self.state, State::Padding(padding))
        else {
            return Ok(());
        };
        match body {
            Body::File => self.sink.end()?,
            Body::LongName(name) => {
                let name = String::from_utf8_lossy(&name)
                    .trim_end_matches('\0')
                    .to_string();
                self.long_name = Some(name);
            }
            Body::Pax(records) => {
                if let Some(path) = pax_path(&records)? {
                    self.pax_path = Some(path);
                }
            }
            Body::Skip => {}
        }
        if padding == 0 {
            self.state = State::Header;
        }
        Ok(())
    }

    fn take(&mut self, mut data: &[u8]) -> Result<(), String> {
        while !data.is_empty() {
            match &mut self.state {
                State::Ended => return Ok(()),
                State::Header => {
                    let take = (BLOCK - self.header.len()).min(data.len());
                    self.header.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if self.header.len() == BLOCK {
                        self.read_header()?;
                    }
                }
                State::Padding(left) => {
                    let take = (*left).min(data.len() as u64);
                    *left -= take;
                    data = &data[take as usize..];
                    if *left == 0 {
                        self.state = State::Header;
                    }
                }
                State::Body { left, body, .. } => {
                    let take = (*left).min(data.len() as u64) as usize;
                    let (piece, rest) = data.split_at(take);
                    *left -= take as u64;
                    match body {
                        Body::File => self.sink.data(piece)?,
                        Body::LongName(name) | Body::Pax(name) => name.extend_from_slice(piece),
                        Body::Skip => {}
                    }
                    data = rest;
                    self.after_body()?;
                }
            }
        }
        Ok(())
    }
}

impl Write for Tar<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if let Some(why) = &self.failed {
            return Err(io::Error::other(why.clone()));
        }
        match self.take(data) {
            Ok(()) => Ok(data.len()),
            Err(why) => {
                self.failed = Some(why.clone());
                Err(io::Error::other(why))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(any(test, feature = "conformance"))]
pub mod fixture {
    //! Tar archives for the tests.

    /// One ustar header.
    pub fn header(name: &str, size: u64, kind: u8) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[108..115].copy_from_slice(b"0000000");
        header[116..123].copy_from_slice(b"0000000");
        header[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        header[136..147].copy_from_slice(b"00000000000");
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        header
    }

    /// An archive of regular files (kind `b'0'`) and directories (a name ending in `/`).
    pub fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries {
            if name.ends_with('/') {
                out.extend(header(name, 0, b'5'));
                continue;
            }
            out.extend(header(name, data.len() as u64, b'0'));
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend(vec![0u8; 1024]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Collect {
        files: Vec<(String, Vec<u8>)>,
        open: bool,
    }

    impl TarSink for Collect {
        fn begin(&mut self, path: &str, size: u64) -> Result<(), String> {
            assert!(!self.open);
            self.open = true;
            self.files
                .push((path.to_string(), Vec::with_capacity(size as usize)));
            Ok(())
        }
        fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.files.last_mut().unwrap().1.extend_from_slice(bytes);
            Ok(())
        }
        fn end(&mut self) -> Result<(), String> {
            self.open = false;
            Ok(())
        }
    }

    fn read(archive: &[u8], step: usize) -> Result<Vec<(String, Vec<u8>)>, String> {
        let mut sink = Collect::default();
        let mut tar = Tar::new(&mut sink);
        for piece in archive.chunks(step) {
            tar.write_all(piece).map_err(|e| e.to_string())?;
        }
        tar.finish()?;
        Ok(sink.files)
    }

    #[test]
    fn fr32_the_files_of_a_tar_archive_arrive_whole_however_it_is_split() {
        let big: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let archive = fixture::archive(&[
            ("x86_64/", b""),
            ("x86_64/36/system/product/app/A.apk", &big),
            ("x86_64/36/system/product/etc/empty.xml", b""),
            ("x86_64/36/system/product/etc/one.xml", b"1"),
        ]);
        for step in [1, 100, 511, 512, 513, 4096, archive.len()] {
            let files = read(&archive, step).unwrap();
            assert_eq!(files.len(), 3, "in pieces of {step}");
            assert_eq!(files[0].0, "x86_64/36/system/product/app/A.apk");
            assert_eq!(files[0].1, big);
            assert_eq!(files[1].1, b"");
            assert_eq!(files[2].1, b"1");
        }
    }

    #[test]
    fn fr32_a_gnu_long_name_and_a_pax_path_name_the_next_file() {
        let long = format!("x86_64/36/system/product/{}/file", "d".repeat(150));
        let mut archive = fixture::header("././@LongLink", long.len() as u64 + 1, b'L');
        archive.extend_from_slice(long.as_bytes());
        archive.push(0);
        archive.resize(1024, 0);
        archive.extend(fixture::header("short", 1, b'0'));
        archive.extend_from_slice(b"a");
        archive.resize(2048, 0);
        let record = "17 path=pax/name\n";
        assert_eq!(record.len(), 17);
        archive.extend(fixture::header("PaxHeader", record.len() as u64, b'x'));
        archive.extend_from_slice(record.as_bytes());
        archive.resize(3072, 0);
        archive.extend(fixture::header("ignored", 1, b'0'));
        archive.extend_from_slice(b"b");
        archive.resize(4096, 0);
        archive.extend(vec![0u8; 1024]);
        let files = read(&archive, 300).unwrap();
        assert_eq!(files[0].0, long);
        assert_eq!(files[1].0, "pax/name");
    }

    #[test]
    fn fr32_links_damage_and_a_cut_archive_are_refused() {
        let mut link = fixture::header("x86_64/36/system/product/app/A.apk", 0, b'2');
        link.extend(vec![0u8; 1024]);
        assert!(read(&link, 512).unwrap_err().contains("link"));
        let mut damaged = fixture::archive(&[("a", b"x")]);
        damaged[10] ^= 1;
        assert!(read(&damaged, 512).unwrap_err().contains("checksum"));
        let whole = fixture::archive(&[("a", &[7u8; 2000])]);
        assert!(read(&whole[..1500], 512).is_err(), "cut short");
    }
}
