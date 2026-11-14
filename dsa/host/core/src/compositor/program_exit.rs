//! How an app's window program ended, and whether its windows come back.
//!
//! A window program that ends because the user ended it (from Task Manager, say) or because
//! its last window closed takes its windows with it, and Android ends their tasks, as any
//! program's windows close with it. A window program that crashed is another matter: the
//! user did nothing, and the app's tasks are still running in Android. So the window manager
//! starts the program again and opens its windows for the same tasks, which keep running.
//!
//! A crash is told apart by the program's exit code: Windows ends a program that an exception
//! took down with the exception's status (`0xC0000005` for an access violation, `0xC0000409`
//! for a fail-fast or an abort, `0xC00000FD` for a stack overflow), whose top two bits are
//! set. A program ended by the user ends with the code given to `TerminateProcess` (1 from
//! Task Manager), and one that returned with its own (0 or 1).
//!
//! A program that crashes again and again would come back for ever; [`CrashRestarts`] lets an
//! app's program start again [`RESTARTS`] times in [`RESTART_WINDOW`], and closes its windows
//! after that, as before.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How many times one app's window program may start again after a crash in
/// [`RESTART_WINDOW`].
pub const RESTARTS: usize = 3;

/// The time the restarts of one app's window program are counted over.
pub const RESTART_WINDOW: Duration = Duration::from_secs(120);

/// `STATUS_CONTROL_C_EXIT`: the program was ended by Ctrl+C or Ctrl+Break, or the session
/// ended. It has the severity of an error but is no crash.
const CONTROL_C_EXIT: u32 = 0xC000_013A;

/// Whether a program that ended with `exit_code` was taken down by an exception.
pub fn ended_by_crash(exit_code: u32) -> bool {
    exit_code & 0xC000_0000 == 0xC000_0000 && exit_code != CONTROL_C_EXIT
}

/// The exit code, in words for the log.
pub fn describe_exit(exit_code: u32) -> String {
    let what = match exit_code {
        0 => "it ended by itself",
        1 => "it was ended, or ended with an error",
        0xC000_0005 => "an access violation",
        0xC000_0374 => "a corrupted heap",
        0xC000_0409 => "a fail-fast or an abort",
        0xC000_00FD => "a stack overflow",
        0xC000_001D => "an illegal instruction",
        0xC000_0094 => "a division by zero",
        0xE06D_7363 => "an uncaught C++ exception",
        CONTROL_C_EXIT => "Ctrl+C or the end of the session",
        code if ended_by_crash(code) => "an exception",
        _ => "its own exit code",
    };
    format!("exit code 0x{exit_code:08X}, {what}")
}

/// The restarts after a crash that each app's window program has had lately.
#[derive(Debug, Default)]
pub struct CrashRestarts {
    restarts: BTreeMap<String, Vec<Instant>>,
}

impl CrashRestarts {
    pub fn new() -> CrashRestarts {
        CrashRestarts::default()
    }

    /// The window program of `package` crashed at `now`. True when it may start again with
    /// its windows; false when it has crashed [`RESTARTS`] times in [`RESTART_WINDOW`]
    /// already, and its windows close.
    pub fn may_restart(&mut self, package: &str, now: Instant) -> bool {
        let times = self.restarts.entry(package.to_string()).or_default();
        times.retain(|at| now.saturating_duration_since(*at) < RESTART_WINDOW);
        if times.len() >= RESTARTS {
            return false;
        }
        times.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr5_an_exception_that_took_a_window_program_down_is_a_crash() {
        for code in [
            0xC000_0005,
            0xC000_0409,
            0xC000_00FD,
            0xC000_0374,
            0xE06D_7363,
        ] {
            assert!(ended_by_crash(code), "0x{code:08X}");
        }
    }

    #[test]
    fn nfr5_a_window_program_the_user_ended_or_that_returned_is_no_crash() {
        // Returned by itself, ended from Task Manager (1), and Ctrl+C or a session's end.
        for code in [0, 1, 2, 259, 0xC000_013A] {
            assert!(!ended_by_crash(code), "0x{code:08X}");
        }
    }

    #[test]
    fn nfr5_the_log_names_the_exception() {
        assert_eq!(
            describe_exit(0xC000_0005),
            "exit code 0xC0000005, an access violation"
        );
        assert_eq!(
            describe_exit(0xC000_0409),
            "exit code 0xC0000409, a fail-fast or an abort"
        );
        assert_eq!(
            describe_exit(0xC0DE_0001),
            "exit code 0xC0DE0001, an exception"
        );
    }

    #[test]
    fn nfr5_a_crashed_window_program_starts_again_a_few_times_then_its_windows_close() {
        let mut restarts = CrashRestarts::new();
        let start = Instant::now();
        for n in 0..RESTARTS {
            assert!(
                restarts.may_restart("com.example.video", start + Duration::from_secs(n as u64))
            );
        }
        // One crash too many in the window: the windows close.
        assert!(!restarts.may_restart("com.example.video", start + Duration::from_secs(10)));
        // Another app's program is counted on its own.
        assert!(restarts.may_restart("com.example.maps", start + Duration::from_secs(10)));
        // Once the old crashes are out of the window, it may start again.
        assert!(restarts.may_restart(
            "com.example.video",
            start + RESTART_WINDOW + Duration::from_secs(3)
        ));
    }
}
