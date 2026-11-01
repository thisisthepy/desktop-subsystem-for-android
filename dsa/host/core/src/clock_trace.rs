//! Pairs the host's clock with the guest's, so the input trace's host and guest lines can be
//! put on one time line.
//!
//! Each host process stamps its trace lines with `host=`, the host's monotonic clock, which
//! every host process shares. The guest's lines carry `mono=`, the guest's `CLOCK_MONOTONIC`.
//! While the trace is on, the session sends the guest's control daemon a
//! [`ClockProbe`](dsa_protocol::control::ClockProbe) once a second with its send time; the
//! daemon answers at once with its own clock, and the session logs one `clock` line per
//! answer:
//!
//! ```text
//! input-trace stage=clock task=0 utc=... mono=... host=1000.400 host_sent=1000.000 guest=500.000 host_received=1000.400 rtt=0.400 offset=-500.200
//! ```
//!
//! `offset` is the guest clock minus the midpoint of the probe's trip, so a guest time `g`
//! is `g - offset` on the host clock, give or take half of `rtt`. A tool that merges the logs
//! picks the answers with the smallest `rtt`.
//!
//! Nothing here names a host OS API: the platform hands in its clock and where lines go.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::input_trace;
use crate::resize_trace::wall_now;

/// How often the session probes the guest's clock while the trace is on.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);

/// The host's monotonic clock, the same in every host process (on Windows, the performance
/// counter), as time since its own origin.
pub trait HostClock: Send + Sync {
    fn now(&self) -> Duration;
}

/// What the session needs to pair the clocks: whether the trace is on for the guest it runs,
/// the host's clock, and where the `clock` lines go.
#[derive(Clone)]
pub struct ClockTrace {
    /// Whether the guest the session runs traces input. The platform sets it each time it
    /// builds the guest's configuration; the session reads it before every probe.
    pub on: Arc<AtomicBool>,
    pub clock: Arc<dyn HostClock>,
    /// Takes each `clock` line.
    pub sink: Arc<dyn Fn(&str) + Send + Sync>,
    /// The time between probes; [`PROBE_INTERVAL`] outside tests.
    pub interval: Duration,
    /// The origin of the lines' `mono`, this process's own clock.
    pub started: Instant,
}

impl ClockTrace {
    pub fn new(
        on: Arc<AtomicBool>,
        clock: Arc<dyn HostClock>,
        sink: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> ClockTrace {
        ClockTrace {
            on,
            clock,
            sink,
            interval: PROBE_INTERVAL,
            started: Instant::now(),
        }
    }

    /// Whether the session probes now.
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// Logs the line for an answer to a probe that left the host at `host_sent` and found
    /// the guest's clock at `guest`, now that it is back.
    pub fn answered(&self, host_sent: Duration, guest: Duration) {
        let line = clock_line(
            host_sent,
            guest,
            self.clock.now(),
            self.started.elapsed(),
            wall_now(),
        );
        (self.sink)(&line);
    }
}

fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.0
}

/// The `clock` line for one probe: sent at `host_sent` and back at `host_received` on the
/// host clock, read at `guest` on the guest's. `mono` and `wall` stamp the line as every
/// trace line is stamped.
pub fn clock_line(
    host_sent: Duration,
    guest: Duration,
    host_received: Duration,
    mono: Duration,
    wall: Duration,
) -> String {
    let rtt = ms(host_received) - ms(host_sent);
    let offset = ms(guest) - (ms(host_sent) + ms(host_received)) / 2.0;
    input_trace::line(
        "clock",
        0,
        &format!(
            "host_sent={:.3} guest={:.3} host_received={:.3} rtt={rtt:.3} offset={offset:.3}",
            ms(host_sent),
            ms(guest),
            ms(host_received)
        ),
        mono,
        wall,
        host_received,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr5_a_clock_line_gives_the_round_trip_and_the_guests_offset_from_its_midpoint() {
        let line = clock_line(
            Duration::from_millis(1000),
            Duration::from_millis(500),
            Duration::from_micros(1_000_400),
            Duration::from_millis(3),
            Duration::from_secs(1),
        );
        assert_eq!(
            line,
            "input-trace stage=clock task=0 utc=1970-01-01T00:00:01.000Z mono=3.000 \
             host=1000.400 host_sent=1000.000 guest=500.000 host_received=1000.400 \
             rtt=0.400 offset=-500.200"
        );
    }
}
