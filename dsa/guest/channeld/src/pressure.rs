//! Telling the host how much memory the guest has, so the host can grow the VM's memory when
//! the guest runs short and take it back when the guest no longer needs it.
//!
//! The daemon samples `/proc/meminfo` and `/proc/pressure/memory` every couple of seconds. It
//! sends a `GuestMemoryPressure` at once while the guest is short, so the host adds memory
//! before apps are killed, at once when a lot of memory has come free (an app closed), so the
//! host takes back what it added without waiting, and every 30 seconds otherwise. This module is the decision and the message; the daemon reads
//! the files.

use std::time::{Duration, Instant};

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{Envelope, GuestMemoryPressure};

/// How often the daemon samples the guest's memory.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// How often a report goes out while the guest is not short.
pub const CALM_INTERVAL: Duration = Duration::from_secs(30);

/// How much more available memory than the last report sends one at once, in KiB.
pub const FREED_KB: u64 = 128 * 1024;

/// One sample of the guest's memory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    /// PSI `some avg10`, in percent; 0 when it cannot be read.
    pub stall_percent: f64,
}

impl Sample {
    /// Reads `/proc/meminfo` and `/proc/pressure/memory` text. Values that cannot be read
    /// are 0.
    pub fn parse(meminfo: &str, psi: &str) -> Sample {
        let field = |name: &str| {
            meminfo
                .lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
                .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let stall_percent = psi
            .lines()
            .find_map(|line| line.strip_prefix("some "))
            .and_then(|rest| {
                rest.split_whitespace()
                    .find_map(|f| f.strip_prefix("avg10="))
            })
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        Sample {
            mem_total_kb: field("MemTotal"),
            mem_available_kb: field("MemAvailable"),
            stall_percent,
        }
    }

    /// Whether the guest is short: less than a quarter of its memory available (at least
    /// 768 MiB), or tasks stalled on memory for 5% of the last 10 seconds. A little earlier
    /// than the host grows, so the host hears it in time.
    pub fn short(&self) -> bool {
        let floor_kb = (self.mem_total_kb / 4).max(768 * 1024);
        self.mem_available_kb < floor_kb || self.stall_percent >= 5.0
    }

    /// The report for the host.
    pub fn frame(&self) -> Vec<u8> {
        let centipercent = (self.stall_percent * 100.0).round().clamp(0.0, 10_000.0);
        dsa_protocol::encode(&Envelope {
            body: Some(Body::GuestMemoryPressure(GuestMemoryPressure {
                mem_total_kb: self.mem_total_kb,
                mem_available_kb: self.mem_available_kb,
                stall_centipercent: centipercent as u32,
            })),
        })
    }
}

/// Decides when a sample goes to the host.
#[derive(Debug, Default)]
pub struct PressureReporter {
    last_sent: Option<Instant>,
    /// The available memory the last report said, in KiB.
    last_available_kb: u64,
}

impl PressureReporter {
    /// Whether to send `sample` now: every sample while the guest is short, every sample
    /// that finds [`FREED_KB`] more available than the last report, else once per
    /// [`CALM_INTERVAL`].
    pub fn due(&mut self, sample: &Sample, now: Instant) -> bool {
        let due = sample.short()
            || sample.mem_available_kb >= self.last_available_kb.saturating_add(FREED_KB)
            || self
                .last_sent
                .is_none_or(|at| now.saturating_duration_since(at) >= CALM_INTERVAL);
        if due {
            self.last_sent = Some(now);
            self.last_available_kb = sample.mem_available_kb;
        }
        due
    }
}
