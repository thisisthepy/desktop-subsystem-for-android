//! What every host does with the guest's serial console log: where the older copy goes when
//! the file is large, and the header line that opens each boot.
//!
//! Each boot is appended to one log file per user, after a header line that says when it
//! started, which image it booted and with which kernel command line. A boot that finds the
//! file over a limit first moves it to `<name>.1`, replacing the copy before, so a noisy guest
//! costs at most two boots' worth of disk.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The one older copy of `path`: the same name with `.1` added.
pub fn old_copy_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// The console log and its older copy as they are named in a user's diagnostics: each name
/// with the file it is.
pub fn diagnostic_files(console_log: &Path) -> [(&'static str, PathBuf); 2] {
    [
        ("host/android-console.log", console_log.to_path_buf()),
        ("host/android-console.1.log", old_copy_path(console_log)),
    ]
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

/// The line that opens a boot in the log, ended by a line break: the host's `version`, when
/// the boot started, the image it booted and the kernel command line.
pub fn boot_header(
    version: &str,
    time: SystemTime,
    image_version: Option<&str>,
    cmdline: &str,
) -> String {
    format!(
        "==== DSA {version} boot at {}, image {}, kernel command line: {cmdline}\n",
        utc_timestamp(time),
        image_version.unwrap_or("(unversioned)"),
    )
}
