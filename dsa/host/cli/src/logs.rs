//! `dsa logcat` and `dsa logs collect`: Android's log as it streams, and the diagnostics
//! archive, both through the local API. Nothing is kept on disk but the archive the user
//! asks for.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use dsa_host_core::guest_log::{priority_letter, utc_time, LogEntry, LogFilter};
use dsa_host_core::host_api::HostApiClient;
use serde_json::{json, Value};

/// How long `dsa logs collect` waits for each piece of the archive: the service waits up to
/// ten minutes for Android's part before it writes the rest.
pub const COLLECT_IDLE: Duration = Duration::from_secs(11 * 60);

/// One record as one JSON object, for programs such as the settings app.
pub fn entry_json(entry: &LogEntry) -> Value {
    if entry.dropped > 0 {
        return json!({ "dropped": entry.dropped });
    }
    json!({
        "time": utc_time(entry.time_ns),
        "time_ns": entry.time_ns,
        "buffer": entry.buffer.name(),
        "priority": priority_letter(entry.priority).to_string(),
        "pid": entry.pid,
        "tid": entry.tid,
        "uid": entry.uid,
        "tag": entry.tag,
        "message": entry.message,
    })
}

/// Writes records as `dsa logcat` prints them. Returns false once the output is closed, as
/// when a pipe's reader has gone.
pub fn write_entries(out: &mut dyn Write, entries: &[LogEntry], as_json: bool) -> bool {
    for entry in entries {
        let line = if as_json {
            entry_json(entry).to_string()
        } else {
            entry.threadtime()
        };
        if writeln!(out, "{line}").is_err() {
            return false;
        }
    }
    out.flush().is_ok()
}

/// Prints the guest log of the caller's Android as `filter` picks it: what the service holds,
/// then each new record until the output closes; with `dump`, what Android's log holds now.
pub fn logcat(
    client: HostApiClient,
    filter: &LogFilter,
    dump: bool,
    as_json: bool,
    out: &mut dyn Write,
) -> Result<String, String> {
    let mut reader = client
        .watch_log(filter, dump)
        .map_err(|e| format!("Android's log cannot be read: {e}"))?;
    loop {
        match reader.next(Duration::from_secs(1)) {
            Ok(None) => {}
            Ok(Some(batch)) => {
                if !write_entries(out, &batch.entries, as_json) {
                    return Ok(String::new());
                }
                if batch.end {
                    return match batch.error {
                        Some(why) => Err(format!("the log ended early: {why}")),
                        None => Ok(String::new()),
                    };
                }
                if let Some(why) = batch.error {
                    eprintln!("dsa: {why}");
                }
            }
            Err(error) => return Err(format!("the log ended: {error}")),
        }
    }
}

/// Saves the diagnostics of the caller's Android and of DSA on this computer to `path`, as one
/// zip archive. A file that could not be finished is removed.
pub fn collect(client: HostApiClient, path: &Path) -> Result<String, String> {
    let mut reader = client
        .collect_diagnostics()
        .map_err(|e| format!("the diagnostics cannot be gathered: {e}"))?;
    let file = std::fs::File::create(path)
        .map_err(|e| format!("{} cannot be written: {e}", path.display()))?;
    let mut out = std::io::BufWriter::new(file);
    match reader.save(&mut out, COLLECT_IDLE) {
        Ok(size) => {
            drop(out);
            Ok(format!(
                "Saved the diagnostics to {} ({} KiB)",
                path.display(),
                size.div_ceil(1024)
            ))
        }
        Err(error) => {
            drop(out);
            let _ = std::fs::remove_file(path);
            Err(format!("the diagnostics were not saved: {error}"))
        }
    }
}
