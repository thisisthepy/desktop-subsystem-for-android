//! Starting and stopping the guest container inside WSL.
//!
//! The guest is an LXC container in a WSL distribution. [`WslLauncher`] runs the container
//! tools in that distribution through `wsl.exe`, as root, without a shell in between
//! unless a command asks for one. Running a command through `wsl.exe` also starts the WSL
//! VM if it is not running yet.
//!
//! This module uses only `std::process`, so its tests run on any platform against a stand-in
//! for `wsl.exe`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::session::GuestLauncher;

/// Where `{timeout}` appears in a stop command, the stop timeout in whole seconds goes.
pub const TIMEOUT_PLACEHOLDER: &str = "{timeout}";

/// Starts and stops the guest container through `wsl.exe`.
#[derive(Debug, Clone)]
pub struct WslLauncher {
    /// The `wsl.exe` to run.
    pub wsl: PathBuf,
    /// The WSL distribution that holds the container.
    pub distribution: String,
    /// Starts the container; must exit 0 on success.
    pub start: Vec<String>,
    /// Asks the container to stop. `{timeout}` is replaced by the stop timeout in seconds.
    pub stop: Vec<String>,
    /// Exits 0 while any process of the container is still running, non-zero otherwise.
    pub is_running: Vec<String>,
    /// How often `stop` checks whether the container has ended.
    pub poll_interval: Duration,
}

impl WslLauncher {
    /// A launcher for the LXC container `container` in `distribution`.
    ///
    /// Container names are limited to letters, digits, `.`, `_` and `-`, because the name
    /// goes into a shell command for the running check.
    pub fn lxc(distribution: &str, container: &str) -> Result<WslLauncher, String> {
        let valid = !container.is_empty()
            && container
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !valid {
            return Err(format!(
                "{container:?} is not a usable container name (letters, digits, '.', '_', '-')"
            ));
        }
        let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
        Ok(WslLauncher {
            wsl: PathBuf::from("wsl.exe"),
            distribution: distribution.to_string(),
            start: strings(&["lxc-start", "-n", container]),
            stop: strings(&["lxc-stop", "-n", container, "-t", TIMEOUT_PLACEHOLDER]),
            is_running: strings(&[
                "sh",
                "-c",
                &format!("lxc-info -n {container} -s | grep -q RUNNING"),
            ]),
            poll_interval: Duration::from_millis(250),
        })
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.wsl);
        let mut all: Vec<OsString> = vec![
            "-d".into(),
            self.distribution.clone().into(),
            "-u".into(),
            "root".into(),
            "--exec".into(),
        ];
        all.extend(args.iter().map(OsString::from));
        command.args(all).stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: the host is a background service, not a console program.
            command.creation_flags(0x0800_0000);
        }
        command
    }

    fn run(&self, args: &[String]) -> Result<Output, String> {
        self.command(args)
            .output()
            .map_err(|error| format!("could not run {}: {error}", self.wsl.display()))
    }

    fn describe_failure(&self, what: &str, output: &Output) -> String {
        let mut message = format!("{what} failed ({})", output.status);
        for stream in [&output.stderr, &output.stdout] {
            let text = decode(stream);
            let text = text.trim();
            if !text.is_empty() {
                message.push_str(": ");
                message.push_str(text);
                break;
            }
        }
        message
    }

    fn running(&self) -> Result<bool, String> {
        Ok(self.run(&self.is_running)?.status.success())
    }
}

impl GuestLauncher for WslLauncher {
    fn start(&mut self) -> Result<(), String> {
        let output = self.run(&self.start)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(self.describe_failure(&self.start.join(" "), &output))
        }
    }

    fn stop(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        if !self.running()? {
            return Ok(());
        }
        let seconds = timeout.as_secs().max(1).to_string();
        let stop: Vec<String> = self
            .stop
            .iter()
            .map(|arg| arg.replace(TIMEOUT_PLACEHOLDER, &seconds))
            .collect();
        let output = self.run(&stop)?;
        let stop_failure =
            (!output.status.success()).then(|| self.describe_failure(&stop.join(" "), &output));
        loop {
            if !self.running()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                let mut message = format!(
                    "the container still has running processes after {} s",
                    timeout.as_secs()
                );
                if let Some(failure) = stop_failure {
                    message.push_str("; ");
                    message.push_str(&failure);
                }
                return Err(message);
            }
            thread::sleep(self.poll_interval);
        }
    }
}

/// Decodes command output. `wsl.exe` writes its own messages as UTF-16LE; what Linux
/// commands print comes through as UTF-8.
pub fn decode(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2
        && bytes.len() % 2 == 0
        && bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count() * 2 >= bytes.len() / 2;
    if looks_utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .to_string()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}
