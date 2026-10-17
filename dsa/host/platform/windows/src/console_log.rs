//! The Android VM's console, kept in one log file per user.
//!
//! Everything the guest kernel and Android's init print on the serial console is appended
//! to `android-<user>.log`, each boot after a header line that says when it started, which
//! image it booted and with which kernel command line. The last lines of the current boot
//! are also kept in memory, so a start that fails can show them to the user at once.
//!
//! The file is bounded: a boot that finds it over [`ROTATE_AT_BYTES`] first renames it to
//! `<name>.1`, replacing the copy before, so at most two boots' worth of a noisy guest are
//! kept.
//!
//! Nothing here touches the pipe the console arrives on (`console_pipe`, Windows only), so
//! it is tested on every platform.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A log over this size at the start of a boot is moved aside to `<name>.1`.
pub const ROTATE_AT_BYTES: u64 = 4 * 1024 * 1024;
/// How many of the last non-empty console lines a failed start reports.
pub const TAIL_LINES: usize = 20;
/// Longer console lines are cut to this many characters in the report (not in the file).
const MAX_LINE_CHARS: usize = 300;
/// A "line" that never ends is kept in pieces of this size, so memory stays bounded.
const MAX_PARTIAL_BYTES: usize = 4096;

/// The one older copy of `path`: the same name with `.1` added.
pub fn old_copy_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Moves `path` to [`old_copy_path`] when it is larger than `limit` bytes, replacing the
/// copy there. Returns whether it did. A missing file is not an error.
pub fn rotate_if_large(path: &Path, limit: u64) -> io::Result<bool> {
    let size = match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if size <= limit {
        return Ok(false);
    }
    let old = old_copy_path(path);
    match std::fs::remove_file(&old) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::rename(path, &old)?;
    Ok(true)
}

/// `time` as an ISO 8601 UTC timestamp to the second, such as `2026-10-17T19:47:05Z`.
pub fn utc_timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970-01-01 to a civil date, after Howard Hinnant's `civil_from_days`.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The line that opens a boot in the log.
pub fn boot_header(time: SystemTime, image_version: Option<&str>, cmdline: &str) -> String {
    format!(
        "==== DSA {} boot at {}, image {}, kernel command line: {cmdline}\n",
        crate::VERSION,
        utc_timestamp(time),
        image_version.unwrap_or("(unversioned)"),
    )
}

/// One console line as it is shown to a person: UTF-8 (invalid bytes replaced), without
/// terminal escape sequences or other control characters, trimmed, and cut to a length a
/// status message can carry.
pub fn clean_line(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI sequences (colours, cursor moves) end at a byte in '@'..='~'.
            if chars.peek() == Some(&'[') {
                chars.next();
                for end in chars.by_ref() {
                    if ('@'..='~').contains(&end) {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '\t' {
            clean.push(' ');
        } else if !c.is_control() {
            clean.push(c);
        }
    }
    let clean = clean.trim();
    if clean.chars().count() > MAX_LINE_CHARS {
        let cut: String = clean.chars().take(MAX_LINE_CHARS).collect();
        format!("{cut}...")
    } else {
        clean.to_string()
    }
}

/// The last non-empty lines of a byte stream that arrives in pieces of any size.
#[derive(Debug, Clone)]
pub struct ConsoleTail {
    lines: VecDeque<String>,
    partial: Vec<u8>,
    keep: usize,
}

impl ConsoleTail {
    /// Keeps the last `keep` non-empty lines.
    pub fn new(keep: usize) -> ConsoleTail {
        ConsoleTail {
            lines: VecDeque::with_capacity(keep),
            partial: Vec::new(),
            keep,
        }
    }

    fn finish_line(&mut self) {
        let line = clean_line(&self.partial);
        self.partial.clear();
        if line.is_empty() || self.keep == 0 {
            return;
        }
        if self.lines.len() == self.keep {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Adds the next piece of the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.finish_line();
            } else {
                self.partial.push(byte);
                if self.partial.len() >= MAX_PARTIAL_BYTES {
                    self.finish_line();
                }
            }
        }
    }

    /// The last non-empty lines, oldest first, including a last line that has not ended.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.lines.iter().cloned().collect();
        let partial = clean_line(&self.partial);
        if !partial.is_empty() && self.keep > 0 {
            if lines.len() == self.keep {
                lines.remove(0);
            }
            lines.push(partial);
        }
        lines
    }
}

/// One boot's console: appended to the log file, and its last lines kept in memory.
///
/// Writing is best effort. A log that cannot be opened or written is reported once, in
/// [`write_error`](Self::write_error) and in the failure report, and the console's last
/// lines are still kept, so a full disk or a locked file never stops a boot or the service.
#[derive(Debug)]
pub struct ConsoleLog {
    path: PathBuf,
    file: Option<File>,
    tail: ConsoleTail,
    write_error: Option<String>,
}

impl ConsoleLog {
    /// Starts a boot in the log at `path`: moves a large log aside, then appends `header`.
    pub fn begin_boot(path: &Path, header: &str) -> ConsoleLog {
        let mut log = ConsoleLog {
            path: path.to_path_buf(),
            file: None,
            tail: ConsoleTail::new(TAIL_LINES),
            write_error: None,
        };
        let opened = (|| {
            if let Some(folder) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(folder)?;
            }
            rotate_if_large(path, ROTATE_AT_BYTES)?;
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            // A boot that ended in the middle of a line still starts the header on its own.
            let ends_open = std::fs::metadata(path)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
                && !last_byte_is_newline(path);
            if ends_open {
                file.write_all(b"\n")?;
            }
            file.write_all(header.as_bytes())?;
            Ok::<File, io::Error>(file)
        })();
        match opened {
            Ok(file) => log.file = Some(file),
            Err(error) => log.fail(&error),
        }
        log
    }

    fn fail(&mut self, error: &io::Error) {
        self.file = None;
        if self.write_error.is_none() {
            let why = format!(
                "the console log {} cannot be written: {error}",
                self.path.display()
            );
            eprintln!("{why}");
            self.write_error = Some(why);
        }
    }

    /// Records the next piece of console output.
    pub fn record(&mut self, bytes: &[u8]) {
        self.tail.push(bytes);
        if let Some(file) = self.file.as_mut() {
            if let Err(error) = file.write_all(bytes) {
                self.fail(&error);
            }
        }
    }

    /// Adds a line of the host's own to the log (not to the console's tail), such as why
    /// the console ended.
    pub fn note(&mut self, text: &str) {
        if let Some(file) = self.file.as_mut() {
            if let Err(error) = file.write_all(format!("\n==== {text}\n").as_bytes()) {
                self.fail(&error);
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The last non-empty console lines of this boot, oldest first.
    pub fn tail(&self) -> Vec<String> {
        self.tail.lines()
    }

    /// Why the log file could not be written, if it could not.
    pub fn write_error(&self) -> Option<&str> {
        self.write_error.as_deref()
    }

    /// What a failed start tells the user about this boot: its last console lines and where
    /// the rest is.
    pub fn failure_report(&self) -> String {
        failure_report(&self.path, &self.tail(), self.write_error())
    }
}

fn last_byte_is_newline(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = File::open(path) else {
        return true;
    };
    let mut byte = [0u8; 1];
    file.seek(SeekFrom::End(-1)).is_err() || file.read_exact(&mut byte).is_err() || byte[0] == b'\n'
}

/// The text a failed start reports about the guest's console: the last lines, one per
/// line and indented, after a line that says where the whole boot is kept.
pub fn failure_report(log_path: &Path, tail: &[String], write_error: Option<&str>) -> String {
    let mut report = if tail.is_empty() {
        format!(
            "The Android console printed nothing during this boot, so the kernel may not have \
             started (log: {}).",
            log_path.display()
        )
    } else {
        let mut text = format!(
            "Last lines of the Android console (the whole boot is in {}):",
            log_path.display()
        );
        for line in tail {
            text.push_str("\n  ");
            text.push_str(line);
        }
        text
    };
    if let Some(why) = write_error {
        report.push_str(&format!("\nNote: {why}."));
    }
    report
}
