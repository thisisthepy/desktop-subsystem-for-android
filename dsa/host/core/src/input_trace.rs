//! A trace of where a touch's time goes on its way to the screen.
//!
//! Every process on the path (the app window host and the window manager here, and in the
//! guest the agent, the hwcomposer and the Vulkan driver) writes lines of the same shape to
//! its log:
//!
//! ```text
//! input-trace stage=app.pointer task=7 utc=2026-10-26T06:23:03.123Z mono=1234.567 ...
//! ```
//!
//! `utc` is the wall clock, so lines from the host and the guest can be merged; the two
//! clocks are not synchronised, so a gap between a host line and a guest line includes their
//! offset. `mono` is milliseconds on the process's own monotonic clock, for gaps within one
//! process. The trace writes nothing unless it is asked for.

use std::time::Duration;

use crate::resize_trace::utc_stamp;

/// The word every trace line starts with, to find them in a log.
pub const TAG: &str = "input-trace";

/// The environment variable that turns the host's lines on: any value but empty or `0`.
pub const DETAIL_ENV: &str = "DSA_INPUT_TRACE";

/// The name of the file whose presence turns the host's lines on, in the DSA folder the
/// process keeps its settings in.
pub const DETAIL_FILE: &str = "input-trace";

/// One trace line. `mono` is the time on the process's monotonic clock, `wall` the time since
/// the Unix epoch, and `detail` the stage's own `key=value` fields.
pub fn line(stage: &str, task_id: i32, detail: &str, mono: Duration, wall: Duration) -> String {
    let mut line = format!(
        "{TAG} stage={stage} task={task_id} utc={} mono={:.3}",
        utc_stamp(wall),
        mono.as_secs_f64() * 1000.0
    );
    if !detail.is_empty() {
        line.push(' ');
        line.push_str(detail);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr5_a_trace_line_names_its_stage_task_and_both_clocks() {
        let line = line(
            "app.present",
            7,
            "took=1.5",
            Duration::from_micros(1_234_567),
            Duration::from_secs(1),
        );
        assert_eq!(
            line,
            "input-trace stage=app.present task=7 utc=1970-01-01T00:00:01.000Z mono=1234.567 took=1.5"
        );
    }
}
