//! The guest's serial console: kept in the user's console log, and its last lines for a start
//! that fails.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use std::time::SystemTime;

use dsa_host_core::boot_signs::BootSigns;
use dsa_host_core::console_log::rotate_if_large;

/// How many of the last lines are kept for a report.
pub const TAIL_LINES: usize = 40;
/// A console log larger than this is moved to `<name>.1` when a boot starts.
const ROTATE_BYTES: u64 = 8 << 20;

/// The last lines of a console, shared with the thread that reads it.
#[derive(Default, Clone)]
pub struct Tail(Arc<Mutex<VecDeque<String>>>);

impl Tail {
    pub fn push(&self, line: String) {
        let mut lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == TAIL_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn text(&self) -> String {
        let lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// Whether any kept line contains `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        let lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        lines.iter().any(|line| line.contains(needle))
    }
}

/// Reads one boot's console from `source` until it ends, into `log` and `tail`.
pub fn capture(
    source: impl Read + Send + 'static,
    log: Option<PathBuf>,
    header: Option<String>,
    tail: Tail,
    signs: Arc<Mutex<BootSigns>>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("dsa-console".into())
        .spawn(move || {
            let mut file = log.as_deref().and_then(open_log);
            if let (Some(file), Some(header)) = (file.as_mut(), header) {
                let _ = write!(file, "{header}");
            }
            let mut reader = BufReader::new(source);
            let mut bytes = Vec::new();
            loop {
                bytes.clear();
                match reader.read_until(b'\n', &mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&bytes)
                            .trim_end_matches(['\r', '\n'])
                            .to_string();
                        if let Some(file) = file.as_mut() {
                            let _ = writeln!(file, "{line}");
                        }
                        signs
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .read_line(&line);
                        tail.push(line);
                    }
                }
            }
        })
}

fn open_log(path: &Path) -> Option<std::fs::File> {
    let _ = rotate_if_large(path, ROTATE_BYTES);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// The line that starts a boot's entry in the console log.
pub fn boot_header(started: SystemTime, version: Option<&str>, cmdline: &str) -> String {
    dsa_host_core::console_log::boot_header(crate::VERSION, started, version, cmdline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr21_the_console_is_kept_and_its_last_lines_reported() {
        let dir = std::env::temp_dir().join(format!("dsa-console-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("console.log");
        let _ = std::fs::remove_file(&log);
        let text: String = (0..50).map(|i| format!("line {i}\r\n")).collect();
        let tail = Tail::default();
        capture(
            std::io::Cursor::new(text.into_bytes()),
            Some(log.clone()),
            Some(boot_header(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1),
                Some("16.0.0-1"),
                "console=ttyS0",
            )),
            tail.clone(),
            Arc::default(),
        )
        .unwrap()
        .join()
        .unwrap();
        let kept = std::fs::read_to_string(&log).unwrap();
        assert!(kept.starts_with("==== DSA "));
        assert!(kept.contains("1970-01-01T00:00:01Z"));
        assert!(kept.contains("line 0\nline 1\n"));
        assert_eq!(tail.text().lines().count(), TAIL_LINES);
        assert!(tail.text().ends_with("line 49"));
        assert!(tail.contains("line 10") && !tail.contains("line 9\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
