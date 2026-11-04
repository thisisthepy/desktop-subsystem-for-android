//! The diagnostics archive: what a user sends with a problem report, gathered on demand.
//!
//! The guest sends its part (its whole log, tombstones, ANR traces, dropbox, pstore, a
//! dumpsys summary) as files on the log port; the host adds its own logs; [`write_archive`]
//! writes all of it as one zip archive to any writer, as it arrives, so the service never
//! stores the archive. Each file is held in memory only until it is complete, and up to
//! [`MAX_FILE_BYTES`].

use std::collections::{BTreeSet, HashMap};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::guest_log::DiagnosticsEvent;

/// The most bytes of one file the archive keeps. A longer file keeps its end, which is the
/// part a report needs, after a line that says what was cut.
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

/// The most files one archive holds.
pub const MAX_FILES: usize = 4096;

/// How long the archive waits for the guest's next piece before it gives up on the rest.
pub const GUEST_IDLE: Duration = Duration::from_secs(60);

/// How long the guest may take to send all of its part.
pub const GUEST_TOTAL: Duration = Duration::from_secs(10 * 60);

/// The CRC-32 a zip archive keeps of each file (IEEE 802.3, reflected).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::default();
    crc.update(data);
    crc.value()
}

/// A CRC-32 computed piece by piece.
#[derive(Debug, Clone, Copy)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Crc32(0xffff_ffff)
    }
}

impl Crc32 {
    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.0;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        self.0 = crc;
    }

    pub fn value(&self) -> u32 {
        !self.0
    }
}

/// A date and time as MS-DOS keeps it in a zip archive, in UTC.
fn dos_time(time: SystemTime) -> (u16, u16) {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let year = year.clamp(1980, 2107) - 1980;
    let date = ((year as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = (((rest / 3600) as u16) << 11)
        | (((rest % 3600 / 60) as u16) << 5)
        | ((rest % 60 / 2) as u16);
    (time, date)
}

struct CentralEntry {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

/// Writes a zip archive of stored (uncompressed) files, one whole file at a time.
pub struct ZipWriter<W: Write> {
    out: W,
    offset: u64,
    entries: Vec<CentralEntry>,
    time: u16,
    date: u16,
}

fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "the archive would pass 4 GiB")
}

impl<W: Write> ZipWriter<W> {
    /// An archive whose files are dated `now`.
    pub fn new(out: W, now: SystemTime) -> ZipWriter<W> {
        let (time, date) = dos_time(now);
        ZipWriter {
            out,
            offset: 0,
            entries: Vec::new(),
            time,
            date,
        }
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        if self.offset > u64::from(u32::MAX) {
            return Err(too_large());
        }
        Ok(())
    }

    /// Adds one file.
    pub fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let size = u32::try_from(data.len()).map_err(|_| too_large())?;
        let offset = u32::try_from(self.offset).map_err(|_| too_large())?;
        let crc = crc32(data);
        let mut header = Vec::with_capacity(30 + name.len());
        header.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        header.extend_from_slice(&20u16.to_le_bytes()); // version needed
        header.extend_from_slice(&(1u16 << 11).to_le_bytes()); // names are UTF-8
        header.extend_from_slice(&0u16.to_le_bytes()); // stored
        header.extend_from_slice(&self.time.to_le_bytes());
        header.extend_from_slice(&self.date.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        header.extend_from_slice(name.as_bytes());
        self.put(&header)?;
        self.put(data)?;
        self.entries.push(CentralEntry {
            name: name.to_string(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    /// Writes the central directory and returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        let start = u32::try_from(self.offset).map_err(|_| too_large())?;
        let entries = std::mem::take(&mut self.entries);
        for entry in &entries {
            let mut record = Vec::with_capacity(46 + entry.name.len());
            record.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            record.extend_from_slice(&20u16.to_le_bytes()); // made by
            record.extend_from_slice(&20u16.to_le_bytes()); // needed
            record.extend_from_slice(&(1u16 << 11).to_le_bytes());
            record.extend_from_slice(&0u16.to_le_bytes());
            record.extend_from_slice(&self.time.to_le_bytes());
            record.extend_from_slice(&self.date.to_le_bytes());
            record.extend_from_slice(&entry.crc.to_le_bytes());
            record.extend_from_slice(&entry.size.to_le_bytes());
            record.extend_from_slice(&entry.size.to_le_bytes());
            record.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            record.extend_from_slice(&0u16.to_le_bytes()); // extra
            record.extend_from_slice(&0u16.to_le_bytes()); // comment
            record.extend_from_slice(&0u16.to_le_bytes()); // disk
            record.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            record.extend_from_slice(&0u32.to_le_bytes()); // external attributes
            record.extend_from_slice(&entry.offset.to_le_bytes());
            record.extend_from_slice(entry.name.as_bytes());
            self.put(&record)?;
        }
        let size = u32::try_from(self.offset - u64::from(start)).map_err(|_| too_large())?;
        let count = u16::try_from(entries.len()).map_err(|_| too_large())?;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&size.to_le_bytes());
        end.extend_from_slice(&start.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.put(&end)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// A host log to put in the archive: its name there, and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFile {
    /// Such as `host/dsa-hostd.log`.
    pub name: String,
    pub path: PathBuf,
    /// For a log shared by every user, whose lines about a user start with that user's ID
    /// (`S-1-5-21-...: `): the user whose lines are kept. Lines about any other user are left
    /// out, so one user's diagnostics never carry another's.
    pub only_user: Option<String>,
}

/// Leaves out the lines of `data` that start with another user's ID than `user`.
pub fn lines_of_user(data: &[u8], user: &str) -> Vec<u8> {
    let mut kept = Vec::with_capacity(data.len());
    for line in data.split_inclusive(|b| *b == b'\n') {
        let about_user = line
            .split(|b| *b == b':')
            .next()
            .filter(|id| id.starts_with(b"S-1-") && !id.contains(&b' '));
        match about_user {
            Some(id) if id != user.as_bytes() => {}
            _ => kept.extend_from_slice(line),
        }
    }
    kept
}

/// Keeps the end of `data` when it is longer than [`MAX_FILE_BYTES`], after a line that says
/// how much was cut.
fn bounded(mut data: Vec<u8>) -> Vec<u8> {
    if data.len() <= MAX_FILE_BYTES {
        return data;
    }
    let cut = data.len() - MAX_FILE_BYTES;
    let mut kept = format!("[the first {cut} bytes were left out]\n").into_bytes();
    kept.extend_from_slice(&data.split_off(cut));
    kept
}

/// Reads the last [`MAX_FILE_BYTES`] of a file.
fn read_end(path: &std::path::Path) -> io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut prefix = Vec::new();
    if len > MAX_FILE_BYTES as u64 {
        let cut = len - MAX_FILE_BYTES as u64;
        file.seek(SeekFrom::Start(cut))?;
        prefix = format!("[the first {cut} bytes were left out]\n").into_bytes();
    }
    let mut data = prefix;
    file.take(MAX_FILE_BYTES as u64).read_to_end(&mut data)?;
    Ok(data)
}

/// What [`write_archive`] put in the archive and what it could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveReport {
    pub files: Vec<String>,
    pub problems: Vec<String>,
}

/// Gives a name not taken yet, by adding `.2`, `.3` and so on.
fn unique(names: &BTreeSet<String>, name: &str) -> String {
    if !names.contains(name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name}.{n}"))
        .find(|candidate| !names.contains(candidate))
        .unwrap_or_else(|| name.to_string())
}

/// Adds a file to the archive under a name not taken yet, unless it is full.
fn add<W: Write>(
    zip: &mut ZipWriter<W>,
    report: &mut ArchiveReport,
    names: &mut BTreeSet<String>,
    name: &str,
    data: Vec<u8>,
) -> io::Result<()> {
    if names.len() >= MAX_FILES {
        report
            .problems
            .push(format!("{name} was left out: the archive holds {MAX_FILES} files at most"));
        return Ok(());
    }
    let name = unique(names, name);
    zip.add(&name, &bounded(data))?;
    names.insert(name.clone());
    report.files.push(name);
    Ok(())
}

/// Writes the archive to `out`: the guest's files as they arrive on `guest` (or, without it,
/// why there are none in `guest_missing`), then `extra` files the caller made (such as the
/// host's recent log records), then `host_files`, then `summary.txt`, which says what is in
/// the archive and what could not be gathered. `about` opens the summary.
pub fn write_archive<W: Write>(
    out: W,
    about: &str,
    guest: Option<Receiver<DiagnosticsEvent>>,
    guest_missing: Option<String>,
    extra: Vec<(String, Vec<u8>)>,
    host_files: &[HostFile],
    now: SystemTime,
) -> io::Result<(W, ArchiveReport)> {
    let mut zip = ZipWriter::new(out, now);
    let mut report = ArchiveReport::default();
    let mut names = BTreeSet::new();
    match (guest, guest_missing) {
        (Some(events), _) => {
            let started = Instant::now();
            let mut open: HashMap<String, Vec<u8>> = HashMap::new();
            loop {
                let left = GUEST_TOTAL.saturating_sub(started.elapsed());
                if left.is_zero() {
                    report.problems.push(format!(
                        "Android had not sent all of its part after {} minutes",
                        GUEST_TOTAL.as_secs() / 60
                    ));
                    break;
                }
                match events.recv_timeout(GUEST_IDLE.min(left)) {
                    Ok(DiagnosticsEvent::File { name, data, last }) => {
                        let piece = open.entry(name.clone()).or_default();
                        // Past the limit only the end is kept, so the buffer stays bounded.
                        piece.extend_from_slice(&data);
                        if piece.len() > 2 * MAX_FILE_BYTES {
                            let cut = piece.len() - MAX_FILE_BYTES;
                            piece.drain(..cut);
                        }
                        if last {
                            let data = open.remove(&name).unwrap_or_default();
                            add(&mut zip, &mut report, &mut names, &name, data)?;
                        }
                    }
                    Ok(DiagnosticsEvent::Done(problem)) => {
                        if let Some(problem) = problem {
                            report.problems.push(format!("Android: {problem}"));
                        }
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        report.problems.push(format!(
                            "Android sent nothing for {} seconds; its part may be incomplete",
                            GUEST_IDLE.as_secs()
                        ));
                        break;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        report
                            .problems
                            .push("Android's part ended before it was complete".into());
                        break;
                    }
                }
            }
            // A file the guest did not finish is still worth having.
            let mut unfinished: Vec<_> = open.into_iter().collect();
            unfinished.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, data) in unfinished {
                report.problems.push(format!("{name} is incomplete"));
                add(&mut zip, &mut report, &mut names, &name, data)?;
            }
        }
        (None, why) => report.problems.push(format!(
            "Android's part is missing: {}",
            why.unwrap_or_else(|| "Android is not running".into())
        )),
    }
    for (name, data) in extra {
        add(&mut zip, &mut report, &mut names, &name, data)?;
    }
    for file in host_files {
        match read_end(&file.path) {
            Ok(data) => {
                let data = match &file.only_user {
                    Some(user) => lines_of_user(&data, user),
                    None => data,
                };
                add(&mut zip, &mut report, &mut names, &file.name, data)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => report.problems.push(format!(
                "{} could not be read: {error}",
                file.path.display()
            )),
        }
    }
    let mut summary = format!("{about}\n\nFiles:\n");
    for name in &report.files {
        summary.push_str(&format!("  {name}\n"));
    }
    if report.problems.is_empty() {
        summary.push_str("\nEverything was gathered.\n");
    } else {
        summary.push_str("\nNot gathered:\n");
        for problem in &report.problems {
            summary.push_str(&format!("  {problem}\n"));
        }
    }
    zip.add("summary.txt", summary.as_bytes())?;
    let out = zip.finish()?;
    Ok((out, report))
}
