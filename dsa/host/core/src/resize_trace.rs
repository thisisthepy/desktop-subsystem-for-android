//! A trace of where the time goes between a task window taking a new size and the window
//! showing a frame of that size.
//!
//! Every process on the path (the app window host, the window manager, the service, and in
//! the guest the agent and the hwcomposer) writes lines of the same shape to its log:
//!
//! ```text
//! resize-trace task=7 size=800x600 stage=app.view utc=2026-10-26T06:23:03.123Z mono=1234.567 ...
//! ```
//!
//! `utc` is the wall clock, so lines from the host and the guest can be merged; the two
//! clocks are not synchronised, so a gap between a host line and a guest line includes their
//! offset. `mono` is milliseconds on the process's own monotonic clock, for gaps within one
//! process.
//!
//! The trace stays quiet in normal use. A summary line (`stage=...summary`) comes at most
//! once per task every [`SUMMARY_INTERVAL`], and says how many it left out. The per-stage
//! lines come only when detail is turned on ([`detail_requested`]), and then at most once
//! per task, stage and size in a row, so a stage repeated for every frame of one size is
//! logged once.

use std::collections::HashMap;
use std::fmt::Write;
use std::time::{Duration, Instant, SystemTime};

/// The word every trace line starts with, to find them in a log.
pub const TAG: &str = "resize-trace";

/// The environment variable that turns on the per-stage lines: any value but empty or `0`.
pub const DETAIL_ENV: &str = "DSA_RESIZE_TRACE";

/// The name of the file whose presence turns on the per-stage lines, in the DSA folder the
/// process keeps its settings in.
pub const DETAIL_FILE: &str = "resize-trace";

/// The shortest gap between two summary lines of one task.
pub const SUMMARY_INTERVAL: Duration = Duration::from_millis(500);

/// Whether the per-stage lines are wanted: the environment variable [`DETAIL_ENV`] holds
/// `env_value`, and the file [`DETAIL_FILE`] exists or not.
pub fn detail_requested(env_value: Option<&str>, file_exists: bool) -> bool {
    let env = env_value.is_some_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0"
    });
    env || file_exists
}

/// The wall clock as a time since the Unix epoch.
pub fn wall_now() -> Duration {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
}

/// `since_epoch` as an ISO 8601 UTC time with milliseconds: `2026-10-26T06:23:03.123Z`.
pub fn utc_stamp(since_epoch: Duration) -> String {
    let secs = since_epoch.as_secs();
    let millis = since_epoch.subsec_millis();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let of_day = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        of_day / 3600,
        (of_day / 60) % 60,
        of_day % 60
    )
}

/// The calendar date `days` days after 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Decides which trace lines one process writes, and formats them.
#[derive(Debug)]
pub struct ResizeTrace {
    detailed: bool,
    origin: Instant,
    /// The size each task's stage was last logged at.
    stages: HashMap<(i32, &'static str), (u32, u32)>,
    /// When each task's last summary went out, and how many were left out since.
    summaries: HashMap<i32, (Instant, usize)>,
}

impl ResizeTrace {
    /// A trace whose monotonic clock starts at `origin`. `detailed` turns on the per-stage
    /// lines.
    pub fn new(detailed: bool, origin: Instant) -> ResizeTrace {
        ResizeTrace {
            detailed,
            origin,
            stages: HashMap::new(),
            summaries: HashMap::new(),
        }
    }

    /// Whether the per-stage lines are on.
    pub fn detailed(&self) -> bool {
        self.detailed
    }

    /// The line for `stage` of the resize of `task_id` to `size`, or `None` when detail is
    /// off or this stage was last logged at the same size.
    pub fn stage(
        &mut self,
        task_id: i32,
        stage: &'static str,
        size: (u32, u32),
        detail: &str,
        now: Instant,
        wall: Duration,
    ) -> Option<String> {
        if !self.detailed {
            return None;
        }
        if self.stages.insert((task_id, stage), size) == Some(size) {
            return None;
        }
        Some(self.line(task_id, stage, size, detail, now, wall))
    }

    /// The summary line `stage` for the resize of `task_id` to `size`, or `None` when the
    /// task's last summary went out less than [`SUMMARY_INTERVAL`] ago. A line after some
    /// were left out says how many (`skipped=`).
    pub fn summary(
        &mut self,
        task_id: i32,
        stage: &'static str,
        size: (u32, u32),
        detail: &str,
        now: Instant,
        wall: Duration,
    ) -> Option<String> {
        let skipped = match self.summaries.get_mut(&task_id) {
            Some((last, skipped)) if now.saturating_duration_since(*last) < SUMMARY_INTERVAL => {
                *skipped += 1;
                return None;
            }
            Some((_, skipped)) => *skipped,
            None => 0,
        };
        self.summaries.insert(task_id, (now, 0));
        let mut detail = detail.to_string();
        if skipped > 0 {
            if !detail.is_empty() {
                detail.push(' ');
            }
            let _ = write!(detail, "skipped={skipped}");
        }
        Some(self.line(task_id, stage, size, &detail, now, wall))
    }

    /// The task has ended: what was kept for it goes.
    pub fn forget(&mut self, task_id: i32) {
        self.stages.retain(|(task, _), _| *task != task_id);
        self.summaries.remove(&task_id);
    }

    fn line(
        &self,
        task_id: i32,
        stage: &str,
        (width, height): (u32, u32),
        detail: &str,
        now: Instant,
        wall: Duration,
    ) -> String {
        let mono = now.saturating_duration_since(self.origin);
        let mut line = format!(
            "{TAG} task={task_id} size={width}x{height} stage={stage} utc={} mono={}.{:03}",
            utc_stamp(wall),
            mono.as_millis(),
            mono.subsec_micros() % 1000
        );
        if !detail.is_empty() {
            line.push(' ');
            line.push_str(detail);
        }
        line
    }
}

/// `duration` in milliseconds with one decimal, for a trace line's detail.
pub fn millis(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn fr4_trace_times_are_utc_with_milliseconds() {
        assert_eq!(utc_stamp(Duration::ZERO), "1970-01-01T00:00:00.000Z");
        // 2026-10-26 06:23:03.123 UTC.
        assert_eq!(
            utc_stamp(Duration::from_millis(1_792_995_783_123)),
            "2026-10-26T06:23:03.123Z"
        );
        // A leap day.
        assert_eq!(
            utc_stamp(Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00.000Z"
        );
    }

    #[test]
    fn fr4_stage_lines_are_off_unless_asked_for() {
        assert!(!detail_requested(None, false));
        assert!(!detail_requested(Some(""), false));
        assert!(!detail_requested(Some("0"), false));
        assert!(detail_requested(Some("1"), false));
        assert!(detail_requested(None, true));

        let start = Instant::now();
        let mut trace = ResizeTrace::new(false, start);
        assert_eq!(
            trace.stage(1, "app.view", (640, 480), "", start, ms(0)),
            None
        );
    }

    #[test]
    fn fr4_a_stage_is_logged_once_per_size() {
        let start = Instant::now();
        let mut trace = ResizeTrace::new(true, start);
        let line = trace
            .stage(
                7,
                "app.present",
                (800, 600),
                "took=1.0",
                start + ms(1234),
                ms(0),
            )
            .unwrap();
        assert_eq!(
            line,
            "resize-trace task=7 size=800x600 stage=app.present \
             utc=1970-01-01T00:00:00.000Z mono=1234.000 took=1.0"
        );
        // Every frame of that size presents again; only the first is logged.
        assert_eq!(
            trace.stage(7, "app.present", (800, 600), "", start + ms(1250), ms(0)),
            None
        );
        // Another stage, another task, or another size is.
        assert!(trace
            .stage(7, "app.view", (800, 600), "", start, ms(0))
            .is_some());
        assert!(trace
            .stage(8, "app.present", (800, 600), "", start, ms(0))
            .is_some());
        assert!(trace
            .stage(7, "app.present", (640, 480), "", start, ms(0))
            .is_some());
        // Back to a size it had before is a new resize.
        assert!(trace
            .stage(7, "app.present", (800, 600), "", start, ms(0))
            .is_some());
        trace.forget(7);
        assert!(trace
            .stage(7, "app.present", (800, 600), "", start, ms(0))
            .is_some());
    }

    #[test]
    fn fr4_summaries_are_rate_limited_and_count_what_they_left_out() {
        let start = Instant::now();
        let mut trace = ResizeTrace::new(false, start);
        assert!(trace
            .summary(3, "app.summary", (500, 400), "waited=80.0", start, ms(0))
            .is_some());
        assert_eq!(
            trace.summary(3, "app.summary", (510, 400), "", start + ms(100), ms(0)),
            None
        );
        assert_eq!(
            trace.summary(3, "app.summary", (520, 400), "", start + ms(200), ms(0)),
            None
        );
        // Another task has its own allowance.
        assert!(trace
            .summary(4, "app.summary", (520, 400), "", start + ms(200), ms(0))
            .is_some());
        let line = trace
            .summary(
                3,
                "app.summary",
                (530, 400),
                "waited=90.0",
                start + SUMMARY_INTERVAL,
                ms(0),
            )
            .unwrap();
        assert!(line.ends_with("waited=90.0 skipped=2"), "{line}");
        assert!(line.contains("size=530x400 stage=app.summary"), "{line}");
    }
}
